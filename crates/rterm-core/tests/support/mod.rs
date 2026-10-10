//! 在进程内起一个真实的 SSH + SFTP 服务端，供核心层的传输路径做集成测试。
//!
//! 栈是现成的：`russh::server` 负责 SSH 侧（密码认证 + session 通道 + `sftp` 子系统），
//! `russh_sftp::server::Handler` 负责 SFTP 侧，后端直接映射到临时目录。客户端走完整的
//! 握手与认证，因此被测的 [`SftpClient`] 与生产路径是同一份代码、同一条 TCP 回路。
//!
//! 故障注入发生在 **SFTP 请求层**（拒绝 open / write / rename，或在写入第 n 字节处撕断），
//! 不是 TCP 层：足以复现「断点保住了」「改名失败」这些路径，但真机上的 socket 半开、重连
//! 时序等问题仍需手工验收。

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Error as AnyhowError;
use rterm_core::SftpClient;
use russh::Pty;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use russh_sftp::server::{Handler as SftpHandler, StatusReply};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// 测试接受的凭据（服务端与客户端共用）。
const USER: &str = "tester";
const PASSWORD: &str = "secret";

/// 服务端与测试共享的故障开关与计数器。
struct Faults {
    /// 还需拒绝的 open / rename 次数。
    reject_open: AtomicU32,
    reject_rename: AtomicU32,

    /// 已接受的写入字节总数。
    written: AtomicU64,
    /// 累计写入达到该值时撕断这次上传（`u64::MAX` = 未武装）。
    break_at: AtomicU64,
    /// 武装后一旦触发就持续拒绝写，直到 [`Faults::reset`]。
    broken: AtomicU32,
    /// 收到过的最小 `SSH_FXP_READ` 偏移，用于证明下载确实跳过了前段。
    first_read: AtomicU64,
    /// 偏移达到该值的**第一个**写被拒一次（其后照常接受）：模拟服务器瞬时抽风，
    /// 而客户端早已把后续写发了出去——续传空洞就是这么来的。`u64::MAX` = 未武装。
    reject_at: AtomicU64,
    /// 最近一次 open 之后第一个被接受的写偏移，以及那个句柄的名字。
    first_write: AtomicU64,
    last_open: std::sync::Mutex<String>,
}

impl Default for Faults {
    /// `break_at` 的「未武装」值是 `u64::MAX`，不是默认的 0——后者会让每次写入都被撕断。
    fn default() -> Self {
        Self {
            reject_open: AtomicU32::new(0),
            reject_rename: AtomicU32::new(0),
            written: AtomicU64::new(0),
            break_at: AtomicU64::new(u64::MAX),
            broken: AtomicU32::new(0),
            first_read: AtomicU64::new(u64::MAX),
            first_write: AtomicU64::new(u64::MAX),
            reject_at: AtomicU64::new(u64::MAX),
            last_open: std::sync::Mutex::new(String::new()),
        }
    }
}

impl Faults {
    /// 消费一次拒绝额度：计数为正则减一并返回 `true`。
    fn take(counter: &AtomicU32) -> bool {
        if counter.load(Ordering::Relaxed) == 0 {
            return false;
        }
        counter.fetch_sub(1, Ordering::Relaxed);
        true
    }

    fn reset(&self) {
        for counter in [&self.reject_open, &self.reject_rename, &self.broken] {
            counter.store(0, Ordering::Relaxed);
        }
        self.reject_at.store(u64::MAX, Ordering::Relaxed);
        self.break_at.store(u64::MAX, Ordering::Relaxed);
        self.first_read.store(u64::MAX, Ordering::Relaxed);
    }
}

/// 一个跑着的测试服务端：地址、jail 根目录、故障开关。
pub(crate) struct TestSftp {
    addr: SocketAddr,
    root: PathBuf,
    faults: Arc<Faults>,
    accept: Option<tokio::task::JoinHandle<()>>,
}

