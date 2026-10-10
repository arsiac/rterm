//! 跳板机（ProxyJump）建链的集成测试：用进程内的真实 SSH 服务端拼出 1 跳 / 2 跳拓扑，
//! 走生产的 [`rterm_core::SshConnection::connect`] 路径。
//!
//! 目标服务端与跳板机都监听 127.0.0.1，直连本就通——所以「连上了」不能证明隧道被用到。
//! 判定依据是各服务端自报的每跳转发计数：只有 direct-tcpip 真的落到跳板机上，
//! 目标才会看到一条来自隧道而非直连的 SSH 会话。

#[allow(dead_code)] // 只用到支撑模块的一部分（跳板机装备），其余是传输测试的装备
mod support;

use std::sync::OnceLock;
use std::time::Duration;

use support::TestSsh;
use tokio::time::{sleep, timeout};

/// 把状态根切到本测试二进制独享的临时目录（known_hosts 写入据此隔离）。
fn init_test_root() {
    static ROOT: OnceLock<()> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("rterm-proxy-jump-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create the test root");
        rterm_config::paths::set_test_root(Some(root));
    });
}

/// 等连接转为「已关闭」（超时即判失败）。
async fn wait_closed(conn: &support::TestSshConnection) {
    timeout(Duration::from_secs(5), async {
        while !conn.conn().is_closed().await {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("断链后会话应收尾");
}

/// 不带跳板机时直连目标，目标不应看到任何转发——这是其它用例的对照组。
#[tokio::test]
async fn a_direct_connection_forwards_nothing() {
    init_test_root();

    let target = TestSsh::start().await;
    let conn = target.connect().await;

    assert_eq!(
        target.forwards(),
        0,
        "直连不该在目标上留下任何 direct-tcpip 记录"
    );
    assert!(!conn.conn().is_closed().await);
}

/// 单跳：经跳板机建立的隧道必须真的被使用，且目标上的 shell 可用。
#[tokio::test]
async fn one_hop_reaches_the_target_through_the_bastion() {
    init_test_root();

    let bastion = TestSsh::start().await;
    let target = TestSsh::start().await;
    let conn = target.connect_via(&[bastion.hop()]).await;

    assert_eq!(bastion.forwards(), 1, "跳板机应受理一次 direct-tcpip");
    assert_eq!(target.forwards(), 0, "目标自己是终点，不该再转发");
    assert!(!conn.conn().is_closed().await);

    // 隧道另一端的 shell 必须能开：证明这条链在协议层面是完整可用的。
    conn.conn()
        .open_shell_channel(80, 24, false, false)
        .await
        .expect("经隧道应能打开 shell 通道");
}

/// 两跳：链上每一环都恰好转发一次，顺序由外到内。
#[tokio::test]
async fn two_hops_chain_through_each_bastion() {
    init_test_root();

    let outer = TestSsh::start().await;
    let inner = TestSsh::start().await;
    let target = TestSsh::start().await;
    let conn = target.connect_via(&[outer.hop(), inner.hop()]).await;

    assert_eq!(outer.forwards(), 1, "外层跳板机应转发到内层");
    assert_eq!(inner.forwards(), 1, "内层跳板机应转发到目标");
    assert_eq!(target.forwards(), 0);
    assert!(!conn.conn().is_closed().await);

    conn.conn()
        .open_shell_channel(80, 24, false, false)
        .await
        .expect("两跳隧道上应能打开 shell 通道");
}

/// 跳板机侧隧道被掐断：目标会话随之终结，`is_closed()` 必须转真。
#[tokio::test]
async fn killing_the_bastion_tunnel_ends_the_target_session() {
    init_test_root();

    let bastion = TestSsh::start().await;
    let target = TestSsh::start().await;
    let conn = target.connect_via(&[bastion.hop()]).await;
    assert!(!conn.conn().is_closed().await);

    bastion.kill_bridges().await;
    wait_closed(&conn).await;
}
