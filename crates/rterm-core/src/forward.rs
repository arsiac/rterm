//! SSH 端口转发引擎：本地（`ssh -L`）、动态 SOCKS5（`ssh -D`）与远程（`ssh -R`）。
//!
//! 引擎不驻留全局：GUI 每建一条 [`SshConnection`](crate::SshConnection) 就可为它 spawn 一个
//! [`ForwardHandle`](ForwardHandle)，规则按索引逐条启停（连接途中动态增减由 `apply_rule` /
//! `remove_rule` 承载）。单条规则建立失败不阻塞其他规则，失败经 [`ForwardEvent`](ForwardEvent)
//! 上报；移除规则时只停止接受新连接，已建立的桥自然排空。
//!
//! 远程转发依赖服务端把回送连接经 `forwarded-tcpip` 通道送回，由 `SshConnection`
//! 目标跳上的客户端回调路由到本模块（[`ForwardRouter`](ForwardRouter)）。

use crate::connection::SshConnection;
use log::debug;
use rterm_config::{ForwardKind, ForwardRule};
use russh::client::Msg;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// 服务端回送的远程转发连接：已确认的 `forwarded-tcpip` 通道 + 连接元数据。
pub(crate) struct IncomingRemote {
    /// 已被确认打开的 SSH 通道（桥接前再转流）。
    pub channel: russh::Channel<Msg>,
}

/// 路由表条目映射：(bind_addr, bind_port) → 规则的回送连接接收端。
type RouterEntries = HashMap<(String, u32), mpsc::Sender<IncomingRemote>>;

/// 远程转发回送连接的路由表：按 (bind_addr, bind_port) 找到对应规则的接收端。
///
/// 随 [`SshConnection`](crate::SshConnection) 创建、挂在目标跳的客户端回调上；每条
/// Remote 规则注册一个发送端、注销时移除，无匹配条目的回送连接一律被拒绝。
#[derive(Clone, Default)]
pub(crate) struct ForwardRouter(Arc<Mutex<RouterEntries>>);

impl ForwardRouter {
    /// 创建空路由表。
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(HashMap::new())))
    }

    /// 为一条远程转发规则注册回送连接接收端（同键重复注册以新替旧）。
    pub(crate) fn register(
        &self,
        bind_addr: String,
        bind_port: u32,
        tx: mpsc::Sender<IncomingRemote>,
    ) {
        self.0
            .lock()
            .expect("router poisoned")
            .insert((bind_addr, bind_port), tx);
    }

    /// 移除一条远程转发规则的注册（返回是否存在）。
    pub(crate) fn unregister(&self, bind_addr: &str, bind_port: u32) -> bool {
        self.0
            .lock()
            .expect("router poisoned")
            .remove(&(bind_addr.to_string(), bind_port))
            .is_some()
    }

    /// 按服务端声明的目的地址 / 端口查找接收端。
    pub(crate) fn lookup(&self, addr: &str, port: u32) -> Option<mpsc::Sender<IncomingRemote>> {
        self.0
            .lock()
            .expect("router poisoned")
            .get(&(addr.to_string(), port))
            .cloned()
    }
}

/// 单条规则的运行事件（GUI 据此渲染转发面板的行状态）。
#[derive(Debug, Clone)]
pub enum RuleEvent {
    /// 规则已就绪：本地 / 动态监听绑定成功，或远程转发已在服务端注册。
    Listening,
    /// 规则建立失败（未在运行；不影响其他规则与连接本身）。
    Failed(ForwardFailure),
    /// 规则已停止（被移除或监听循环意外退出）。
    Closed,
    /// 活跃连接数变化（携带当前值）。
    Connections(usize),
}

/// 规则建立失败的原因（结构化供 GUI 本地化；detail 为面向日志的英文补充）。
#[derive(Debug, Clone)]
pub enum ForwardFailure {
    /// 本地绑定失败（端口被占 / 无权限 / accept 异常）。
    Bind {
        /// 绑定地址。
        addr: String,
        /// 绑定端口。
        port: u16,
        /// 底层错误描述。
        detail: String,
    },
    /// 服务端拒绝远程转发注册（含 GatewayPorts 关闭导致外发地址被拒的场景）。
    RemoteDenied {
        /// 请求注册的绑定地址。
        addr: String,
        /// 请求注册的绑定端口。
        port: u16,
    },
    /// 规则结构不合法（未通过 `ForwardRule::validate`）。
    Invalid {
        /// 校验失败描述。
        detail: String,
    },
}

