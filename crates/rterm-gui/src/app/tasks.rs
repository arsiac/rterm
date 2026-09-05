//! 异步任务

use crate::app::hostkey;
use crate::app::tabs;
use crate::i18n::localize_error;
use crate::message::{Message, ResizeSender};
use crate::t;
use futures::SinkExt;
use rterm_config::SessionConfig;
use rterm_core::{CoreError, FileEntry, SessionSecrets, SftpClient, SshConnection};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

/// 建立 SSH 连接的流任务。
///
/// 握手遇到未知 / 变更主机密钥时，把弹窗消息转发给 GUI 并原地等待用户决定，
/// 最终结果（无论成败）经 `SessionConnected` 回流。超时分段计量：弹窗等待
/// 不计入，用户答复后重新计满；`timeout` 为 0 表示不限制。
pub(crate) async fn connect_stream_task(
    tab_id: u64,
    id: String,
    config: SessionConfig,
    secrets: SessionSecrets,
    timeout: u64,
    output: &mut futures::channel::mpsc::Sender<Message>,
) {
    let (prompt_tx, mut prompt_rx) = mpsc::channel(1);
    // 握手跑在独立任务：超时 / 放弃时可 abort 彻底清理——russh 的 Handle::drop
    // 不会中止会话任务，仅靠丢弃句柄会泄漏悬挂的握手。
    let mut connect = tokio::spawn(async move {
        SshConnection::connect(&config, &secrets, prompt_tx)
            .await
            .map(Arc::new)
    });
    let mut deadline = connect_deadline(timeout);
    loop {
        tokio::select! {
            res = &mut connect => {
                let result = match res {
                    Ok(r) => r.map_err(|e| localize_error(&e)),
                    Err(e) => Err(t!("app.connect_task_error", err => e)),
                };
                // 记录连接最终结果：成功 / 失败（含超时）均落日志，便于排查连接问题。
                match &result {
                    Ok(_) => log::info!("Connected: session {id} (tab {tab_id})"),
                    Err(e) => log::error!("Connection failed: session {id} (tab {tab_id}): {e}"),
                }
                let _ = output
                    .send(Message::Tabs(tabs::Message::SessionConnected(
                        tab_id, id, result,
                    )))
                    .await;
                return;
            }
            prompt = prompt_rx.recv() => if let Some((prompt, reply)) = prompt {
                let _ = output
                    .send(Message::HostKey(hostkey::Message::Prompt(
                        tab_id,
                        prompt,
                        reply.clone(),
                    )))
                    .await;
                // 阻塞等用户点「接受 / 拒绝」，此期间 select 不会轮询 `deadline`。
                // sleep 记的是绝对时刻，故等待多久都会让 deadline 过期，下面必须重置。
                let _ = reply.decided().await;
                // 用户已答复，重新计满剩余流程（认证）的超时。
                deadline = connect_deadline(timeout);
                // 注：`prompt_tx` 随连接任务终止而关闭后，`recv()` 会立刻反复返回 `None`，
                // 随后的 if-let 不做事、直接回到 select——此时靠 `connect` 分支收尾。
            },
            // 整段握手（含等待主机密钥确认）超时：中止任务并回报超时错误。
            _ = &mut deadline => {
                connect.abort();
                log::error!("Connection timeout: session {id} (tab {tab_id})");
                let _ = output
                    .send(Message::Tabs(tabs::Message::SessionConnected(
                        tab_id,
                        id,
                        Err(t!("app.connect_timeout")),
                    )))
                    .await;
                return;
            }
        }
    }
}

/// 连接阶段超时的可重置 future；`timeout` 为 0 时返回永不完成的占位。
pub(crate) fn connect_deadline(timeout: u64) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    match timeout {
        0 => Box::pin(std::future::pending()),
        secs => Box::pin(sleep(Duration::from_secs(secs))),
    }
}