impl TestSftp {
    /// 在 `root` 上启动服务端，监听 127.0.0.1 的随机端口。
    pub(crate) async fn start(root: PathBuf) -> Self {
        let faults = Arc::new(Faults::default());
        let config = Arc::new(russh::server::Config {
            keys: vec![
                PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
                    .expect("generate a throwaway host key"),
            ],
            auth_rejection_time: Duration::from_secs(0),
            ..Default::default()
        });
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind an ephemeral port");
        let addr = listener.local_addr().expect("listener address");
        let accept = {
            let faults = Arc::clone(&faults);
            let root = root.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let config = Arc::clone(&config);
                    let session = SshSession::new(Arc::clone(&faults), root.clone());
                    tokio::spawn(async move {
                        if let Err(e) = russh::server::run_stream(config, stream, session).await {
                            log::debug!("test ssh session ended: {e}");
                        }
                    });
                }
            })
        };
        Self {
            addr,
            root,
            faults,
            accept: Some(accept),
        }
    }

    /// 建一条已认证的连接，返回可直接喂给核心层的客户端。
    pub(crate) async fn client(&self) -> TestConnection {
        let config = Arc::new(russh::client::Config::default());
        let mut handle = russh::client::connect(config, self.addr, TestClient)
            .await
            .expect("connect to the test server");
        let auth = handle
            .authenticate_password(USER, PASSWORD)
            .await
            .expect("password auth round trip");
        assert!(auth.success(), "the test server must accept the password");

        let channel = handle.channel_open_session().await.expect("open a session");
        channel
            .request_subsystem(true, "sftp")
            .await
            .expect("request the sftp subsystem");
        let session = SftpSession::new(channel.into_stream())
            .await
            .expect("sftp handshake");
        TestConnection {
            client: SftpClient::new(session),
            handle: Some(handle),
        }
    }

    /// jail 内某个名字对应的真实路径（测试据此直接读写「远端」文件）。
    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.root.join(name.trim_start_matches('/'))
    }

    /// jail 内该名字当前的字节数，不存在时为 0。
    pub(crate) fn len(&self, name: &str) -> usize {
        std::fs::metadata(self.path(name))
            .map(|m| m.len() as usize)
            .unwrap_or(0)
    }

    /// 让接下来 `n` 次 open / write / rename 失败。
    pub(crate) fn fail_opens(&self, n: u32) {
        self.faults.reject_open.store(n, Ordering::Relaxed);
    }

    pub(crate) fn fail_renames(&self, n: u32) {
        self.faults.reject_rename.store(n, Ordering::Relaxed);
    }

    /// 偏移首次达到 `bytes` 的那个写请求被拒**一次**，其后的写照常接受（服务器瞬时抽风）。
    pub(crate) fn reject_write_at(&self, bytes: u64) {
        self.faults.reject_at.store(bytes, Ordering::Relaxed);
    }

    /// 累计写入即将越过 `bytes` 时撕断这次上传：那一块起，每个 write 都被拒。
    pub(crate) fn break_upload_at(&self, bytes: u64) {
        self.faults.broken.store(0, Ordering::Relaxed);
        self.faults.break_at.store(bytes, Ordering::Relaxed);
    }

    /// 服务端实际接受的写入字节数。
    pub(crate) fn written(&self) -> u64 {
        self.faults.written.load(Ordering::Relaxed)
    }

    /// 最近一次 open 之后**第一个被接受**的写偏移（`u64::MAX` = 还没写过）。
    /// 断点续传真正要验的就是这一个数：本轮第一次写必须落在记下的断点上。
    pub(crate) fn first_write_offset(&self) -> u64 {
        self.faults.first_write.load(Ordering::Relaxed)
    }

    /// 收到过的最小读偏移（`u64::MAX` 表示一次都没读过）。
    pub(crate) fn first_read_offset(&self) -> u64 {
        self.faults.first_read.load(Ordering::Relaxed)
    }

    /// 清掉所有故障与计数（`written` 保留，测试据此算断点）。
    pub(crate) fn reset_faults(&self) {
        self.faults.reset();
    }
}

