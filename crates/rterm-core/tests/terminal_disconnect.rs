//! 终端桥接断开原因的集成测试：连到进程内的真实 SSH 服务端（见 `support`），
//! 覆盖单测碰不到的三条退出路径——连接仍活时 shell 通道 EOF、传输层死亡、
//! 本地主动停止。
//!
//! 判定依据是桥接退出那一刻的 `SshConnection::is_closed()`，故每个用例都要把
//! 「连接的状态」与「通道的结局」组合出目标形态，再断言 [`DisconnectReason`]。

#[allow(dead_code)] // 支撑模块里另含 SFTP 测试的装备，本文件只用其中一部分
mod support;

use rterm_core::{BridgeState, DisconnectReason, spawn_terminal_bridge};
use std::os::fd::IntoRawFd;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use support::{TestSsh, TestSshConnection};
use tokio::time::{sleep, timeout};

/// 把状态根切到本测试二进制独享的临时目录。
///
/// `OnceLock` 保证只设一次（同二进制内所有用例共用**同一个**根是安全的：
/// 它们读写的 known_hosts 内容一致，且与开发沙箱 / 真实配置隔离）；
/// 用同步 `fn` 保证该全局赋值不发生在任何 await 点之间。
fn init_test_root() {
    static ROOT: OnceLock<()> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("rterm-disconnect-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create the test root");
        rterm_config::paths::set_test_root(Some(root));
    });
}

/// 起连接并挂桥接，返回 `(测试连接, 桥接状态)`；`ssh` 由调用方持有（服务端随其存活）。
///
/// 桥接的本地端（out / in）被转成裸 fd 留在进程里：pump 只认远端事件与停止请求，
/// 不会因测试把本地管道关掉而走错分支；测试进程结束后 fd 由内核统一回收。
async fn bridge_up(ssh: &TestSsh) -> (TestSshConnection, Arc<BridgeState>) {
    let conn = ssh.connect().await;
    let (out, r#in, state, _resize_tx) =
        spawn_terminal_bridge(Arc::clone(conn.conn()), 80, 24, None, false, false, None)
            .await
            .expect("bridge setup must succeed against the test server");
    let _ = (out.into_raw_fd(), r#in.into_raw_fd());
    (conn, state)
}

/// 等桥接结束归因（超时即判失败）。
async fn wait_finished(state: &BridgeState) {
    timeout(Duration::from_secs(5), async {
        while !state.is_finished() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the bridge must finish within the timeout");
}

/// 远端 shell 退出（`exit` / `logout`）：传输仍活，只是 shell 通道结束。
///
/// 由**服务端**关闭通道。客户端自己 `Channel::close()` 观察不到这一形态：
/// russh 只在收到 CHANNEL_CLOSE 时才把 Close 转发给读端，客户端主动关闭后
/// 自己的读端并不会收到 EOF。
#[tokio::test]
async fn a_channel_eof_while_the_transport_lives_is_reported_as_channel_eof() {
    init_test_root();

    let ssh = TestSsh::start().await;
    let (conn, state) = bridge_up(&ssh).await;

    ssh.close_shell().await;
    assert!(
        !conn.conn().is_closed().await,
        "关掉一条通道不等于传输死亡：连接必须仍然活着"
    );

    wait_finished(&state).await;
    assert_eq!(
        state.reason(),
        DisconnectReason::ChannelEof,
        "连接活着而通道结束，应判为「远端会话已结束」而非「连接已断开」"
    );
}

/// 传输层死亡（拔网线 / sshd 重启 / 保活超时）：会话任务收尾。
#[tokio::test]
async fn a_dead_transport_is_reported_as_transport_died() {
    init_test_root();

    let ssh = TestSsh::start().await;
    let (conn, state) = bridge_up(&ssh).await;
    conn.kill_transport().await;

    timeout(Duration::from_secs(5), async {
        while !conn.conn().is_closed().await {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("主动断开后会话任务应收尾");

    wait_finished(&state).await;
    assert_eq!(state.reason(), DisconnectReason::TransportDied);
}

/// 本地主动停止（关标签 / 关窗口）：连接完好也不得被误判成故障。
#[tokio::test]
async fn a_local_stop_is_reported_as_local_stop() {
    init_test_root();

    let ssh = TestSsh::start().await;
    let (conn, state) = bridge_up(&ssh).await;

    assert!(!conn.conn().is_closed().await, "停止前连接是活的");
    state.request_stop();

    wait_finished(&state).await;
    assert_eq!(
        state.reason(),
        DisconnectReason::LocalStop,
        "本地停不是断开，不该用故障文案"
    );
}
