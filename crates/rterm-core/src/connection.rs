//! 基于 russh 的 SSH 连接管理。
//!
//! 每个 [`SshConnection`] 封装一条到远程主机的 russh 连接，可复用同一条连接
//! 打开多个通道：交互式 shell 通道（供终端标签页桥接）与 sftp 子系统通道
//! （供文件管理面板使用）。

use crate::{CoreError, CoreErrorKind, host_key};
use log::debug;
use rterm_config::{AuthMethod, JumpHost, SessionConfig};
use russh::Pty;
use russh::client::{self, Config, Handle, Handler};
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};
use zeroize::Zeroizing;

/// 弹窗所需的密钥信息。
///
/// `mismatch` 为 `Some` 时表示与 known_hosts 已记录指纹不一致（携带旧指纹），
/// GUI 应渲染红色警告形态而非普通确认框。
#[derive(Clone)]
pub struct HostKeyPrompt {
    /// 目标主机名或 IP 地址（用于展示与 known_hosts 条目定位）。
    pub host: String,
    /// 目标端口（SSH 默认 22）。
    pub port: u16,
    /// 密钥算法类型（如 `ssh-ed25519`、`ssh-rsa`）。
    pub key_type: String,
    /// 服务器公钥的指纹（用于展示与比对）。
    pub fingerprint: String,
    /// 与 known_hosts 已记录指纹不一致时携带旧指纹；为 `None` 表示未知主机。
    pub mismatch: Option<String>,
}

/// 连接所需的明文凭据（已用保险库解密，仅短暂停留在内存）。
///
/// 由 GUI 在发起连接前用凭据保险库（`rterm-crypto` 的 `Vault`）解密
/// [`rterm_config::AuthMethod`] 中的信封得到，随后传入 [`SshConnection::connect`]。
/// 明文以 [`Zeroizing`] 持有，drop 时自动擦除。
///
/// 本 crate 不依赖 `rterm-crypto`：解密在 GUI 侧完成，核心层只认明文，便于独立测试。
#[derive(Clone, Default)]
pub struct SessionSecrets {
    /// 密码认证口令；非密码认证时为 `None`。
    pub password: Option<Zeroizing<String>>,
    /// 私钥口令；无口令或公钥认证未设置时为 `None`。
    pub key_passphrase: Option<Zeroizing<String>>,
}

/// 一跳跳板机的建连输入：连接参数 + 已解密凭据。
///
/// 由 GUI 在发起连接前组装（顺序与 [`rterm_config::SessionConfig::jumps`] 一致，由外到内）；
/// 目标主机本身的连接参数与凭据仍走 [`SshConnection::connect`] 的 `config` / `secrets` 形参。
#[derive(Clone)]
pub struct HopSpec {
    /// 跳板机连接参数。
    pub config: JumpHost,
    /// 跳板机凭据明文（已由保险库解密，`Zeroizing` 持有）。
    pub secrets: SessionSecrets,
}

/// 一次握手与认证所需的端点信息（目标主机或某一跳跳板机）。
///
/// 目标用 [`SessionConfig`]、跳板机用 [`JumpHost`]，二者字段同形但类型不同；
/// 本结构把握手路径真正要用的部分抽出，避免为跳板机伪造一个 [`SessionConfig`]。
#[derive(Clone, Copy)]
struct Endpoint<'a> {
    host: &'a str,
    port: u16,
    username: &'a str,
    auth: &'a AuthMethod,
}

impl<'a> From<&'a SessionConfig> for Endpoint<'a> {
    fn from(c: &'a SessionConfig) -> Self {
        Self {
            host: &c.host,
            port: c.port,
            username: &c.username,
            auth: &c.auth,
        }
    }
}

impl<'a> From<&'a JumpHost> for Endpoint<'a> {
    fn from(j: &'a JumpHost) -> Self {
        Self {
            host: &j.host,
            port: j.port,
            username: &j.username,
            auth: &j.auth,
        }
    }
}

/// 用户决定的回复句柄；用 `Option` + `take()` 保证 `reply` 只生效一次。
///
/// iced `Message` 要求 `Clone`，而这里需要「一次性」语义（`watch::Sender` 本身是可 `Clone`
/// 的，问题不在能否克隆，而在必须只能回复一次），故把 `Sender` 包进 `Option` 由 `take` 消费。
///
/// [`HostKeyReply::decided`] 返回 `None` 表示没有可用决定（发送端已被 `reply` 取走，
/// 或底层 `watch` 通道被丢弃）；调用方一律视为拒绝。
#[derive(Clone)]
pub struct HostKeyReply {
    /// 一次性的信任决定发送端；用 `Option` 包裹以便 `reply` 以 `take()` 消费，
    /// 保证同一句柄只回复一次。
    tx: Arc<Mutex<Option<watch::Sender<Option<bool>>>>>,
}

