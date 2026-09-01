//! 应用根状态 [`App`] 与 iced 应用生命周期（boot / update / view / subscription）。
//!
//! 本模块负责把 UI 交互消息 [`Message`] 映射到对核心层
//! [`rterm_core`] 的调用，并通过 [`iced::Task::perform`] 把异步 SSH / SFTP 操作卸载到
//! tokio 运行时，避免阻塞 GUI 主线程。

use crate::t;

use crate::layout;
use crate::message::{Message, ResizeSender};
use crate::state::{CenterView, SftpView, ToastKind};
use crate::widget::term::settings::{FontSettings, Settings as TermSettings, ThemeSettings};
use crate::widget::term::{
    BackendCommand, Command as TermCommand, Event as TermEvent, RusshPty, Terminal,
};
use crate::widget::toast::{ToastLevel, Toaster, toast, toaster};
use iced::{Element, Subscription, Task};
use log::{debug, error};
use rterm_config::{AppConfig, SessionConfig, SessionStore};
use rterm_core::{ConnectionStatus, SessionSecrets, SshConnection};
use rterm_crypto::Vault;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

/// 初始 / 默认终端列数与行数（远端 PTY 首帧尺寸；随后由本地 resize 事件校正）。
const DEFAULT_COLS: u32 = 80;
/// 默认终端行数。
const DEFAULT_ROWS: u32 = 24;
/// 全局提示横幅自动消失的存活时长（每 0.5s 轮询一次，故实际消失在 TTL~TTL+0.5s 内）。
const TOAST_TTL: Duration = Duration::from_secs(4);

/// GUI 应用根状态。
pub struct App {
    /// 当前选中的会话（用于“打开终端 / 文件”等操作的默认目标），属导航态，被 tabs / sftp 共享。
    pub active_session: Option<String>,
    /// 会话管理模块状态：会话列表与编辑器（原散落在 `App` 上的 `store` / `sessions` /
    /// `hovered_session` / `collapsed_groups` / `editor` 收归此处）。模块经 `Message::Session`
    /// 接收意图、经 `Message::SessionEvent` 回写父状态，自身绝不修改 `App`。
    pub session: session::State,
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
    /// 主密码模块状态：设置 / 解锁 / 更改流程的全部 UI 字段（含 `MpwStage`、各弹窗输入等）。
    /// 模块经 `Message::MasterPw` 接收意图、经 `Message::MasterPwEvent` 回写父状态，
    /// 自身绝不修改 `App` 的 `vault` / `sessions` / `config`。
    pub masterpw: masterpw::State,
}

impl App {
    /// 应用启动：加载会话存储与配置，返回初始状态与任务。
    pub fn new() -> (App, Task<Message>) {
        let store = SessionStore::new()
            .map_err(|e| error!("初始化会话存储失败: {e}"))
            .ok();
        // 读取加密文件头（含模式标志），判断首启动 / 解锁 / 自动解锁。
        let header = store.as_ref().and_then(|s| s.load_crypto_header().ok());

        // 加载应用级偏好配置；失败则回退到默认值（path 为空，后续保存会被跳过并记录日志）。
        let config = AppConfig::new()
            .map_err(|e| error!("初始化应用配置失败: {e}"))
            .unwrap_or_default();

        // 解析首启动 / 自动解锁，得到初始保险库与会话列表：
        // - 无文件头（首次运行）：生成随机密钥（模式 0）存入钥匙串并落盘，零弹窗、零配置。
        // - 有文件头：尝试用钥匙串 DEK 静默解锁（模式 0 跳过哨兵校验；模式 1 校验且受
        //   `remember_master_key` 控制）。失败时模式 1 回退到解锁弹窗，模式 0 视为异常。
        let mut keyring_warning = false;
        let (initial_vault, sessions) = match &header {
            None => {
                let vault = rterm_crypto::Vault::create_random();
                crate::vault_keyring::store_dek_quietly(&vault.dek_bytes());
                if let Some(store) = store.as_ref()
                    && let Err(e) = store.save(&[], vault.header())
                {
                    error!("写入初始加密文件头失败: {e}");
                }
                debug!("首次运行：生成随机密钥（模式 0），已存入系统钥匙串");
                (Some(vault), Vec::new())
            }
            Some(h) => {
                let sessions = store
                    .as_ref()
                    .and_then(|s| s.load().ok())
                    .unwrap_or_default();
                let try_keyring = if h.master_password_set {
                    config.remember_master_key
                } else {
                    true
                };
                let mut vault = if try_keyring {
                    crate::vault_keyring::load_dek()
                        .ok()
                        .flatten()
                        .and_then(|dek| rterm_crypto::Vault::from_dek(&dek, h).ok())
                } else {
                    None
                };
                if vault.is_some() {
                    debug!("系统钥匙串自动解锁成功，跳过主密码弹窗");
                } else if h.master_password_set {
                    debug!("钥匙串无缓存或校验失败，将弹解锁框");
                } else {
                    // 模式 0 钥匙串取不到随机密钥（被清空 / 钥匙串异常）。模式 0 本无主密码，
                    // 绝不该弹「解锁」框把用户卡死：就地重生本机随机密钥让应用可用，并提示
                    // 既有凭据可能失效（旧密钥不可恢复）。
                    error!("模式 0 钥匙串中无随机密钥，重生本机密钥（既有凭据可能失效）");
                    let recovered = rterm_crypto::Vault::create_random();
                    crate::vault_keyring::store_dek_quietly(&recovered.dek_bytes());
                    keyring_warning = true;
                    vault = Some(recovered);
                }
                (vault, sessions)
            }
        };

        // 两栏 `pane_grid` 布局态交由 `app::panes` 模块构建（初始左栏固定像素宽度 320，
        // 比例按 320 / (1144 - 活动栏宽) 设置，与 iced 窗口默认宽度一致）。
        let panes = panes::State::new();

        // 设置弹窗模块状态须在 `config` move 进结构体之前构建，因其读取 `config.ui_font` /
        // `config.terminal_font` 构建下拉框选项。
        let settings = settings::State::new(&config);

        let mut app = App {
            session: session::State::new(store, sessions),
            active_session: None,
            center: CenterView::Sessions,
            tabs: tabs::State::new(),
            terminal_focused: true,
            window_focus_saved: None,
            panes,
            config,
            sftp: sftp::State::default(),
            transfer: transfer::State::default(),
            settings,
            status: None,
            toaster: toaster(),
            hostkey: hostkey::State::default(),
            updates: updates::State::new(),
            vault: initial_vault.map(std::sync::Arc::new),
            masterpw: masterpw::State::new(),
        };

        // 模式 0 钥匙串缺失后就地重生随机密钥：提示用户既有凭据可能失效。
        if keyring_warning {
            app.set_toast(
                crate::state::ToastKind::Warning,
                t!("masterpw.keyring_lost").to_string(),
            );
        }

        // 启动自动检查：是否发起（自动检查开关 + 24h 节流）由更新检查模块判定，
        // 时间戳经 `updates::Event::SetLastCheck` 上行，由父层写回配置并落盘。
        let updates_ctx = app.updates_ctx();
        let check_task = app
            .updates
            .update(updates::Message::CheckOnStartup, &updates_ctx)
            .map(Message::UpdatesEvent);

        (app, check_task)
    }