impl Drop for TestSftp {
    fn drop(&mut self) {
        if let Some(accept) = self.accept.take() {
            accept.abort();
        }
    }
}

/// 一条已建立的测试连接：客户端 + 可用于制造「会话终结」的 SSH 句柄。
pub(crate) struct TestConnection {
    pub(crate) client: SftpClient,
    handle: Option<russh::client::Handle<TestClient>>,
}

impl TestConnection {
    /// 撕掉整条 SSH 连接，之后的请求应当被判为会话终结。
    pub(crate) async fn disconnect(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "test", "en")
                .await;
        }
    }
}

/// 面向 [`rterm_core::SshConnection`]（生产建连路径）的测试服务端。
///
/// SFTP 侧用 [`TestSftp`]，终端桥接侧用本结构：两者共用同一份 [`SshSession`] handler
/// 与一次性主机密钥。建连时自动答复主机密钥确认（测试不关心 TOFU 流程），
/// 返回的 [`TestSshConnection`] 可直接喂给 `spawn_terminal_bridge`。
///
/// `allow(dead_code)`：本模块被多个集成测试共享，只有终端桥接测试用到这两个结构。
#[allow(dead_code)]
pub(crate) struct TestSsh {
    addr: SocketAddr,
    accept: Option<tokio::task::JoinHandle<()>>,
    /// 最近一条连接的 shell 通道（服务端侧），供用例模拟「远端 shell 退出」。
    shell_channel: Arc<tokio::sync::Mutex<Option<Channel<Msg>>>>,
    /// 经本服务端转发的 direct-tcpip 次数，供用例证明流量确实过境。
    forwards: Arc<AtomicU32>,
    /// 各条 TCP 桥泵任务的中止柄，供用例掐断隧道。
    bridges: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
}

