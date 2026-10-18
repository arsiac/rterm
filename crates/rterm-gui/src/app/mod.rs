//! 应用根状态 [`App`] 与 iced 应用生命周期（boot / update / view / subscription）。

pub(crate) mod boot;
pub(crate) mod connect;
pub(crate) mod contexts;
pub(crate) mod events;
pub(crate) mod folder_job;
pub(crate) mod hostkey;
pub(crate) mod masterpw;
pub(crate) mod panes;
pub(crate) mod paste;
pub(crate) mod remote_edit;
pub(crate) mod routing;
pub(crate) mod search;
pub(crate) mod session;
pub(crate) mod session_io;
pub(crate) mod settings;
pub(crate) mod sftp;
pub(crate) mod shortcuts;
pub(crate) mod snippets;
pub(crate) mod tabs;
pub(crate) mod tasks;
pub(crate) mod terminal_bridge;
pub(crate) mod transfer;
pub(crate) mod updates;
pub(crate) mod window;

/// 串行化所有改写进程级全局状态根（`rterm_config::paths::set_test_root`）的测试。
///
/// 状态根全局唯一，跨模块并行的测试会相互覆盖对方重定向的临时目录（如 masterpw 的
/// `sessions.toml` 与 settings 的 `known_hosts`），故各模块的此类测试共用这一把锁。
#[cfg(test)]
pub(crate) static STATE_ROOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

use crate::layout;
use crate::message::Message;
use crate::state::{CenterView, SftpView};
use crate::widget::toast::Toaster;
use iced::{Element, Subscription, Task};
use rterm_config::AppConfig;
use rterm_core::ConnectionStatus;
use rterm_crypto::Vault;
use std::sync::Arc;
use std::time::Duration;

/// 初始 / 默认终端列数与行数（远端 PTY 首帧尺寸；随后由本地 resize 事件校正）。
pub(crate) const DEFAULT_COLS: u32 = 80;
/// 默认终端行数。
pub(crate) const DEFAULT_ROWS: u32 = 24;
/// 全局提示横幅自动消失的存活时长（每 0.5s 轮询一次，故实际消失在 TTL~TTL+0.5s 内）。
pub(crate) const TOAST_TTL: Duration = Duration::from_secs(4);

