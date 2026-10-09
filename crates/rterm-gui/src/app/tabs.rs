//! 终端标签模块：标签生命周期与标签栏导航（切换 / 关闭 / 列表 dropdown / 窗口焦点）

use crate::app::sftp;
use crate::message::ResizeSender;
use crate::state::TerminalTab;
use crate::t;
use crate::terminal_pane;
use crate::widget::term::Event as TerminalEvent;
use iced::Task;
use iced::widget::Id;
use rterm_core::{BridgeState, ConnectionStatus, DisconnectReason, SshConnection};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

/// 终端桥接就绪后回传的结果：conout 读端、conin 写端、桥接结束状态与尺寸发送端。
/// 抽成别名以免 `TerminalOpened` 变体与 `terminal_opened` 参数触发 `type_complexity`。
type BridgeResult = Result<
    (
        Arc<std::fs::File>,
        Arc<std::fs::File>,
        Arc<BridgeState>,
        ResizeSender,
    ),
    String,
>;

/// 响铃视觉提示（标签闪烁）的持续时长。
const BELL_FLASH_DURATION: Duration = Duration::from_millis(150);

/// 断开横幅闪烁提示（「按 Enter 重新连接」）的持续时长。
///
/// 比响铃闪烁长得多：响铃是「有事发生」的一瞥，而这条提示要让人读出下一步该按什么键。
const BANNER_FLASH_DURATION: Duration = Duration::from_millis(1000);

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
    /// 下一个响铃提示序号（自增；到期消息按序号匹配，作废旧计时器）。
    pub(crate) next_bell_seq: u64,
    /// 下一个断开横幅闪烁序号（自增；到期消息按序号匹配，作废旧计时器）。
    pub(crate) next_banner_seq: u64,
    /// 正在拖拽的标签 id（按下标签即置位，左键释放 / 窗口失焦时清除）。
    dragging_tab: Option<u64>,
    /// 光标当前悬停的标签 id（驱动标签悬停底色；拖拽中同时驱动实时重排）。
    hovered_tab: Option<u64>,
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
    /// 标签被按下：立即激活该标签并进入拖拽态。
    TabPressed(u64),
    /// 光标进入某标签：更新悬停；若正在拖拽且目标不是被拖标签，则实时重排。
    TabHoverEnter(u64),
    /// 光标离开某标签：仅当悬停者正是它时清空（避免重排后错序的 exit 误清他人）。
    TabHoverExit(u64),
    /// 左键释放（全局订阅兜底）：结束拖拽态。
    TabDragEnd,
    /// 终端桥接就绪：挂载终端组件（父层执行）。
    TerminalOpened(u64, BridgeResult),
    /// 终端桥接结束：按退出原因置标签状态（远端会话结束 / 连接断开）。
    TerminalDisconnected(u64, DisconnectReason),
    /// 终端挂载完成：强制刷新首屏。
    TerminalReady(u64),
    /// 终端部件事件（键盘 / 鼠标 / 后端回调），转发父层处理。
    Terminal(TerminalEvent),
    /// SSH 连接结果回流：成功拉起桥接，失败置错。
    SessionConnected(u64, String, Result<Arc<SshConnection>, String>),
    /// 窗口焦点变化：保存 / 还原终端聚焦态。
    WindowFocused(bool),
    /// 终端收到响铃（BEL）：置本标签的闪烁提示并安排到期清除（开关已由父层按配置过滤）。
    Bell(u64),
    /// 响铃闪烁到期（携带标签 id 与本次提示序号）：序号匹配才清除，旧计时器不得熄灭新闪烁。
    BellFlashExpired(u64, u64),
    /// 断开态下按键被拦下（非回车，父层过滤后发来）：闪烁横幅提示「按 Enter 重新连接」，
    /// 让按键有可见回执而不是被静默丢弃。
    DisconnectedKeyPressed(u64),
    /// 横幅闪烁到期（携带标签 id 与本次提示序号）：序号匹配才清除，旧计时器不得熄灭新闪烁。
    BannerFlashExpired(u64, u64),
    /// 断开态下按下回车 / 点击横幅「重新连接」按钮：请求重连该标签。
    ReconnectRequested(u64),
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
        Arc<BridgeState>,
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
    /// 断开态下的重连请求（回车 / 横幅按钮）：父层据此发起重连连接。
    ReconnectTab(u64, String),
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
            next_bell_seq: 0,
            next_banner_seq: 0,
            dragging_tab: None,
            hovered_tab: None,
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

    /// 是否正处于标签拖拽中（订阅按此条件挂载拖拽结束监听）。
    pub(crate) fn dragging(&self) -> bool {
        self.dragging_tab.is_some()
    }

    /// 光标当前悬停的标签 id（渲染层据此驱动悬停底色）。
    pub(crate) fn hovered(&self) -> Option<u64> {
        self.hovered_tab
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

    /// 取某标签终端当前工作目录（cwd）的快照（按需加锁克隆，不持有锁跨调用）。
    ///
    /// 由核心层桥接 pump 扫描 OSC 7 序列实时写入 `TerminalTab::cwd`；文件管理
    /// 「进入终端目录」按钮经此读取。尚未捕获到任何 OSC 7（如远端 shell 未输出
    /// 提示符、或不支持注入）时返回 `None`，调用方应回退为提示用户。
    pub(crate) fn terminal_cwd(&self, id: u64) -> Option<String> {
        self.tabs
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.cwd.lock().ok().and_then(|g| g.clone()))
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
            bridge: None,
            disconnect_reason: None,
            cwd: std::sync::Arc::new(Mutex::new(None)),
            title,
            bell_flash: None,
            banner_flash: None,
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
            Message::TabPressed(tab_id) => {
                self.dragging_tab = Some(tab_id);
                self.select_tab(tab_id, ctx, sftp)
            }
            Message::TabHoverEnter(tab_id) => {
                self.hovered_tab = Some(tab_id);
                if let Some(dragged) = self.dragging_tab
                    && dragged != tab_id
                {
                    move_tab(&mut self.tabs, dragged, tab_id);
                }
                Task::none()
            }
            Message::TabHoverExit(tab_id) => {
                if self.hovered_tab == Some(tab_id) {
                    self.hovered_tab = None;
                }
                Task::none()
            }
            Message::TabDragEnd => {
                self.dragging_tab = None;
                Task::none()
            }
            Message::TerminalOpened(tab_id, result) => self.terminal_opened(tab_id, result, ctx),
            Message::TerminalDisconnected(tab_id, reason) => {
                self.terminal_disconnected(tab_id, reason)
            }
            Message::TerminalReady(tab_id) => Task::done(Event::TerminalReady(tab_id)),
            Message::Terminal(event) => Task::done(Event::TerminalEvent(event)),
            Message::SessionConnected(tab_id, id, result) => {
                self.session_connected(tab_id, id, result)
            }
            Message::WindowFocused(focused) => self.window_focused(focused, ctx),
            Message::Bell(tab_id) => self.bell_flash(tab_id),
            Message::BellFlashExpired(tab_id, seq) => {
                self.clear_bell_flash(tab_id, seq);
                Task::none()
            }
            Message::DisconnectedKeyPressed(tab_id) => self.banner_flash(tab_id),
            Message::BannerFlashExpired(tab_id, seq) => {
                self.clear_banner_flash(tab_id, seq);
                Task::none()
            }
            Message::ReconnectRequested(tab_id) => match self.reconnect_target(tab_id) {
                Some(e) => Task::done(e),
                None => Task::none(),
            },
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
            "Closing tab: tab {tab_id} session {}",
            closed_session.as_deref().unwrap_or("<none>")
        );
        let mut events = vec![Event::RemoveHostKeyForTab(tab_id)];
        // 请求该标签的桥接停止，让核心层 pump 尽快退出（释放服务端管道句柄，
        // 进而使 win_io 后台读/写线程退出），避免关标签后进程残留。
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id)
            && let Some(bridge) = &tab.bridge
        {
            bridge.request_stop();
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

    /// 应用窗口关闭：请求全部标签的桥接停止，使核心层 pump 与 win_io 后台线程尽快退出。
    ///
    /// 窗口关闭时 `App` 会整体 drop，标签未必逐个走 `CloseTab`；此处显式通知所有 pump，
    /// 避免后台任务与进程残留。返回空任务（窗口本身由 iced 默认行为关闭）。
    fn window_closing(&mut self) -> Task<Event> {
        for tab in self.tabs.iter_mut() {
            if let Some(bridge) = &tab.bridge {
                bridge.request_stop();
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

    /// 处理终端桥接结束：记录退出原因（「远端会话已结束」与「连接已断开」文案据此分流），
    /// 把该标签状态翻为 `Error` 并提示，已打开的终端组件保留仅更新状态指示。
    fn terminal_disconnected(&mut self, tab_id: u64, reason: DisconnectReason) -> Task<Event> {
        let status_msg = if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.status = ConnectionStatus::Error;
            tab.disconnect_reason = Some(reason);
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
            // 拖拽态一并清除：窗外释放不保证送达（拖拽结束订阅收不到），否则拖拽会一直挂着。
            self.dragging_tab = None;
            Task::batch([
                Task::done(Event::SetWindowFocusSaved(Some(ctx.terminal_focused))),
                Task::done(Event::SetTerminalFocused(false)),
            ])
        }
    }

    /// 置本标签的响铃闪烁并安排到期清除：到期消息经自回路回流（同 `app::transfer` 的退避
    /// 定时器），不新增订阅；期间标签被关闭则到期时查无此人，静默跳过。
    fn bell_flash(&mut self, tab_id: u64) -> Task<Event> {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return Task::none();
        };
        let seq = self.next_bell_seq;
        self.next_bell_seq += 1;
        tab.bell_flash = Some(seq);
        Task::perform(
            // `sleep` 必须写在 `async` 块内部：`update` 里没有 tokio 运行时上下文，
            // 在外层构造 `tokio::time::sleep` 会直接 panic（同 `app::transfer::retry_timer`）。
            async move { tokio::time::sleep(BELL_FLASH_DURATION).await },
            move |()| Event::Emit(Box::new(Message::BellFlashExpired(tab_id, seq))),
        )
    }

    /// 清除响铃闪烁；仅当序号仍为本次提示时生效，旧计时器的到期消息到此作废。
    fn clear_bell_flash(&mut self, tab_id: u64, seq: u64) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id)
            && tab.bell_flash == Some(seq)
        {
            tab.bell_flash = None;
        }
    }

    /// 置本标签断开横幅的闪烁并安排到期清除（自回路模式同 [`Self::bell_flash`]）。
    ///
    /// 断开态下的键入在父层被拦下（写进已死桥接只会静默丢弃），非回车的按键经此让横幅
    /// 闪出「按 Enter 重新连接」——闪烁本身就是「按键收到了、但写不进去」的可见回执。
    fn banner_flash(&mut self, tab_id: u64) -> Task<Event> {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return Task::none();
        };
        let seq = self.next_banner_seq;
        self.next_banner_seq += 1;
        tab.banner_flash = Some(seq);
        Task::perform(
            // `sleep` 必须写在 `async` 块内部（同 `bell_flash` 的说明）。
            async move { tokio::time::sleep(BANNER_FLASH_DURATION).await },
            move |()| Event::Emit(Box::new(Message::BannerFlashExpired(tab_id, seq))),
        )
    }

    /// 清除横幅闪烁；仅当序号仍为本次提示时生效，旧计时器的到期消息到此作废。
    fn clear_banner_flash(&mut self, tab_id: u64, seq: u64) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id)
            && tab.banner_flash == Some(seq)
        {
            tab.banner_flash = None;
        }
    }

    /// 校验并解析重连请求的目标（回车 / 横幅按钮）：仅断开中的标签接受。
    ///
    /// 排除 `Connected`（迟到触发不得再开连接）与 `Connecting`（该标签自己的重连已在途）；
    /// 另排除「该会话已有标签在连接中」——连按回车时首条请求已开出新连接（落在新标签上），
    /// 在途的重复请求到此时查到在途连接即被丢弃，不会并发起第二条。
    fn reconnect_target(&self, tab_id: u64) -> Option<Event> {
        let tab = self.tabs.iter().find(|t| t.id == tab_id)?;
        if !matches!(
            tab.status,
            ConnectionStatus::Error | ConnectionStatus::Disconnected
        ) {
            return None;
        }
        let pending = self
            .tabs
            .iter()
            .any(|t| t.status == ConnectionStatus::Connecting && t.session_id == tab.session_id);
        if pending {
            return None;
        }
        Some(Event::ReconnectTab(tab_id, tab.session_id.clone()))
    }
}