#[allow(dead_code)]
impl TestSsh {
    /// 在 127.0.0.1 的随机端口上启动一个接受密码认证的 SSH 服务端。
    pub(crate) async fn start() -> Self {
        let config = Arc::new(russh::server::Config {
            keys: vec![
                PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
                    .expect("generate a throwaway host key"),
            ],
            auth_rejection_time: Duration::from_secs(0),
            ..Default::default()
        });
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind an ephemeral port");
        let addr = listener.local_addr().expect("listener address");
        let shell_channel = Arc::new(tokio::sync::Mutex::new(None));
        let forwards = Arc::new(AtomicU32::new(0));
        let bridges: Arc<Mutex<Vec<tokio::task::AbortHandle>>> = Arc::new(Mutex::new(Vec::new()));
        let accept = {
            let config = Arc::clone(&config);
            let root = std::env::temp_dir().join(format!("rterm-ssh-{}", std::process::id()));
            std::fs::create_dir_all(&root).expect("create the ssh jail");
            let shell_channel = Arc::clone(&shell_channel);
            let forwards = Arc::clone(&forwards);
            let bridges = Arc::clone(&bridges);
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let config = Arc::clone(&config);
                    let session = SshSession::with_shell_slot(
                        Arc::new(Faults::default()),
                        root.clone(),
                        Arc::clone(&shell_channel),
                        Arc::clone(&forwards),
                        Arc::clone(&bridges),
                    );
                    tokio::spawn(async move {
                        if let Err(e) = russh::server::run_stream(config, stream, session).await {
                            log::debug!("test ssh session ended: {e}");
                        }
                    });
                }
            })
        };
        Self {
            addr,
            accept: Some(accept),
            shell_channel,
            forwards,
            bridges,
        }
    }

    /// 本服务端监听端口（跳板机测试据此把它当作「目标主机」来连）。
    pub(crate) fn port(&self) -> u16 {
        self.addr.port()
    }

    /// 经本服务端转发的 direct-tcpip 次数。
    pub(crate) fn forwards(&self) -> u32 {
        self.forwards.load(Ordering::Relaxed)
    }

    /// 掐断所有经本机转发的 TCP 桥（模拟跳板机侧隧道断裂）。
    pub(crate) async fn kill_bridges(&self) {
        let mut bridges = self.bridges.lock().await;
        for handle in bridges.drain(..) {
            handle.abort();
        }
    }

    /// 模拟远端 shell 退出：从**服务端侧**关闭 shell 通道（等价于远端进程结束、
    /// 服务端发 CHANNEL_CLOSE），但保留整条 SSH 连接。
    ///
    /// 这是「连接活着而通道 EOF」的唯一正确造法：客户端自己 `Channel::close()`
    /// 不会让客户端自己的读端看到 Close（russh 只在**收到** CHANNEL_CLOSE 时把
    /// Close 转发给通道），故那条路径观察不到任何变化。
    ///
    /// 等待服务端收到 shell 请求（通道入槽）后再关闭：客户端 `request_shell` 的
    /// 应答与服务端 handler 落槽之间没有先后保证，直接关会扑空。
    pub(crate) async fn close_shell(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let taken = self.shell_channel.lock().await.take();
            if let Some(channel) = taken {
                channel.close().await.expect("close the shell channel");
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the test server never saw a shell request"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// 本服务端对应的会话配置（用户名 / 密码与本服务端一致）。
    fn session_config(&self) -> rterm_config::SessionConfig {
        rterm_config::SessionConfig {
            id: "test".to_string(),
            name: "test".to_string(),
            host: Ipv4Addr::LOCALHOST.to_string(),
            port: self.addr.port(),
            username: USER.to_string(),
            auth: rterm_config::AuthMethod::Password { password: None },
            group: None,
            jumps: Vec::new(),
        }
    }

    /// 本服务端接受的明文凭据。
    fn secrets() -> rterm_core::SessionSecrets {
        rterm_core::SessionSecrets {
            password: Some(zeroize::Zeroizing::new(PASSWORD.to_string())),
            key_passphrase: None,
        }
    }

    /// 一条以本服务端为跳板机的 [`rterm_core::HopSpec`]。
    pub(crate) fn hop(&self) -> rterm_core::HopSpec {
        rterm_core::HopSpec {
            config: rterm_config::JumpHost {
                host: Ipv4Addr::LOCALHOST.to_string(),
                port: self.addr.port(),
                username: USER.to_string(),
                auth: rterm_config::AuthMethod::Password { password: None },
            },
            secrets: Self::secrets(),
        }
    }

    /// 经生产入口 [`rterm_core::SshConnection::connect`] 建一条已认证的连接。
    ///
    /// 主机密钥确认由后台任务自动答复「信任」：测试不覆盖 TOFU 弹窗流程，
    /// 只想拿到一具可桥接的活连接。
    pub(crate) async fn connect(&self) -> TestSshConnection {
        self.connect_via(&[]).await
    }

    /// 同 [`TestSsh::connect`]，但先经给定跳板链（由外到内）建立隧道再握手。
    pub(crate) async fn connect_via(&self, jumps: &[rterm_core::HopSpec]) -> TestSshConnection {
        let config = self.session_config();
        let (prompt_tx, mut prompt_rx) =
            tokio::sync::mpsc::channel::<(rterm_core::HostKeyPrompt, rterm_core::HostKeyReply)>(1);
        tokio::spawn(async move {
            while let Some((_prompt, reply)) = prompt_rx.recv().await {
                reply.reply(true);
            }
        });
        let conn = rterm_core::SshConnection::connect(
            &config,
            &Self::secrets(),
            jumps,
            None,
            0,
            prompt_tx,
        )
        .await
        .expect("connect to the test ssh server");
        assert!(!conn.is_closed().await, "新建连接必然是活的");
        TestSshConnection {
            conn: Some(Arc::new(conn)),
        }
    }
}

impl Drop for TestSsh {
    fn drop(&mut self) {
        if let Some(accept) = self.accept.take() {
            accept.abort();
        }
    }
}

/// 一条经生产路径建立的测试连接：可直接喂给终端桥接。
#[allow(dead_code)] // 同 `TestSsh`：仅终端桥接测试使用
pub(crate) struct TestSshConnection {
    /// 待桥接的 SSH 连接；[`TestSshConnection::release`] 取走后为 `None`。
    pub(crate) conn: Option<Arc<rterm_core::SshConnection>>,
}

#[allow(dead_code)]
impl TestSshConnection {
    /// 此刻持有的连接（取引用，不改变持有状态）。
    pub(crate) fn conn(&self) -> &Arc<rterm_core::SshConnection> {
        self.conn.as_ref().expect("connection already released")
    }

    /// 撕掉整条传输（模拟拔网线 / sshd 重启）：向服务端发送断开报文，
    /// russh 会话任务随之收尾、`is_closed()` 转真。
    pub(crate) async fn kill_transport(&self) {
        self.conn().disconnect("test").await;
    }
}

/// SSH 服务端 handler：只认密码认证，把 `sftp` 子系统接到 [`SftpBackend`]，
/// 并为 direct-tcpip 请求架一条到目标主机的 TCP 桥（跳板机语义）。
struct SshSession {
    faults: Arc<Faults>,
    root: PathBuf,
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    /// 服务端侧最近一条 shell 通道的存放位（仅供 [`TestSsh::close_shell`] 使用）。
    shell_slot: Option<Arc<tokio::sync::Mutex<Option<Channel<Msg>>>>>,
    /// 已接受的 direct-tcpip 次数（供用例证明流量确实经过了本机）。
    forwards: Arc<AtomicU32>,
    /// 各条 TCP 桥泵任务的中止柄（仅供 [`TestSsh::kill_bridges`] 掐断隧道）。
    bridges: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
}

impl SshSession {
    fn new(faults: Arc<Faults>, root: PathBuf) -> Self {
        Self::build(
            faults,
            root,
            None,
            Arc::new(AtomicU32::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        )
    }

    /// 同 [`SshSession::new`]，但把 shell 通道与转发计数 / 桥柄寄存到宿主的共享槽位。
    #[allow(dead_code)] // 仅终端桥接与跳板机测试走这条构造
    fn with_shell_slot(
        faults: Arc<Faults>,
        root: PathBuf,
        shell_slot: Arc<tokio::sync::Mutex<Option<Channel<Msg>>>>,
        forwards: Arc<AtomicU32>,
        bridges: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
    ) -> Self {
        Self::build(faults, root, Some(shell_slot), forwards, bridges)
    }

    fn build(
        faults: Arc<Faults>,
        root: PathBuf,
        shell_slot: Option<Arc<tokio::sync::Mutex<Option<Channel<Msg>>>>>,
        forwards: Arc<AtomicU32>,
        bridges: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
    ) -> Self {
        Self {
            faults,
            root,
            channels: Arc::default(),
            shell_slot,
            forwards,
            bridges,
        }
    }
}

impl russh::server::Handler for SshSession {
    type Error = AnyhowError;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == USER && password == PASSWORD {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            })
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    /// 接受任何 PTY 请求：测试不关心终端参数，只要客户端侧 `request_pty` 成功。
    async fn pty_request(
        &mut self,
        channel_id: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel_id)?;
        Ok(())
    }

    /// 接受 shell 请求但**不启动任何进程**：通道收到客户端数据后原样丢弃，
    /// 远端不产生输出——终端桥接测试只需一条「打开即静默」的 shell 通道。
    /// 若配置了共享槽位，通道本体移交给 [`TestSsh::close_shell`] 备用。
    async fn shell_request(
        &mut self,
        channel_id: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel_id)?;
        if let Some(slot) = &self.shell_slot {
            let channel = self
                .channels
                .lock()
                .await
                .remove(&channel_id)
                .expect("a shell request must follow its session channel");
            *slot.lock().await = Some(channel);
        }
        Ok(())
    }

    /// 接受窗口尺寸变更（桥接 pump 会在 I/O 间隙下发）。
    async fn window_change_request(
        &mut self,
        channel_id: ChannelId,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel_id)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(channel_id)?;
            return Ok(());
        }
        let channel = self
            .channels
            .lock()
            .await
            .remove(&channel_id)
            .expect("a subsystem request must follow its session channel");
        session.channel_success(channel_id)?;
        russh_sftp::server::run(
            channel.into_stream(),
            SftpBackend::new(Arc::clone(&self.faults), self.root.clone()),
        )
        .await;
        Ok(())
    }

    /// 跳板机语义：把请求的 direct-tcpip 通道桥到真实的 `host_to_connect:port_to_connect`。
    ///
    /// 这正是 sshd 的 `direct-tcpip` 转发。目标不可达时按协议回 `ConnectFailed`，
    /// 让客户端在建连时就拿到明确失败，而非挂在半开的通道上。
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Ok(stream) =
            tokio::net::TcpStream::connect((host_to_connect, port_to_connect as u16)).await
        else {
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        self.forwards.fetch_add(1, Ordering::Relaxed);
        reply.accept().await;
        let mut chan = channel.into_stream();
        let mut stream = stream;
        let bridge = tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut chan, &mut stream).await;
        });
        self.bridges.lock().await.push(bridge.abort_handle());
        Ok(())
    }
}

