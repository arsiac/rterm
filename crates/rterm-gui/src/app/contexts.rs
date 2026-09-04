//! 父状态到子模块的只读上下文适配层，以及父状态的通用写入动作。

use crate::app::{App, masterpw, session, settings, tabs, transfer, updates};
use crate::state::{SftpView, ToastKind};
use crate::t;
use crate::widget::toast::{ToastLevel, toast};
use log::error;
use rterm_core::ConnectionStatus;

/// 取当前活动标签的 SFTP 视图（只读，供渲染层使用）。无活动标签时返回 `None`。
pub(crate) fn active_sftp(app: &App) -> Option<&SftpView> {
    app.sftp.active(app.tabs.active())
}

/// 会话列表用的聚合状态：取该会话全部标签中「最靠前」的那个状态
/// （已连接 > 连接中 > 失败 > 未连接）。
///
/// 连接状态本身按标签独立维护，这里只做派生展示：同会话只要还有一个标签连着，
/// 会话行就显示已连接，不因新开标签把它打回「连接中」。
pub(crate) fn session_status(app: &App, id: &str) -> ConnectionStatus {
    let mut has_connecting = false;
    let mut has_error = false;
    for tab in app.tabs.list().iter().filter(|t| t.session_id == id) {
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

/// 组装会话管理模块所需的只读上下文（当前保险库）。
///
/// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此加密保存凭据但绝不写回，
/// 写回经 `session::Event` 由父层落地。
pub(crate) fn session_ctx(app: &App) -> session::Ctx {
    session::Ctx {
        vault: app.vault.clone(),
    }
}

/// 持久化应用级偏好配置（忽略失败并记录日志）。
pub(crate) fn save_config(app: &mut App) {
    if let Err(e) = app.config.save() {
        error!("failed to save app config: {e}");
    }
}

/// 组装主密码模块所需的只读上下文（store / vault / sessions / 记住开关）。
///
/// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此计算但绝不写回，
/// 写回经 `masterpw::Event` 由父层落地。
pub(crate) fn masterpw_ctx(app: &App) -> masterpw::Ctx {
    masterpw::Ctx {
        store: app.session.store.clone(),
        vault: app.vault.clone(),
        sessions: app.session.sessions.clone(),
        remember: app.config.remember_master_key,
    }
}

/// 组装设置弹窗模块所需的只读上下文（当前 `AppConfig`）。
///
/// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此构建下拉框选项但绝不写回，
/// 写回经 `settings::Event` 由父层落地。
pub(crate) fn settings_ctx(app: &App) -> settings::Ctx {
    settings::Ctx {
        config: app.config.clone(),
    }
}

/// 组装更新检查模块所需的只读上下文（自动检查开关 + 上次检查时间戳）。
///
/// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此判定节流，
/// 但**绝不写父状态**，时间戳写回经 `updates::Event::SetLastCheck` 由父层落地。
pub(crate) fn updates_ctx(app: &App) -> updates::Ctx {
    updates::Ctx {
        auto_check: app.config.auto_check_updates,
        last_check_unix: app.config.last_update_check_unix,
    }
}

/// 组装终端标签模块所需的只读上下文（中心视图 / 活动会话 / 终端聚焦态 / 失焦保存态）。
///
/// 每次 `update` 调用前重建，确保模块读到最新父状态；仅含 owned 数据（不借用 `App`），
/// 以免与 `app.tabs` 的可变借用冲突。SFTP 查询经 `update` 的 `sftp` 参数单独传入。
pub(crate) fn tabs_ctx(app: &App) -> tabs::Ctx {
    tabs::Ctx {
        center: app.center,
        active_session: app.active_session.clone(),
        terminal_focused: app.terminal_focused,
        window_focus_saved: app.window_focus_saved,
    }
}

/// 组装传输模块所需的只读上下文（当前活动标签的 SFTP 客户端 + 远端目录）。
///
/// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此取客户端执行上传 / 下载，
/// 但绝不写回父状态，写回经 `transfer::Event` 由父层落地 / 转发。
pub(crate) fn transfer_ctx(app: &App) -> transfer::Ctx {
    let tab_id = app.tabs.active().unwrap_or(0);
    let (client, remote_dir) = app
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
///
/// [`TOAST_TTL`]: super::TOAST_TTL
pub(crate) fn set_toast(app: &mut App, kind: ToastKind, msg: String) {
    let (level, title) = match kind {
        ToastKind::Success => (ToastLevel::Success, t!("app.toast.success")),
        ToastKind::Error => (ToastLevel::Error, t!("app.toast.error")),
        ToastKind::Warning => (ToastLevel::Warning, t!("app.toast.warning")),
    };
    app.toaster.push(
        toast(msg)
            .level(level)
            .title(title)
            .duration(super::TOAST_TTL.as_secs()),
    );
}

/// 是否允许开启 S3 / WebDAV 同步。
///
/// 仅当用户已设置主密码（模式 1，`master_password_set = true`）时为 `true`：
/// 该模式下凭据由主密码派生 DEK 加密、文件自包含，可跨设备靠口令解密；
/// 模式 0（随机密钥仅存本机钥匙串）的凭据无法在别的设备还原，故禁止同步。
/// 同步功能接入时，在同步开关 handler / 面板调用此判定并据此禁用 / 拦截开启。
pub(crate) fn can_enable_sync(app: &App) -> bool {
    app.vault
        .as_ref()
        .map(|v| v.header().master_password_set)
        .unwrap_or(false)
}
