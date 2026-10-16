//! 会话启动作业的集成测试：合成出的首段输入必须**逐字节**抵达远端 shell 通道，
//! 且要求隐藏回显时 PTY 请求确实携带 `ECHO=0`。判定依据是服务端记录的客户端输入与
//! PTY 模式位；测试用的 shell 通道打开即静默，故客户端的唯一写入就是这段注入。

#[allow(dead_code)] // 支撑模块里另含 SFTP / 跳板机装备，本文件只用其中一部分
mod support;

use rterm_core::{BridgeInput, PtySize, compose_initial_input, spawn_terminal_bridge};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use support::TestSsh;
use tokio::time::sleep;

/// 把状态根切到本测试二进制独享的临时目录（同 `terminal_disconnect.rs`）。
fn init_test_root() {
    static ROOT: OnceLock<()> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("rterm-startup-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create the test root");
        rterm_config::paths::set_test_root(Some(root));
    });
}

/// 起桥接（本地管道端故意泄漏，pump 只认远端事件与停止请求）。
async fn bridge_up(ssh: &TestSsh, input: BridgeInput) {
    let conn = ssh.connect().await;
    let (out, r#in, _state, _resize_tx) =
        spawn_terminal_bridge(Arc::clone(conn.conn()), PtySize::new(80, 24), input, None)
            .await
            .expect("bridge setup must succeed against the test server");
    std::mem::forget(out);
    std::mem::forget(r#in);
}

/// 等客户端输入收满 `want.len()` 字节（超时即返回现状，由调用方的断言判定失败）。
async fn wait_for_input(ssh: &TestSsh, want: usize) -> Vec<u8> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let got = ssh.shell_input().await;
        if got.len() >= want || std::time::Instant::now() > deadline {
            return got;
        }
        sleep(Duration::from_millis(10)).await;
    }
}

/// 追踪 cwd 的共享容器（钩子开关打开时的前置条件）。
fn cwd_tracker() -> Option<Arc<Mutex<Option<String>>>> {
    Some(Arc::new(Mutex::new(None)))
}

/// 只有作业、无钩子且不隐藏：注入仅为「空操作行 + 作业行」，逐字节抵达且未关回显。
#[tokio::test]
async fn the_job_reaches_the_remote_shell_verbatim_with_echo_on() {
    init_test_root();

    let ssh = TestSsh::start().await;
    let input = BridgeInput {
        cwd: None,
        cwd_bootstrap: false,
        startup_command: Some("cd /srv && make".to_string()),
        hide_injected: false,
    };
    bridge_up(&ssh, input).await;

    let want = b":\ncd /srv && make\n";
    let got = wait_for_input(&ssh, want.len()).await;
    assert_eq!(got, want, "注入字节须原样、且是唯一的客户端写入");
    assert!(!ssh.echo_disabled().await, "未要求隐藏时不应关闭回显");
}

/// 钩子与作业并存且要求隐藏：首行恢复回显、次行作业，PTY 请求带 `ECHO=0`。
#[tokio::test]
async fn a_hidden_injection_disables_then_restores_echo() {
    init_test_root();

    let ssh = TestSsh::start().await;
    let job = "export FOO=bar";
    let input = BridgeInput {
        cwd: cwd_tracker(),
        cwd_bootstrap: true,
        startup_command: Some(job.to_string()),
        hide_injected: true,
    };
    bridge_up(&ssh, input).await;

    let expected = compose_initial_input(true, true, Some(job))
        .bytes
        .expect("钩子与作业并存时必有字节");
    let got = wait_for_input(&ssh, expected.len()).await;
    assert_eq!(got, expected, "注入字节须与合成结果逐字节一致");
    assert!(ssh.echo_disabled().await, "隐藏注入时 PTY 须以 ECHO=0 建立");
    assert!(
        got.windows(b"stty echo\n".len())
            .any(|w| w == b"stty echo\n"),
        "隐藏注入须以 stty echo 恢复回显"
    );
    let text = String::from_utf8_lossy(&got);
    assert!(
        text.ends_with(&format!("{job}\n")),
        "作业须在末行、独立成行"
    );
}

/// 只开钩子（无作业）：字节与隐藏注入路径逐字节一致（回归锁）。
#[tokio::test]
async fn a_jobless_bootstrap_keeps_its_existing_bytes() {
    init_test_root();

    let ssh = TestSsh::start().await;
    let input = BridgeInput {
        cwd: cwd_tracker(),
        cwd_bootstrap: true,
        startup_command: None,
        hide_injected: true,
    };
    bridge_up(&ssh, input).await;

    let expected = compose_initial_input(true, true, None)
        .bytes
        .expect("钩子存在时必有字节");
    let got = wait_for_input(&ssh, expected.len()).await;
    assert_eq!(got, expected, "仅钩子时注入字节须与合成结果一致");
    // 关回显与字节同源派生：只有钩子而无作业且要求隐藏时，仍需关回显并恢复。
    assert!(ssh.echo_disabled().await);
}

/// 既不追踪 cwd 也无作业：客户端**一个字节都不写**，PTY 也不关回显。
#[tokio::test]
async fn no_hook_and_no_job_writes_nothing() {
    init_test_root();

    let ssh = TestSsh::start().await;
    let input = BridgeInput {
        cwd: None,
        cwd_bootstrap: true,
        startup_command: None,
        hide_injected: true,
    };
    bridge_up(&ssh, input).await;

    // 无正向信号可等，给桥接一段起步时间后断言「仍是空的」。
    sleep(Duration::from_millis(300)).await;
    assert!(
        ssh.shell_input().await.is_empty(),
        "无钩子无作业时不该向远端写入任何字节"
    );
    assert!(!ssh.echo_disabled().await, "无写入时不应关闭回显");
}