/// 测试客户端：主机密钥一律放行（服务端用的是一次性随机密钥）。
struct TestClient;

impl russh::client::Handler for TestClient {
    type Error = AnyhowError;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// 映射到真实目录的 SFTP 后端。句柄表按连接独占，故状态直接放在 `&mut self` 里。
struct SftpBackend {
    faults: Arc<Faults>,
    root: PathBuf,
    files: HashMap<String, std::fs::File>,
    dirs: HashMap<String, (Vec<PathBuf>, usize)>,
    next_handle: u64,
}

impl SftpBackend {
    fn new(faults: Arc<Faults>, root: PathBuf) -> Self {
        Self {
            faults,
            root,
            files: HashMap::new(),
            dirs: HashMap::new(),
            next_handle: 0,
        }
    }

    /// 把远端路径解析到 jail 内，拒绝 `..` 逃逸。
    fn resolve(&self, path: &str) -> Result<PathBuf, StatusReply> {
        let relative = path.trim_start_matches('/');
        if relative.split('/').any(|part| part == "..") {
            return Err(StatusCode::PermissionDenied.with_message("outside of the jail"));
        }
        Ok(self.root.join(relative))
    }

    fn claim_handle(&mut self) -> String {
        self.next_handle += 1;
        format!("h{}", self.next_handle)
    }

