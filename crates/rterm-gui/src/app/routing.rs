//! 消息路由表：把顶层 [`Message`] 分发到各子模块的 `update` 或 `events` 的落地函数。

use crate::app::App;
use crate::app::connect;
use crate::app::contexts;
use crate::app::{
    events, hostkey, masterpw, panes, paste, search, session, session_io, settings, sftp,
    shortcuts, snippets, tabs, window,
};
use crate::message::Message;
use crate::state::{CenterView, ToastKind};
use crate::t;
use iced::Task;
use rterm_config::keybindings::{Chord, ChordKey};
use rterm_core::ConnectionStatus;

/// iced `update`：根据消息更新状态，并可返回后续任务。
pub(crate) fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::SwitchCenter(view) => handle_switch_center(app, view),
        // 会话管理模块：路由进模块自身 `update`，上行事件经 `Message::SessionEvent` 回收。
        Message::Session(m) => {
            let ctx = contexts::session_ctx(app);
            app.session.update(m, &ctx).map(Message::SessionEvent)
        }
        Message::SessionEvent(e) => events::apply_session_event(app, e),
        // 会话文件模块：路由进模块自身 `update`，上行事件经 `Message::SessionIoEvent` 回收。
        Message::SessionIo(m) => {
            let ctx = contexts::session_io_ctx(app);
            app.session_io.update(m, &ctx).map(Message::SessionIoEvent)
        }
        Message::SessionIoEvent(e) => events::apply_session_io_event(app, e),
        // 终端标签模块：路由进模块自身 `update`，上行事件经 `Message::TabsEvent` 回收。
        Message::Tabs(m) => {
            // 切换标签先关闭搜索条：搜索态绑定单个终端，不跨标签延续（残留高亮会指向另一个终端）。
            // TabPressed 一并处理：标签按下即切换活动标签。
            let close = if app.search.is_open()
                && matches!(
                    m,
                    tabs::Message::SelectTab(..)
                        | tabs::Message::SwitchTab(..)
                        | tabs::Message::TabPressed(..)
                ) {
                app.search
                    .update(search::Message::Close)
                    .map(Message::SearchEvent)
            } else {
                Task::none()
            };
            let ctx = contexts::tabs_ctx(app);
            let tabs = app.tabs.update(m, &ctx, &app.sftp).map(Message::TabsEvent);
            Task::batch([close, tabs])
        }
        Message::TabsEvent(e) => events::apply_tabs_event(app, e),
        Message::ToastDismissed(id) => {
            app.toaster.dismiss(id);
            Task::none()
        }
        Message::ToastHovered(id, hovered) => {
            // 鼠标悬浮时暂停自动消失计时（`crate::widget::toast` 内部在离开时重置起始时刻）。
            app.toaster.set_hovered(id, hovered);
            Task::none()
        }
        Message::ToastTick => {
            // 定时心跳：移除所有已超过展示时长的 toast（悬浮中的除外）。
            app.toaster.dismiss_expired();
            Task::none()
        }
        Message::HostKey(m) => app.hostkey.update(m).map(Message::HostKeyEvent),
        Message::HostKeyEvent(e) => match e {
                // 主机密钥模块自包含：决定仅回复内部句柄并出队，无父态需写，故空匹配。
            },
        Message::Noop => Task::none(),
        // 两栏布局比例：路由进 panes 模块（其上行事件为空，纯自包含几何）。
        Message::Panes(m) => app.panes.update(m).map(Message::PanesEvent),
        Message::PanesEvent(_e) => Task::none(),
        // 主窗口生命周期：路由进 window 模块，上行事件经 `Message::WindowEvent` 落地。
        Message::Window(m) => {
            // 收到关闭请求时先通知所有标签断开桥接，使后台 pump / 线程尽快退出；
            // 无论是否开启窗口记忆都需执行，确保进程干净退出。
            let cleanup = if matches!(m, window::Message::CloseRequested(_)) {
                let ctx = contexts::tabs_ctx(app);
                app.tabs
                    .update(tabs::Message::WindowClosing, &ctx, &app.sftp)
                    .map(Message::TabsEvent)
            } else {
                Task::none()
            };
            let ctx = contexts::window_ctx(app);
            let window = app.window.update(m, &ctx).map(Message::WindowEvent);
            Task::batch([cleanup, window])
        }
        Message::WindowEvent(e) => events::apply_window_event(app, e),
        Message::Sftp(m) => {
            // 「进入终端目录」按钮：SFTP 模块无终端访问权，需在此读取活动终端的 cwd
            // 后转交既有的 `SftpCd` 完成跳转；尚未捕获到 cwd（远端未输出提示符 / 不支持
            // 注入）时以 toast 提示，而非静默无反应。
            if let sftp::Message::SftpGotoTerminalDir = &m {
                let active = app.tabs.active().unwrap_or(0);
                match app.tabs.terminal_cwd(active) {
                    Some(cwd) => {
                        return app
                            .sftp
                            .update(sftp::Message::SftpCd(cwd), active)
                            .map(Message::SftpEvent);
                    }
                    None => {
                        return Task::done(Message::SftpEvent(sftp::Event::Toast(
                            ToastKind::Error,
                            t!("sftp.cwd_unavailable"),
                        )));
                    }
                }
            }
            app.sftp
                .update(m, app.tabs.active().unwrap_or(0))
                .map(Message::SftpEvent)
        }
        Message::SftpEvent(e) => events::apply_sftp_event(app, e),
        Message::Transfer(m) => {
            let ctx = contexts::transfer_ctx(app);
            app.transfer.update(m, &ctx).map(Message::TransferEvent)
        }
        Message::TransferEvent(e) => events::apply_transfer_event(app, e),
        Message::AppKey { chord, status } => handle_app_key(app, &chord, status),
        // 右键按下：两个列表各自按自己的悬浮态决定选中谁，详见 `handle_right_press`。
        Message::RightPress => handle_right_press(app),
        // 终端右键菜单：复制同步取选区写剪贴板；粘贴需异步读剪贴板，结果另行回流。
        Message::TerminalCopy => handle_terminal_copy(app),
        Message::TerminalPaste => iced::clipboard::read().map(Message::TerminalPasteLoaded),
        Message::TerminalPasteLoaded(contents) => handle_terminal_paste(app, contents),
        // 粘贴确认模块：路由进模块自身 `update`，放行事件经 `Message::PasteEvent` 落地。
        Message::Paste(m) => {
            let ctx = contexts::paste_ctx(app);
            app.paste.update(m, &ctx).map(Message::PasteEvent)
        }
        Message::PasteEvent(e) => events::apply_paste_event(app, e),
        // 终端内搜索模块：路由进模块自身 `update`，上行事件经 `Message::SearchEvent` 落地。
        Message::Search(m) => app.search.update(m).map(Message::SearchEvent),
        Message::SearchEvent(e) => events::apply_search_event(app, e),
        // 命令片段模块：路由进模块自身 `update`，上行事件经 `Message::SnippetsEvent` 落地。
        Message::Snippets(m) => {
            let ctx = contexts::snippets_ctx(app);
            app.snippets.update(m, &ctx).map(Message::SnippetsEvent)
        }
        Message::SnippetsEvent(e) => events::apply_snippets_event(app, e),
        // 设置弹窗模块：路由进模块自身 `update`，上行事件经 `Message::SettingsEvent` 回收。
        Message::Settings(m) => {
            // 到达即收起活动栏菜单（点菜单项 / 按 OpenSettings 快捷键都会走到这里）。
            if matches!(&m, settings::Message::Toggle) {
                app.settings_menu_open = false;
            }
            let ctx = contexts::settings_ctx(app);
            app.settings.update(m, &ctx).map(Message::SettingsEvent)
        }
        Message::SettingsEvent(e) => events::apply_settings_event(app, e),
        // 活动栏设置菜单：翻转下拉菜单展开态（点外 / Esc 经 DropDown 的 on_dismiss 亦走此处）。
        Message::ToggleSettingsMenu => {
            app.settings_menu_open = !app.settings_menu_open;
            Task::none()
        }
        // 快捷键弹窗模块：路由进模块自身 `update`，上行事件经 `Message::ShortcutsEvent` 回收。
        Message::Shortcuts(m) => {
            // 到达即收起活动栏菜单（点菜单项 / 关闭按钮 / 按快捷键都会走到这里）。
            app.settings_menu_open = false;
            app.shortcuts.update(m).map(Message::ShortcutsEvent)
        }
        Message::ShortcutsEvent(_e) => Task::none(),
        Message::Updates(m) => {
            let ctx = contexts::updates_ctx(app);
            app.updates.update(m, &ctx).map(Message::UpdatesEvent)
        }
        Message::UpdatesEvent(e) => events::apply_updates_event(app, e),
        Message::MasterPw(m) => {
            let ctx = contexts::masterpw_ctx(app);
            app.masterpw.update(m, &ctx).map(Message::MasterPwEvent)
        }
        Message::MasterPwEvent(e) => events::apply_masterpw_event(app, e),
    }
}