    /// iced `update`：根据消息更新状态，并可返回后续任务。
    pub fn update(&mut self, message: Message) -> iced::Task<Message> {
        match message {
            Message::SwitchCenter(view) => misc::handle_switch_center(self, view),
            // 会话管理模块：路由进模块自身 `update`，上行事件经 `Message::SessionEvent` 回收。
            Message::Session(m) => {
                let ctx = self.session_ctx();
                self.session.update(m, &ctx).map(Message::SessionEvent)
            }
            Message::SessionEvent(e) => match e {
                // 以下各分支把模块上行的意图落地为父状态变更；模块绝不写父状态。
                session::Event::Connect(id) => {
                    // 开标签并异步发起连接（标签 / 连接生命周期属父层，不在会话模块内）。
                    let tab_id = self.open_tab(&id);
                    self.connect_session(tab_id, &id)
                }
                session::Event::OpenFiles(id) => {
                    // 打开该会话的文件管理：委托 `open_files`（按当前活动标签建立 SFTP）。
                    self.open_files(&id).map(Message::Sftp)
                }
                session::Event::SessionDeleted(id) => {
                    // 关闭该会话的全部终端标签（标签各自持有的连接随标签 drop 释放）。
                    self.close_session_tabs(&id);
                    Task::none()
                }
                session::Event::Status(s) => {
                    self.status = s;
                    Task::none()
                }
                session::Event::Toast(kind, msg) => {
                    self.set_toast(kind, msg);
                    Task::none()
                }
                // 自回路：把内部消息派发回会话模块自身，形成「文件对话框完成 → 回填字段 / 导入导出」闭环。
                session::Event::Emit(m) => {
                    let ctx = self.session_ctx();
                    self.session.update(*m, &ctx).map(Message::SessionEvent)
                }
            },
            // 终端标签模块：路由进模块自身 `update`，上行事件经 `Message::TabsEvent` 回收。
            Message::Tabs(m) => {
                let ctx = self.tabs_ctx();
                self.tabs
                    .update(m, &ctx, &self.sftp)
                    .map(Message::TabsEvent)
            }
            Message::TabsEvent(e) => match e {
                // 以下各分支把模块上行的意图落地为父状态变更；模块绝不写父状态。
                tabs::Event::SetActiveSession(s) => {
                    self.active_session = s;
                    Task::none()
                }
                tabs::Event::SetCenter(v) => {
                    self.center = v;
                    Task::none()
                }
                tabs::Event::SetTerminalFocused(b) => {
                    self.terminal_focused = b;
                    Task::none()
                }
                tabs::Event::SetWindowFocusSaved(o) => {
                    self.window_focus_saved = o;
                    Task::none()
                }
                tabs::Event::SetStatus(s) => {
                    self.status = Some(s);
                    Task::none()
                }
                // 自动打开该会话的 SFTP：委托 `open_files`（挑标签 / 切导航态 / 建通道）。
                tabs::Event::OpenSftp(id) => self.open_files(&id).map(Message::Sftp),
                // 为已连接标签拉起终端桥接（标签 / 连接生命周期属父层，不在标签模块内）。
                tabs::Event::OpenTerminalBridge(tab_id, conn) => {
                    self.open_terminal_bridge(tab_id, conn)
                }
                // 桥接就绪后挂载终端组件（widget 生命周期属父层）。
                tabs::Event::SpawnTerminal(tab_id, conout, conin, disc, resize) => {
                    self.spawn_terminal_widget(tab_id, conout, conin, disc, resize)
                }
                // 关闭标签时清理其挂起的主机密钥确认。
                tabs::Event::RemoveHostKeyForTab(tab_id) => {
                    self.hostkey.remove_for_tab(tab_id);
                    Task::none()
                }
                // 转发终端部件事件给父层（父层拥有的 widget 交互逻辑）。
                tabs::Event::TerminalEvent(ev) => self.handle_terminal_event(ev),
                // 终端挂载完成后强制重绘一次，避免 canvas 缓存停留在空白首帧。
                tabs::Event::TerminalReady(tab_id) => {
                    if let Some(t) = self.tabs.tab_mut(tab_id)
                        && let Some(term) = t.terminal.as_mut()
                    {
                        term.handle(TermCommand::ProxyToBackend(BackendCommand::Resize(
                            None, None,
                        )));
                    }
                    Task::none()
                }
                // 切标签后滚动标签栏到目标位置。
                tabs::Event::ScrollTo(x) => iced::widget::operation::scroll_to(
                    self.tabs.scroll_id(),
                    iced::widget::scrollable::AbsoluteOffset { x, y: 0.0 },
                ),
                // 自回路：把内部消息派发回标签模块自身，形成「SwitchTab → SelectTab」闭环。
                tabs::Event::Emit(m) => {
                    let ctx = self.tabs_ctx();
                    self.tabs
                        .update(*m, &ctx, &self.sftp)
                        .map(Message::TabsEvent)
                }
            },
            Message::ToastDismissed(id) => {
                self.toaster.dismiss(id);
                Task::none()
            }
            Message::ToastHovered(id, hovered) => {
                // 鼠标悬浮时暂停自动消失计时（`crate::widget::toast` 内部在离开时重置起始时刻）。
                self.toaster.set_hovered(id, hovered);
                Task::none()
            }
            Message::ToastTick => {
                // 定时心跳：移除所有已超过展示时长的 toast（悬浮中的除外）。
                self.toaster.dismiss_expired();
                Task::none()
            }
            Message::HostKey(m) => self.hostkey.update(m).map(Message::HostKeyEvent),
            Message::HostKeyEvent(e) => match e {
                // 主机密钥模块自包含：决定仅回复内部句柄并出队，无父态需写，故空匹配。
            },
            Message::Noop => Task::none(),
            // 两栏布局比例：路由进 panes 模块（其上行事件为空，纯自包含几何）。
            Message::Panes(m) => self.panes.update(m).map(Message::PanesEvent),
            Message::PanesEvent(_e) => Task::none(),
            Message::Sftp(m) => self
                .sftp
                .update(m, self.tabs.active().unwrap_or(0))
                .map(Message::SftpEvent),
            Message::SftpEvent(e) => match e {
                sftp::Event::Toast(kind, msg) => {
                    self.set_toast(kind, msg);
                    Task::none()
                }
                sftp::Event::NavigateTo(v) => {
                    self.center = v;
                    Task::none()
                }
                // 内联输入态退出：焦点回到终端这一主区域。
                sftp::Event::FocusTerminal => {
                    self.terminal_focused = true;
                    Task::none()
                }
                // 上传 / 下载意图上行：转发给传输模块，由其管理队列执行（父层只做路由，不碰传输态）。
                sftp::Event::StartUpload(paths) => {
                    let tab_id = self.tabs.active().unwrap_or(0);
                    let ctx = self.transfer_ctx();
                    self.transfer
                        .update(transfer::Message::Upload(tab_id, paths), &ctx)
                        .map(Message::TransferEvent)
                }
                sftp::Event::StartDownload(name, local) => {
                    let tab_id = self.tabs.active().unwrap_or(0);
                    let ctx = self.transfer_ctx();
                    self.transfer
                        .update(transfer::Message::Download(tab_id, name, local), &ctx)
                        .map(Message::TransferEvent)
                }
                // 自回路：把内部消息派发回 SFTP 模块自身，形成「写操作完成 → 重新列举」等闭环。
                sftp::Event::Emit(m) => self
                    .sftp
                    .update(*m, self.tabs.active().unwrap_or(0))
                    .map(Message::SftpEvent),
            },
            Message::Transfer(m) => {
                let ctx = self.transfer_ctx();
                self.transfer.update(m, &ctx).map(Message::TransferEvent)
            }
            Message::TransferEvent(e) => match e {
                transfer::Event::Toast(kind, msg) => {
                    self.set_toast(kind, msg);
                    Task::none()
                }
                // 上传成功：刷新对应标签的 SFTP 目录——经 `Message::Sftp` 派发给 SFTP 模块，
                // 由父层中转，传输模块绝不直接写 SFTP 视图。
                transfer::Event::RefreshDir(tab_id) => {
                    self.sftp.refresh(tab_id).map(Message::SftpEvent)
                }
                // 自回路：把内部消息派发回传输模块自身，形成「进度 / 完成 → 下一传输」闭环。
                transfer::Event::Emit(m) => {
                    let ctx = self.transfer_ctx();
                    self.transfer.update(*m, &ctx).map(Message::TransferEvent)
                }
            },
            Message::Escape => misc::handle_escape(self),
            // 设置弹窗模块：路由进模块自身 `update`，上行事件经 `Message::SettingsEvent` 回收。
            Message::Settings(m) => {
                let ctx = self.settings_ctx();
                self.settings.update(m, &ctx).map(Message::SettingsEvent)
            }
            Message::SettingsEvent(e) => match e {
                // 以下各分支把模块上行的配置值写回 `AppConfig` 并落盘；模块绝不写父状态。
                settings::Event::ConnectTimeout(v) => {
                    self.config.connect_timeout = v;
                    self.save_config();
                    Task::none()
                }
                settings::Event::FontSize(v) => {
                    self.config.font_size = v;
                    self.save_config();
                    // 即时热替换到所有已打开的终端标签（沿用当前已选终端字体）。
                    App::apply_terminal_font(&mut self.tabs, &self.config.terminal_font, v);
                    Task::none()
                }
                settings::Event::Theme(v) => {
                    self.config.theme = v;
                    self.save_config();
                    Task::none()
                }
                settings::Event::UiFont(v) => {
                    self.config.ui_font = v;
                    self.save_config();
                    Task::none()
                }
                settings::Event::TerminalFont(v) => {
                    // 先按当前值热替换所有已打开的终端标签，再移动 `v` 写入配置。
                    App::apply_terminal_font(&mut self.tabs, &v, self.config.font_size);
                    self.config.terminal_font = v;
                    self.save_config();
                    Task::none()
                }
                settings::Event::TerminalTheme(v) => {
                    // 先按当前值解析调色板热替换所有已打开的终端标签，再移动 `v` 写入配置。
                    let palette = crate::terminal_theme::resolve_terminal_theme(&v);
                    for tab in self.tabs.list_mut().iter_mut() {
                        if let Some(term) = tab.terminal.as_mut() {
                            term.handle(TermCommand::ChangeTheme(Box::new(palette.clone())));
                        }
                    }
                    self.config.terminal_theme = v;
                    self.save_config();
                    Task::none()
                }
                settings::Event::LogLevel(v) => {
                    self.config.log_level = v;
                    self.save_config();
                    Task::none()
                }
                settings::Event::Language(v) => {
                    // 即时切换全局 locale 并重绘（view 每帧重建）；下拉框重建已在模块内完成。
                    self.config.language = v;
                    rust_i18n::set_locale(v.as_locale());
                    self.save_config();
                    Task::none()
                }
                // 打开日志目录为纯副作用，已在模块内完成，此处仅需确认（无状态需写）。
                settings::Event::OpenLogFolder => Task::none(),
                settings::Event::AutoCheckUpdates(v) => {
                    self.config.auto_check_updates = v;
                    self.save_config();
                    Task::none()
                }
            },
            Message::Updates(m) => {
                let ctx = self.updates_ctx();
                self.updates.update(m, &ctx).map(Message::UpdatesEvent)
            }
            Message::UpdatesEvent(e) => match e {
                // 确立节流窗口：写回「上次检查时间戳」并落盘（模块绝不写配置）。
                updates::Event::SetLastCheck(ts) => {
                    self.config.last_update_check_unix = ts;
                    self.save_config();
                    Task::none()
                }
                // 自回路：把内部消息派发回更新检查模块自身，形成「检查完成 → 更新横幅」闭环。
                updates::Event::Emit(m) => {
                    let ctx = self.updates_ctx();
                    self.updates.update(*m, &ctx).map(Message::UpdatesEvent)
                }
            },
            Message::MasterPw(m) => {
                let ctx = self.masterpw_ctx();
                self.masterpw.update(m, &ctx).map(Message::MasterPwEvent)
            }
            Message::MasterPwEvent(e) => match e {
                masterpw::Event::SetVault(v) => {
                    self.vault = Some(v);
                    Task::none()
                }
                masterpw::Event::SetSessions(s) => {
                    self.session.sessions = s;
                    Task::none()
                }
                masterpw::Event::SetRemember(v) => {
                    self.config.remember_master_key = v;
                    self.save_config();
                    // 同步钥匙串 DEK：开启且保险库就绪 → 存入（下次自动解锁）；关闭 → 删除，
                    // 回到每次启动输入主密码（钥匙串不可用时的错误被静默忽略）。
                    if v {
                        if let Some(vault) = self.vault.as_ref() {
                            crate::vault_keyring::store_dek_quietly(&vault.dek_bytes());
                        }
                    } else {
                        crate::vault_keyring::delete_dek_quietly();
                    }
                    Task::none()
                }
                masterpw::Event::Toast(kind, msg) => {
                    self.set_toast(kind, msg);
                    Task::none()
                }
                // 自回路：把内部消息派发回主密码模块自身，形成「写操作完成 → 重新列举」等闭环。
                masterpw::Event::Emit(m) => {
                    let ctx = self.masterpw_ctx();
                    self.masterpw.update(*m, &ctx).map(Message::MasterPwEvent)
                }
            },
        }
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

        let resize = iced::event::listen_with(|event, _status, _window| match event {
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
                // 窗口关闭请求：先通知所有标签的桥接断开，使后台 pump / 线程尽快退出，
                // 避免进程残留；窗口本身由 iced 默认行为（exit_on_close_request）关闭。
                Some(Message::Tabs(tabs::Message::WindowClosing))
            }
            _ => None,
        });