/// 一条规则的运行事件（含规则索引）。
#[derive(Debug, Clone)]
pub struct ForwardEvent {
    /// 规则在会话 forwards 列表中的下标。
    pub rule_index: usize,
    /// 事件本体。
    pub event: RuleEvent,
}

/// 单条规则的后台任务句柄。
struct RuleTask {
    /// 监听 / 注册循环任务的终止柄（只终止循环；已建立的桥自然排空）。
    abort: tokio::task::AbortHandle,
    /// 规则类型（移除时决定是否向服务端注销远程转发）。
    kind: ForwardKind,
    /// 注册的绑定地址 / 端口（移除远程规则时用于注销与清理路由表）。
    bind: (String, u16),
    /// 代次号：同索引被替换后，迟到的自清消息凭它识别并跳过新规则。
    generation: u64,
}

/// 端口转发引擎句柄：按规则索引管理监听任务，随一条 SSH 连接逐条启停；可克隆。
///
/// 由 [`ForwardEngine::spawn`](ForwardEngine::spawn) 创建，GUI 持有于终端标签状态。
/// `drop` 只终止本地监听任务；主动收尾先调 [`clear`](ForwardHandle::clear)——它还会
/// 向服务端注销远程转发。
#[derive(Clone)]
pub struct ForwardHandle {
    inner: Arc<ForwardHandleInner>,
}

struct ForwardHandleInner {
    conn: Arc<SshConnection>,
    tasks: Arc<tokio::sync::Mutex<HashMap<usize, RuleTask>>>,
    events: mpsc::Sender<ForwardEvent>,
    /// 自清消息发送端（规则任务退出后通知摘除句柄表条目）。
    cleanup_tx: mpsc::Sender<(usize, u64)>,
    /// 规则代次计数：apply_rule 分配，自清完成后核对代次防止误删新规则。
    next_gen: AtomicU64,
}

/// 端口转发引擎工厂。
pub struct ForwardEngine;

impl ForwardEngine {
    /// 为一条连接启动转发引擎，按序应用规则（仅 enabled 的规则会真正启动）。
    ///
    /// 事件（监听就绪 / 失败 / 连接计数）经 events 上报；接收端被丢弃后引擎
    /// 仍继续转发，只是无人再收到状态。
    pub async fn spawn(
        conn: Arc<SshConnection>,
        rules: &[ForwardRule],
        events: mpsc::Sender<ForwardEvent>,
    ) -> ForwardHandle {
        let tasks: Arc<tokio::sync::Mutex<HashMap<usize, RuleTask>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let (cleanup_tx, mut cleanup_rx) = mpsc::channel::<(usize, u64)>(16);
        // 自清泵：规则任务自然结束（失败 / 监听循环退出）后代次核对无误即摘除条目。
        tokio::spawn({
            let tasks = Arc::clone(&tasks);
            async move {
                while let Some((index, generation)) = cleanup_rx.recv().await {
                    let mut tasks = tasks.lock().await;
                    if tasks.get(&index).map(|t| t.generation) == Some(generation) {
                        tasks.remove(&index);
                    }
                }
            }
        });
        let handle = ForwardHandle {
            inner: Arc::new(ForwardHandleInner {
                conn,
                tasks,
                events,
                cleanup_tx,
                next_gen: AtomicU64::new(0),
            }),
        };
        for (index, rule) in rules.iter().enumerate() {
            handle.apply_rule(index, rule).await;
        }
        handle
    }
}

