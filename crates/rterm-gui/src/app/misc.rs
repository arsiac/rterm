use crate::app::App;
use crate::app::hostkey;
use crate::app::{session, settings, sftp};
use crate::message::Message;
use crate::state::CenterView;
use iced::Task;
use rterm_core::ConnectionStatus;

/// 处理 Esc：按层级优先关闭主机密钥弹窗、设置弹窗、会话编辑器或 SFTP 对话框。
///
/// 除主机密钥外，各分支都**经模块自身 `update` 派发**（`Toggle` / `CancelEdit` /
/// `SftpCancelDialog`），父层不直接改子模块状态——关哪个、清什么由模块自己决定。
pub(crate) fn handle_escape(app: &mut App) -> iced::Task<Message> {
    if !app.hostkey.is_empty() {
        app.hostkey
            .update(hostkey::Message::Dismiss)
            .map(Message::HostKeyEvent)
    } else if app.settings.show_settings {
        let ctx = app.settings_ctx();
        app.settings
            .update(settings::Message::Toggle, &ctx)
            .map(Message::SettingsEvent)
    } else if app.session.editor.is_some() {
        let ctx = app.session_ctx();
        app.session
            .update(session::Message::CancelEdit, &ctx)
            .map(Message::SessionEvent)
    } else if let Some(tab_id) = app.tabs.active() {
        app.sftp
            .update(sftp::Message::SftpCancelDialog, tab_id)
            .map(Message::SftpEvent)
    } else {
        Task::none()
    }
}

/// 处理右键按下：把光标所在的会话 / 文件条目标记为选中，使右键菜单作用于哪一条可见。
///
/// 两个列表都派发（经模块自身 `update`），由模块按各自己的悬浮态决定选中谁：
/// 光标不在其列表项上时为空操作，故右击终端 / 空白处不会误改选中项。
pub(crate) fn handle_right_press(app: &mut App) -> iced::Task<Message> {
    let tab_id = app.tabs.active().unwrap_or(0);
    let sftp = app
        .sftp
        .update(sftp::Message::SftpSelectHovered, tab_id)
        .map(Message::SftpEvent);
    let ctx = app.session_ctx();
    let session = app
        .session
        .update(session::Message::SessionSelectHovered, &ctx)
        .map(Message::SessionEvent);
    Task::batch([sftp, session])
}

/// 切换中央视图；切到文件管理时若活动标签尚未打开 SFTP 则自动打开。
pub(crate) fn handle_switch_center(app: &mut App, view: CenterView) -> iced::Task<Message> {
    app.center = view;
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
            return app.open_files(&id).map(Message::Sftp);
        }
    }
    Task::none()
}
