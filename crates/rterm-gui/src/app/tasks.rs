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
                    Ok(_) => log::info!("连接成功: 会话 {id} (标签 {tab_id})"),
                    Err(e) => log::error!("连接失败: 会话 {id} (标签 {tab_id}): {e}"),
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
                log::error!("连接超时: 会话 {id} (标签 {tab_id})");
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
        rterm_core::spawn_terminal_bridge(&conn, cols, rows).await?;

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
