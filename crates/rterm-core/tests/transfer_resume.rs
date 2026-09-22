//! 上传 / 下载续传的集成测试：连到进程内的真实 SSH + SFTP 服务端（见 `support`），
//! 走的是与生产同一条协议栈，覆盖单元测试碰不到的部分——`ResumeAt::Offset(n)` 的打开与
//! 定位、断点在服务端上的真实长度、改名兜底、会话终结的分类。

mod support;

use rterm_core::{CoreError, CoreErrorKind, ErrorClass, ResumeAt};
use support::{Sandbox, TestSftp, payload};

const KILO: usize = 1024;
const MEGA: usize = 1024 * 1024;

/// 冒烟：整套桩能跑通一次「上传 + 改名」，且字节确实落在真名文件里。
#[tokio::test]
async fn an_upload_lands_under_the_final_name_after_rename() {
    let sandbox = Sandbox::new("smoke");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;

    let local = sandbox.local_file("big.bin", 300 * KILO);
    conn.client
        .upload_with_progress(&local, "/big.bin.part", ResumeAt::Start, |_, _, _| {})
        .await
        .expect("upload should succeed");
    conn.client
        .rename("/big.bin.part", "/big.bin")
        .await
        .expect("rename should succeed");

    assert_eq!(server.len("/big.bin"), 300 * KILO);
    assert!(
        !server.path("/big.bin.part").exists(),
        "改名后暂存文件不应存在"
    );
    assert_eq!(read_remote(&server, "/big.bin"), payload(300 * KILO));
}

/// 续传轮次只应发送尾部字节：前 n 字节再写一遍是最隐蔽的错误（大小仍对，内容错位）。
#[tokio::test]
async fn a_resumed_upload_sends_only_the_tail() {
    let sandbox = Sandbox::new("resume-tail");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;

    let total = MEGA;
    let staged = 300 * KILO;
    // 上一轮留下的暂存：前 staged 字节与源端逐字节相同（`payload` 的前缀稳定）。
    sandbox.remote_file("big.bin.part", staged);
    let local = sandbox.local_file("big.bin", total);

    conn.client
        .upload_with_progress(
            &local,
            "/big.bin.part",
            ResumeAt::Offset(staged as u64),
            |_, _, _| {},
        )
        .await
        .expect("resumed upload should succeed");

    assert_eq!(
        server.first_write_offset(),
        staged as u64,
        "本轮第一次写必须落在断点上"
    );
    assert_eq!(
        server.written(),
        (total - staged) as u64,
        "只应上传未落盘的尾部"
    );
    assert_eq!(
        read_remote(&server, "/big.bin.part"),
        payload(total),
        "拼接后的暂存必须与源端逐字节一致"
    );
}

/// 中断 → 断点保住 → 继续 → 内容正确：上传续传的整条链路，对应手工用例 B。
///
/// 尺寸要跨过至少一个确证水位（核心层 `CONFIRM_INTERVAL` = 4 MiB），否则「断点」只能是 0。
#[tokio::test]
async fn an_interrupted_upload_resumes_from_the_bytes_that_landed() {
    let sandbox = Sandbox::new("interrupted");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;

    let total = 6 * MEGA;
    let local = sandbox.local_file("big.bin", total);

    server.break_upload_at(5 * MEGA as u64);
    let first = conn
        .client
        .upload_with_progress(&local, "/big.bin.part", ResumeAt::Start, |_, _, _| {})
        .await;
    assert!(first.is_err(), "写入被撕断后本轮应当失败");
    let landed = server.len("/big.bin.part");
    assert!(
        (4 * MEGA..4 * MEGA + 2 * KILO).contains(&landed),
        "失败轮次应把暂存截回已确证水位（约 4 MiB），实际 {landed}"
    );
    assert_eq!(
        read_remote(&server, "/big.bin.part"),
        payload(landed),
        "留在磁盘上的必须始终是源端的连续前缀"
    );
    assert!(
        !server.path("/big.bin").exists(),
        "改名成功前不得出现真名文件"
    );

    // 下一轮的起点就是磁盘上的长度——与 GUI 的 `resume_offset` 同一条规则。
    server.reset_faults();
    conn.client
        .upload_with_progress(
            &local,
            "/big.bin.part",
            ResumeAt::Offset(landed as u64),
            |_, _, _| {},
        )
        .await
        .expect("the second round should finish");
    assert_eq!(
        server.first_write_offset(),
        landed as u64,
        "第二轮必须从断点开始写，而不是从头"
    );
    conn.client
        .rename("/big.bin.part", "/big.bin")
        .await
        .expect("rename after resume");

    assert_eq!(read_remote(&server, "/big.bin"), payload(total));
}

