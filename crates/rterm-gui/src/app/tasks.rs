//! 异步任务

use crate::app::hostkey;
use crate::app::tabs;
use crate::i18n::localize_error;
use crate::message::{Message, ResizeSender};
use crate::state::{ConflictDecision, ConflictRow};
use crate::t;
use futures::SinkExt;
use rterm_config::{ConnectOptions, SessionConfig};
use rterm_core::{CoreError, FileEntry, HopSpec, SessionSecrets, SftpClient, SshConnection};
use std::collections::HashMap;
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
/// 不计入，用户答复后重新计满。`opts` 是本次建连的配置快照（超时、保活间隔与保活判死
/// 次数），三者只在建连时生效，改动配置需重连才见效。
pub(crate) async fn connect_stream_task(
    tab_id: u64,
    id: String,
    config: SessionConfig,
    secrets: SessionSecrets,
    jumps: Vec<HopSpec>,
    opts: ConnectOptions,
    output: &mut futures::channel::mpsc::Sender<Message>,
) {
    let (prompt_tx, mut prompt_rx) = mpsc::channel(1);
    // 握手跑在独立任务：超时 / 放弃时可 abort 彻底清理——russh 的 Handle::drop
    // 不会中止会话任务，仅靠丢弃句柄会泄漏悬挂的握手。
    let mut connect = tokio::spawn(async move {
        SshConnection::connect(
            &config,
            &secrets,
            &jumps,
            opts.keepalive_interval,
            opts.keepalive_max,
            prompt_tx,
        )
        .await
        .map(Arc::new)
    });
    let mut deadline = connect_deadline(opts.timeout);
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
                deadline = connect_deadline(opts.timeout);
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

/// 创建终端桥接（进程内 OUT/IN 双管道 + 打开 shell 通道），返回管道两端、桥接结束状态与尺寸发送端。
///
/// `size` / `input` / `inject_out` 依次为初始终端尺寸、首段注入输入与 pump 启动前写入 OUT 管道的
/// 显示字节；桥接结束时置位 `state.finished` 并经 `disconnect_tx` 按标签通知 GUI 置 `Error`。
pub(crate) async fn open_terminal_task(
    conn: Arc<SshConnection>,
    size: rterm_core::PtySize,
    input: rterm_core::BridgeInput,
    inject_out: Option<Vec<u8>>,
    disconnect_tx: mpsc::Sender<rterm_core::DisconnectReason>,
) -> Result<
    (
        std::sync::Arc<std::fs::File>,
        std::sync::Arc<std::fs::File>,
        Arc<rterm_core::BridgeState>,
        ResizeSender,
    ),
    CoreError,
> {
    let (conout, conin, state, resize_tx) =
        rterm_core::spawn_terminal_bridge(conn, size, input, inject_out.as_deref()).await?;

    let polled = state.clone();
    let watcher_tx = disconnect_tx;
    tokio::spawn(async move {
        loop {
            if polled.is_finished() {
                let _ = watcher_tx.send(polled.reason()).await;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    });

    Ok((
        std::sync::Arc::new(conout),
        std::sync::Arc::new(conin),
        state,
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
///   （如选择 `foo/` 文件夹，内部 `foo/a/b.txt` 对应远端相对路径 `foo/a/b.txt`），
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

/// 单个路径段的可用判据：非空、不是 `.` / `..`、且不含 `\`。
/// Windows 的路径语义把 `\` 当分隔符（Windows OpenSSH 上建 `a\b` 直接报 `NoSuchFile`），
/// POSIX 却把它当合法字面字符——同一份输入两种结果，一律拒绝比猜服务器属于哪一派可靠。
pub(crate) fn valid_name_component(part: &str) -> bool {
    !part.is_empty() && part != "." && part != ".." && !part.contains('\\')
}

/// 重命名只接受当前目录下的单个名字：带 `/` 会把条目搬出当前目录，不是重命名的语义。
pub(crate) fn valid_rename_name(name: &str) -> bool {
    !name.contains('/') && valid_name_component(name)
}

/// 把「新建文件夹」输入换算成要建的两个路径：需逐级补齐的父目录、待创建的末级目录。
/// 名字允许 `logs` 也允许 `logs/2026/10`，一律相对当前目录；`.` / `..` 会跳出当前目录，
/// 以 `/` 开头会切出空段，非法段一律返回 `None`。
pub(crate) fn new_dir_targets(path: &str, name: &str) -> Option<(String, String)> {
    let mut parts = Vec::new();
    for part in name.trim().trim_end_matches('/').split('/') {
        let part = part.trim();
        if !valid_name_component(part) {
            return None;
        }
        parts.push(part);
    }
    let target = join_path(path, &parts.join("/"));
    Some((parent_path(&target), target))
}

/// 判定上传项与目标目录现有条目的同名冲突，返回 `(同名文件, 同名目录)`：不带层级的相对路径就是
/// 本层的名字，带层级的（文件夹上传）取第一段，按远端条目自身的类型归类。只比最终名，故 `X.part`
/// 这类暂存名与 `X` 永不相等，上一轮失败的半成品不会被当成冲突追问。
pub(crate) fn upload_conflicts(
    rels: &[&str],
    existing: &HashMap<String, bool>,
) -> (Vec<String>, Vec<String>) {
    let mut seen: Vec<&str> = Vec::new();
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for rel in rels {
        // 一个文件夹会展开成成百上千条相对路径，首层同名只问一次。
        let head = rel.split('/').next().unwrap_or(rel);
        if head.is_empty() || seen.contains(&head) {
            continue;
        }
        seen.push(head);
        match existing.get(head) {
            Some(true) => dirs.push(head.to_string()),
            Some(false) => files.push(head.to_string()),
            None => {}
        }
    }
    (files, dirs)
}

/// 为重名冲突生成 `x (1).ext` 式递增候选，跳过目录快照里已占用的名字。
///
/// 目录不拆扩展名（`logs.old (1)`）；文件在最后一个点前插入；点开头文件整体视为干，不造 ` (1).bashrc`。
pub(crate) fn rename_candidate(
    existing: &HashMap<String, bool>,
    name: &str,
    is_dir: bool,
) -> String {
    let (stem, ext) = match (is_dir, name.rfind('.')) {
        (false, Some(i)) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    let mut n = 1u32;
    loop {
        let candidate = format!("{stem} ({n}){ext}");
        if !existing.contains_key(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// 把探测出的同名文件与目录装配成对话框决策行，候选名按目录快照排重，初始决策一律覆盖。
pub(crate) fn build_conflict_rows(
    files: &[String],
    dirs: &[String],
    existing: &HashMap<String, bool>,
) -> Vec<ConflictRow> {
    let mut rows: Vec<ConflictRow> = files
        .iter()
        .map(|name| ConflictRow {
            name: name.clone(),
            is_dir: false,
            new_name: rename_candidate(existing, name, false),
            decision: ConflictDecision::Overwrite,
        })
        .collect();
    rows.extend(dirs.iter().map(|name| ConflictRow {
        name: name.clone(),
        is_dir: true,
        new_name: rename_candidate(existing, name, true),
        decision: ConflictDecision::Overwrite,
    }));
    rows
}

/// 按各行决策把上传项展开成最终入队项：跳过即剔除，重命名改写首层段、保留内部层级；
/// 未冲突的项原样保留。同一首层的所有层级项随该行的决策整组处置。
pub(crate) fn apply_upload_decisions(
    items: &[(std::path::PathBuf, String)],
    rows: &[ConflictRow],
) -> Vec<(std::path::PathBuf, String)> {
    items
        .iter()
        .filter_map(|(local, rel)| {
            let head = rel.split('/').next().unwrap_or(rel);
            let Some(row) = rows.iter().find(|r| r.name == head) else {
                return Some((local.clone(), rel.clone()));
            };
            match row.decision {
                ConflictDecision::Overwrite => Some((local.clone(), rel.clone())),
                ConflictDecision::Skip => None,
                ConflictDecision::Rename => {
                    let rest = &rel[head.len()..];
                    let new_rel = if rest.is_empty() {
                        row.new_name.clone()
                    } else {
                        format!("{}{rest}", row.new_name)
                    };
                    Some((local.clone(), new_rel))
                }
            }
        })
        .collect()
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

    /// 多级名字要拆成「父目录逐级补齐 + 末级单独创建」两个目标；非法写法一律拒绝。
    #[test]
    fn new_dir_targets_splits_nested_names_and_rejects_escapes() {
        assert_eq!(
            new_dir_targets("/srv", "logs/2026/10"),
            Some((
                "/srv/logs/2026".to_string(),
                "/srv/logs/2026/10".to_string()
            ))
        );
        assert_eq!(
            new_dir_targets("/", "logs"),
            Some(("/".to_string(), "/logs".to_string()))
        );
        // 首尾空白与结尾斜杠是输入噪音，不算非法。
        assert_eq!(
            new_dir_targets("/var/log", "  reports/ "),
            Some(("/var/log".to_string(), "/var/log/reports".to_string()))
        );
        for bad in [
            "",
            "   ",
            "/",
            "/abs",
            "a//b",
            "..",
            "../x",
            "a/../b",
            ".",
            r"a\b",
            r"logs\2026",
        ] {
            assert_eq!(new_dir_targets("/srv", bad), None, "应判非法: {bad:?}");
        }
    }

    /// 重命名只收单个名字：分隔符与点段都会把条目搬出当前目录。
    #[test]
    fn valid_rename_name_rejects_separators_and_dot_segments() {
        assert!(valid_rename_name("notes.txt"));
        assert!(valid_rename_name("我的 文档"));
        for bad in ["", ".", "..", "../x", "docs/a.txt", r"a\b"] {
            assert!(!valid_rename_name(bad), "应判非法: {bad:?}");
        }
    }

    /// 同名判定只看首层：文件命中文件、层级项按第一段命中目录，未命中则一概不问。
    #[test]
    fn upload_conflicts_checks_first_level_names() {
        let existing: HashMap<String, bool> = [
            ("a.txt".to_string(), false),
            ("docs".to_string(), true),
            ("data.bin.part".to_string(), false),
        ]
        .into_iter()
        .collect();
        let rels = ["a.txt", "docs/1.md", "docs/sub/2.md", "new.bin", "data.bin"];
        assert_eq!(
            upload_conflicts(&rels, &existing),
            (vec!["a.txt".to_string()], vec!["docs".to_string()])
        );
        // 目录里没有同名（只有上一轮的半成品）→ 不问，直接入队。
        let empty: HashMap<String, bool> = HashMap::new();
        assert_eq!(upload_conflicts(&rels, &empty), (Vec::new(), Vec::new()));
    }

    /// 归类以远端条目的类型为准：本地传 `notes` 这个文件，远端同名的是目录，仍按目录报。
    #[test]
    fn upload_conflicts_classifies_by_remote_kind() {
        let existing: HashMap<String, bool> = [("notes".to_string(), true)].into_iter().collect();
        assert_eq!(
            upload_conflicts(&["notes"], &existing),
            (Vec::new(), vec!["notes".to_string()])
        );
    }

    /// 重命名候选按快照排重递增；目录与点开头面板文件不拆扩展名。
    #[test]
    fn rename_candidate_increments_past_taken_names() {
        let existing: HashMap<String, bool> = [
            ("notes.txt".to_string(), false),
            ("notes (1).txt".to_string(), false),
            (".bashrc".to_string(), false),
            ("docs".to_string(), true),
            ("logs.old".to_string(), true),
        ]
        .into_iter()
        .collect();
        // (1) 已被占，下一候选是 (2)。
        assert_eq!(
            rename_candidate(&existing, "notes.txt", false),
            "notes (2).txt"
        );
        // 点开头整体视为干：不造出 " (1).bashrc"。
        assert_eq!(rename_candidate(&existing, ".bashrc", false), ".bashrc (1)");
        // 目录带点不拆扩展名。
        assert_eq!(
            rename_candidate(&existing, "logs.old", true),
            "logs.old (1)"
        );
        assert_eq!(rename_candidate(&existing, "docs", true), "docs (1)");
    }

    /// 决策行按「文件在前、目录在后」装配，候选一律预生成，初始决策全为覆盖。
    #[test]
    fn build_conflict_rows_defaults_to_overwrite() {
        let existing: HashMap<String, bool> =
            [("a.txt".to_string(), false), ("docs".to_string(), true)]
                .into_iter()
                .collect();
        let rows = build_conflict_rows(&["a.txt".to_string()], &["docs".to_string()], &existing);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "a.txt");
        assert_eq!(rows[0].new_name, "a (1).txt");
        assert!(!rows[0].is_dir);
        assert_eq!(rows[1].name, "docs");
        assert_eq!(rows[1].new_name, "docs (1)");
        assert!(rows[1].is_dir);
        assert!(
            rows.iter()
                .all(|r| r.decision == ConflictDecision::Overwrite)
        );
    }

    /// 处置按首层整组展开：跳过剔全组、重命名只改写首层且保留层级，未冲突项原样。
    #[test]
    fn apply_upload_decisions_rewrites_head_and_drops_skips() {
        let items: Vec<(PathBuf, String)> = vec![
            ("/l/a.txt".into(), "a.txt".into()),
            ("/l/d1/x.md".into(), "docs/x.md".into()),
            ("/l/d2/sub/y.md".into(), "docs/sub/y.md".into()),
            ("/l/new.bin".into(), "new.bin".into()),
        ];
        let rows = vec![
            ConflictRow {
                name: "a.txt".to_string(),
                is_dir: false,
                new_name: "a (1).txt".to_string(),
                decision: ConflictDecision::Skip,
            },
            ConflictRow {
                name: "docs".to_string(),
                is_dir: true,
                new_name: "docs (1)".to_string(),
                decision: ConflictDecision::Rename,
            },
        ];
        let kept = apply_upload_decisions(&items, &rows);
        let rels: Vec<&str> = kept.iter().map(|(_, r)| r.as_str()).collect();
        assert_eq!(rels, vec!["docs (1)/x.md", "docs (1)/sub/y.md", "new.bin"]);
    }

    /// 覆盖决策下入队项必须原样不动：与未弹决策框时的直传行为逐项一致。
    #[test]
    fn apply_upload_decisions_overwrite_keeps_items_untouched() {
        let items: Vec<(PathBuf, String)> = vec![
            ("/l/a.txt".into(), "a.txt".into()),
            ("/l/d/x.md".into(), "docs/x.md".into()),
        ];
        let rows = vec![ConflictRow {
            name: "a.txt".to_string(),
            is_dir: false,
            new_name: "a (1).txt".to_string(),
            decision: ConflictDecision::Overwrite,
        }];
        assert_eq!(apply_upload_decisions(&items, &rows), items);
    }
}
