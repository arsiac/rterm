//! 终端标签模块：标签生命周期与标签栏导航（切换 / 关闭 / 列表 dropdown / 窗口焦点）

use crate::app::sftp;
use crate::message::ResizeSender;
use crate::state::TerminalTab;
use crate::t;
use crate::terminal_pane;
use crate::widget::term::Event as TerminalEvent;
use iced::Task;
use iced::widget::Id;
use rterm_core::{ConnectionStatus, SshConnection};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 终端桥接就绪后回传的结果：conout 读端、conin 写端、断开标志与尺寸发送端。
/// 抽成别名以免 `TerminalOpened` 变体与 `terminal_opened` 参数触发 `type_complexity`。
type BridgeResult = Result<
    (
        Arc<std::fs::File>,
        Arc<std::fs::File>,
        Arc<AtomicBool>,
        ResizeSender,
    ),
    String,
>;

/// 标签模块只读上下文：来自父层 `App` 的共享导航态（供联动写回判定）。
/// 每次 `update` 前重建，确保读到最新父状态；仅持有 owned 数据，不借用 `App`，
/// 以免与 `self.tabs` 的可变借用冲突。SFTP 视图的查询经 `update` 的 `sftp` 参数单独传入。
pub struct Ctx {
    /// 中心面板当前内容（决定切标签时是否自动开 SFTP）。
    pub center: crate::state::CenterView,
    /// 当前活动会话（关标签后用于重算活动会话指针）。
    pub active_session: Option<String>,
    /// 终端是否聚焦（窗口焦点变化时还原 / 保存）。
    pub terminal_focused: bool,
    /// 窗口失焦前保存的终端聚焦态（窗口焦点变化时还原 / 保存）。
    pub window_focus_saved: Option<bool>,
}

/// 标签模块内部状态：标签列表与标签栏导航态（仅模块自身可变）。
pub struct State {
    /// 终端标签页列表。
    pub(crate) tabs: Vec<TerminalTab>,
    /// 当前活动标签 id。
    pub(crate) active_tab: Option<u64>,
    /// 下一个标签自增 id。
    pub(crate) next_tab_id: u64,
    /// 标签栏左侧标签列表 dropdown 是否展开。
    pub(crate) show_tab_list: bool,
    /// 标签栏水平 scrollable 的部件 id，供 dropdown 选签后程序化滚动定位。
    pub(crate) tab_bar_scroll: Id,
}

/// 标签模块内部消息：UI 意图与后台任务结果，由父层经 `Message::Tabs` 路由进本模块。
#[derive(Clone)]
pub enum Message {
    /// 选择某标签：置活动 + 聚焦 + 联动会话，必要时自动开 SFTP。
    SelectTab(u64),
    /// 从列表 dropdown 切到某标签：收起列表并滚动定位（复用 SelectTab 逻辑）。
    SwitchTab(u64),
    /// 关闭标签：移除并清理，更新活动指针与导航态。
    CloseTab(u64),
    /// 应用窗口关闭请求：置位所有标签的桥接断开标志，让后台 pump / 线程尽快退出。
    WindowClosing,
    /// 切换标签列表 dropdown 显隐。
    ToggleTabList,
    /// 终端桥接就绪：挂载终端组件（父层执行）。
    TerminalOpened(u64, BridgeResult),
    /// 终端桥接断开：置标签为 Error。
    TerminalDisconnected(u64),
    /// 终端挂载完成：强制刷新首屏。
    TerminalReady(u64),
    /// 终端部件事件（键盘 / 鼠标 / 后端回调），转发父层处理。
    Terminal(TerminalEvent),
    /// SSH 连接结果回流：成功拉起桥接，失败置错。
    SessionConnected(u64, String, Result<Arc<SshConnection>, String>),
    /// 窗口焦点变化：保存 / 还原终端聚焦态。
    WindowFocused(bool),
}