impl HostKeyReply {
    /// 创建一对 `watch` 通道并返回包裹一次性发送端的回复句柄。
    ///
    /// 公开此构造器以便测试与诊断：连接握手路径经 [`SshConnection::connect`] 拿到句柄后
    /// 在 `decided()` 上挂起等待 `reply`；测试可借此模拟用户决定。
    pub fn new() -> Self {
        Self {
            tx: Arc::new(Mutex::new(Some(watch::channel(None).0))),
        }
    }

    /// 回复用户的信任决定（仅首次调用生效）。
    pub fn reply(&self, trust: bool) {
        if let Some(tx) = self.tx.lock().unwrap().take() {
            let _ = tx.send(Some(trust));
        }
    }

    /// 等待用户做出决定；`Option` 已被 `reply` 取走时立即返回 `None`（视为拒绝）。
    pub async fn decided(&self) -> Option<bool> {
        let mut rx = self.tx.lock().unwrap().as_ref()?.subscribe();
        loop {
            // changed 在无新值时挂起；发送端被丢弃则返回 Err。
            // 本函数持有 `Arc` 克隆，等待期间发送端不会被全部丢弃，故 Err 实际不可达。
            rx.changed().await.ok()?;
            if let Some(v) = *rx.borrow_and_update() {
                return Some(v);
            }
        }
    }
}

impl Default for HostKeyReply {
    /// 等价于 [`HostKeyReply::new`]，返回携带一次性发送端的回复句柄。
    fn default() -> Self {
        Self::new()
    }
}

/// russh 客户端处理器：在握手时校验服务器主机密钥，未知或变更时经 `prompt_tx` 请求用户确认。
pub(crate) struct ClientHandler {
    /// 目标主机（用于 known_hosts 条目定位与弹窗展示）。
    host: String,
    /// 目标端口（SSH 默认 22）。
    port: u16,
    /// 未知 / 变更密钥时向 GUI 发送确认请求；接收端被丢弃意味着无人能确认，按拒绝处理。
    prompt_tx: mpsc::Sender<(HostKeyPrompt, HostKeyReply)>,
}

impl Handler for ClientHandler {
    /// 该 Handler 的错误类型，复用 russh 的 `Error`。
    type Error = russh::Error;

    /// 校验服务器主机密钥：known_hosts 命中且一致则静默接受；未知或指纹变更时
    /// 经 `prompt_tx` 请求用户确认并在握手内原地等待（russh 的 async Handler
    /// 允许任意长时间 await）。返回 `false` 使 russh 以 `UnknownKey` 中止握手。
    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let fp = host_key::fingerprint(key);
        let status = match host_key::check_host_key(&self.host, self.port, &fp) {
            Ok(status) => status,
            Err(e) => {
                log::error!(
                    "Failed to read known_hosts, rejecting {host}:{port}: {e}",
                    host = self.host,
                    port = self.port
                );
                return Ok(false);
            }
        };
        let mismatch = match &status {
            host_key::HostKeyStatus::Known => return Ok(true),
            host_key::HostKeyStatus::Mismatch { stored } => Some(stored.clone()),
            host_key::HostKeyStatus::Unknown => None,
        };

        let prompt = HostKeyPrompt {
            host: self.host.clone(),
            port: self.port,
            key_type: host_key::key_type(key),
            fingerprint: fp.clone(),
            mismatch,
        };
        let reply = HostKeyReply::new();
        let is_mismatch = prompt.mismatch.is_some();
        if self.prompt_tx.send((prompt, reply.clone())).await.is_err() {
            log::warn!(
                "Host key confirmation channel closed, treating as rejected {host}:{port}",
                host = self.host,
                port = self.port
            );
            return Ok(false);
        }
        // 无人回复（句柄被丢弃 / 通道关闭）一律视为拒绝。
        let trusted = reply.decided().await.unwrap_or(false);
        if !trusted {
            debug!(
                "用户拒绝 {host}:{port} 的主机密钥 {fp}",
                host = self.host,
                port = self.port
            );
            return Ok(false);
        }