/// 处理候选按键：录制态 → Esc → 搜索条 / 删除确认框开条态 → 捕获态 → 键位表，依次分发。
///
/// 录制与 Esc 必须先于「已捕获即丢弃」（遮罩会拦键，否则收不到）；搜索条开条时只放行 Enter / ↑ / ↓。
pub(crate) fn handle_app_key(
    app: &mut App,
    chord: &Chord,
    status: iced::event::Status,
) -> Task<Message> {
    if app.shortcuts.is_recording() {
        return app
            .shortcuts
            .update(shortcuts::Message::RecordChord(*chord))
            .map(Message::ShortcutsEvent);
    }
    if is_plain_escape(chord) {
        return handle_escape(app);
    }
    // 弹窗打开分支必须早于 `Captured` 丢弃：模态遮罩会把按键标为已捕获，而弹窗正需要这些键。
    if app.snippets.palette.open {
        return palette_nav(app, chord);
    }
    if app.search.is_open() {
        return search_nav(app, chord);
    }
    // 删除确认框开态：Enter 即确认（右键 → 删除 → 回车，与命令行删除同步骤数）。
    // 与 palette / 搜索条同理，必须早于 `Captured` 丢弃——模态遮罩会把按键标为已捕获。
    if chord.key == ChordKey::Enter
        && !chord.ctrl
        && !chord.shift
        && !chord.alt
        && !chord.logo
        && matches!(
            app.sftp
                .active(app.tabs.active())
                .and_then(|v| v.dialog.as_ref()),
            Some(crate::state::SftpDialog::DeleteFile { .. })
                | Some(crate::state::SftpDialog::DeleteTree { .. })
        )
        && let Some(tab_id) = app.tabs.active()
    {
        return app
            .sftp
            .update(sftp::Message::SftpDialogConfirm, tab_id)
            .map(Message::SftpEvent);
    }
    if status == iced::event::Status::Captured {
        return Task::none();
    }
    match app.shortcuts.command_for(chord) {
        Some(command) => dispatch_command(app, command),
        None => Task::none(),
    }
}