/// 把 `dragged` 标签移到 `target` 标签的**当前索引**处（占据其位置，其余相对顺序不变）。
///
/// 先取目标原索引再 remove / insert，使被拖标签落在目标标签所在的一侧（向左拖到目标左侧、
/// 向右拖到目标右侧），连续扫过即逐格归位。任一 id 不存在、或二者相同时不动，
/// 返回是否发生移动。重排只动 `Vec` 顺序：`active_tab`、闪铃、SFTP 映射全部按 id 引用。
fn move_tab(tabs: &mut Vec<TerminalTab>, dragged: u64, target: u64) -> bool {
    let (Some(from), Some(to)) = (
        tabs.iter().position(|t| t.id == dragged),
        tabs.iter().position(|t| t.id == target),
    ) else {
        return false;
    };
    if from == to {
        return false;
    }
    let tab = tabs.remove(from);
    tabs.insert(to, tab);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只读上下文桩：响铃路径不读其中任何字段，取默认值即可。
    fn ctx() -> Ctx {
        Ctx {
            center: crate::state::CenterView::Sessions,
            active_session: None,
            terminal_focused: true,
            window_focus_saved: None,
        }
    }

    /// 到期消息必须按序号匹配：连续响铃时，旧计时器不得提前熄灭新一轮的闪烁。
    #[test]
    fn only_the_latest_bell_expiry_clears_the_flash() {
        let mut state = State::new();
        let tab = state.add("s1".to_string(), "s1".to_string());
        let sftp = sftp::State::default();

        let _ = state.update(Message::Bell(tab), &ctx(), &sftp);
        let first = state.list()[0].bell_flash.expect("响铃后应处于闪烁态");
        let _ = state.update(Message::Bell(tab), &ctx(), &sftp);
        let _ = state.update(Message::BellFlashExpired(tab, first), &ctx(), &sftp);
        assert!(
            state.list()[0].bell_flash.is_some(),
            "旧到期消息不得清掉新闪烁"
        );

        let latest = state.list()[0].bell_flash.expect("第二轮闪烁仍在");
        let _ = state.update(Message::BellFlashExpired(tab, latest), &ctx(), &sftp);
        assert!(state.list()[0].bell_flash.is_none(), "本次到期应清除闪烁");
    }

    /// 迟到 / 落空的响铃消息（标签已关闭）不得 panic，也不得影响其它标签。
    #[test]
    fn a_bell_for_an_unknown_tab_is_a_no_op() {
        let mut state = State::new();
        let tab = state.add("s1".to_string(), "s1".to_string());
        let sftp = sftp::State::default();

        let _ = state.update(Message::Bell(tab), &ctx(), &sftp);
        let _ = state.update(Message::Bell(404), &ctx(), &sftp);
        let _ = state.update(Message::BellFlashExpired(404, 0), &ctx(), &sftp);
        assert!(
            state.list()[0].bell_flash.is_some(),
            "其它标签的闪烁不受影响"
        );
    }

    /// 断开横幅闪烁：同响铃序号语义，旧到期消息不得提前熄灭新一轮闪烁。
    #[test]
    fn only_the_latest_banner_expiry_clears_the_flash() {
        let mut state = State::new();
        let tab = state.add("s1".to_string(), "s1".to_string());
        let sftp = sftp::State::default();

        let _ = state.update(Message::DisconnectedKeyPressed(tab), &ctx(), &sftp);
        let first = state.list()[0].banner_flash.expect("按键后横幅应闪烁");
        let _ = state.update(Message::DisconnectedKeyPressed(tab), &ctx(), &sftp);
        let _ = state.update(Message::BannerFlashExpired(tab, first), &ctx(), &sftp);
        assert!(
            state.list()[0].banner_flash.is_some(),
            "旧到期消息不得清掉新闪烁"
        );

        let latest = state.list()[0].banner_flash.expect("第二轮闪烁仍在");
        let _ = state.update(Message::BannerFlashExpired(tab, latest), &ctx(), &sftp);
        assert!(state.list()[0].banner_flash.is_none(), "本次到期应清除闪烁");
    }

    /// 迟到 / 落空的横幅消息（标签已关闭）不得 panic。
    #[test]
    fn banner_messages_for_an_unknown_tab_are_no_ops() {
        let mut state = State::new();
        // 存在一个健康标签：落空的消息不得影响它。
        let _ = state.add("s1".to_string(), "s1".to_string());
        let sftp = sftp::State::default();

        let _ = state.update(Message::DisconnectedKeyPressed(404), &ctx(), &sftp);
        let _ = state.update(Message::BannerFlashExpired(404, 0), &ctx(), &sftp);
        let _ = state.update(Message::ReconnectRequested(404), &ctx(), &sftp);
        assert!(state.list()[0].banner_flash.is_none());
    }

    /// 重连请求只接受断开中的标签；该会话已有在途连接（连按回车的重复请求）同样拒绝。
    #[test]
    fn reconnect_target_accepts_only_disconnected_tabs_without_pending_connect() {
        let mut state = State::new();
        let a = state.add("s1".to_string(), "s1".to_string());
        let set_status = |state: &mut State, id: u64, status| {
            state.tabs.iter_mut().find(|t| t.id == id).unwrap().status = status;
        };

        // 断开态（Error / Disconnected）且无在途连接：接受。
        set_status(&mut state, a, ConnectionStatus::Error);
        assert!(matches!(
            state.reconnect_target(a),
            Some(Event::ReconnectTab(id, s)) if id == a && s == "s1"
        ));
        set_status(&mut state, a, ConnectionStatus::Disconnected);
        assert!(state.reconnect_target(a).is_some());

        // 标签已恢复 / 自己的重连在途：拒绝。
        set_status(&mut state, a, ConnectionStatus::Connected);
        assert!(state.reconnect_target(a).is_none());
        set_status(&mut state, a, ConnectionStatus::Connecting);
        assert!(state.reconnect_target(a).is_none());

        // 同会话另一标签在连接中（连按回车开出的新连接在途）：重复请求被丢弃。
        set_status(&mut state, a, ConnectionStatus::Error);
        let b = state.add("s1".to_string(), "s1".to_string());
        assert!(state.reconnect_target(a).is_none());

        // 在途连接结束后重新接受。
        set_status(&mut state, b, ConnectionStatus::Connected);
        assert!(state.reconnect_target(a).is_some());

        // 其它会话的在途连接不影响本会话的重连。
        let _c = state.add("s2".to_string(), "s2".to_string());
        assert!(state.reconnect_target(a).is_some());
    }

    mod drag_tests {
        use super::*;

        /// 建 n 个标签，返回按创建序的 id 列表。
        fn tabs_with(state: &mut State, n: usize) -> Vec<u64> {
            (1..=n)
                .map(|i| state.add(format!("s{i}"), format!("t{i}")))
                .collect()
        }

        /// 当前标签顺序（id 列表）。
        fn order(state: &State) -> Vec<u64> {
            state.list().iter().map(|t| t.id).collect()
        }

        #[test]
        fn move_tab_right_places_dragged_at_target_index() {
            let mut state = State::new();
            let ids = tabs_with(&mut state, 4);

            assert!(move_tab(&mut state.tabs, ids[0], ids[2]));
            assert_eq!(order(&state), vec![ids[1], ids[2], ids[0], ids[3]]);
        }

        #[test]
        fn move_tab_left_places_dragged_before_target() {
            let mut state = State::new();
            let ids = tabs_with(&mut state, 4);

            assert!(move_tab(&mut state.tabs, ids[3], ids[1]));
            assert_eq!(order(&state), vec![ids[0], ids[3], ids[1], ids[2]]);
        }

        #[test]
        fn move_tab_covers_adjacent_and_boundary_positions() {
            let mut state = State::new();
            let ids = tabs_with(&mut state, 4);

            // 相邻右移：A 越过 B。
            assert!(move_tab(&mut state.tabs, ids[0], ids[1]));
            assert_eq!(order(&state), vec![ids[1], ids[0], ids[2], ids[3]]);

            // 从中间拖到末位。
            assert!(move_tab(&mut state.tabs, ids[0], ids[3]));
            assert_eq!(order(&state), vec![ids[1], ids[2], ids[3], ids[0]]);

            // 从末位拖回首位。
            assert!(move_tab(&mut state.tabs, ids[0], ids[1]));
            assert_eq!(order(&state), ids);
        }

        #[test]
        fn move_tab_ignores_unknown_or_same_ids() {
            let mut state = State::new();
            let ids = tabs_with(&mut state, 2);

            assert!(!move_tab(&mut state.tabs, 999, ids[0]));
            assert!(!move_tab(&mut state.tabs, ids[0], 999));
            assert!(!move_tab(&mut state.tabs, ids[0], ids[0]));
            assert_eq!(order(&state), ids);
        }

        #[test]
        fn tab_pressed_activates_and_starts_dragging() {
            let mut state = State::new();
            let ids = tabs_with(&mut state, 2);
            let sftp = sftp::State::default();

            let _ = state.update(Message::TabPressed(ids[0]), &ctx(), &sftp);

            assert_eq!(state.active(), Some(ids[0]));
            assert!(state.dragging());
        }

        #[test]
        fn hover_without_drag_only_tracks_hovered() {
            let mut state = State::new();
            let ids = tabs_with(&mut state, 3);
            let sftp = sftp::State::default();

            let _ = state.update(Message::TabHoverEnter(ids[2]), &ctx(), &sftp);

            assert_eq!(state.hovered(), Some(ids[2]));
            assert_eq!(order(&state), ids);
        }

        #[test]
        fn hover_while_dragging_reorders_in_realtime() {
            let mut state = State::new();
            let ids = tabs_with(&mut state, 4);
            let sftp = sftp::State::default();
            let _ = state.update(Message::TabPressed(ids[0]), &ctx(), &sftp);

            // 进入被拖标签自身：不重排。
            let _ = state.update(Message::TabHoverEnter(ids[0]), &ctx(), &sftp);
            assert_eq!(order(&state), ids);

            // 进入 C：A 实时归位到 C 的索引处。
            let _ = state.update(Message::TabHoverEnter(ids[2]), &ctx(), &sftp);
            assert_eq!(order(&state), vec![ids[1], ids[2], ids[0], ids[3]]);
        }

        #[test]
        fn hover_exit_does_not_clear_another_tab() {
            let mut state = State::new();
            let ids = tabs_with(&mut state, 2);
            let sftp = sftp::State::default();

            let _ = state.update(Message::TabHoverEnter(ids[0]), &ctx(), &sftp);
            // 重排后错序到达的 exit（悬停者已是别人）不得清空当前悬停。
            let _ = state.update(Message::TabHoverExit(ids[1]), &ctx(), &sftp);
            assert_eq!(state.hovered(), Some(ids[0]));

            let _ = state.update(Message::TabHoverExit(ids[0]), &ctx(), &sftp);
            assert_eq!(state.hovered(), None);
        }

        #[test]
        fn drag_end_and_window_unfocus_clear_dragging() {
            let mut state = State::new();
            let ids = tabs_with(&mut state, 2);
            let sftp = sftp::State::default();

            let _ = state.update(Message::TabPressed(ids[0]), &ctx(), &sftp);
            let _ = state.update(Message::TabDragEnd, &ctx(), &sftp);
            assert!(!state.dragging());

            let _ = state.update(Message::TabPressed(ids[0]), &ctx(), &sftp);
            let _ = state.update(Message::WindowFocused(false), &ctx(), &sftp);
            assert!(!state.dragging());
        }
    }
}