        // 用户接受后落盘：未知主机追加新条目，指纹变更覆盖旧条目。
        let result = if is_mismatch {
            host_key::replace_host_key(&self.host, self.port, &fp)
        } else {
            host_key::trust_host_key(&self.host, self.port, &fp)
        };
        match result {
            Ok(()) => Ok(true),
            Err(e) => {
                log::error!(
                    "Failed to record known host, rejecting {host}:{port}: {e}",
                    host = self.host,
                    port = self.port
                );
                Ok(false)
            }
        }
    }
}

/// 一条已建立的 SSH 连接。
///
/// 同一条连接上可打开多个通道（shell / sftp）；内部句柄 [`Handle`] 包裹在 [`Arc`] +
/// [`AsyncMutex`] 中，使 GUI 的多个并发任务可以安全地持有它。
///
/// 注意：一条连接对应一个终端标签，同会话多标签各自建连，不共享同一条连接。
pub struct SshConnection {
    /// russh 客户端句柄（可克隆引用，受互斥锁保护以支持并发通道操作）。
    handle: Arc<AsyncMutex<Handle<ClientHandler>>>,
    /// 中间跳的句柄（由外到内）。
    ///
    /// 仅用于 [`Self::disconnect`] 显式断开整条链与诊断；russh 会话任务不靠句柄续命，
    /// 任一跳死亡都会让目标流 EOF、目标会话收尾，[`Self::is_closed`] 随之转真。
    jumps: Vec<Handle<ClientHandler>>,
}

/// 组装 russh 客户端配置：保活间隔与判死次数由宿主传入，其余取 russh 默认。
///
/// `keepalive_max` 语义照 russh：`0` = **照常发保活包但永不因无应答判死**；`n > 0` =
/// 无应答计数超过 `n` 时断开，即约 `(n + 1)` 个保活周期后才判定连接已死。
/// 上界由配置层裁剪（`rterm_config::MAX_KEEPALIVE_MAX`），此处不再兜底。
fn client_config(keepalive: Option<Duration>, keepalive_max: usize) -> Config {
    Config {
        keepalive_interval: keepalive,
        keepalive_max,
        ..Default::default()
    }
}

/// 在已建立传输并完成密钥交换的句柄上完成认证（密码 / 公钥 / agent）。
///
/// 目标主机与每一跳跳板机都复用本函数；凭据已由调用方解密为明文（见 [`SessionSecrets`]）。
async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    endpoint: Endpoint<'_>,
    secrets: &SessionSecrets,
) -> Result<(), CoreError> {
    match endpoint.auth {
        AuthMethod::Password { .. } => {
            debug!("使用密码认证: {}", endpoint.username);
            let password = secrets
                .password
                .as_ref()
                .ok_or_else(|| CoreError::ssh_msg(CoreErrorKind::MissingPassword))?;
            let result = handle
                .authenticate_password(endpoint.username, password.as_str())
                .await
                .map_err(|e| CoreError::ssh(CoreErrorKind::AuthPasswordRequest, e))?;
            if !result.success() {
                return Err(CoreError::ssh_msg(CoreErrorKind::AuthPasswordRejected));
            }
        }
        AuthMethod::PublicKey {
            key_path,
            passphrase: _,
        } => {
            debug!("使用公钥认证: {}", key_path.display());
            let pass = secrets.key_passphrase.as_ref().map(|p| p.as_str());
            let key = russh::keys::PrivateKey::read_openssh_file(key_path)
                .map_err(|e| CoreError::ssh(CoreErrorKind::ReadKey, e))?;
            let key = match pass {
                Some(pass) => key
                    .decrypt(pass)
                    .map_err(|e| CoreError::ssh(CoreErrorKind::DecryptKey, e))?,
                None => key,
            };
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
            let result = handle
                .authenticate_publickey(endpoint.username, key)
                .await
                .map_err(|e| CoreError::ssh(CoreErrorKind::AuthPublicKeyRequest, e))?;
            if !result.success() {
                return Err(CoreError::ssh_msg(CoreErrorKind::AuthPublicKeyRejected));
            }
        }
        AuthMethod::Agent => {
            debug!("使用 SSH agent 认证");
            #[cfg(unix)]
            let mut agent = russh::keys::agent::client::AgentClient::connect_env()
                .await
                .map_err(|e| CoreError::ssh(CoreErrorKind::AgentConnect, e))?;
            #[cfg(windows)]
            let mut agent = russh::keys::agent::client::AgentClient::connect_pageant()
                .await
                .map_err(|e| CoreError::ssh(CoreErrorKind::AgentConnectPageant, e))?;
            let identities = agent
                .request_identities()
                .await
                .map_err(|e| CoreError::ssh(CoreErrorKind::AgentIdentities, e))?;
            let mut authed = false;
            for id in identities {
                let pubkey = id.public_key().into_owned();
                if let Ok(result) = handle
                    .authenticate_publickey_with(endpoint.username, pubkey, None, &mut agent)
                    .await
                    && result.success()
                {
                    authed = true;
                    break;
                }
            }
            if !authed {
                return Err(CoreError::ssh_msg(CoreErrorKind::AgentAuthFailed));
            }
        }
    }
    debug!("SSH 认证成功: {}", endpoint.username);
    Ok(())
}