/// GUI 应用根状态。
pub struct App {
    /// 当前选中的会话（用于“打开终端 / 文件”等操作的默认目标），属导航态，被 tabs / sftp 共享。
    pub active_session: Option<String>,
    /// 会话管理模块状态：会话列表与编辑器（原散落在 `App` 上的 `store` / `sessions` /
    /// `hovered_session` / `collapsed_groups` / `editor` 收归此处）。模块经 `Message::Session`
    /// 接收意图、经 `Message::SessionEvent` 回写父状态，自身绝不修改 `App`。
    pub session: session::State,
    /// 会话文件导入 / 导出模块状态：两条口令弹窗的输入、目标路径与进度。
    ///
    /// 模块经 `Message::SessionIo` 接收意图、经 `Message::SessionIoEvent` 回写父状态（导入合并后的
    /// 会话列表），自身绝不修改 `App`。会话面板的菜单只发意图，文件对话框与加解密落盘全归本模块。
    pub session_io: session_io::State,
    /// 中心面板当前展示的内容。
    pub center: CenterView,
    /// 终端标签页模块状态：标签列表与标签栏导航态（原散落在 `App` 上的 `tabs` / `active_tab` /
    /// `next_tab_id` / `show_tab_list` / `tab_bar_scroll` 收归此处）。模块经 `Message::Tabs`
    /// 接收意图、经 `Message::TabsEvent` 回写父状态（导航 / 焦点等共享态仍在 `App`），自身绝不修改 `App`。
    pub tabs: tabs::State,
    /// 终端是否持有键盘焦点（区别于「活动标签」：焦点可能在文件管理输入框等其它控件）。
    ///
    /// 用于驱动终端主体焦点边框与标签三态，解决「光标常亮、无法判断终端是否被聚焦」的问题。
    pub terminal_focused: bool,
    /// 窗口失去焦点前保存的 `terminal_focused`，窗口重新获得焦点时还原，
    /// 避免切走再切回后误判聚焦态（如离开时焦点本就在文件管理器输入框）。
    pub window_focus_saved: Option<bool>,
    /// 两栏 `pane_grid` 布局态（中心左栏固定像素宽度 + 分隔比例 + 各 pane 标识 + 窗口宽度）。
    /// 由 `app::panes` 模块持有，父层只经 `Message::Panes` 路由、绝不直写。
    pub panes: panes::State,
    /// 应用级偏好配置（UI 设置），即时持久化到 `~/.config/rterm/config.toml`。
    pub config: AppConfig,
    /// SFTP 模块状态：每标签文件管理视图（原散落在 `TerminalTab.sftp`）。模块经 `Message::Sftp`
    /// 接收意图、经 `Message::SftpEvent` 回写父状态，自身绝不修改 `App`。
    pub sftp: sftp::State,
    /// 文件传输模块状态：每标签上传 / 下载队列 + 任务 id 分配器 + 取消句柄注册表（原散落在
    /// `SftpView.transfers` 与 `sftp::State` 的 `next_transfer_id` / `abort_handles`）。模块经
    /// `Message::Transfer` 接收意图、经 `Message::TransferEvent` 回写父状态，自身绝不修改 `App`。
    pub transfer: transfer::State,
    /// 设置弹窗模块状态：弹窗显示开关、当前分类、两个字体下拉框态（原散落在 `App` 上的
    /// `show_settings` / `settings_category` / `ui_font_combo` / `terminal_font_combo` 收归此处）。
    /// 模块经 `Message::Settings` 接收意图、经 `Message::SettingsEvent` 回写 `config`，
    /// 自身绝不修改 `App`。
    pub settings: settings::State,
    /// 活动栏底部设置入口的下拉菜单是否展开（菜单项：设置 / 快捷键）；属导航态，由父层持有。
    pub settings_menu_open: bool,
    /// 快捷键管理模块状态：弹窗显示开关。模块经 `Message::Shortcuts` 接收意图，
    /// 经 `Message::ShortcutsEvent` 回写父状态，自身绝不修改 `App`。
    pub shortcuts: shortcuts::State,
    /// 粘贴确认模块状态：待确认的大量粘贴与标签级豁免记录。三条粘贴入口在此收口，
    /// 达阈值者弹窗询问。模块经 `Message::Paste` 接收意图、经 `Message::PasteEvent` 回写父状态。
    pub paste: paste::State,
    /// 远程文件本地编辑模块状态：编辑会话表（右键「本地编辑」发起，保存后经确认框回传）。
    pub remote_edit: remote_edit::State,
    /// 终端内搜索模块状态：搜索条开关、查询串与匹配开关。入口为 Ctrl+Shift+F（终端聚焦时）
    /// 与终端右键菜单「查找」。模块经 `Message::Search` 接收意图、经 `Message::SearchEvent`
    /// 由父层落地后端命令与焦点，自身绝不修改 `App`。
    pub search: search::State,
    /// 命令片段模块状态：片段清单内存储（`snippets.toml`）、过滤 / 选中 / 悬浮态与编辑弹窗草稿。
    /// 模块经 `Message::Snippets` 接收意图、经 `Message::SnippetsEvent` 回写父状态（写入终端 /
    /// toast / 状态栏），自身绝不修改 `App`。
    pub snippets: snippets::State,
    /// 状态栏提示。
    pub status: Option<String>,
    /// toast 通知管理器：所有全局提示（导入 / 导出结果、连接状态、SFTP 反馈等）经
    /// `set_toast` 推入，由 `crate::widget::toast` 在屏幕角落渲染并自动超时消失。
    pub toaster: Toaster<Message>,
    /// 待用户确认的主机密钥弹窗模块状态：握手暂停中的确认队列（并发连接各占一项，渲染取队首）。
    /// 模块经 `Message::HostKey` 接收意图、经 `Message::HostKeyEvent` 回写父状态，自身绝不修改 `App`。
    pub hostkey: hostkey::State,
    /// 更新检查模块状态：顶部更新提示横幅（版本号 + 发布页 URL）；节流所需的时间戳仍在
    /// `AppConfig`（由父层持有）。模块经 `Message::Updates` 接收意图、经 `Message::UpdatesEvent`
    /// 回写时间戳，自身绝不修改 `App`。
    pub updates: updates::State,
    /// 凭据保险库（解密所需的 DEK 就绪后持有）。启动自动解锁成功后立即填充，故默认情况
    /// （模式 0 钥匙串 / 模式 1 记住主口令）下主密码弹窗不会出现。
    pub vault: Option<Arc<Vault>>,
    /// 会话文件「存在但不可用」的原因（`None` 表示可用或首启动）。
    /// 置值时保险库缺席、会话列表为空、**禁止任何写盘**（当首启动处理会覆盖用户文件）；
    /// 主密码解锁弹窗须抑制（无有效头可解），改由状态栏 / toast 提示修复文件。
    pub sessions_file_problem: Option<rterm_config::SessionsFileProblem>,
    /// 主密码模块状态：设置 / 解锁 / 更改流程的全部 UI 字段（含 `MpwStage`、各弹窗输入等）。
    /// 模块经 `Message::MasterPw` 接收意图、经 `Message::MasterPwEvent` 回写父状态，
    /// 自身绝不修改 `App` 的 `vault` / `sessions` / `config`。
    pub masterpw: masterpw::State,
    /// 主窗口生命周期模块状态：记录主窗口 id，并在关闭时按是否最大化决定写回窗口尺寸。
    /// 模块经 `Message::Window` 接收意图、经 `Message::WindowEvent` 由父层落地，自身绝不修改 `App`。
    pub window: window::State,
}