/// 搜索条开条态的上下文键（不做成可绑定项）：Enter / Shift+Enter / ↑ / ↓ 就地跳转，其余不派发。
fn search_nav(app: &mut App, chord: &Chord) -> Task<Message> {
    if chord.ctrl || chord.alt || chord.logo {
        return Task::none();
    }
    let message = match chord.key {
        ChordKey::Enter if chord.shift => search::Message::Previous,
        ChordKey::Enter | ChordKey::ArrowDown => search::Message::Next,
        ChordKey::ArrowUp => search::Message::Previous,
        _ => return Task::none(),
    };
    app.search.update(message).map(Message::SearchEvent)
}

/// 快速命令弹窗打开态的上下文键：Enter 发送、↑ / ↓ 移动高亮、弹窗唤起键 toggle 关闭，
/// 其余不派发（同 `search_nav` 的取舍——输入框只收字符，方向键与回车由全局分支就地接管）。
fn palette_nav(app: &mut App, chord: &Chord) -> Task<Message> {
    let message = if app.shortcuts.command_for(chord) == Some(shortcuts::Command::OpenSnippets) {
        // 与终端保留集同源查表，用户改键后关闭键随之变化，不写死默认值。
        snippets::Message::TogglePalette
    } else {
        match chord.key {
            ChordKey::Enter => snippets::Message::PaletteSend,
            ChordKey::ArrowDown => snippets::Message::PaletteNext,
            ChordKey::ArrowUp => snippets::Message::PalettePrevious,
            _ => return Task::none(),
        }
    };
    let ctx = contexts::snippets_ctx(app);
    app.snippets
        .update(message, &ctx)
        .map(Message::SnippetsEvent)
}