        // 监听键盘：Esc 取消对话框 / 内联输入态；F2 对选中的 SFTP 条目触发重命名。
        // `listen_with` 只接受非捕获闭包，故 F2 只发无状态消息，选中项判定放在 `update`。
        let keys = iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. }) => match key {
                iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape) => {
                    Some(Message::Escape)
                }
                iced::keyboard::Key::Named(iced::keyboard::key::Named::F2) => {
                    Some(Message::Sftp(sftp::Message::SftpRenameShortcut))
                }
                _ => None,
            },
            _ => None,
        });

        // toast 通知的自动消失心跳：每 0.5s 触发一次，由 update 调用 `dismiss_expired` 移除到期项。
        let toast_tick = iced::time::every(Duration::from_millis(500)).map(|_| Message::ToastTick);

        iced::Subscription::batch(
            terminals
                .into_iter()
                .chain(std::iter::once(resize))
                .chain(std::iter::once(keys))
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
        self.sftp.active(self.tabs.active())
    }

    /// 会话列表用的聚合状态：取该会话全部标签中「最靠前」的那个状态
    /// （已连接 > 连接中 > 失败 > 未连接）。
    ///
    /// 连接状态本身按标签独立维护，这里只做派生展示：同会话只要还有一个标签连着，
    /// 会话行就显示已连接，不因新开标签把它打回「连接中」。
    pub fn session_status(&self, id: &str) -> ConnectionStatus {
        let mut has_connecting = false;
        let mut has_error = false;
        for tab in self.tabs.list().iter().filter(|t| t.session_id == id) {
            match tab.status {
                ConnectionStatus::Connected => return ConnectionStatus::Connected,
                ConnectionStatus::Connecting => has_connecting = true,
                ConnectionStatus::Error => has_error = true,
                ConnectionStatus::Disconnected => {}
            }
        }
        if has_connecting {
            ConnectionStatus::Connecting
        } else if has_error {
            ConnectionStatus::Error
        } else {
            ConnectionStatus::Disconnected
        }
    }

    // ===================== 内部辅助 =====================

    /// 由会话配置 + 保险库解密出连接所需的明文凭据。
    fn build_secrets(cfg: &SessionConfig, vault: &Vault) -> Result<SessionSecrets, String> {
        match &cfg.auth {
            rterm_config::AuthMethod::Password { password } => {
                let pw = match password {
                    Some(env) => Some(vault.decrypt(env).map_err(|_| t!("app.decrypt_failed"))?),
                    // 凭据缺省（如仅导入连接配置、尚未补填密码）：连接前拦截，
                    // 提示用户先在编辑器中填写密码，避免把空口令发往远端。
                    None => return Err(t!("app.no_password")),
                };
                Ok(SessionSecrets {
                    password: pw,
                    key_passphrase: None,
                })
            }
            rterm_config::AuthMethod::PublicKey { passphrase, .. } => {
                let kp = match passphrase {
                    Some(env) => Some(vault.decrypt(env).map_err(|_| t!("app.decrypt_failed"))?),
                    None => None,
                };
                Ok(SessionSecrets {
                    password: None,
                    key_passphrase: kp,
                })
            }
            rterm_config::AuthMethod::Agent => Ok(SessionSecrets {
                password: None,
                key_passphrase: None,
            }),
        }
    }

    /// 组装会话管理模块所需的只读上下文（当前保险库）。
    ///
    /// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此加密保存凭据但绝不写回，
    /// 写回经 `session::Event` 由父层落地。
    fn session_ctx(&self) -> session::Ctx {
        session::Ctx {
            vault: self.vault.clone(),
        }
    }

    /// 持久化应用级偏好配置（忽略失败并记录日志）。
    fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            error!("保存应用配置失败: {e}");
        }
    }

    /// 组装主密码模块所需的只读上下文（store / vault / sessions / 记住开关）。
    ///
    /// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此计算但绝不写回，
    /// 写回经 `masterpw::Event` 由父层落地。
    fn masterpw_ctx(&self) -> masterpw::Ctx {
        masterpw::Ctx {
            store: self.session.store.clone(),
            vault: self.vault.clone(),
            sessions: self.session.sessions.clone(),
            remember: self.config.remember_master_key,
        }
    }

    /// 组装设置弹窗模块所需的只读上下文（当前 `AppConfig`）。
    ///
    /// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此构建下拉框选项但绝不写回，
    /// 写回经 `settings::Event` 由父层落地。
    fn settings_ctx(&self) -> settings::Ctx {
        settings::Ctx {
            config: self.config.clone(),
        }
    }

    /// 组装更新检查模块所需的只读上下文（自动检查开关 + 上次检查时间戳）。
    ///
    /// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此判定节流，
    /// 但**绝不写父状态**，时间戳写回经 `updates::Event::SetLastCheck` 由父层落地。
    fn updates_ctx(&self) -> updates::Ctx {
        updates::Ctx {
            auto_check: self.config.auto_check_updates,
            last_check_unix: self.config.last_update_check_unix,
        }
    }

    /// 组装终端标签模块所需的只读上下文（中心视图 / 活动会话 / 终端聚焦态 / 失焦保存态）。
    ///
    /// 每次 `update` 调用前重建，确保模块读到最新父状态；仅含 owned 数据（不借用 `App`），
    /// 以免与 `self.tabs` 的可变借用冲突。SFTP 查询经 `update` 的 `sftp` 参数单独传入。
    fn tabs_ctx(&self) -> tabs::Ctx {
        tabs::Ctx {
            center: self.center,
            active_session: self.active_session.clone(),
            terminal_focused: self.terminal_focused,
            window_focus_saved: self.window_focus_saved,
        }
    }

    /// 组装传输模块所需的只读上下文（当前活动标签的 SFTP 客户端 + 远端目录）。
    ///
    /// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此取客户端执行上传 / 下载，
    /// 但绝不写回父状态，写回经 `transfer::Event` 由父层落地 / 转发。
    fn transfer_ctx(&self) -> transfer::Ctx {
        let tab_id = self.tabs.active().unwrap_or(0);
        let (client, remote_dir) = self
            .sftp
            .tab(tab_id)
            .map(|v| (v.client.clone(), v.path.clone()))
            .unwrap_or((None, ".".to_string()));
        transfer::Ctx {
            tab_id,
            client,
            remote_dir,
        }
    }

    /// 推送一条 toast 通知。
    ///
    /// 将 [`ToastKind`] 映射为 `crate::widget::toast` 的级别与标题，并按 [`TOAST_TTL`] 设定自动消失时长。
    /// 多条通知会在屏幕角落堆叠，超过容量上限时最旧者被自动挤出。
    pub(crate) fn set_toast(&mut self, kind: ToastKind, msg: String) {
        let (level, title) = match kind {
            ToastKind::Success => (ToastLevel::Success, t!("app.toast.success")),
            ToastKind::Error => (ToastLevel::Error, t!("app.toast.error")),
            ToastKind::Warning => (ToastLevel::Warning, t!("app.toast.warning")),
        };
        self.toaster.push(
            toast(msg)
                .level(level)
                .title(title)
                .duration(TOAST_TTL.as_secs()),
        );
    }

    /// 是否允许开启 S3 / WebDAV 同步。
    ///
    /// 仅当用户已设置主密码（模式 1，`master_password_set = true`）时为 `true`：
    /// 该模式下凭据由主密码派生 DEK 加密、文件自包含，可跨设备靠口令解密；
    /// 模式 0（随机密钥仅存本机钥匙串）的凭据无法在别的设备还原，故禁止同步。
    /// 同步功能接入时，在同步开关 handler / 面板调用此判定并据此禁用 / 拦截开启。
    pub fn can_enable_sync(&self) -> bool {
        self.vault
            .as_ref()
            .map(|v| v.header().master_password_set)
            .unwrap_or(false)
    }

    /// 打开某会话的文件管理：定位目标标签、切换导航态，视图重置与建通道 / 列举交 sftp 模块。
    ///
    /// SFTP 视图归属于该会话的某个终端标签（每标签独立记录自己的文件上下文）：
    /// 优先使用当前活动标签（若它正属于该会话），否则取该会话的第一个标签。
    ///
    /// 返回 `Task<sftp::Message>`：由调用方 `.map(Message::Sftp)` 接入顶层路由。
    /// 父层只挑标签与切导航态，**不写 SFTP 视图**——那部分在 `sftp::Message::SftpOpenSession`
    /// 里由模块自己完成。
    fn open_files(&mut self, id: &str) -> Task<sftp::Message> {
        // 优先当前活动标签（用户正交互的那个）；否则取该会话的第一个标签。
        let tab_id = self
            .tabs
            .active()
            .and_then(|active| {
                self.tabs
                    .list()
                    .iter()
                    .find(|t| t.id == active && t.session_id == id)
                    .map(|t| t.id)
            })
            .or_else(|| {
                self.tabs
                    .list()
                    .iter()
                    .find(|t| t.session_id == id)
                    .map(|t| t.id)
            });
        let Some(tab_id) = tab_id else {
            self.status = Some(t!("app.open_terminal_first"));
            return Task::none();
        };
        // 聚焦该会话的终端标签，使终端与文件管理上下文保持一致。
        self.center = CenterView::Files;
        self.tabs.set_active(tab_id);
        self.active_session = Some(id.to_string());
        // 优先复用该标签已建立的 SFTP 通道；否则取该标签独占的 SSH 连接用于新建通道。
        let client = self.sftp.tab(tab_id).and_then(|s| s.client.clone());
        let conn = self
            .tabs
            .list()
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.conn.clone());
        if client.is_none() && conn.is_none() {
            self.status = Some(t!("app.connect_session_first"));
            return Task::none();
        }
        Task::done(sftp::Message::SftpOpenSession(
            tab_id,
            id.to_string(),
            client,
            conn,
        ))
    }

    /// 关闭某会话的全部终端标签（标签各自持有的连接与 SFTP 通道随标签移除释放）。
    ///
    /// 由 `handle_delete_session` 在删除会话时调用。
    fn close_session_tabs(&mut self, id: &str) {
        self.tabs.remove_by_session(id);
    }

    /// 为指定标签发起 SSH 连接任务：解密凭据 → 生成连接配置 → 拉起异步握手。
    fn connect_session(&mut self, tab_id: u64, id: &str) -> Task<Message> {
        let Some(cfg) = self.session.sessions.iter().find(|s| s.id == id).cloned() else {
            return Task::none();
        };
        // 凭据信封必须由保险库解密为明文后再交给连接任务（core 层不持有主密钥）。
        let Some(vault) = self.vault.clone() else {
            self.status = Some(t!("app.vault_locked"));
            return Task::none();
        };
        let secrets = match Self::build_secrets(&cfg, &vault) {
            Ok(s) => s,
            Err(e) => {
                self.status = Some(e);
                return Task::none();
            }
        };
        let id = id.to_string();
        // 连接超时取自应用配置；0 表示不限制。
        // 用 stream 而非 perform：握手可能在主机密钥弹窗处中途暂停，需要向 GUI
        // 发送中途消息后再等用户决定，perform 只有唯一最终输出无法胜任。
        let timeout = self.config.connect_timeout;
        Task::stream(iced::stream::channel(
            8,
            move |mut output: futures::channel::mpsc::Sender<Message>| async move {
                connect_stream_task(tab_id, id, cfg, secrets, timeout, &mut output).await;
            },
        ))
    }

    /// 双击会话时立即创建标签（终端区为空，显示“连接中”），并把该标签标记为连接中。
    ///
    /// 标签在连接成功前即存在，使失败原因可直接显示在该标签内，而非仅状态栏。
    /// 连接结果由 [`Self::connect_session`] 异步发起、经 `SessionConnected` 回流处理。
    /// 返回新标签 id，连接结果据此回落到本标签，不影响同会话的其它标签。
    fn open_tab(&mut self, id: &str) -> u64 {
        let title = self
            .session
            .sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| id.to_string());
        let tab_id = self.tabs.add(id.to_string(), title);
        // SFTP 视图随标签创建（由 sftp 模块按标签 id 管理）
        self.sftp.ensure(tab_id);
        tab_id
    }

    /// 把当前终端字体（族名 + 字号）热替换到所有已打开的终端标签。
    ///
    /// 复用 `Terminal::handle(ChangeFont)` 路径，仅替换 `Terminal` 内部的 `TermFont`
    /// 并触发重绘，不重建 widget；旧 `Font` 持有的 `Cow::Owned` 族名随丢弃释放，无泄漏。
    fn apply_terminal_font(tabs: &mut self::tabs::State, font_name: &str, size: f32) {
        let font_type = crate::font::resolve_terminal_font(font_name);
        for tab in tabs.list_mut().iter_mut() {
            if let Some(term) = tab.terminal.as_mut() {
                term.handle(TermCommand::ChangeFont(FontSettings {
                    size,
                    scale_factor: 1.3,
                    font_type,
                }));
            }
        }
    }

    /// 为已存在的（连接中）标签挂载连接并发起桥接任务。
    fn open_terminal_bridge(&mut self, tab_id: u64, conn: Arc<SshConnection>) -> Task<Message> {
        if let Some(tab) = self.tabs.tab_mut(tab_id) {
            tab.conn = Some(conn.clone());
        }
        // 桥接结束（断线）时经 `disconnect_rx` 回发 `TerminalDisconnected(tab_id)`，
        // 按标签（而非按会话）把状态置为 `Error`。
        let (disconnect_tx, mut disconnect_rx) = mpsc::channel::<()>(1);
        let bridge = Task::perform(
            open_terminal_task(conn, DEFAULT_COLS, DEFAULT_ROWS, disconnect_tx),
            move |res| {
                Message::Tabs(tabs::Message::TerminalOpened(
                    tab_id,
                    res.map_err(|e| e.to_string()),
                ))
            },
        );
        let disconnect = Task::perform(
            async move {
                let _ = disconnect_rx.recv().await;
            },
            move |_| Message::Tabs(tabs::Message::TerminalDisconnected(tab_id)),
        );
        Task::batch([bridge, disconnect])
    }

    /// 桥接就绪后用返回的本地 OUT/IN 双管道端创建终端组件，并自动聚焦。
    fn spawn_terminal_widget(
        &mut self,
        tab_id: u64,
        conout: std::sync::Arc<std::fs::File>,
        conin: std::sync::Arc<std::fs::File>,
        disconnect: std::sync::Arc<std::sync::atomic::AtomicBool>,
        resize_tx: ResizeSender,
    ) -> Task<Message> {
        // 把本地管道同步端包成 russh 自定义 pty，直接桥接远端 shell 通道。
        let conout = conout.try_clone().expect("克隆本地管道输出句柄失败");
        let conin = conin.try_clone().expect("克隆本地管道输入句柄失败");
        let russh_pty = RusshPty::new(conout, conin, disconnect.clone(), resize_tx.clone());
        let settings = TermSettings {
            backend: Default::default(),
            font: FontSettings {
                size: self.config.font_size,
                scale_factor: 1.3,
                font_type: crate::font::resolve_terminal_font(&self.config.terminal_font),
            },
            theme: ThemeSettings::new(Box::new(crate::terminal_theme::resolve_terminal_theme(
                &self.config.terminal_theme,
            ))),
        };
        match Terminal::new_with_pty(tab_id, settings, russh_pty) {
            Ok(terminal) => {
                if let Some(tab) = self.tabs.tab_mut(tab_id) {
                    tab.terminal = Some(terminal);
                    tab.resize_tx = Some(resize_tx);
                    // 记录桥接断开标志，关标签 / 关窗口时置位以通知 pump 退出。
                    tab.disconnect = Some(disconnect.clone());
                    // 终端组件就绪即代表连接可用，此时才把本标签标记为已连接，
                    // 使文件管理等依赖 Connected 的逻辑与终端实际可用状态一致。
                    tab.status = ConnectionStatus::Connected;
                    tab.error = None;
                }
                self.status = Some(t!("app.terminal_ready"));
                // 焦点由 app 级 `terminal_focused` 驱动并传入 widget，此处置 true
                // 即代表新建终端持有键盘焦点（光标实心、可输入）。
                self.terminal_focused = true;
                // 延迟一小段时间后强制重绘一次，
                // 以覆盖订阅激活与首屏远端数据到达的时序差（避免首屏空白或显示不全）。
                Task::batch([Task::perform(
                    sleep(std::time::Duration::from_millis(80)),
                    move |_| Message::Tabs(tabs::Message::TerminalReady(tab_id)),
                )])
            }
            Err(e) => {
                self.status = Some(t!("app.terminal_create_failed", err => e));
                self.tabs.list_mut().retain(|t| t.id != tab_id);
                Task::none()
            }
        }
    }

    /// 处理终端部件后端回调（键盘 / 鼠标 / resize 等）。
    ///
    /// 当前版本的 `crate::widget::term::Event` 仅含 `BackendCall` 一个变体，故直接解构。
    fn handle_terminal_event(&mut self, event: TermEvent) -> Task<Message> {
        let TermEvent::BackendCall(id, backend_cmd) = event;
        // 点击 / 选择 / 滚轮 / 键入等「用户与终端的交互」都以非 Resize 的 BackendCall 形式到达；
        // 一旦出现即说明键盘焦点已落回终端，恢复聚焦态（修正「点回终端边框仍显未聚焦」）。
        // 必须排除 `ProcessAlacrittyEvent`：它是 PTY 输出经订阅推送的事件，不代表用户交互——
        // 否则后台终端一有输出（日志、运行命令）就会误把焦点判为聚焦，而用户其实在文件管理器输入。
        let is_user_interaction = matches!(
            &backend_cmd,
            BackendCommand::SelectStart(..)
                | BackendCommand::SelectUpdate(..)
                | BackendCommand::MouseReport(..)
                | BackendCommand::Scroll(..)
                | BackendCommand::ProcessLink(..)
                | BackendCommand::Write(..)
        );
        if is_user_interaction {
            self.terminal_focused = true;
        }
        if let Some(tab) = self.tabs.tab_mut(id) {
            // 本地终端尺寸变化时，转发到远端（window-change）。
            if let BackendCommand::Resize(Some(layout), Some(font)) = &backend_cmd {
                let cols = (layout.width / font.width).floor().max(1.0) as u32;
                let rows = (layout.height / font.height).floor().max(1.0) as u32;
                if let Some(tx) = &tab.resize_tx {
                    let _ = tx.try_send((cols, rows));
                }
            }
            // 将命令转交终端组件（写入 PTY / 调整布局等）。
            if let Some(term) = tab.terminal.as_mut() {
                term.handle(TermCommand::ProxyToBackend(backend_cmd));
            }
        }
        Task::none()
    }
}
pub(crate) mod hostkey;
pub(crate) mod masterpw;
pub(crate) mod misc;
pub(crate) mod panes;
pub(crate) mod session;
pub(crate) mod settings;
pub(crate) mod sftp;
pub(crate) mod tabs;
pub(crate) mod tasks;
pub(crate) mod transfer;
pub(crate) mod updates;

use self::tasks::*;