/// 标签模块上行事件：需父层配合的副作用。模块绝不写 `App`。
#[derive(Clone)]
pub enum Event {
    /// 写回当前活动会话（切标签 / 连接成功 / 关标签后重算）。
    SetActiveSession(Option<String>),
    /// 写回中心视图（关到最后一个标签时回到会话管理）。
    SetCenter(crate::state::CenterView),
    /// 写回终端聚焦态（切标签 / 窗口焦点变化 / 终端就绪）。
    SetTerminalFocused(bool),
    /// 写回窗口失焦前保存的聚焦态（窗口焦点变化）。
    SetWindowFocusSaved(Option<bool>),
    /// 写回状态栏提示。
    SetStatus(String),
    /// 自动打开该会话的 SFTP（切到文件管理且尚未打开时）。
    OpenSftp(String),
    /// 为已连接标签拉起终端桥接（父层执行）。
    OpenTerminalBridge(u64, Arc<SshConnection>),
    /// 桥接就绪后挂载终端组件（父层执行）。
    SpawnTerminal(
        u64,
        Arc<std::fs::File>,
        Arc<std::fs::File>,
        Arc<AtomicBool>,
        ResizeSender,
    ),
    /// 关闭标签时清理其挂起的主机密钥确认。
    RemoveHostKeyForTab(u64),
    /// 转发终端部件事件给父层（父层拥有的 widget 交互逻辑）。
    TerminalEvent(TerminalEvent),
    /// 终端挂载完成后强制重绘。
    TerminalReady(u64),
    /// 切标签后滚动标签栏到目标位置。
    ScrollTo(f32),
    /// 自回路：把模块内部消息派发回自身（SwitchTab 复用 SelectTab）。
    Emit(Box<Message>),
}

impl State {
    /// 构建空标签态：初始无标签、下一个 id 从 1 起、列表收起、生成唯一滚动 id。
    pub fn new() -> Self {
        State {
            tabs: Vec::new(),
            active_tab: None,
            next_tab_id: 1,
            show_tab_list: false,
            tab_bar_scroll: Id::unique(),
        }
    }

    /// 当前标签列表（只读，供渲染层使用）。
    pub(crate) fn list(&self) -> &[TerminalTab] {
        &self.tabs
    }

    /// 当前活动标签 id（只读）。
    pub(crate) fn active(&self) -> Option<u64> {
        self.active_tab
    }

    /// 标签列表 dropdown 是否展开（只读）。
    pub(crate) fn show_list(&self) -> bool {
        self.show_tab_list
    }

    /// 标签栏滚动部件 id（克隆，供 scroll_to 操作）。
    pub(crate) fn scroll_id(&self) -> Id {
        self.tab_bar_scroll.clone()
    }

    /// 置活动标签（open_files 等父层导航操作调用，模块自身逻辑直接用 `active_tab` 字段）。
    pub(crate) fn set_active(&mut self, id: u64) {
        self.active_tab = Some(id);
    }

    /// 取某标签的可变引用：父层（App）在挂载终端组件 / 桥接 / 热替换字体等
    /// 自身拥有的 widget 生命周期操作中经此修改标签内部字段，子模块不调用。
    pub(crate) fn tab_mut(&mut self, id: u64) -> Option<&mut TerminalTab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    /// 父层（App）拥有的 widget 操作（热替换字体 / 主题）经此迭代全部标签。
    pub(crate) fn list_mut(&mut self) -> &mut Vec<TerminalTab> {
        &mut self.tabs
    }

    /// 新建标签：自增 id、置连接中、追加到列表并设为活动标签；返回新标签 id。
    ///
    /// SFTP 视图随标签创建（由父层调用 `sftp.ensure` 完成，模块不持有 SFTP 态）。
    pub(crate) fn add(&mut self, session_id: String, title: String) -> u64 {
        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(TerminalTab {
            id: tab_id,
            session_id,
            status: ConnectionStatus::Connecting,
            error: None,
            conn: None,
            terminal: None,
            resize_tx: None,
            disconnect: None,
            title,
        });
        self.active_tab = Some(tab_id);
        tab_id
    }