/// 创建终端桥接（进程内 OUT/IN 双管道 + 打开 shell 通道），返回 conout 读端、conin 写端、
/// 断开标志与尺寸发送端。
///
/// 桥接任务结束时核心层会置位 `disconnect` 标志；此处另起一个轻量 watcher 轮询该标志，
/// 一旦翻转即经 `disconnect_tx` 按标签通知 GUI（`Message::TerminalDisconnected`），
/// 最终由 `tabs::State::terminal_disconnected` 把该标签状态置为 `Error`。
pub(crate) async fn open_terminal_task(
    conn: Arc<SshConnection>,
    cols: u32,
    rows: u32,
    disconnect_tx: mpsc::Sender<()>,
    cwd: crate::state::TerminalTabCwd,
    cwd_bootstrap: bool,
) -> Result<
    (
        std::sync::Arc<std::fs::File>,
        std::sync::Arc<std::fs::File>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        ResizeSender,
    ),
    CoreError,
> {
    let (conout, conin, disconnect, resize_tx) =
        rterm_core::spawn_terminal_bridge(&conn, cols, rows, cwd, cwd_bootstrap).await?;

    let disc = disconnect.clone();
    let watcher_tx = disconnect_tx;
    tokio::spawn(async move {
        loop {
            if disc.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = watcher_tx.send(()).await;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    });

    Ok((
        std::sync::Arc::new(conout),
        std::sync::Arc::new(conin),
        disconnect,
        resize_tx,
    ))
}

/// 建立 SFTP 通道并返回客户端。
pub(crate) async fn open_sftp_task(conn: Arc<SshConnection>) -> Result<Arc<SftpClient>, CoreError> {
    let session = conn.open_sftp().await?;
    Ok(Arc::new(SftpClient::new(session)))
}

/// 解析远端路径为绝对路径，并列举该目录（两步合并，避免 UI 直接持有相对路径）。
pub(crate) async fn resolve_and_list_task(
    client: Arc<SftpClient>,
    path: String,
) -> Result<(String, Vec<FileEntry>), CoreError> {
    let abs = client.resolve_path(&path).await?;
    let entries = client.list_dir(&abs).await?;
    Ok((abs, entries))
}

// ===================== 小工具 =====================

/// 拼接远端路径：处理 "." 与 ".." 及分隔符。
pub(crate) fn join_path(base: &str, name: &str) -> String {
    if name == ".." {
        return parent_path(base);
    }
    if base == "." || base.is_empty() {
        return name.to_string();
    }
    // 根目录 "/" 直接拼接子项，保留前导分隔符，避免丢失路径前缀。
    if base == "/" {
        return format!("/{name}");
    }
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// 取远端路径的上级目录。
pub(crate) fn parent_path(path: &str) -> String {
    if path == "." || path.is_empty() {
        return ".".to_string();
    }
    // 文件系统根目录 "/" 没有上级，原样返回，避免出现无意义的 ".."。
    if path == "/" {
        return "/".to_string();
    }
    let trimmed = path.trim_end_matches('/');
    // 形如 "/" 或 "//" 等仅含分隔符的路径，其上级仍是根目录。
    if trimmed.is_empty() {
        return "/".to_string();
    }
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
        None => ".".to_string(),
    }
}

/// 把本地路径（文件或目录）展开为可上传项列表，保留目录层级。
///
/// - 传入文件：返回单一项，远端相对路径即文件名（平铺到远端当前目录）。
/// - 传入目录：递归收集其下所有文件，远端相对路径以该目录名为顶层前缀
///   （如拖入 `foo/`，内部 `foo/a/b.txt` 对应远端相对路径 `foo/a/b.txt`），
///   从而还原本地目录树；目录本身不计入，由传输模块在执行时按需创建远端父目录。
///
/// 返回 `(本地绝对路径, 远端相对路径)` 列表，供 `transfer::Message::Upload` 使用。
pub(crate) fn collect_upload_items(root: &Path) -> Vec<(PathBuf, String)> {
    let mut items = Vec::new();
    let Ok(meta) = std::fs::metadata(root) else {
        return items;
    };
    if meta.is_file() {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        items.push((root.to_path_buf(), name));
        return items;
    }
    // 目录：以目录自身名称为顶层前缀，递归收集子项。每个子项的 `rel_prefix` 需是包含其自身
    // 名称的完整相对路径，故在此拼好 `base/子项名` 再下传（递归内文件分支直接采用该前缀）。
    let base = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Ok(read) = std::fs::read_dir(root) {
        for entry in read.flatten() {
            let child = entry.path();
            let child_name = child
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let child_rel = if base.is_empty() {
                child_name.clone()
            } else {
                format!("{base}/{child_name}")
            };
            collect_upload_items_recursive(&child, &child_rel, &mut items);
        }
    }
    items
}

/// `collect_upload_items` 的递归 worker：把 `path` 下所有文件收集进 `items`，
/// 每个文件的远端相对路径以 `rel_prefix` 为前缀（目录层级由此还原）。
fn collect_upload_items_recursive(
    path: &Path,
    rel_prefix: &str,
    items: &mut Vec<(PathBuf, String)>,
) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.is_file() {
        // `rel_prefix` 已是包含本文件名的完整远端相对路径，直接采用，避免再追加文件名造成重复。
        items.push((path.to_path_buf(), rel_prefix.to_string()));
        return;
    }
    if meta.is_dir()
        && let Ok(read) = std::fs::read_dir(path)
    {
        for entry in read.flatten() {
            let child = entry.path();
            let child_name = child
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let child_rel = if rel_prefix.is_empty() {
                child_name.clone()
            } else {
                format!("{rel_prefix}/{child_name}")
            };
            collect_upload_items_recursive(&child, &child_rel, items);
        }
    }
}