impl ForwardHandle {
    /// 启动或替换一条规则（同索引已有任务先移除）。
    ///
    /// 规则被校验，enabled = false 的规则等价于移除（不启动新任务）。
    pub async fn apply_rule(&self, rule_index: usize, rule: &ForwardRule) {
        self.remove_rule(rule_index).await;
        if !rule.enabled {
            return;
        }
        if let Err(detail) = rule.validate() {
            report(
                &self.inner.events,
                ForwardEvent {
                    rule_index,
                    event: RuleEvent::Failed(ForwardFailure::Invalid {
                        detail: detail.to_string(),
                    }),
                },
            )
            .await;
            return;
        }
        let count = Arc::new(AtomicUsize::new(0));
        let bind = (rule.bind_addr().to_string(), rule.bind_port);
        let inner = match rule.kind {
            ForwardKind::Local | ForwardKind::Dynamic => tokio::spawn(run_local_forward(
                self.inner.conn.clone(),
                rule.clone(),
                rule_index,
                count.clone(),
                self.inner.events.clone(),
            )),
            ForwardKind::Remote => tokio::spawn(run_remote_forward(
                self.inner.conn.clone(),
                rule.clone(),
                rule_index,
                count.clone(),
                self.inner.events.clone(),
            )),
        };
        let generation = self.inner.next_gen.fetch_add(1, Ordering::Relaxed);
        let index = rule_index;
        let tasks = Arc::clone(&self.inner.tasks);
        let cleanup = self.inner.cleanup_tx.clone();
        // 包一层自清任务：规则任务退出后把句柄表条目摘掉（GUI 状态以事件为准，
        // 条目只影响重复 remove 的幂等性）。
        let abort = inner.abort_handle();
        tokio::spawn(async move {
            let _ = inner.await;
            let _ = cleanup.send((index, generation)).await;
            drop(tasks);
        });
        self.inner.tasks.lock().await.insert(
            rule_index,
            RuleTask {
                abort,
                kind: rule.kind,
                bind,
                generation,
            },
        );
    }

    /// 移除一条规则：停止接受新连接并自然排空存量桥；远程规则同时向服务端
    /// best-effort 注销监听（服务端是否掐断存量连接由服务端决定）。
    pub async fn remove_rule(&self, rule_index: usize) {
        let task = self.inner.tasks.lock().await.remove(&rule_index);
        let Some(task) = task else {
            return;
        };
        task.abort.abort();
        // 任务自然结束（失败 / 监听循环退出）时已经自己上报过状态，不再补发 Closed。
        if !task.abort.is_finished() {
            report(
                &self.inner.events,
                ForwardEvent {
                    rule_index,
                    event: RuleEvent::Closed,
                },
            )
            .await;
        }
        if task.kind == ForwardKind::Remote {
            self.inner
                .conn
                .forward_router()
                .unregister(&task.bind.0, task.bind.1 as u32);
            let handle_arc = self.inner.conn.ssh_handle();
            let handle = handle_arc.lock().await;
            if let Err(e) = handle
                .cancel_tcpip_forward(&task.bind.0, task.bind.1 as u32)
                .await
            {
                debug!(
                    "cancel_tcpip_forward {}:{} failed (link may be gone): {e}",
                    task.bind.0, task.bind.1
                );
            }
        }
    }

    /// 停止全部规则（含向服务端注销远程转发）。
    pub async fn clear(&self) {
        let indexes: Vec<usize> = self.inner.tasks.lock().await.keys().copied().collect();
        for index in indexes {
            self.remove_rule(index).await;
        }
    }

    /// 当前在运行的规则索引（调试与测试用）。
    pub async fn active_rules(&self) -> Vec<usize> {
        self.inner.tasks.lock().await.keys().copied().collect()
    }
}

impl Drop for ForwardHandleInner {
    fn drop(&mut self) {
        // 只终止本地监听任务；远程注册的注销需要 await，留给连接关闭收尾
        // （GUI 关标签前应先调 clear()）。
        if let Ok(mut tasks) = self.tasks.try_lock() {
            for (_, task) in tasks.drain() {
                task.abort.abort();
            }
        }
    }
}

/// 上报事件（接收端消失后静默降级为无状态转发）。
async fn report(events: &mpsc::Sender<ForwardEvent>, event: ForwardEvent) {
    let _ = events.send(event).await;
}