    fn file_mut(&mut self, handle: &str) -> Result<&mut std::fs::File, StatusReply> {
        self.files
            .get_mut(handle)
            .ok_or_else(|| StatusCode::BadMessage.with_message("unknown handle"))
    }
}

/// 本地 IO 错误 → SFTP 状态码。`NotFound` 必须是 `NoSuchFile`：调用方靠它区分
/// 「文件不存在」与其它失败，而上传首轮本就没有 `.part`。
fn io_status(kind: ErrorKind, detail: impl std::fmt::Display) -> StatusReply {
    let code = match kind {
        ErrorKind::NotFound => StatusCode::NoSuchFile,
        ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    };
    code.with_message(detail.to_string())
}

fn ok_status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_string(),
        language_tag: "en-US".to_string(),
    }
}

impl SftpHandler for SftpBackend {
    type Error = StatusReply;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported.with_message("not implemented by the test server")
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        // 不宣告任何扩展：`limits@openssh.com` 缺席时客户端没有写流水线可依赖，
        // 这是最保守的情形。
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        if Faults::take(&self.faults.reject_open) {
            return Err(StatusCode::PermissionDenied.with_message("injected open failure"));
        }
        let path = self.resolve(&filename)?;
        // 直接采用 russh-sftp 的 OpenFlags → OpenOptions 映射，故「WRITE 不带 CREATE」
        // 在这里与真服务器一样是「文件不存在就报错」。
        let file = std::fs::OpenOptions::from(pflags)
            .open(&path)
            .map_err(|e| io_status(e.kind(), e))?;
        self.faults.first_write.store(u64::MAX, Ordering::Relaxed);
        let handle = self.claim_handle();
        *self.faults.last_open.lock().expect("last_open lock") = handle.clone();
        self.files.insert(handle.clone(), file);
        Ok(Handle { id, handle })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        if self.faults.broken.load(Ordering::Relaxed) != 0 {
            return Err(StatusCode::Failure.with_message("injected link drop"));
        }
        // 一次性瞬时拒绝：只拒这一个请求，其后的写在途照收——正是「失败点之后还有数据落盘」
        // 那个真实场景，也是暂存空洞的来源。
        let reject_at = self.faults.reject_at.load(Ordering::Relaxed);
        if offset >= reject_at
            && self
                .faults
                .reject_at
                .compare_exchange(reject_at, u64::MAX, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            return Err(StatusCode::Failure.with_message("injected transient write failure"));
        }
        let break_at = self.faults.break_at.load(Ordering::Relaxed);
        let before = self.faults.written.load(Ordering::Relaxed);
        // 真服务器不会「写一半」：一个 SSH_FXP_WRITE 要么整体成功要么整体失败，故越界时整块拒绝。
        let allowed = if before + data.len() as u64 > break_at {
            0
        } else {
            data.len()
        };