/// 建立一跳的连接并完成认证（不含跳板机错误归属包装）。
///
/// `prev` 为 `None` 时直连 `endpoint`（TCP）；为 `Some` 时先在上一跳句柄上开一条到
/// `endpoint` 的 `direct-tcpip` 转发通道，再以该通道的流作为传输层跑 SSH 握手。
async fn establish_hop(
    prev: Option<&Handle<ClientHandler>>,
    endpoint: Endpoint<'_>,
    secrets: &SessionSecrets,
    ssh_config: &Arc<Config>,
    prompt_tx: mpsc::Sender<(HostKeyPrompt, HostKeyReply)>,
) -> Result<Handle<ClientHandler>, CoreError> {
    let handler = ClientHandler {
        host: endpoint.host.to_string(),
        port: endpoint.port,
        prompt_tx,
    };
    let mut handle = match prev {
        None => client::connect(
            Arc::clone(ssh_config),
            (endpoint.host, endpoint.port),
            handler,
        )
        .await
        .map_err(|e| CoreError::ssh(CoreErrorKind::Connect, e))?,
        Some(prev) => {
            // originator 是信息性字段（服务端一般不校验），统一填回环地址。
            let channel = prev
                .channel_open_direct_tcpip(
                    endpoint.host.to_string(),
                    endpoint.port as u32,
                    "127.0.0.1",
                    0,
                )
                .await
                .map_err(|e| CoreError::ssh(CoreErrorKind::ChannelOpen, e))?;
            client::connect_stream(Arc::clone(ssh_config), channel.into_stream(), handler)
                .await
                .map_err(|e| CoreError::ssh(CoreErrorKind::Connect, e))?
        }
    };
    authenticate(&mut handle, endpoint, secrets).await?;
    Ok(handle)
}

/// 把某一跳的失败包上跳序号与该跳 host，便于多跳排障与本地化文案。
fn hop_error(index: usize, host: &str, err: CoreError) -> CoreError {
    CoreError::ssh(
        CoreErrorKind::JumpConnect {
            index,
            host: host.to_string(),
        },
        err,
    )
}

impl SshConnection {
    /// 根据会话配置建立并认证一条 SSH 连接（密码 / 公钥 / agent，主机密钥经 `prompt_tx` 询问）。
    ///
    /// `jumps` 为跳板链（由外到内，空即直连）：逐跳建隧道到 `config`；`keepalive` /
    /// `keepalive_max` 为整条链的保活间隔与判死阈值（`None` / `0` 表示关闭），仅建连时生效。
    pub async fn connect(
        config: &SessionConfig,
        secrets: &SessionSecrets,
        jumps: &[HopSpec],
        keepalive: Option<Duration>,
        keepalive_max: u64,
        prompt_tx: mpsc::Sender<(HostKeyPrompt, HostKeyReply)>,
    ) -> Result<Self, CoreError> {
        debug!(
            "正在连接 {}:{}（经 {} 跳跳板机）",
            config.host,
            config.port,
            jumps.len()
        );
        // `keepalive_max` 已由配置层裁剪到上界，此处仅做 russh 所需的窄化。
        let ssh_config = Arc::new(client_config(keepalive, keepalive_max as usize));

        // 逐跳建立隧道：第 0 跳直连，其后各跳都经上一跳的 direct-tcpip 转发。
        let mut jump_handles: Vec<Handle<ClientHandler>> = Vec::with_capacity(jumps.len());
        for (index, hop) in jumps.iter().enumerate() {
            let handle = establish_hop(
                jump_handles.last(),
                Endpoint::from(&hop.config),
                &hop.secrets,
                &ssh_config,
                prompt_tx.clone(),
            )
            .await
            .map_err(|e| hop_error(index, &hop.config.host, e))?;
            jump_handles.push(handle);
        }

        // 目标主机：经最后一跳转发（无跳板机时即直连）。
        let handle = establish_hop(
            jump_handles.last(),
            Endpoint::from(config),
            secrets,
            &ssh_config,
            prompt_tx,
        )
        .await?;

        debug!("SSH 认证成功: {}", config.username);
        Ok(Self {
            handle: Arc::new(AsyncMutex::new(handle)),
            jumps: jump_handles,
        })
    }

