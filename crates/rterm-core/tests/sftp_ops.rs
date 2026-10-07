//! SFTP 目录操作的集成测试：连到进程内的真实 SSH + SFTP 服务端（见 `support`），
//! 走的是与生产同一条协议栈，覆盖单元测试碰不到的部分——删除前的目录探测。

#[allow(dead_code)] // 只用到测试桩的一部分（目录操作），其余是传输测试的装备
mod support;

use rterm_core::{CoreError, CoreErrorKind};
use support::{Sandbox, TestSftp};

/// 空目录可以删除。
#[tokio::test]
async fn deleting_an_empty_directory_succeeds() {
    let sandbox = Sandbox::new("rmdir-empty");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;

    std::fs::create_dir(server.path("empty")).expect("create the remote dir");

    conn.client
        .remove_dir("/empty")
        .await
        .expect("an empty directory must be removable");

    assert!(!server.path("empty").exists(), "目录应从远端消失");
}

/// 非空目录必须报出语义化的「目录非空」，而非服务器笼统的 failure。
#[tokio::test]
async fn deleting_a_non_empty_directory_reports_it_as_not_empty() {
    let sandbox = Sandbox::new("rmdir-non-empty");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;

    std::fs::create_dir(server.path("docs")).expect("create the remote dir");
    std::fs::write(server.path("docs/note.txt"), b"keep me").expect("seed a file");

    let err = conn
        .client
        .remove_dir("/docs")
        .await
        .expect_err("a non-empty directory must not be removed");
    assert!(
        matches!(
            &err,
            CoreError::Sftp {
                kind: CoreErrorKind::DirNotEmpty,
                ..
            }
        ),
        "错误必须是语义化的 DirNotEmpty，实际 {err}"
    );
    assert!(
        server.path("docs/note.txt").exists(),
        "删除失败后目录内容必须原样保留"
    );
}