        let file = self.file_mut(&handle)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| io_status(e.kind(), e))?;
        file.write_all(&data[..allowed])
            .map_err(|e| io_status(e.kind(), e))?;
        file.flush().map_err(|e| io_status(e.kind(), e))?;
        self.faults
            .written
            .store(before + allowed as u64, Ordering::Relaxed);
        // 只记「本轮这个句柄」的第一次写：上一轮在途的迟到写可能落在别处，不得混进来。
        if *self.faults.last_open.lock().expect("last_open lock") == handle {
            let _ = self.faults.first_write.compare_exchange(
                u64::MAX,
                offset,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }

        if allowed < data.len() {
            self.faults.broken.store(1, Ordering::Relaxed);
            return Err(StatusCode::Failure.with_message("injected link drop"));
        }
        Ok(ok_status(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        self.faults.first_read.fetch_min(offset, Ordering::Relaxed);
        let file = self.file_mut(&handle)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| io_status(e.kind(), e))?;
        let mut buf = vec![0u8; len as usize];
        let read = file.read(&mut buf).map_err(|e| io_status(e.kind(), e))?;
        if read == 0 {
            return Err(StatusCode::Eof.with_message("end of file"));
        }
        buf.truncate(read);
        Ok(Data { id, data: buf })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.files.remove(&handle);
        self.dirs.remove(&handle);
        Ok(ok_status(id))
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let resolved = self.resolve(&path)?;
        let meta = std::fs::metadata(&resolved).map_err(|e| io_status(e.kind(), e))?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let resolved = self.resolve(&path)?;
        let meta = std::fs::symlink_metadata(&resolved).map_err(|e| io_status(e.kind(), e))?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let meta = self
            .file_mut(&handle)?
            .metadata()
            .map_err(|e| io_status(e.kind(), e))?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        // 只认 size：核心层失败时用它把暂存截回已确证前缀，与真服务器的 ftruncate 同义。
        if let Some(len) = attrs.size {
            self.file_mut(&handle)?
                .set_len(len)
                .map_err(|e| io_status(e.kind(), e))?;
        }
        Ok(ok_status(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        if Faults::take(&self.faults.reject_rename) {
            return Err(StatusCode::Failure.with_message("injected rename failure"));
        }
        let from = self.resolve(&oldpath)?;
        let to = self.resolve(&newpath)?;
        std::fs::rename(&from, &to).map_err(|e| io_status(e.kind(), e))?;
        Ok(ok_status(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let resolved = self.resolve(&filename)?;
        std::fs::remove_file(&resolved).map_err(|e| io_status(e.kind(), e))?;
        Ok(ok_status(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let resolved = self.resolve(&path)?;
        std::fs::create_dir(&resolved).map_err(|e| io_status(e.kind(), e))?;
        Ok(ok_status(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        let resolved = self.resolve(&path)?;
        std::fs::remove_dir(&resolved).map_err(|e| io_status(e.kind(), e))?;
        Ok(ok_status(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let resolved = self.resolve(&path)?;
        let entries: Vec<PathBuf> = std::fs::read_dir(&resolved)
            .map_err(|e| io_status(e.kind(), e))?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect();
        let handle = self.claim_handle();
        self.dirs.insert(handle.clone(), (entries, 0));
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let (entries, cursor) = self
            .dirs
            .get_mut(&handle)
            .ok_or_else(|| StatusCode::BadMessage.with_message("unknown handle"))?;
        if *cursor >= entries.len() {
            return Err(StatusCode::Eof.with_message("end of directory"));
        }
        let path = entries[*cursor].clone();
        *cursor += 1;
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let attrs = path
            .metadata()
            .map(|m| FileAttributes::from(&m))
            .unwrap_or_default();
        Ok(Name {
            id,
            files: vec![File::new(name, attrs)],
        })
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let virtual_path = if path.is_empty() || path == "." {
            "/".to_string()
        } else {
            path
        };
        Ok(Name {
            id,
            files: vec![File::new(virtual_path, FileAttributes::dummy())],
        })
    }
}

/// 测试沙箱：一个远端 jail + 一个本地工作目录，`Drop` 时一起删掉。
pub(crate) struct Sandbox {
    base: PathBuf,
    pub(crate) remote: PathBuf,
    pub(crate) local: PathBuf,
}

static SANDBOX_SEQ: AtomicU64 = AtomicU64::new(0);

impl Sandbox {
    pub(crate) fn new(tag: &str) -> Self {
        let id = SANDBOX_SEQ.fetch_add(1, Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("rterm-core-{tag}-{}-{id}", std::process::id()));
        let sandbox = Self {
            remote: base.join("remote"),
            local: base.join("local"),
            base,
        };
        std::fs::create_dir_all(&sandbox.remote).expect("create the remote jail");
        std::fs::create_dir_all(&sandbox.local).expect("create the local workdir");
        sandbox
    }

    /// 在本地工作目录写一个 `bytes` 长的文件，返回其路径。
    pub(crate) fn local_file(&self, name: &str, bytes: usize) -> PathBuf {
        let path = self.local.join(name);
        std::fs::write(&path, payload(bytes)).expect("write the local file");
        path
    }

    /// 在远端 jail 里放一个文件（用来预置半成品的 `.part`）。
    pub(crate) fn remote_file(&self, name: &str, bytes: usize) -> PathBuf {
        let path = self.remote.join(name);
        std::fs::write(&path, payload(bytes)).expect("write the remote file");
        path
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// 可预测的字节流：每 4 KiB 一段各不相同，断点错位时内容必然对不上。
pub(crate) fn payload(bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes);
    let mut block = 0u32;
    while out.len() < bytes {
        let mut chunk = Vec::with_capacity(4096);
        for i in 0..1024u32 {
            let seed = block
                .wrapping_mul(2_654_435_761)
                .wrapping_add(i.wrapping_mul(40_503));
            chunk.extend_from_slice(&seed.to_le_bytes());
        }
        block += 1;
        let take = chunk.len().min(bytes - out.len());
        out.extend_from_slice(&chunk[..take]);
    }
    out
}