    /// 关闭某会话的全部标签；若活动指针悬空则回退到最后一个标签（空则置 `None`）。
    pub(crate) fn remove_by_session(&mut self, id: &str) {
        self.tabs.retain(|t| t.session_id != id);
        if !self.tabs.iter().any(|t| Some(t.id) == self.active_tab) {
            self.active_tab = self.tabs.last().map(|t| t.id);
        }
    }

    /// 处理一条标签消息，返回需父层落地的事件流。
    ///
    /// `sftp` 仅用于「切到文件管理时判断某标签是否已开 SFTP」的查询（传入引用而非塞进 `Ctx`，
    /// 避免对 `App` 的整体借用与 `self.tabs` 的可变借用冲突）。
    pub fn update(&mut self, msg: Message, ctx: &Ctx, sftp: &sftp::State) -> Task<Event> {
        match msg {
            Message::SelectTab(tab_id) => self.select_tab(tab_id, ctx, sftp),
            Message::SwitchTab(tab_id) => self.switch_tab(tab_id),
            Message::CloseTab(tab_id) => self.close_tab(tab_id, ctx),
            Message::WindowClosing => self.window_closing(),
            Message::ToggleTabList => {
                self.show_tab_list = !self.show_tab_list;
                Task::none()
            }
            Message::TerminalOpened(tab_id, result) => self.terminal_opened(tab_id, result, ctx),
            Message::TerminalDisconnected(tab_id) => self.terminal_disconnected(tab_id),
            Message::TerminalReady(tab_id) => Task::done(Event::TerminalReady(tab_id)),
            Message::Terminal(event) => Task::done(Event::TerminalEvent(event)),
            Message::SessionConnected(tab_id, id, result) => {
                self.session_connected(tab_id, id, result)
            }
            Message::WindowFocused(focused) => self.window_focused(focused, ctx),
        }
    }

    /// 切换到指定终端标签：置为活动标签、落入键盘焦点，并联动会话、必要时自动打开 SFTP。
    fn select_tab(&mut self, tab_id: u64, ctx: &Ctx, sftp: &sftp::State) -> Task<Event> {
        self.active_tab = Some(tab_id);
        // 显式切到某终端标签即视为键盘焦点落入该终端（区别于仅切换标签栏的高亮选中）。
        let mut events = vec![Event::SetTerminalFocused(true)];
        let tab_info = self.tabs.iter().find(|t| t.id == tab_id).map(|t| {
            (
                t.session_id.clone(),
                t.status,
                sftp.tab_session(t.id).is_none(),
            )
        });
        if let Some((session_id, status, sftp_not_opened)) = tab_info {
            events.push(Event::SetActiveSession(Some(session_id.clone())));
            // 切换到文件管理视图时，若新标签尚未打开 SFTP 且它自己已连接，
            // 自动为其建立 SFTP 通道，使用户切换标签即可看到文件列表。
            if ctx.center == crate::state::CenterView::Files
                && sftp_not_opened
                && status == ConnectionStatus::Connected
            {
                events.push(Event::OpenSftp(session_id));
            }
        }
        Task::batch(events.into_iter().map(Task::done).collect::<Vec<_>>())
    }

    /// 切换到目标标签：收起列表并滚动到该标签使其可见，复用 SelectTab 的切换与联动逻辑。
    fn switch_tab(&mut self, tab_id: u64) -> Task<Event> {
        self.show_tab_list = false;
        // 目标标签的 x 偏移按其左侧全部标签的估算宽度累加（系数与渲染截断同源）；
        // 滚到标签左缘对齐可视区起点即可保证可见（单标签 ≤160px，远窄于标签栏）。
        let mut x = terminal_pane::TAB_ROW_LEFT_PADDING;
        for tab in &self.tabs {
            if tab.id == tab_id {
                break;
            }
            x += terminal_pane::estimated_tab_width(&terminal_pane::tab_label(&self.tabs, tab))
                + terminal_pane::TAB_SPACING;
        }
        Task::batch([
            Task::done(Event::Emit(Box::new(Message::SelectTab(tab_id)))),
            Task::done(Event::ScrollTo(x)),
        ])
    }

