//! 「会话 ↔ 标签 ↔ 连接」生命周期联动：解密凭据、建立连接、开关标签、打开文件管理。

use crate::app::App;
use crate::app::sftp;
use crate::app::tasks::connect_stream_task;
use crate::message::Message;
use crate::state::CenterView;
use crate::t;
use iced::Task;
use rterm_config::{AuthMethod, SessionConfig};
use rterm_core::{ConnectionStatus, HopSpec, SessionSecrets};
use rterm_crypto::Vault;

/// 由某一认证方式 + 保险库解密出连接所需的明文凭据。
///
/// 目标主机与每一跳跳板机共用：跳板机的 `auth` 与目标同形，故凭据解密逻辑只写一份。
fn build_secrets(auth: &AuthMethod, vault: &Vault) -> Result<SessionSecrets, String> {
    match auth {
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

/// 组装跳板链的建连输入（顺序与 `cfg.jumps` 一致，由外到内；每一跳各解密自己的凭据）。
fn build_jumps(cfg: &SessionConfig, vault: &Vault) -> Result<Vec<HopSpec>, String> {
    cfg.jumps
        .iter()
        .map(|hop| {
            Ok(HopSpec {
                config: hop.clone(),
                secrets: build_secrets(&hop.auth, vault)?,
            })
        })
        .collect()
}

/// 打开某会话的文件管理：定位目标标签、切换导航态，视图重置与建通道 / 列举交 sftp 模块。
///
/// SFTP 视图归属于该会话的某个终端标签（每标签独立记录自己的文件上下文）：
/// 优先使用当前活动标签（若它正属于该会话），否则取该会话的第一个标签。
///
/// 目标标签已打开文件管理时只切视图与活动标签，不重复派发打开消息：那会重置浏览路径与
/// 列表，把用户已深入的目录打回远端家目录。
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
    // 该标签已打开文件管理：保留其浏览位置与进行中的内联输入，仅切到此视图即可。
    if app.sftp.tab_session(tab_id).is_some() {
        return Task::none();
    }
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
/// 由 `handle_delete_session` 在删除会话时调用。返回被移除的标签 id，供调用方补做按标签的
/// 清理（如通知传输模块中止任务并归还并发额度）——整体移除不会逐个走标签关闭事件。
pub(crate) fn close_session_tabs(app: &mut App, id: &str) -> Vec<u64> {
    let closed: Vec<u64> = app
        .tabs
        .list()
        .iter()
        .filter(|t| t.session_id == id)
        .map(|t| t.id)
        .collect();
    app.tabs.remove_by_session(id);
    closed
}

/// 连接前置条件不满足（保险库未解锁 / 凭据解密失败）时，把已受理的标签退回失败态。
///
/// 受理方已把标签置为「连接中」，不退回会永久停在连接中且重连入口被防重复挡下；
/// 退回后原因显示在该标签上（覆盖层 / 横幅），可修正后重试。
fn fail_tab(app: &mut App, tab_id: u64, msg: String) -> Task<Message> {
    if let Some(tab) = app.tabs.tab_mut(tab_id) {
        tab.status = ConnectionStatus::Error;
        tab.error = Some(msg.clone());
    }
    app.status = Some(msg);
    Task::none()
}

/// 为指定标签发起 SSH 连接任务：解密凭据 → 生成连接配置 → 拉起异步握手。
pub(crate) fn connect_session(app: &mut App, tab_id: u64, id: &str) -> Task<Message> {
    let Some(cfg) = app.session.sessions.iter().find(|s| s.id == id).cloned() else {
        // 会话记录已不存在（其标签通常已被一并关闭，此处仅为防御）：退回失败态，
        // 否则重连受理时置下的「连接中」无人复位，标签会卡在「正在重新连接…」。
        return fail_tab(app, tab_id, t!("app.session_missing"));
    };
    // 凭据信封必须由保险库解密为明文后再交给连接任务（core 层不持有主密钥）。
    let Some(vault) = app.vault.clone() else {
        return fail_tab(app, tab_id, t!("app.vault_locked").to_string());
    };
    let secrets = match build_secrets(&cfg.auth, &vault) {
        Ok(s) => s,
        Err(e) => return fail_tab(app, tab_id, e),
    };
    // 跳板链的每跳凭据各自解密；任一跳缺凭据都会在建立隧道前拦下。
    let jumps = match build_jumps(&cfg, &vault) {
        Ok(j) => j,
        Err(e) => return fail_tab(app, tab_id, e),
    };
    let id = id.to_string();
    // 开始建立连接：记录会话与标签，便于追踪连接生命周期与失败排查。
    log::info!("Establishing connection: session {id} (tab {tab_id})");
    // 建连参数取当前配置的快照一次性传入（超时、保活间隔、保活判死次数，各自语义见
    // `ConnectOptions` 的字段文档；三者只在建连时生效，改动需重连）。
    // 用 stream 而非 perform：握手可能在主机密钥弹窗处中途暂停，需要向 GUI
    // 发送中途消息后再等用户决定，perform 只有唯一最终输出无法胜任。
    let opts = app.config.connection.connect_options();
    Task::stream(iced::stream::channel(
        8,
        move |mut output: futures::channel::mpsc::Sender<Message>| async move {
            connect_stream_task(tab_id, id, cfg, secrets, jumps, opts, &mut output).await;
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
