//! 桥接注入字节的集成测试：`inject_out` 必须**先于**远端任何输出写进 OUT 管道
//! （重连提示行出现在提示符之前依赖这一点）。

#[allow(dead_code)] // 支撑模块里另含 SFTP 测试的装备，本文件只用其中一部分
mod support;

use rterm_core::spawn_terminal_bridge;
use std::io::Read;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use support::TestSsh;
use tokio::time::sleep;

/// 把状态根切到本测试二进制独享的临时目录（同 `terminal_disconnect.rs`）。
fn init_test_root() {
    static ROOT: OnceLock<()> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("rterm-notice-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create the test root");
        rterm_config::paths::set_test_root(Some(root));
    });
}

/// 注入的一整段字节（复位 + 清屏 + 提示行，含换行）原样且最先出现在本地读端。
///
/// 测试服务端的 shell 通道打开即静默（无进程、无输出），首读到的必然是注入内容；
/// 若注入晚于 pump 启动（或未注入），用例因字节不符 / 收不满而失败。
#[tokio::test]
async fn injected_bytes_arrive_first_and_verbatim() {
    init_test_root();

    let ssh = TestSsh::start().await;
    let conn = ssh.connect().await;
    let payload =
        "\x1b[?1049l\x1b[0m\x1b[H\x1b[2J\x1b[3J[rterm] 会话已重新开始，之前的进程未恢复\r\n"
            .to_string();
    let (mut out, _in, _state, _resize_tx) = spawn_terminal_bridge(
        Arc::clone(conn.conn()),
        80,
        24,
        None,
        false,
        false,
        Some(payload.as_bytes()),
    )
    .await
    .expect("bridge setup must succeed against the test server");

    let expected = payload.as_bytes();
    let mut got = vec![0u8; expected.len()];
    let mut filled = 0;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    // 读端为非阻塞 fd（Unix）：轮询直到收满；写序已由 spawn_terminal_bridge 保证先于
    // pump，这里只等送达。
    while filled < expected.len() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out reading the injected bytes (got {filled}/{} bytes): {:?}",
            expected.len(),
            &got[..filled]
        );
        match out.read(&mut got[filled..]) {
            Ok(0) => panic!("the OUT read end hit EOF before the injection was read"),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("failed to read from the OUT pipe: {e}"),
        }
    }
    assert_eq!(&got[..], expected, "注入字节必须最先到达本地读端且内容原样");
}