    /// 关闭标签：移除指定标签，清理连接与弹窗，并更新活动指针与导航态。
    fn close_tab(&mut self, tab_id: u64, ctx: &Ctx) -> Task<Event> {
        // 记录被关标签所属会话：关完后若活动会话指针仍指向它，需改指当前活动标签的会话。
        let closed_session = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .map(|t| t.session_id.clone());
        // 正常退出标签页（用户主动关闭）：记录标签与所属会话，便于排查资源残留 / 连接未释放。
        log::info!(
            "关闭标签页: 标签 {tab_id} 会话 {}",
            closed_session.as_deref().unwrap_or("<无>")
        );
        let mut events = vec![Event::RemoveHostKeyForTab(tab_id)];
        // 置位该标签的桥接断开标志，让核心层 pump 任务尽快退出（释放服务端管道句柄，
        // 进而使 win_io 后台读/写线程退出），避免关标签后进程残留。
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id)
            && let Some(d) = &tab.disconnect
        {
            d.store(true, Ordering::SeqCst);
        }
        // 本标签若有暂停在主机密钥弹窗上的握手，一并按拒绝处理，
        // 否则弹窗仍会挂在队列里等待一个已被关闭的连接（由父层执行清理）。
        self.tabs.retain(|t| t.id != tab_id);
        if self.active_tab == Some(tab_id) {
            self.active_tab = self.tabs.last().map(|t| t.id);
        }
        // 关闭最后一个标签后已无终端可显示，自动回到会话管理；
        // 同时清空当前会话指针。该标签自带的 SFTP 视图随标签 drop 一并释放，
        // 不会被活动栏“文件”按钮经 SwitchCenter 自动复活。
        let active_session = ctx.active_session.clone();
        if self.tabs.is_empty() {
            events.push(Event::SetCenter(crate::state::CenterView::Sessions));
            events.push(Event::SetActiveSession(None));
        } else if active_session == closed_session {
            // 当前会话指针正指向刚断开的会话，改指当前活动标签所属会话。
            let new_session = self
                .active_tab
                .and_then(|id| self.tabs.iter().find(|t| t.id == id))
                .map(|t| t.session_id.clone());
            events.push(Event::SetActiveSession(new_session));
        }
        Task::batch(events.into_iter().map(Task::done).collect::<Vec<_>>())
    }

    /// 应用窗口关闭：置位全部标签的桥接断开标志，使核心层 pump 与 win_io 后台线程尽快退出。
    ///
    /// 窗口关闭时 `App` 会整体 drop，标签未必逐个走 `CloseTab`；此处显式通知所有 pump，
    /// 避免后台任务与进程残留。返回空任务（窗口本身由 iced 默认行为关闭）。
    fn window_closing(&mut self) -> Task<Event> {
        for tab in self.tabs.iter_mut() {
            if let Some(d) = &tab.disconnect {
                d.store(true, Ordering::SeqCst);
            }
        }
        Task::none()
    }
    fn terminal_opened(&mut self, tab_id: u64, result: BridgeResult, ctx: &Ctx) -> Task<Event> {
        match result {
            Ok((conout, conin, disconnect, resize_tx)) => Task::done(Event::SpawnTerminal(
                tab_id, conout, conin, disconnect, resize_tx,
            )),
            Err(e) => {
                let mut events = vec![Event::SetStatus(e)];
                // 终端打开失败时该标签被丢弃；同会话其它标签各自持有自己的连接，不受影响。
                let closed_session = self
                    .tabs
                    .iter()
                    .find(|t| t.id == tab_id)
                    .map(|t| t.session_id.clone());
                self.tabs.retain(|t| t.id != tab_id);
                if self.active_tab == Some(tab_id) {
                    self.active_tab = self.tabs.last().map(|t| t.id);
                }
                let active_session = ctx.active_session.clone();
                if self.tabs.is_empty() {
                    events.push(Event::SetCenter(crate::state::CenterView::Sessions));
                    events.push(Event::SetActiveSession(None));
                } else if active_session == closed_session {
                    let new_session = self
                        .active_tab
                        .and_then(|id| self.tabs.iter().find(|t| t.id == id))
                        .map(|t| t.session_id.clone());
                    events.push(Event::SetActiveSession(new_session));
                }
                Task::batch(events.into_iter().map(Task::done).collect::<Vec<_>>())
            }
        }
    }

    /// 处理终端桥接断开：把该标签状态翻为 `Error` 并提示原因，已打开的终端组件保留仅更新状态指示。
    fn terminal_disconnected(&mut self, tab_id: u64) -> Task<Event> {
        let status_msg = if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.status = ConnectionStatus::Error;
            t!("app.disconnected", id => tab.session_id.clone())
        } else {
            return Task::none();
        };
        Task::done(Event::SetStatus(status_msg))
    }

    /// 连接结果回流：处理 SSH 连接成功或失败，成功则拉起终端桥接（父层执行）。
    fn session_connected(
        &mut self,
        tab_id: u64,
        id: String,
        result: Result<Arc<SshConnection>, String>,
    ) -> Task<Event> {
        // 目标标签在连接期间（含弹窗等待）可能已被关闭，此时结果无处安放，仅记状态栏。
        if !self.tabs.iter().any(|t| t.id == tab_id) {
            let msg = match &result {
                Ok(_) => t!("app.tab_closed", id => id),
                Err(e) => e.clone(),
            };
            return Task::done(Event::SetStatus(msg));
        }
        match result {
            Ok(conn) => {
                // 连接已建立但桥接尚未就绪：保持“连接中”，直到 TerminalOpened
                // 真正拉起终端后再置 Connected，避免文件管理等依赖 Connected 的逻辑抢跑。
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                    tab.error = None;
                }
                // 记录最近连接的会话，供切换到文件管理时自动打开其 SFTP。
                Task::batch([
                    Task::done(Event::SetActiveSession(Some(id.clone()))),
                    Task::done(Event::SetStatus(t!("app.conn_established", id => id))),
                    Task::done(Event::OpenTerminalBridge(tab_id, conn)),
                ])
            }
            Err(e) => {
                // 失败原因落在发起本次连接的那个标签上，同会话其它已连接的标签不受影响。
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                    tab.status = ConnectionStatus::Error;
                    tab.error = Some(e.clone());
                }
                Task::done(Event::SetStatus(e))
            }
        }
    }

    /// 处理窗口焦点变化：失去焦点时保存并清除终端聚焦态，重新获得时还原。
    fn window_focused(&mut self, focused: bool, ctx: &Ctx) -> Task<Event> {
        if focused {
            // 窗口重新获得焦点：还原切走前保存的终端聚焦态（如离开时焦点就在文件管理器）。
            let new_focus = ctx.window_focus_saved.unwrap_or(ctx.terminal_focused);
            Task::batch([
                Task::done(Event::SetTerminalFocused(new_focus)),
                Task::done(Event::SetWindowFocusSaved(None)),
            ])
        } else {
            // 窗口失去焦点：终端必然不再接收键盘输入，先保存当前态再置否。
            Task::batch([
                Task::done(Event::SetWindowFocusSaved(Some(ctx.terminal_focused))),
                Task::done(Event::SetTerminalFocused(false)),
            ])
        }
    }
}
