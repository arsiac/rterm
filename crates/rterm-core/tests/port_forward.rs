//! 端口转发的集成测试：本地（-L）走进程内真实 SSH 服务端的 direct-tcpip 桥，
//! 用例经生产路径（[`rterm_core::SshConnection::connect`](rterm_core::SshConnection::connect)）
//! 建连后用 [`rterm_core::ForwardEngine`](rterm_core::ForwardEngine) 启动规则。
//!
//! 判定依据：服务端自报的 direct-tcpip 计数（流量确实经过服务端桥接）+ 本地 TCP 字节往返。

#[allow(dead_code)] // 只用到支撑模块的一部分，其余是传输 / 终端测试的装备
mod support;

use std::sync::OnceLock;
use std::time::Duration;

use rterm_config::{ForwardKind, ForwardRule};
use rterm_core::{ForwardEngine, RuleEvent};
use support::TestSsh;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

/// 把状态根切到本测试二进制独享的临时目录（known_hosts 写入据此隔离）。
fn init_test_root() {
    static ROOT: OnceLock<()> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("rterm-port-forward-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create the test root");
        rterm_config::paths::set_test_root(Some(root));
    });
}

/// 申请一个当前空闲的本地端口（先绑 :0 再放开；测试并发下存在理论竞争，可接受）。
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind :0");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

/// 起一个本地 echo 服务，返回监听地址端口。
async fn spawn_echo_server() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind echo");
    let port = listener.local_addr().expect("local addr").port();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut rd, mut wr) = tokio::io::split(tcp);
                let _ = tokio::io::copy(&mut rd, &mut wr).await;
            });
        }
    });
    port
}

/// 一条本地转发规则：客户端监听 127.0.0.1:bind_port，服务端连向 echo。
fn local_rule(bind_port: u16, target_port: u16) -> ForwardRule {
    ForwardRule {
        kind: ForwardKind::Local,
        bind_address: String::new(),
        bind_port,
        target_host: Some("127.0.0.1".into()),
        target_port: Some(target_port),
        enabled: true,
    }
}

/// 等某条规则的下一个事件（跳过连接计数事件），超时即失败。
async fn next_status(
    rx: &mut tokio::sync::mpsc::Receiver<rterm_core::ForwardEvent>,
    rule_index: usize,
) -> RuleEvent {
    loop {
        let event = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("event arrives in time")
            .expect("engine alive");
        assert_eq!(event.rule_index, rule_index, "事件来自预期规则");
        if matches!(event.event, RuleEvent::Connections(_)) {
            continue;
        }
        return event.event;
    }
}

/// -L：经服务端桥接后字节原样往返，服务端转发计数递增。
#[tokio::test]
async fn local_forward_round_trips_through_the_server() {
    init_test_root();
    let ssh = TestSsh::start().await;
    let conn = ssh.connect().await;

    let echo_port = spawn_echo_server().await;
    let bind_port = free_port();
    let forwards_before = ssh.forwards();

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle =
        ForwardEngine::spawn(conn.conn().clone(), &[local_rule(bind_port, echo_port)], tx).await;

    // 监听就绪后，本地写入的内容应经服务端桥到 echo 再回来。
    assert!(matches!(
        next_status(&mut rx, 0).await,
        RuleEvent::Listening
    ));
    let mut tcp = TcpStream::connect(("127.0.0.1", bind_port))
        .await
        .expect("connect to the local listener");
    tcp.write_all(b"ping-through-forward").await.expect("write");
    let mut buf = vec![0u8; b"ping-through-forward".len()];
    tcp.read_exact(&mut buf).await.expect("echo back");
    assert_eq!(buf, b"ping-through-forward");

    assert!(
        ssh.forwards() > forwards_before,
        "服务端必须看到 direct-tcpip 落到自己身上"
    );

    // 移除规则：监听停止，端口随后可复用。
    handle.remove_rule(0).await;
    assert!(matches!(next_status(&mut rx, 0).await, RuleEvent::Closed));
    drop(handle);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        TcpListener::bind(("127.0.0.1", bind_port)).await.is_ok(),
        "移除规则后监听端口应被释放"
    );
}