/// 是否为不带任何修饰键的 Esc（上下文键判据）。
fn is_plain_escape(chord: &Chord) -> bool {
    !chord.ctrl && !chord.shift && !chord.alt && !chord.logo && chord.key == ChordKey::Escape
}

/// 把键位表命中的命令派发为对应模块的既有消息（与菜单 / 按钮入口行为一致）。
pub(crate) fn dispatch_command(app: &mut App, command: shortcuts::Command) -> Task<Message> {
    match command {
        shortcuts::Command::OpenSettings => {
            Task::done(Message::Settings(settings::Message::Toggle))
        }
        shortcuts::Command::OpenShortcuts => {
            Task::done(Message::Shortcuts(shortcuts::Message::Toggle))
        }
        shortcuts::Command::ToggleSidebar => {
            Task::done(Message::Panes(panes::Message::ToggleCenter))
        }
        shortcuts::Command::NewSession => {
            Task::done(Message::Session(session::Message::NewSession))
        }
        // 搜索条绑定单个终端，故取当前活动标签；无活动标签时为空操作。
        shortcuts::Command::OpenSearch => match app.tabs.active() {
            Some(tab_id) => Task::done(Message::Search(search::Message::Open { tab_id })),
            None => Task::none(),
        },
        shortcuts::Command::OpenSnippets => {
            Task::done(Message::Snippets(snippets::Message::TogglePalette))
        }
        shortcuts::Command::SftpRename => {
            Task::done(Message::Sftp(sftp::Message::SftpRenameShortcut))
        }
    }
}

/// 处理 Esc：按层级优先关闭弹窗。
pub(crate) fn handle_escape(app: &mut App) -> iced::Task<Message> {
    if !app.hostkey.is_empty() {
        app.hostkey
            .update(hostkey::Message::Dismiss)
            .map(Message::HostKeyEvent)
    } else if app.session_io.export_open || app.session_io.import_open {
        // 导入导出弹窗压在全部主密码弹窗之上，故先于它们处理；保存框开启或派生 / 落盘进行中都不可
        // 打断，由模块自身忽略本消息——否则对话框的返回值会投进一个已经没人看的状态机。
        let ctx = contexts::session_io_ctx(app);
        let msg = if app.session_io.export_open {
            session_io::Message::ExportCancel
        } else {
            session_io::Message::ImportCancel
        };
        app.session_io
            .update(msg, &ctx)
            .map(Message::SessionIoEvent)
    } else if app.masterpw.confirm_disable {
        // 确认框压在设置弹窗之上，故必须先于设置分支处理，否则 Esc 会穿过它去关底下的弹窗。
        let ctx = contexts::masterpw_ctx(app);
        app.masterpw
            .update(masterpw::Message::DisableCancel, &ctx)
            .map(Message::MasterPwEvent)
    } else if app.paste.pending().is_some() {
        // 粘贴确认框：Esc 等价于取消（丢弃本次粘贴，不写入）。
        let ctx = contexts::paste_ctx(app);
        app.paste
            .update(paste::Message::Cancel, &ctx)
            .map(Message::PasteEvent)
    } else if app.settings.show_settings {
        let ctx = contexts::settings_ctx(app);
        app.settings
            .update(settings::Message::Toggle, &ctx)
            .map(Message::SettingsEvent)
    } else if app.shortcuts.show_shortcuts {
        // 快捷键弹窗与设置弹窗同层（互斥），Esc 等价于点关闭。
        app.shortcuts
            .update(shortcuts::Message::Toggle)
            .map(Message::ShortcutsEvent)
    } else if app.snippets.palette.open {
        // 快速命令弹窗：Esc 关闭并把键盘焦点交还终端（模态层级：设置 / 快捷键之下、搜索条之上）。
        let ctx = contexts::snippets_ctx(app);
        app.snippets
            .update(snippets::Message::ClosePalette, &ctx)
            .map(Message::SnippetsEvent)
    } else if app.search.is_open() {
        // 搜索条是内联输入态：弹窗（含设置）全部让位后优先于会话编辑器 / SFTP 对话框关闭。
        app.search
            .update(search::Message::Close)
            .map(Message::SearchEvent)
    } else if app.session.editor.is_some() {
        let ctx = contexts::session_ctx(app);
        app.session
            .update(session::Message::CancelEdit, &ctx)
            .map(Message::SessionEvent)
    } else if app.snippets.editor.is_some() {
        // 片段编辑弹窗：Esc 丢弃草稿。
        let ctx = contexts::snippets_ctx(app);
        app.snippets
            .update(snippets::Message::CancelEdit, &ctx)
            .map(Message::SnippetsEvent)
    } else if let Some(tab_id) = app.tabs.active() {
        app.sftp
            .update(sftp::Message::SftpCancelDialog, tab_id)
            .map(Message::SftpEvent)
    } else {
        Task::none()
    }
}