    /// 会话任务是否已收尾（传输层已死）。
    ///
    /// 桥接 pump 在 shell 通道读到 EOF 时凭此区分两种形态完全一致的死法：远端
    /// shell 正常退出会 drop 通道发送端，传输层死亡同样会，单看 EOF 无法分辨
    /// （见 russh `ChannelRx::poll_read`）。本方法转发 russh `Handle::is_closed()`
    /// ——即会话任务（负责收发整条连接）是否已退出，退出即说明传输层已死而非
    /// 只是某个通道关闭。
    pub async fn is_closed(&self) -> bool {
        let handle = self.handle.lock().await;
        handle.is_closed()
    }

    /// 主动断开这条连接（向服务端发送断开原因后关闭传输）。
    ///
    /// 用于「确定不再复用」的收尾：与单纯丢弃本对象不同（那只会离开本地会话任务，
    /// 不发任何报文），本方法会走完 russh 的断开流程，使会话任务收尾、
    /// [`Self::is_closed`] 转真，服务端也能立刻看到链路结束。
    ///
    /// 有跳板链时先断目标、再**逆序**断各跳（最内层先断，逐层向外），避免先断外层时
    /// 内层连接还在向其发送通道报文。
    pub async fn disconnect(&self, reason: &str) {
        let handle = self.handle.lock().await;
        if let Err(e) = handle
            .disconnect(russh::Disconnect::ByApplication, reason, "en")
            .await
        {
            debug!("断开连接时出错（链路可能已断）: {e}");
        }
        drop(handle);
        for jump in self.jumps.iter().rev() {
            if let Err(e) = jump
                .disconnect(russh::Disconnect::ByApplication, reason, "en")
                .await
            {
                debug!("断开跳板机连接时出错（链路可能已断）: {e}");
            }
        }
    }

    /// 打开一个带 PTY 的交互式 shell 通道（供终端标签页桥接）。
    ///
    /// 返回的通道由核心层桥接到进程内管道，再交给 GUI 的终端渲染层呈现。
    pub async fn open_shell_channel(
        &self,
        cols: u32,
        rows: u32,
        cwd_bootstrap: bool,
        suppress_bootstrap_echo: bool,
    ) -> Result<russh::Channel<client::Msg>, CoreError> {
        debug!("打开 shell 通道 ({}x{})", cols, rows);
        let handle = self.handle.lock().await;
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| CoreError::ssh(CoreErrorKind::ChannelOpen, e))?;
        let terminal_modes: &[(Pty, u32)] = if cwd_bootstrap && suppress_bootstrap_echo {
            &[(Pty::ECHO, 0)]
        } else {
            &[]
        };
        channel
            .request_pty(true, "xterm-256color", cols, rows, 0, 0, terminal_modes)
            .await
            .map_err(|e| CoreError::ssh(CoreErrorKind::RequestPty, e))?;
        channel
            .request_shell(true)
            .await
            .map_err(|e| CoreError::ssh(CoreErrorKind::StartShell, e))?;
        Ok(channel)
    }

    /// 打开 sftp 子系统通道。
    pub async fn open_sftp(&self) -> Result<russh_sftp::client::SftpSession, CoreError> {
        debug!("打开 sftp 子系统通道");
        let handle = self.handle.lock().await;
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| CoreError::ssh(CoreErrorKind::SftpChannelOpen, e))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| CoreError::ssh(CoreErrorKind::SftpSubsystem, e))?;
        let stream = channel.into_stream();
        let sftp = russh_sftp::client::SftpSession::new(stream)
            .await
            .map_err(|e| CoreError::sftp(CoreErrorKind::SftpInit, e))?;
        Ok(sftp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 宿主传入的保活选择必须原样进入 russh 客户端配置（防止退回硬编码）。
    ///
    /// `keepalive_max = 0` 尤其要钉住：russh 把 0 解释为「照常发保活、但永不因无应答判死」，
    /// 不能当作「未设置」而被隐式替换成某个正数。
    #[test]
    fn client_config_carries_the_host_keepalive_choice() {
        let cfg = client_config(Some(Duration::from_secs(45)), 2);
        assert_eq!(cfg.keepalive_interval, Some(Duration::from_secs(45)));
        assert_eq!(cfg.keepalive_max, 2);

        let off = client_config(None, 0);
        assert_eq!(off.keepalive_interval, None);
        assert_eq!(off.keepalive_max, 0, "0 应原样透传");
    }
}