/// 本地 / 动态转发主循环：绑定监听，逐连接派发桥任务。
async fn run_local_forward(
    conn: Arc<SshConnection>,
    rule: ForwardRule,
    rule_index: usize,
    count: Arc<AtomicUsize>,
    events: mpsc::Sender<ForwardEvent>,
) {
    let bind_addr = rule.bind_addr().to_string();
    let bind_port = rule.bind_port;
    let listener = match TcpListener::bind((bind_addr.as_str(), bind_port)).await {
        Ok(listener) => listener,
        Err(e) => {
            report(
                &events,
                ForwardEvent {
                    rule_index,
                    event: RuleEvent::Failed(ForwardFailure::Bind {
                        addr: bind_addr,
                        port: bind_port,
                        detail: e.to_string(),
                    }),
                },
            )
            .await;
            return;
        }
    };
    debug!(
        "forward: listening on {bind_addr}:{bind_port} ({:?})",
        rule.kind
    );
    report(
        &events,
        ForwardEvent {
            rule_index,
            event: RuleEvent::Listening,
        },
    )
    .await;

    loop {
        match listener.accept().await {
            Ok((tcp, peer)) => {
                let conn = conn.clone();
                let rule = rule.clone();
                let count = count.clone();
                let events = events.clone();
                tokio::spawn(async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    report(
                        &events,
                        ForwardEvent {
                            rule_index,
                            event: RuleEvent::Connections(count.load(Ordering::Relaxed)),
                        },
                    )
                    .await;
                    match rule.kind {
                        ForwardKind::Local => {
                            bridge_local(conn, tcp, &rule, peer).await;
                        }
                        ForwardKind::Dynamic => match socks_handshake(tcp).await {
                            Ok((tcp, target)) => {
                                bridge_to(conn, tcp, &target.0, target.1, peer).await;
                            }
                            Err(detail) => {
                                debug!("forward: SOCKS5 handshake from {peer} failed: {detail}");
                            }
                        },
                        ForwardKind::Remote => unreachable!("remote rules have no local listener"),
                    }
                    count.fetch_sub(1, Ordering::Relaxed);
                    report(
                        &events,
                        ForwardEvent {
                            rule_index,
                            event: RuleEvent::Connections(count.load(Ordering::Relaxed)),
                        },
                    )
                    .await;
                });
            }
            Err(e) => {
                debug!("forward: accept on {bind_addr}:{bind_port} failed: {e}");
                break;
            }
        }
    }
    report(
        &events,
        ForwardEvent {
            rule_index,
            event: RuleEvent::Closed,
        },
    )
    .await;
}

/// 在服务端侧开一条到目的地的 direct-tcpip 通道并与本地 TCP 桥接。
async fn bridge_to(
    conn: Arc<SshConnection>,
    mut tcp: TcpStream,
    target_host: &str,
    target_port: u16,
    peer: std::net::SocketAddr,
) {
    let channel = {
        let handle_arc = conn.ssh_handle();
        let handle = handle_arc.lock().await;
        handle
            .channel_open_direct_tcpip(
                target_host.to_string(),
                target_port as u32,
                peer.ip().to_string(),
                peer.port() as u32,
            )
            .await
    };
    match channel {
        Ok(channel) => {
            let mut chan = channel.into_stream();
            let _ = copy_bidirectional(&mut chan, &mut tcp).await;
        }
        Err(e) => {
            // 目的地不可达 / 服务端拒绝：本地连接随 tcp drop 关闭。
            debug!("forward: channel_open_direct_tcpip to {target_host}:{target_port} failed: {e}");
        }
    }
}

/// 本地转发连接桥（目的固定，直接复用 bridge_to）。
async fn bridge_local(
    conn: Arc<SshConnection>,
    tcp: TcpStream,
    rule: &ForwardRule,
    peer: std::net::SocketAddr,
) {
    let host = rule.target_addr().to_string();
    let port = rule.target_port.expect("validated");
    bridge_to(conn, tcp, &host, port, peer).await;
}