/// -L：目的端口不可达时，本地连接被关闭（读到 EOF），规则与连接本身不受影响。
#[tokio::test]
async fn local_forward_closes_client_when_target_is_unreachable() {
    init_test_root();
    let ssh = TestSsh::start().await;
    let conn = ssh.connect().await;

    let dead_port = free_port();
    let bind_port = free_port();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle =
        ForwardEngine::spawn(conn.conn().clone(), &[local_rule(bind_port, dead_port)], tx).await;
    assert!(matches!(
        next_status(&mut rx, 0).await,
        RuleEvent::Listening
    ));

    let mut tcp = TcpStream::connect(("127.0.0.1", bind_port))
        .await
        .expect("local listener accepts");
    // 服务端桥接 ConnectFailed → 通道被关 → 本地连接 EOF。
    let mut buf = Vec::new();
    timeout(Duration::from_secs(5), tcp.read_to_end(&mut buf))
        .await
        .expect("connection closes in time")
        .expect("read to end");
    assert!(buf.is_empty(), "不可达目的地不应有任何数据");

    // 连接本身与规则都还活着：换一条可达规则不影响本用例，直接验证引擎仍在。
    assert_eq!(handle.active_rules().await, vec![0]);
}

/// 引擎按 enabled 过滤：停用规则不启动，后续 apply_rule 可再启用。
#[tokio::test]
async fn disabled_rules_do_not_start_until_applied() {
    init_test_root();
    let ssh = TestSsh::start().await;
    let conn = ssh.connect().await;

    let echo_port = spawn_echo_server().await;
    let bind_port = free_port();
    let mut rule = local_rule(bind_port, echo_port);
    rule.enabled = false;

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = ForwardEngine::spawn(conn.conn().clone(), &[rule], tx).await;
    assert!(
        handle.active_rules().await.is_empty(),
        "停用规则不应启动监听"
    );

    // 动态启用：同一端口立即可连。
    let mut enabled = local_rule(bind_port, echo_port);
    enabled.enabled = true;
    handle.apply_rule(0, &enabled).await;
    assert!(matches!(
        next_status(&mut rx, 0).await,
        RuleEvent::Listening
    ));
    let mut tcp = TcpStream::connect(("127.0.0.1", bind_port))
        .await
        .expect("listener up after enable");
    tcp.write_all(b"hi").await.expect("write");
    let mut buf = [0u8; 2];
    tcp.read_exact(&mut buf).await.expect("echo");
    assert_eq!(&buf, b"hi");
}

