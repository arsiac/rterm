//! 基于 russh 的 SSH 连接管理。
//!
//! 每个 [`SshConnection`] 封装一条到远程主机的 russh 连接，可复用同一条连接
//! 打开多个通道：交互式 shell 通道（供终端标签页桥接）与 sftp 子系统通道
//! （供文件管理面板使用）。

use crate::{CoreError, host_key};
use log::debug;
use rterm_config::{AuthMethod, SessionConfig};
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
                    "读取 known_hosts 失败，拒绝 {host}:{port}: {e}",
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
                "主机密钥确认通道已关闭，视为拒绝 {host}:{port}",
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
                    "记录已知主机失败，拒绝 {host}:{port}: {e}",
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
}

impl SshConnection {
    /// 根据会话配置建立并认证一条 SSH 连接。
    ///
    /// 认证方式依据 [`AuthMethod`]：密码 / 公钥文件 / SSH agent。
    /// 主机密钥未经 known_hosts 确认时经 `prompt_tx` 请求 GUI 弹窗，等待用户决定。
    pub async fn connect(
        config: &SessionConfig,
        secrets: &SessionSecrets,
        prompt_tx: mpsc::Sender<(HostKeyPrompt, HostKeyReply)>,
    ) -> Result<Self, CoreError> {
        debug!("正在连接 {}:{}", config.host, config.port);
        let ssh_config = Config {
            keepalive_interval: Some(Duration::from_secs(20)),
            ..Default::default()
        };

        let handler = ClientHandler {
            host: config.host.clone(),
            port: config.port,
            prompt_tx,
        };

        // 建立 TCP 连接并完成密钥交换（含主机密钥校验，可能在弹窗处暂停）。
        let mut handle = client::connect(
            Arc::new(ssh_config),
            (config.host.as_str(), config.port),
            handler,
        )
        .await
        .map_err(|e| CoreError::ssh("连接失败", e))?;

        // 认证需要可变句柄。凭据已由调用方用保险库解密为明文（见 `SessionSecrets`）。
        match &config.auth {
            AuthMethod::Password { .. } => {
                debug!("使用密码认证: {}", config.username);
                let password = secrets
                    .password
                    .as_ref()
                    .ok_or_else(|| CoreError::ssh_msg("缺少解密后的密码"))?;
                let result = handle
                    .authenticate_password(&config.username, password.as_str())
                    .await
                    .map_err(|e| CoreError::ssh("密码认证请求失败", e))?;
                if !result.success() {
                    return Err(CoreError::ssh_msg("密码认证被服务器拒绝"));
                }
            }
            AuthMethod::PublicKey {
                key_path,
                passphrase: _,
            } => {
                debug!("使用公钥认证: {}", key_path.display());
                let pass = secrets.key_passphrase.as_ref().map(|p| p.as_str());
                let key = russh::keys::PrivateKey::read_openssh_file(key_path)
                    .map_err(|e| CoreError::ssh("读取私钥失败", e))?;
                let key = match pass {
                    Some(pass) => key
                        .decrypt(pass)
                        .map_err(|e| CoreError::ssh("解密私钥失败", e))?,
                    None => key,
                };
                let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
                let result = handle
                    .authenticate_publickey(&config.username, key)
                    .await
                    .map_err(|e| CoreError::ssh("公钥认证请求失败", e))?;
                if !result.success() {
                    return Err(CoreError::ssh_msg("公钥认证被服务器拒绝"));
                }
            }
            AuthMethod::Agent => {
                debug!("使用 SSH agent 认证");
                let mut agent = russh::keys::agent::client::AgentClient::connect_env()
                    .await
                    .map_err(|e| CoreError::ssh("连接 SSH agent 失败", e))?;
                let identities = agent
                    .request_identities()
                    .await
                    .map_err(|e| CoreError::ssh("获取 agent 密钥失败", e))?;
                let mut authed = false;
                for id in identities {
                    let pubkey = id.public_key().into_owned();
                    if let Ok(result) = handle
                        .authenticate_publickey_with(&config.username, pubkey, None, &mut agent)
                        .await
                        && result.success()
                    {
                        authed = true;
                        break;
                    }
                }
                if !authed {
                    return Err(CoreError::ssh_msg("SSH agent 认证失败或无可用身份"));
                }
            }
        }

        debug!("SSH 认证成功: {}", config.username);
        Ok(Self {
            handle: Arc::new(AsyncMutex::new(handle)),
        })
    }

    /// 打开一个带 PTY 的交互式 shell 通道（供终端标签页桥接）。
    ///
    /// 返回的通道由核心层桥接到进程内管道，再交给 GUI 的终端渲染层呈现。
    pub async fn open_shell_channel(
        &self,
        cols: u32,
        rows: u32,
    ) -> Result<russh::Channel<client::Msg>, CoreError> {
        debug!("打开 shell 通道 ({}x{})", cols, rows);
        let handle = self.handle.lock().await;
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| CoreError::ssh("打开通道失败", e))?;
        channel
            .request_pty(true, "xterm-256color", cols, rows, 0, 0, &[])
            .await
            .map_err(|e| CoreError::ssh("请求 PTY 失败", e))?;
        channel
            .request_shell(true)
            .await
            .map_err(|e| CoreError::ssh("启动 shell 失败", e))?;
        Ok(channel)
    }

    /// 打开 sftp 子系统通道。
    pub async fn open_sftp(&self) -> Result<russh_sftp::client::SftpSession, CoreError> {
        debug!("打开 sftp 子系统通道");
        let handle = self.handle.lock().await;
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| CoreError::ssh("打开 sftp 通道失败", e))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| CoreError::ssh("请求 sftp 子系统失败", e))?;
        let stream = channel.into_stream();
        let sftp = russh_sftp::client::SftpSession::new(stream)
            .await
            .map_err(|e| CoreError::sftp("初始化 sftp 会话失败", e))?;
        Ok(sftp)
    }
}