/// 递归确保远端目录存在（文件夹上传时还原本地目录树所需）。
///
/// 先算出从最外层到 `dir` 的各级目录链（如 `["/", "/a", "/a/b"]`），再逐级创建缺失的目录；
/// 已存在（`list_dir` 成功）或创建失败但实已存在（并发 / 竞态）的视为 OK，避免误报错。
/// 采用迭代而非 async 递归，规避「递归 async fn 需 Box 间接」的编译限制。
pub(crate) async fn ensure_remote_dir(client: &SftpClient, dir: &str) -> Result<(), CoreError> {
    // 收集 dir 及其全部祖先目录（从最内层到最外层）。
    let mut chain = Vec::new();
    let mut cur = dir.to_string();
    loop {
        chain.push(cur.clone());
        let parent = parent_path(&cur);
        if parent.is_empty() || parent == cur {
            break;
        }
        cur = parent;
    }
    chain.reverse(); // 反转后最外层在前，确保先建父目录再建子目录。
    for d in chain {
        // 已存在则跳过；否则创建，创建失败但实已存在（竞态）也跳过。
        if client.list_dir(&d).await.is_ok() {
            continue;
        }
        if let Err(e) = client.create_dir(&d).await {
            if client.list_dir(&d).await.is_ok() {
                continue;
            }
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn collect_upload_items_flattens_preserving_hierarchy() {
        let tmp = std::env::temp_dir().join(format!("rterm_up_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("foo/bar")).unwrap();
        fs::write(tmp.join("foo/a.txt"), b"a").unwrap();
        fs::write(tmp.join("foo/bar/b.txt"), b"b").unwrap();

        // 单文件：远端相对路径即文件名（平铺）。
        let items = collect_upload_items(&tmp.join("foo/a.txt"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].1, "a.txt");

        // 目录：顶层以目录名 "foo" 为前缀，保留内部层级。
        let mut rels: Vec<String> = collect_upload_items(&tmp.join("foo"))
            .into_iter()
            .map(|i| i.1)
            .collect();
        rels.sort();
        assert_eq!(
            rels,
            vec!["foo/a.txt".to_string(), "foo/bar/b.txt".to_string()]
        );

        let _ = fs::remove_dir_all(&tmp);
    }
}
