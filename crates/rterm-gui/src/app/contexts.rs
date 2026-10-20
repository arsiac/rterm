//! 父状态到子模块的只读上下文适配层，以及父状态的通用写入动作。

use crate::app::{
    App, forwards, masterpw, paste, remote_edit, session, session_io, settings, snippets, tabs,
    transfer, updates, window,
};
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

/// 组装主窗口模块所需的只读上下文（是否开启窗口大小记忆）。
///
/// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此决定关闭时是否查询 / 写回尺寸，
/// 写回经 `window::Event` 由父层落地。
pub(crate) fn window_ctx(app: &App) -> window::Ctx {
    window::Ctx {
        remember_window_size: app.config.window.remember_size,
    }
}

/// 组装会话文件模块所需的只读上下文（存储 / 保险库 / 会话列表快照）。
/// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此加解密并自行落盘，但**绝不写回**，
/// 合并结果经 `session_io::Event::SetSessions` 由父层落地（与主密码模块同规）。
pub(crate) fn session_io_ctx(app: &App) -> session_io::Ctx {
    session_io::Ctx {
        store: app.session.store.clone(),
        vault: app.vault.clone(),
        sessions: app.session.sessions.clone(),
    }
}

/// 持久化应用级偏好配置（失败则记日志并弹 toast 提示用户）。
pub(crate) fn save_config(app: &mut App) {
    if let Err(e) = app.config.save() {
        error!("failed to save app config: {e}");
        set_toast(
            app,
            ToastKind::Error,
            t!("app.config_save_failed", err => e.to_string()),
        );
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
        remember: app.config.security.remember_master_key,
        sessions_file_unusable: app.sessions_file_problem.is_some(),
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
        auto_check: app.config.updates.auto_check,
        last_check_unix: app.config.updates.last_check_unix,
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

/// 组装粘贴确认模块所需的只读上下文（拦截开关与量级阈值）。
///
/// 每次 `update` 调用前重建，确保模块读到最新父状态；阈值只经此注入、绝不缓存进模块状态，
/// 否则设置界面改动后模块仍读旧值。
pub(crate) fn paste_ctx(app: &App) -> paste::Ctx {
    paste::Ctx {
        paste_warn_enabled: app.config.terminal.paste_warn_enabled,
        paste_warn_lines: app.config.terminal.paste_warn_lines,
        paste_warn_bytes: app.config.terminal.paste_warn_bytes,
        paste_warn_control_chars: app.config.terminal.paste_warn_control_chars,
    }
}

/// 组装传输模块所需的只读上下文（当前活动标签的 SFTP 客户端 + 远端目录 + 全局传输策略）。
///
/// 每次 `update` 调用前重建，确保模块读到最新父状态；模块据此取客户端执行上传 / 下载，
/// 并据并发上限重新调度，但绝不写回父状态，写回经 `transfer::Event` 由父层落地 / 转发。
///
/// 并发上限与自动重试次数**只经此注入**、绝不缓存进传输模块的状态：缓存会让设置界面改动后
/// 模块仍读旧值，表现为「拖动滑块没反应，得再点一下才生效」。
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
        // 各标签**当前**的客户端快照：启动 / 重试时优先取它，而不是传输记录里入队那一刻捕获
        // 的那一份（会话重建后同一标签会换上新客户端，排队中的记录也要用新通道）。
        clients: app.sftp.live_clients().collect(),
        max_concurrent: app.config.transfer.max_concurrent,
        retry_attempts: app.config.transfer.retry_attempts,
    }
}

/// 组装远程编辑模块所需的只读上下文（各标签此刻的 SFTP 客户端 + 外部编辑器命令）。
///
/// 每次 `update` 前重建：会话重建后同一标签会换上新客户端，编辑会话的回传与指纹比对都要用它。
pub(crate) fn remote_edit_ctx(app: &App) -> remote_edit::Ctx {
    remote_edit::Ctx {
        clients: app.sftp.live_clients().collect(),
        editor: app.config.files.editor.clone(),
    }
}

/// 组装命令片段模块所需的只读上下文（发送目标快照）。
///
/// 每次 `update` 前重建；仅含 owned 数据（不借用 `App`），以免与 `app.snippets` 的可变借用冲突。
pub(crate) fn snippets_ctx(app: &App) -> snippets::Ctx {
    let (sessions, total_sessions) =
        snippets::broadcast_targets(app.tabs.list(), app.tabs.active());
    snippets::Ctx {
        active: app.tabs.active(),
        sessions,
        total_sessions,
    }
}

/// 组装端口转发模块所需的只读上下文（作用域会话的规则快照与连接状态）。
///
/// 作用域解析：活动标签优先取其会话（活动标签存在即如此，无论是否已连接）；否则取
/// 无标签时由会话行右键设置的 `forwards.scope`；再否则为空。`live` 仅在作用域来自
/// 活动标签且该标签已连接时为真（此时面板按标签渲染运行时行状态）。
///
/// 每次 `update` 前重建；仅含 owned 数据（不借用 `App`），以免与 `app.forwards` 的
/// 可变借用冲突。
pub(crate) fn forwards_ctx(app: &App) -> forwards::Ctx {
    let active = app
        .tabs
        .active()
        .and_then(|id| app.tabs.list().iter().find(|t| t.id == id));
    let (session_id, live, tab_id) = match active {
        Some(tab) => (
            Some(tab.session_id.clone()),
            tab.status == ConnectionStatus::Connected,
            (tab.status == ConnectionStatus::Connected).then_some(tab.id),
        ),
        None => (app.forwards.scope.clone(), false, None),
    };
    let rules = session_id
        .as_deref()
        .and_then(|id| app.session.sessions.iter().find(|s| s.id == id))
        .map(|s| s.forwards.clone())
        .unwrap_or_default();
    forwards::Ctx {
        session_id,
        rules,
        live,
        tab_id,
        writable: app.vault.is_some(),
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