impl App {
    /// 应用启动：自行加载应用配置，委托 [`crate::app::boot`] 完成装配。
    ///
    /// 主要供测试与默认入口使用；正式启动路径应走 [`App::with_config`]，
    /// 以复用入口处已加载的配置、避免重复读盘。
    pub fn new() -> (App, Task<Message>) {
        boot::new(AppConfig::new().unwrap_or_default())
    }

    /// 应用启动：接收调用方已加载的应用配置，委托 [`crate::app::boot`] 完成装配。
    pub fn with_config(config: AppConfig) -> (App, Task<Message>) {
        boot::new(config)
    }

    /// iced `update`：委托 [`crate::app::routing`] 完成消息路由与事件落地。
    pub fn update(&mut self, message: Message) -> iced::Task<Message> {
        routing::update(self, message)
    }

    /// iced `view`：构建三栏界面。
    pub fn view(&self) -> Element<'_, Message, iced::Theme, iced::Renderer> {
        layout::view(self)
    }

    /// iced `subscription`：合并三路订阅——各终端标签的后端事件流、窗口尺寸 / 焦点变化
    /// （按固定左宽重算 `pane_grid` 比例）、Esc / F2 键盘监听，以及 toast 自动消失心跳。
    pub fn subscription(&self) -> Subscription<Message> {
        let terminals = self
            .tabs
            .list()
            .iter()
            .filter_map(|t| t.terminal.as_ref())
            .map(|term| {
                term.subscription()
                    .map(|e| Message::Tabs(tabs::Message::Terminal(e)))
            })
            .collect::<Vec<_>>();

        let resize = iced::event::listen_with(|event, _status, window| match event {
            iced::Event::Window(iced::window::Event::Opened { .. }) => {
                // 窗口创建完成：记录主窗口 id，供关闭时查询是否最大化 / 尺寸。
                Some(Message::Window(window::Message::Opened(window)))
            }
            iced::Event::Window(iced::window::Event::Resized(size)) => {
                Some(Message::Panes(panes::Message::WindowResized(size.width)))
            }
            iced::Event::Window(iced::window::Event::Focused) => {
                Some(Message::Tabs(tabs::Message::WindowFocused(true)))
            }
            iced::Event::Window(iced::window::Event::Unfocused) => {
                Some(Message::Tabs(tabs::Message::WindowFocused(false)))
            }
            iced::Event::Window(iced::window::Event::CloseRequested) => {
                // 窗口关闭请求：交由窗口模块决定先落盘尺寸再关闭（最大化时不覆盖旧尺寸）。
                // 关闭前的桥接断开会随之在父层落地。
                Some(Message::Window(window::Message::CloseRequested(window)))
            }
            _ => None,
        });

        // 监听键盘：按键统一转成和弦（`Message::AppKey`）在 `update` 里分发。常规态只发「带修饰键
        // 或具名键」的按下（纯字符键是打字，不发以免洪泛）；录制态需捕获任意键以给出「裸键会吞输入」
        // 的提示，故切换监听。`listen_with` 只接受 `fn` 指针，故用两个具名函数而非捕获闭包。
        let keys = if self.shortcuts.is_recording() {
            iced::event::listen_with(record_key_message)
        } else {
            iced::event::listen_with(shortcut_key_message)
        };

        // 监听鼠标右键按下：把光标下的会话 / 文件条目标记为选中（右键菜单即针对该条目）。
        // 必须全局监听——`iced_aw::ContextMenu` 在内部处理右键时会 `capture_event()`，
        // 行内 `mouse_area::on_right_press` 因此收不到该事件；与 F2 同理，只发无状态消息。
        let right_press = iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Right)) => {
                Some(Message::RightPress)
            }
            _ => None,
        });

        // 监听鼠标左键释放：拖拽标签期间挂载，兜底结束拖拽态。释放点可能落在终端区 /
        // 其它面板甚至窗外，标签 mouse_area 的释放回调只在自身范围内触发，故须全局监听。
        let drag_end = self.tabs.dragging().then(|| {
            iced::event::listen_with(|event, _status, _window| match event {
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
                    iced::mouse::Button::Left,
                )) => Some(Message::Tabs(tabs::Message::TabDragEnd)),
                _ => None,
            })
        });

        // toast 通知的自动消失心跳：每 0.5s 触发一次，由 update 调用 `dismiss_expired` 移除到期项。
        let toast_tick = iced::time::every(Duration::from_millis(500)).map(|_| Message::ToastTick);

        iced::Subscription::batch(
            terminals
                .into_iter()
                .chain(std::iter::once(resize))
                .chain(std::iter::once(keys))
                .chain(std::iter::once(right_press))
                .chain(drag_end)
                .chain(std::iter::once(toast_tick))
                .chain(std::iter::once(self.sftp.subscription().map(Message::Sftp)))
                .chain(std::iter::once(
                    self.transfer.subscription().map(Message::Transfer),
                ))
                .chain(std::iter::once(
                    self.hostkey.subscription().map(Message::HostKey),
                )),
        )
    }

    // ===================== SFTP 视图访问 =====================

    /// 取当前活动标签的 SFTP 视图（只读，供渲染层使用）。无活动标签时返回 `None`。
    pub(crate) fn active_sftp(&self) -> Option<&SftpView> {
        contexts::active_sftp(self)
    }

    /// 会话列表用的聚合状态：取该会话全部标签中「最靠前」的那个状态
    /// （已连接 > 连接中 > 失败 > 未连接）。
    ///
    /// 实现见 [`contexts::session_status`]。
    pub fn session_status(&self, id: &str) -> ConnectionStatus {
        contexts::session_status(self, id)
    }

    /// 是否允许开启 S3 / WebDAV 同步。
    ///
    /// 仅当用户已设置主密码（模式 1，`master_password_set = true`）时为 `true`：
    /// 该模式下凭据由主密码派生 DEK 加密、文件自包含，可跨设备靠口令解密；
    /// 模式 0（随机密钥仅存本机钥匙串）的凭据无法在别的设备还原，故禁止同步。
    /// 同步功能接入时，在同步开关 handler / 面板调用此判定并据此禁用 / 拦截开启。
    pub fn can_enable_sync(&self) -> bool {
        contexts::can_enable_sync(self)
    }
}

/// 常规按键监听：只把「带修饰键或具名键」的按下发成候选和弦（纯字符键是打字，不发消息）。
fn shortcut_key_message(
    event: iced::Event,
    status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) => {
            if !crate::keymap::is_shortcut_candidate(&key, modifiers) {
                return None;
            }
            crate::keymap::chord_from_key(&key, modifiers)
                .map(|chord| Message::AppKey { chord, status })
        }
        _ => None,
    }
}

/// 录制态按键监听：记录任意可表示按键（含裸字符键），供录制器判据与提示。
fn record_key_message(
    event: iced::Event,
    status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) => {
            crate::keymap::chord_from_key(&key, modifiers)
                .map(|chord| Message::AppKey { chord, status })
        }
        _ => None,
    }
}