/// 处理右键按下：把光标所在的会话 / 文件 / 常用命令行标记为选中，使右键菜单作用于哪一条可见。
///
/// 三个列表都派发（经模块自身 `update`），由模块按各自悬浮态决定选中谁：
/// 光标不在其列表项上时为空操作，故右击终端 / 空白处不会误改选中项。
pub(crate) fn handle_right_press(app: &mut App) -> iced::Task<Message> {
    let tab_id = app.tabs.active().unwrap_or(0);
    let sftp = app
        .sftp
        .update(sftp::Message::SftpSelectHovered, tab_id)
        .map(Message::SftpEvent);
    let ctx = contexts::session_ctx(app);
    let session = app
        .session
        .update(session::Message::SessionSelectHovered, &ctx)
        .map(Message::SessionEvent);
    let snippet_ctx = contexts::snippets_ctx(app);
    let snippets = app
        .snippets
        .update(snippets::Message::SnippetSelectHovered, &snippet_ctx)
        .map(Message::SnippetsEvent);
    Task::batch([sftp, session, snippets])
}

/// 终端右键菜单「复制」：把活动终端的选区文本写入系统剪贴板。
///
/// 空选区不写入，以免清空用户剪贴板（菜单已在无选区时禁用该项，此处仅作兜底）。
fn handle_terminal_copy(app: &mut App) -> iced::Task<Message> {
    let text = app
        .tabs
        .active()
        .and_then(|id| app.tabs.list().iter().find(|t| t.id == id))
        .and_then(|tab| tab.terminal.as_ref())
        .map(|term| term.selection_text())
        .unwrap_or_default();
    if text.is_empty() {
        return iced::Task::none();
    }
    iced::clipboard::write(text)
}

/// 终端右键菜单「粘贴」：把读回的剪贴板文本交给粘贴确认模块（判定后再决定直接写入或弹窗）。
fn handle_terminal_paste(app: &mut App, contents: Option<String>) -> iced::Task<Message> {
    let Some(text) = contents.filter(|s| !s.is_empty()) else {
        return iced::Task::none();
    };
    let Some(tab_id) = app.tabs.active() else {
        return iced::Task::none();
    };
    let ctx = contexts::paste_ctx(app);
    app.paste
        .update(paste::Message::Request { tab_id, text }, &ctx)
        .map(Message::PasteEvent)
}

/// 切换中央视图；切到文件管理时若活动标签尚未打开 SFTP 则自动打开。
pub(crate) fn handle_switch_center(app: &mut App, view: CenterView) -> iced::Task<Message> {
    app.center = view;
    // 切换视图时，如果侧边栏已收起则自动展开
    if app.panes.center_collapsed {
        app.panes.center_collapsed = false;
    }
    // 切到文件管理且当前活动标签尚未打开 SFTP 时，自动在该标签上打开 SFTP，
    // 使面板显示当前标签自己的文件上下文。
    if view == CenterView::Files {
        // 以活动标签自身的状态为准：同会话其它标签未就绪不影响本标签。
        let target = (|| {
            let tab_id = app.tabs.active()?;
            let tab = app.tabs.list().iter().find(|t| t.id == tab_id)?;
            // 该标签已打开 SFTP 则无需自动打开。
            if app.sftp.tab_session(tab.id).is_some() {
                return None;
            }
            (tab.status == ConnectionStatus::Connected).then(|| tab.session_id.clone())
        })();
        if let Some(id) = target {
            return connect::open_files(app, &id).map(Message::Sftp);
        }
    }
    Task::none()
}