/// `Offset(n)` 用的是不带 CREATE 的 WRITE：半成品不在时应当报错，而不是悄悄建个空文件
/// 再往 n 位置写（那会产出稀疏文件或 EINVAL）。
#[tokio::test]
async fn resuming_onto_a_missing_staging_file_fails_without_creating_it() {
    let sandbox = Sandbox::new("no-staging");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;

    let local = sandbox.local_file("big.bin", 64 * KILO);
    let err = conn
        .client
        .upload_with_progress(
            &local,
            "/absent.bin.part",
            ResumeAt::Offset(4096),
            |_, _, _| {},
        )
        .await
        .expect_err("opening a missing staging file must fail");

    assert!(
        matches!(
            err,
            CoreError::Sftp {
                kind: CoreErrorKind::OpenRemoteFile,
                ..
            }
        ),
        "应当报「打开远端文件失败」，实际是 {err:?}"
    );
    assert!(
        !server.path("/absent.bin.part").exists(),
        "失败的续传不得在远端留下新文件"
    );
}

/// 从头重传必须先截断暂存：否则上一轮的尾巴会留在文件末尾，而大小看着是对的。
#[tokio::test]
async fn starting_over_truncates_the_stale_tail() {
    let sandbox = Sandbox::new("truncate");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;

    sandbox.remote_file("big.bin.part", 800 * KILO);
    let local = sandbox.local_file("big.bin", 500 * KILO);

    conn.client
        .upload_with_progress(&local, "/big.bin.part", ResumeAt::Start, |_, _, _| {})
        .await
        .expect("restarting from zero should succeed");

    assert_eq!(
        read_remote(&server, "/big.bin.part"),
        payload(500 * KILO),
        "旧的 300 KiB 尾巴必须被截掉"
    );
}

/// 改名失败时暂存必须原样保住（内容已完整），修好后在同一瞬间完成——对应手工用例 F。
#[tokio::test]
async fn a_failed_rename_keeps_the_staged_upload_and_a_retry_completes_it() {
    let sandbox = Sandbox::new("rename");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;

    let local = sandbox.local_file("big.bin", 200 * KILO);
    conn.client
        .upload_with_progress(&local, "/big.bin.part", ResumeAt::Start, |_, _, _| {})
        .await
        .expect("upload should succeed");

    server.fail_renames(1);
    let err = conn
        .client
        .rename("/big.bin.part", "/big.bin")
        .await
        .expect_err("the injected rename failure should surface");
    assert!(
        matches!(err, CoreError::Sftp { .. }),
        "改名失败应作为 SFTP 错误返回：{err:?}"
    );
    assert_eq!(
        server.len("/big.bin.part"),
        200 * KILO,
        "改名失败不该动暂存里的数据"
    );

    conn.client
        .rename("/big.bin.part", "/big.bin")
        .await
        .expect("rename succeeds once the fault is gone");
    assert_eq!(read_remote(&server, "/big.bin"), payload(200 * KILO));
}

/// 下载侧的对偶：本地已有前 n 字节时，服务端只应被读到第 n 字节之后的数据。
#[tokio::test]
async fn a_resumed_download_skips_the_bytes_already_on_disk() {
    let sandbox = Sandbox::new("download-resume");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;

    let total = MEGA;
    let staged = 300 * KILO;
    sandbox.remote_file("src.bin", total);
    let partial = sandbox.local_file("src.bin.part", staged);

    conn.client
        .download_with_progress(
            "/src.bin",
            &partial,
            ResumeAt::Offset(staged as u64),
            |_, _, _| {},
        )
        .await
        .expect("resumed download should succeed");

    assert_eq!(
        server.first_read_offset(),
        staged as u64,
        "服务端不该被要求重读已落盘的前段"
    );
    assert_eq!(std::fs::read(&partial).expect("read local"), payload(total));
}

/// 续传判定的地基：远端 stat 必须同时给得出大小与修改时间（缺 mtime 会让指纹恒判「变了」，
/// 于是永远不续传——这在单元测试里是看不出来的）。
#[tokio::test]
async fn a_remote_fingerprint_carries_size_and_modification_time() {
    let sandbox = Sandbox::new("fingerprint");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;

    sandbox.remote_file("src.bin", 120 * KILO);
    let before = conn
        .client
        .remote_fingerprint("/src.bin")
        .await
        .expect("stat the remote file");
    assert_eq!(before.len, 120 * KILO as u64);
    assert!(before.modified.is_some(), "服务端应报出 mtime");
    assert_eq!(
        conn.client
            .remote_fingerprint("/src.bin")
            .await
            .expect("stat again"),
        before,
        "同一文件两次 stat 必须给出同一指纹"
    );

    // 缺失的暂存是首轮的常态：调用方按「半成品为 0」处理，这里只要求错误可辨识。
    let err = conn
        .client
        .remote_fingerprint("/absent.bin")
        .await
        .expect_err("a missing file must not look like a zero-length one");
    assert!(
        matches!(
            err,
            CoreError::Sftp {
                kind: CoreErrorKind::RemoteMetadata,
                ..
            }
        ),
        "应报「读远端元数据失败」：{err:?}"
    );
}