/// 远程转发主循环：向服务端注册监听，消费回送连接并桥向客户端侧目的地。
async fn run_remote_forward(
    conn: Arc<SshConnection>,
    rule: ForwardRule,
    rule_index: usize,
    count: Arc<AtomicUsize>,
    events: mpsc::Sender<ForwardEvent>,
) {
    let bind_addr = rule.bind_addr().to_string();
    let bind_port = rule.bind_port;
    {
        let handle_arc = conn.ssh_handle();
        let handle = handle_arc.lock().await;
        match handle.tcpip_forward(&bind_addr, bind_port as u32).await {
            Ok(granted) => {
                debug!("forward: remote {bind_addr}:{granted} registered (asked {bind_port})");
            }
            Err(e) => {
                debug!("forward: tcpip_forward {bind_addr}:{bind_port} denied: {e}");
                report(
                    &events,
                    ForwardEvent {
                        rule_index,
                        event: RuleEvent::Failed(ForwardFailure::RemoteDenied {
                            addr: bind_addr,
                            port: bind_port,
                        }),
                    },
                )
                .await;
                return;
            }
        }
    }
    let (tx, mut rx) = mpsc::channel::<IncomingRemote>(16);
    conn.forward_router()
        .register(bind_addr.clone(), bind_port as u32, tx);
    report(
        &events,
        ForwardEvent {
            rule_index,
            event: RuleEvent::Listening,
        },
    )
    .await;

    let target_host = rule.target_addr().to_string();
    let target_port = rule.target_port.expect("validated");
    while let Some(incoming) = rx.recv().await {
        let count = count.clone();
        let events = events.clone();
        let target_host = target_host.clone();
        tokio::spawn(async move {
            count.fetch_add(1, Ordering::Relaxed);
            report(
                &events,
                ForwardEvent {
                    rule_index,
                    event: RuleEvent::Connections(count.load(Ordering::Relaxed)),
                },
            )
            .await;
            let mut chan = incoming.channel.into_stream();
            match TcpStream::connect((target_host.as_str(), target_port)).await {
                Ok(mut tcp) => {
                    let _ = copy_bidirectional(&mut chan, &mut tcp).await;
                }
                Err(e) => {
                    // 客户端侧目的地不可达：通道随 drop 关闭，服务端侧连接 EOF。
                    debug!("forward: remote target {target_host}:{target_port} unreachable: {e}");
                }
            }
            count.fetch_sub(1, Ordering::Relaxed);
            report(
                &events,
                ForwardEvent {
                    rule_index,
                    event: RuleEvent::Connections(count.load(Ordering::Relaxed)),
                },
            )
            .await;
        });
    }
    // 路由表条目被移除（规则被替换 / 注销）时循环退出。
    report(
        &events,
        ForwardEvent {
            rule_index,
            event: RuleEvent::Closed,
        },
    )
    .await;
}

/// 解析 SOCKS5 请求中的目的地址（ATYP + 地址字节 + 端口），返回可直接用于
/// TcpStream::connect 的 (host, port)（IPv6 字面量已加方括号）。失败返回应答复的
/// REP 错误码。
fn socks_target(atyp: u8, addr: &[u8], port: u16) -> Result<(String, u16), u8> {
    match atyp {
        0x01 if addr.len() == 4 => Ok((
            std::net::Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]).to_string(),
            port,
        )),
        0x03 if !addr.is_empty() => match String::from_utf8(addr.to_vec()) {
            Ok(host) => Ok((host, port)),
            Err(_) => Err(0x01),
        },
        0x04 if addr.len() == 16 => {
            let ip = std::net::Ipv6Addr::new(
                u16::from_be_bytes([addr[0], addr[1]]),
                u16::from_be_bytes([addr[2], addr[3]]),
                u16::from_be_bytes([addr[4], addr[5]]),
                u16::from_be_bytes([addr[6], addr[7]]),
                u16::from_be_bytes([addr[8], addr[9]]),
                u16::from_be_bytes([addr[10], addr[11]]),
                u16::from_be_bytes([addr[12], addr[13]]),
                u16::from_be_bytes([addr[14], addr[15]]),
            );
            Ok((format!("[{ip}]"), port))
        }
        _ => Err(0x01),
    }
}

/// 从客户端问候中挑选认证方法：支持无认证（0x00），否则回 0xFF（无可接受方法）。
fn pick_socks_method(methods: &[u8]) -> u8 {
    if methods.contains(&0x00) { 0x00 } else { 0xFF }
}

