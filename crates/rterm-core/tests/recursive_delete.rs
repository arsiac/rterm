//! 递归删除的集成测试：连到进程内的真实 SSH + SFTP 服务端（见 `support`），
//! 删除落在真实文件系统上——嵌套顺序、部分失败与前置守卫都走生产同一条协议栈。

#[allow(dead_code)] // 只用到测试桩的目录侧装备，故障注入与传输件属其它测试
mod support;

use rterm_core::{CoreError, CoreErrorKind};
use support::{Sandbox, TestSftp};

/// 嵌套树全删：报告计数如实，目录整棵消失。
#[tokio::test]
async fn removes_a_nested_tree_and_reports_counts() {
    let sandbox = Sandbox::new("rm-tree");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;
    std::fs::create_dir_all(sandbox.remote.join("a/b/c")).unwrap();
    std::fs::write(sandbox.remote.join("a/x.txt"), b"1").unwrap();
    std::fs::write(sandbox.remote.join("a/b/y.txt"), b"2").unwrap();
    std::fs::write(sandbox.remote.join("a/b/c/z.txt"), b"3").unwrap();

    let report = conn
        .client
        .remove_dir_recursive("/a")
        .await
        .expect("recursive removal should succeed");

    assert_eq!(report.files, 3, "三个文件都应计入");
    assert_eq!(report.dirs, 3, "a、a/b、a/b/c 三层目录都应删除");
    assert!(report.errors.is_empty(), "不应有失败项");
    assert!(!sandbox.remote.join("a").exists(), "根目录本身也必须被删掉");
}

/// 守卫：空路径与 jail 根在任何删除动作**之前**被拒绝，磁盘零破坏。
#[tokio::test]
async fn refuses_root_and_empty_before_deleting_anything() {
    let sandbox = Sandbox::new("rm-guard");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;
    std::fs::write(sandbox.remote.join("keep.txt"), b"x").unwrap();

    for bad in ["", "/", "//"] {
        assert!(
            conn.client.remove_dir_recursive(bad).await.is_err(),
            "{bad:?} 必须被守卫拒绝"
        );
    }
    assert!(
        sandbox.remote.join("keep.txt").exists(),
        "守卫拒绝不得顺带删掉任何内容"
    );
}

/// 深度守卫：嵌套超限报 `DirTooDeep`，且发生在游树阶段——一个字节都没删。
#[tokio::test]
async fn refuses_excessively_deep_trees_without_deleting() {
    let sandbox = Sandbox::new("rm-deep");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;
    let mut deep = sandbox.remote.join("d");
    for _ in 0..70 {
        deep = deep.join("x");
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(sandbox.remote.join("d/top.txt"), b"keep me").unwrap();

    let err = conn
        .client
        .remove_dir_recursive("/d")
        .await
        .expect_err("超深必须报错");
    assert!(
        matches!(
            err,
            CoreError::Sftp {
                kind: CoreErrorKind::DirTooDeep,
                ..
            }
        ),
        "应报 DirTooDeep，实际 {err:?}"
    );
    assert!(
        sandbox.remote.join("d/top.txt").exists(),
        "游树阶段就中止：树内不得有任何损失"
    );
}

/// 尽力而为的部分失败：只读目录里的文件删不动，其余照常删完；留存项的父目录自然删不掉。
#[cfg(unix)]
#[tokio::test]
async fn keeps_going_past_a_locked_directory() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new("rm-partial");
    let server = TestSftp::start(sandbox.remote.clone()).await;
    let conn = server.client().await;
    std::fs::create_dir_all(sandbox.remote.join("t/locked")).unwrap();
    std::fs::write(sandbox.remote.join("t/free.txt"), b"1").unwrap();
    std::fs::write(sandbox.remote.join("t/locked/f.txt"), b"2").unwrap();
    std::fs::set_permissions(
        sandbox.remote.join("t/locked"),
        std::fs::Permissions::from_mode(0o555),
    )
    .unwrap();

    let report = conn
        .client
        .remove_dir_recursive("/t")
        .await
        // 尽力而为不是失败：调用拿到的是报告，失败清单在报告里。
        .expect("partial failure must surface as a report, not an error");
    // 立刻恢复可写，否则沙箱清理与后续断言都会被权限挡住。
    std::fs::set_permissions(
        sandbox.remote.join("t/locked"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    assert_eq!(report.files, 1, "删得动的文件必须照常删掉");
    assert!(
        report.errors.iter().any(|(p, _)| p.ends_with("f.txt")),
        "锁住目录里的文件应记为失败项"
    );
    assert!(
        !report.errors.iter().any(|(p, _)| p.ends_with("free.txt")),
        "已删除的条目不得重复出现在失败清单"
    );
    assert!(sandbox.remote.join("t/locked/f.txt").exists());
    assert!(
        sandbox.remote.join("t").exists(),
        "有留存项时父目录删不掉，停在可补救态"
    );
    assert!(
        !sandbox.remote.join("t/free.txt").exists(),
        "自由文件应已消失"
    );
}
