//! 消息路由表：把顶层 [`Message`] 分发到各子模块的 `update` 或 `events` 的落地函数。

use crate::app::App;
use crate::app::connect;
use crate::app::contexts;
use crate::app::{events, hostkey, session, settings, sftp};
use crate::message::Message;
use crate::state::{CenterView, ToastKind};
use crate::t;
use iced::Task;
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
        // 终端标签模块：路由进模块自身 `update`，上行事件经 `Message::TabsEvent` 回收。
        Message::Tabs(m) => {
            let ctx = contexts::tabs_ctx(app);
            app.tabs.update(m, &ctx, &app.sftp).map(Message::TabsEvent)
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
        Message::Escape => handle_escape(app),
        // 右键按下：两个列表各自按自己的悬浮态决定选中谁，详见 `handle_right_press`。
        Message::RightPress => handle_right_press(app),
        // 设置弹窗模块：路由进模块自身 `update`，上行事件经 `Message::SettingsEvent` 回收。
        Message::Settings(m) => {
            let ctx = contexts::settings_ctx(app);
            app.settings.update(m, &ctx).map(Message::SettingsEvent)
        }
        Message::SettingsEvent(e) => events::apply_settings_event(app, e),
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
        let ctx = contexts::settings_ctx(app);
        app.settings
            .update(settings::Message::Toggle, &ctx)
            .map(Message::SettingsEvent)
    } else if app.session.editor.is_some() {
        let ctx = contexts::session_ctx(app);
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
    let ctx = contexts::session_ctx(app);
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
            return connect::open_files(app, &id).map(Message::Sftp);
        }
    }
    Task::none()
}