/// 会话终结的分类：整条 SSH 连接被断开后，这个客户端上的任何请求都只会立刻失败，
/// 上层必须据此停下自动重试（与瞬时故障分开，否则重试预算全烧在死会话上）。
#[tokio::test]
async fn requests_on_a_dead_session_are_classified_as_session_gone() {
    let sandbox = Sandbox::new("dead-session");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let mut conn = server.client().await;

    let local = sandbox.local_file("big.bin", 16 * KILO);
    conn.disconnect().await;

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        conn.client
            .upload_with_progress(&local, "/big.bin.part", ResumeAt::Start, |_, _, _| {}),
    )
    .await
    .expect("a dead session must fail fast, not hang");
    let err = outcome.expect_err("uploading over a dead session must fail");
    assert_eq!(
        err.class(),
        ErrorClass::SessionGone,
        "错误应被分类为会话终结，实际为 {err:?}"
    );
}

/// 暂存空洞的回归用例。
///
/// 场景：连接还活着，只有**一个**写请求被服务器拒掉（瞬时抽风 / 限流）。russh-sftp 的
/// `File::poll_write` 要等排队深度触顶才回收最早的 ack，故失败通常滞后被发现，其间的写早已
/// 发出并被接受——磁盘上于是出现「正确前缀 + 全零空洞 + 空洞之上正确」，而 `len` 比空洞更大。
/// 按 `len` 续传会永久跳过那段零，产出大小正确、内容错误的文件还报告成功。
///
/// 核心层的对策是定期确证 + 失败时截回（见 `sftp.rs` 的 `Watermark`），故这里断言的不变量是：
/// **留在磁盘上的始终是源端的连续前缀**。
#[tokio::test]
async fn a_transient_write_failure_leaves_no_hole_in_the_staging_file() {
    let sandbox = Sandbox::new("transient");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;

    let total = 6 * MEGA;
    let local = sandbox.local_file("big.bin", total);

    server.reject_write_at(2 * MEGA as u64);
    conn.client
        .upload_with_progress(&local, "/x.part", ResumeAt::Start, |_, _, _| {})
        .await
        .expect_err("one rejected write should abort the round");

    let landed = server.len("/x.part");
    assert!(landed < total, "本轮确实没能传完（{landed} / {total}）");
    assert_eq!(
        read_remote(&server, "/x.part"),
        payload(landed),
        "磁盘上的长度必须等价于一段可信连续前缀，不得有全零空洞"
    );

    // 按磁盘长度续传，最终结果必须与源端逐字节一致。
    server.reset_faults();
    conn.client
        .upload_with_progress(
            &local,
            "/x.part",
            ResumeAt::Offset(landed as u64),
            |_, _, _| {},
        )
        .await
        .expect("the next round resumes from the trimmed prefix");
    assert_eq!(read_remote(&server, "/x.part"), payload(total));
}

/// 上传的第一次落盘动作只能碰暂存：连暂存都打不开时，用户原有的远端同名文件必须分毫无动。
#[tokio::test]
async fn an_unopenable_staging_file_leaves_the_users_final_name_alone() {
    let sandbox = Sandbox::new("untouched");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;

    // 用户原有的远端文件：内容刻意与待上传文件不同（否则「没被动过」与「被前缀覆盖」分不清）。
    let existing = vec![0xABu8; 20 * KILO];
    std::fs::write(server.path("/big.bin"), &existing).expect("seed the remote file");

    server.fail_opens(1);
    let local = sandbox.local_file("big.bin", 500 * KILO);
    let err = conn
        .client
        .upload_with_progress(&local, "/big.bin.part", ResumeAt::Start, |_, _, _| {})
        .await
        .expect_err("the injected open failure should surface");
    assert!(
        matches!(
            err,
            CoreError::Sftp {
                kind: CoreErrorKind::CreateRemoteFile,
                ..
            }
        ),
        "起点为 0 时走 create，错误应带该语义：{err:?}"
    );
    assert_eq!(
        read_remote(&server, "/big.bin"),
        existing,
        "原有文件不得被截断或替换"
    );
    assert!(
        !server.path("/big.bin.part").exists(),
        "打不开暂存时不该留下暂存"
    );
}

fn read_remote(server: &TestSftp, name: &str) -> Vec<u8> {
    std::fs::read(server.path(name)).unwrap_or_default()
}