/// -D：SOCKS5 CONNECT 经服务端桥到 echo；BIND 命令被拒（REP 0x07）。
#[tokio::test]
async fn dynamic_forward_serves_socks5_connect_and_rejects_bind() {
    init_test_root();
    let ssh = TestSsh::start().await;
    let conn = ssh.connect().await;

    let echo_port = spawn_echo_server().await;
    let bind_port = free_port();
    let rule = ForwardRule {
        kind: ForwardKind::Dynamic,
        bind_address: String::new(),
        bind_port,
        target_host: None,
        target_port: None,
        enabled: true,
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = ForwardEngine::spawn(conn.conn().clone(), &[rule], tx).await;
    assert!(matches!(
        next_status(&mut rx, 0).await,
        RuleEvent::Listening
    ));
    let forwards_before = ssh.forwards();

    let mut tcp = TcpStream::connect(("127.0.0.1", bind_port))
        .await
        .expect("connect to the local listener");

    // 问候：只提供无认证，应答 METHOD=0x00。
    tcp.write_all(&[0x05, 0x01, 0x00]).await.expect("greeting");
    let mut method = [0u8; 2];
    tcp.read_exact(&mut method).await.expect("method reply");
    assert_eq!(method, [0x05, 0x00], "应选中无认证方法");

    // CONNECT 到本地 echo（ATYP=1 IPv4）。
    let mut request = vec![0x05, 0x01, 0x00, 0x01];
    request.extend_from_slice(&[127, 0, 0, 1]);
    request.extend_from_slice(&echo_port.to_be_bytes());
    tcp.write_all(&request).await.expect("connect request");
    let mut reply = [0u8; 10];
    tcp.read_exact(&mut reply).await.expect("connect reply");
    assert_eq!(reply[1], 0x00, "CONNECT 应成功");

    // 字节经隧道往返。
    tcp.write_all(b"socks-ping").await.expect("write");
    let mut buf = [0u8; 10];
    tcp.read_exact(&mut buf).await.expect("echo");
    assert_eq!(&buf, b"socks-ping");
    assert!(ssh.forwards() > forwards_before, "流量必须经过服务端桥");
    drop(tcp);

    // BIND 命令被拒（REP=0x07 command not supported）。
    let mut tcp = TcpStream::connect(("127.0.0.1", bind_port))
        .await
        .expect("reconnect");
    tcp.write_all(&[0x05, 0x01, 0x00])
        .await
        .expect("greeting 2");
    let mut method = [0u8; 2];
    tcp.read_exact(&mut method).await.expect("method reply 2");
    tcp.write_all(&[0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
        .expect("bind request");
    let mut reply = [0u8; 10];
    tcp.read_exact(&mut reply).await.expect("bind reply");
    assert_eq!(reply[1], 0x07, "BIND 应被回 command not supported");

    handle.clear().await;
}

/// -R：服务端端口收到连接后回送，客户端桥到本地 echo，字节往返。
#[tokio::test]
async fn remote_forward_delivers_connections_to_the_client() {
    init_test_root();
    let ssh = TestSsh::start().await;
    let conn = ssh.connect().await;

    let echo_port = spawn_echo_server().await;
    let bind_port = free_port();
    let rule = ForwardRule {
        kind: ForwardKind::Remote,
        bind_address: String::new(),
        bind_port,
        target_host: Some("127.0.0.1".into()),
        target_port: Some(echo_port),
        enabled: true,
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = ForwardEngine::spawn(conn.conn().clone(), &[rule], tx).await;
    assert!(matches!(
        next_status(&mut rx, 0).await,
        RuleEvent::Listening
    ));

    // 连接的是「服务端」的监听端口；数据应回送经客户端桥到本地 echo。
    let mut tcp = TcpStream::connect(("127.0.0.1", bind_port))
        .await
        .expect("connect to the server-side listener");
    tcp.write_all(b"reverse-ping").await.expect("write");
    let mut buf = [0u8; 12];
    tcp.read_exact(&mut buf).await.expect("echo back");
    assert_eq!(&buf, b"reverse-ping");

    handle.clear().await;
}

/// -R：服务端拒绝注册（模拟 GatewayPorts 关闭）时规则报 Failed，连接与其他规则不受影响。
#[tokio::test]
async fn remote_forward_denied_reports_failure_without_killing_the_connection() {
    init_test_root();
    let ssh = TestSsh::start_denying_remote().await;
    let conn = ssh.connect().await;

    let bind_port = free_port();
    let rule = ForwardRule {
        kind: ForwardKind::Remote,
        bind_address: String::new(),
        bind_port,
        target_host: Some("127.0.0.1".into()),
        target_port: Some(22),
        enabled: true,
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    // 本用例只消费事件流：句柄不入变量，引擎随连接 drop 即收尾。
    let _handle = ForwardEngine::spawn(conn.conn().clone(), &[rule], tx).await;

    match next_status(&mut rx, 0).await {
        RuleEvent::Failed(rterm_core::ForwardFailure::RemoteDenied { addr, port }) => {
            assert_eq!(addr, "127.0.0.1");
            assert_eq!(port, bind_port);
        }
        other => panic!("应报 RemoteDenied，实际 {other:?}"),
    }
    assert!(
        !conn.conn().is_closed().await,
        "单条规则失败不得中断连接本身"
    );
}

/// -R：移除规则时向服务端注销监听，服务端端口被释放。
#[tokio::test]
async fn remote_forward_cancel_releases_the_server_port() {
    init_test_root();
    let ssh = TestSsh::start().await;
    let conn = ssh.connect().await;

    let bind_port = free_port();
    let rule = ForwardRule {
        kind: ForwardKind::Remote,
        bind_address: String::new(),
        bind_port,
        target_host: Some("127.0.0.1".into()),
        target_port: Some(22),
        enabled: true,
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = ForwardEngine::spawn(conn.conn().clone(), &[rule], tx).await;
    assert!(matches!(
        next_status(&mut rx, 0).await,
        RuleEvent::Listening
    ));

    handle.remove_rule(0).await;
    assert!(matches!(next_status(&mut rx, 0).await, RuleEvent::Closed));

    // 服务端（与本测试同机）已注销监听：同端口可再次绑定即证明释放。
    timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", bind_port)) {
                drop(listener);
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("server port released after cancel");
}
