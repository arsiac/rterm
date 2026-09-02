//! 「会话 ↔ 标签 ↔ 连接」生命周期联动：解密凭据、建立连接、开关标签、打开文件管理。

use crate::app::App;
use crate::app::sftp;
use crate::app::tasks::connect_stream_task;
use crate::message::Message;
use crate::state::CenterView;
use crate::t;
use iced::Task;
use rterm_config::{AuthMethod, SessionConfig};
use rterm_core::SessionSecrets;
use rterm_crypto::Vault;

/// 由会话配置 + 保险库解密出连接所需的明文凭据。
fn build_secrets(cfg: &SessionConfig, vault: &Vault) -> Result<SessionSecrets, String> {
    match &cfg.auth {
        AuthMethod::Password { password } => {
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
        AuthMethod::PublicKey { passphrase, .. } => {
            let kp = match passphrase {
                Some(env) => Some(vault.decrypt(env).map_err(|_| t!("app.decrypt_failed"))?),
                None => None,
            };
            Ok(SessionSecrets {
                password: None,
                key_passphrase: kp,
            })
        }
        AuthMethod::Agent => Ok(SessionSecrets {
            password: None,
            key_passphrase: None,
        }),
    }
}

/// 打开某会话的文件管理：定位目标标签、切换导航态，视图重置与建通道 / 列举交 sftp 模块。
///
/// SFTP 视图归属于该会话的某个终端标签（每标签独立记录自己的文件上下文）：
/// 优先使用当前活动标签（若它正属于该会话），否则取该会话的第一个标签。
///
/// 返回 `Task<sftp::Message>`：由调用方 `.map(Message::Sftp)` 接入顶层路由。
/// 父层只挑标签与切导航态，**不写 SFTP 视图**——那部分在 `sftp::Message::SftpOpenSession`
/// 里由模块自己完成。
pub(crate) fn open_files(app: &mut App, id: &str) -> Task<sftp::Message> {
    // 优先当前活动标签（用户正交互的那个）；否则取该会话的第一个标签。
    let tab_id = app
        .tabs
        .active()
        .and_then(|active| {
            app.tabs
                .list()
                .iter()
                .find(|t| t.id == active && t.session_id == id)
                .map(|t| t.id)
        })
        .or_else(|| {
            app.tabs
                .list()
                .iter()
                .find(|t| t.session_id == id)
                .map(|t| t.id)
        });
    let Some(tab_id) = tab_id else {
        app.status = Some(t!("app.open_terminal_first"));
        return Task::none();
    };
    // 聚焦该会话的终端标签，使终端与文件管理上下文保持一致。
    app.center = CenterView::Files;
    app.tabs.set_active(tab_id);
    app.active_session = Some(id.to_string());
    // 优先复用该标签已建立的 SFTP 通道；否则取该标签独占的 SSH 连接用于新建通道。
    let client = app.sftp.tab(tab_id).and_then(|s| s.client.clone());
    let conn = app
        .tabs
        .list()
        .iter()
        .find(|t| t.id == tab_id)
        .and_then(|t| t.conn.clone());
    if client.is_none() && conn.is_none() {
        app.status = Some(t!("app.connect_session_first"));
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
pub(crate) fn close_session_tabs(app: &mut App, id: &str) {
    app.tabs.remove_by_session(id);
}

/// 为指定标签发起 SSH 连接任务：解密凭据 → 生成连接配置 → 拉起异步握手。
pub(crate) fn connect_session(app: &mut App, tab_id: u64, id: &str) -> Task<Message> {
    let Some(cfg) = app.session.sessions.iter().find(|s| s.id == id).cloned() else {
        return Task::none();
    };
    // 凭据信封必须由保险库解密为明文后再交给连接任务（core 层不持有主密钥）。
    let Some(vault) = app.vault.clone() else {
        app.status = Some(t!("app.vault_locked"));
        return Task::none();
    };
    let secrets = match build_secrets(&cfg, &vault) {
        Ok(s) => s,
        Err(e) => {
            app.status = Some(e);
            return Task::none();
        }
    };
    let id = id.to_string();
    // 连接超时取自应用配置；0 表示不限制。
    // 用 stream 而非 perform：握手可能在主机密钥弹窗处中途暂停，需要向 GUI
    // 发送中途消息后再等用户决定，perform 只有唯一最终输出无法胜任。
    let timeout = app.config.connect_timeout;
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
/// 连接结果由 [`connect_session`] 异步发起、经 `SessionConnected` 回流处理。
/// 返回新标签 id，连接结果据此回落到本标签，不影响同会话的其它标签。
pub(crate) fn open_tab(app: &mut App, id: &str) -> u64 {
    let title = app
        .session
        .sessions
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| id.to_string());
    let tab_id = app.tabs.add(id.to_string(), title);
    // SFTP 视图随标签创建（由 sftp 模块按标签 id 管理）
    app.sftp.ensure(tab_id);
    tab_id
}
