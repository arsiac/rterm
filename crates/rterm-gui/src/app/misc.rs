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