/// 与一条入站连接完成 SOCKS5 握手（无认证、仅 CONNECT），返回可桥接的本地流与
/// 请求的目的地。失败时流已被回复错误码并丢弃。
async fn socks_handshake(mut tcp: TcpStream) -> Result<(TcpStream, (String, u16)), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn reply_error(tcp: &mut TcpStream, code: u8) {
        // REP 置错误码，其余字段置零（ATYP=1 + 4 字节零地址 + 2 字节零端口）。
        let _ = tcp
            .write_all(&[0x05, code, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await;
    }

    // 问候：VER + NMETHODS + METHODS。
    let mut head = [0u8; 2];
    tcp.read_exact(&mut head)
        .await
        .map_err(|e| format!("read greeting: {e}"))?;
    if head[0] != 0x05 {
        return Err(format!("unsupported SOCKS version {}", head[0]));
    }
    let mut methods = vec![0u8; head[1] as usize];
    tcp.read_exact(&mut methods)
        .await
        .map_err(|e| format!("read methods: {e}"))?;
    let method = pick_socks_method(&methods);
    if method != 0x00 {
        let _ = tcp.write_all(&[0x05, 0xFF]).await;
        return Err("client offers no acceptable auth method".into());
    }
    tcp.write_all(&[0x05, 0x00])
        .await
        .map_err(|e| format!("write method: {e}"))?;

    // 请求：VER + CMD + RSV + ATYP。
    let mut head = [0u8; 4];
    tcp.read_exact(&mut head)
        .await
        .map_err(|e| format!("read request: {e}"))?;
    if head[0] != 0x05 {
        return Err(format!("unsupported request version {}", head[0]));
    }
    // 地址与端口必须读完再判命令：Windows 关闭仍有未读数据的连接会发 RST，
    // 客户端可能连同错误应答一起收到连接重置。
    let (addr, _atyp_len) = match head[3] {
        0x01 => {
            let mut buf = [0u8; 4];
            tcp.read_exact(&mut buf)
                .await
                .map_err(|e| format!("read ipv4: {e}"))?;
            (buf.to_vec(), 4usize)
        }
        0x03 => {
            let mut len = [0u8; 1];
            tcp.read_exact(&mut len)
                .await
                .map_err(|e| format!("read domain len: {e}"))?;
            let mut buf = vec![0u8; len[0] as usize];
            tcp.read_exact(&mut buf)
                .await
                .map_err(|e| format!("read domain: {e}"))?;
            (buf, len[0] as usize)
        }
        0x04 => {
            let mut buf = [0u8; 16];
            tcp.read_exact(&mut buf)
                .await
                .map_err(|e| format!("read ipv6: {e}"))?;
            (buf.to_vec(), 16usize)
        }
        other => {
            reply_error(&mut tcp, 0x08).await; // address type not supported
            return Err(format!("unsupported address type {other}"));
        }
    };
    let mut port = [0u8; 2];
    tcp.read_exact(&mut port)
        .await
        .map_err(|e| format!("read port: {e}"))?;
    if head[1] != 0x01 {
        reply_error(&mut tcp, 0x07).await; // command not supported
        return Err(format!("unsupported command {}", head[1]));
    }
    let (host, port) = match socks_target(head[3], &addr, u16::from_be_bytes(port)) {
        Ok(target) => target,
        Err(code) => {
            reply_error(&mut tcp, code).await;
            return Err("malformed target address".into());
        }
    };

    // 成功应答：REP=0，BND 以 ATYP=1 的零地址占位。
    let _ = tcp
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await;
    Ok((tcp, (host, port)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pick_socks_method：含无认证则选无认证，否则拒绝（0xFF）。
    #[test]
    fn socks_method_selection() {
        assert_eq!(pick_socks_method(&[0x00]), 0x00);
        assert_eq!(pick_socks_method(&[0x02, 0x00]), 0x00);
        assert_eq!(pick_socks_method(&[0x01, 0x02]), 0xFF);
        assert_eq!(pick_socks_method(&[]), 0xFF);
    }

    /// socks_target：IPv4 / 域名 / IPv6 与畸形输入。
    #[test]
    fn socks_target_parsing() {
        assert_eq!(
            socks_target(0x01, &[127, 0, 0, 1], 1080).ok(),
            Some(("127.0.0.1".to_string(), 1080))
        );
        assert_eq!(
            socks_target(0x03, b"example.com", 80).ok(),
            Some(("example.com".to_string(), 80))
        );
        assert_eq!(
            socks_target(0x04, &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], 443).ok(),
            Some(("[::1]".to_string(), 443))
        );
        // 长度不符 / 空域名 → REP 0x01。
        assert_eq!(socks_target(0x01, &[127, 0, 0], 80), Err(0x01));
        assert_eq!(socks_target(0x03, &[], 80), Err(0x01));
        assert_eq!(socks_target(0x09, &[0], 80), Err(0x01));
    }
}
