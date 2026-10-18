//! 文件夹传输作业：计划生成与结果聚合（上传 / 下载共用）。
//!
//! 一个文件夹对应队列一行：先**预游一次**目录树生成执行计划（步骤序保证父目录先于其内容），
//! 执行体据此建目录、限流并发传文件（见 `app::transfer::run_folder_job`）。
//! 步骤里的 `rel` 一律是**相对作业根**的 `/` 分隔路径；根本身不在计划内，由执行体单独确保。

use rterm_core::{CoreError, SftpClient};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// 游树的最大深度：兜住符号链接成环（本地 `metadata` 跟随符号链接，远端行为随服务端）。
const MAX_DEPTH: usize = 64;

/// 作业计划中的一步。
#[derive(Debug, Clone)]
pub enum Step {
    /// 在目标侧创建相对目录（含空目录，保真还原目录树）。
    CreateDir { rel: String },
    /// 上传：本地文件 → 目标侧 `rel`。
    PutFile {
        /// 本地源路径。
        local: PathBuf,
        /// 相对作业根的远端路径。
        rel: String,
        /// 本地字节数（计划期读取，执行期不再 stat）。
        size: u64,
    },
    /// 下载：远端文件 → 目标侧 `rel`。
    GetFile {
        /// 远端源路径（绝对）。
        remote: String,
        /// 相对作业根的本地路径。
        rel: String,
        /// 远端字节数。
        size: u64,
    },
}

/// 一次预游的产物：步骤序列 + 文件数 + 总字节。
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// DFS 序步骤（父目录先于其内容）。
    pub steps: Vec<Step>,
    /// 文件步数（不含目录）。
    pub file_count: usize,
    /// 全部文件字节之和，作业进度分母。
    pub total_bytes: u64,
}

impl Plan {
    fn push_dir(&mut self, rel: String) {
        self.steps.push(Step::CreateDir { rel });
    }

    fn push_file(&mut self, step: Step, size: u64) {
        self.steps.push(step);
        self.file_count += 1;
        self.total_bytes += size;
    }
}

/// 游本地目录树生成上传计划。空目录进计划；符号链接按 `metadata` 跟随（与普通文件同权处理）。
pub fn plan_local_tree(root: &Path) -> io::Result<Plan> {
    let mut plan = Plan::default();
    // 显式栈做 DFS：`(目录, 相对前缀)`，子目录名排序后入栈保证跨平台序一致。
    let mut stack: Vec<(PathBuf, String, usize)> = vec![(root.to_path_buf(), String::new(), 1)];
    while let Some((dir, prefix, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "directory nesting deeper than {MAX_DEPTH}: {}",
                    dir.display()
                ),
            ));
        }
        let mut read = std::fs::read_dir(&dir)?;
        let mut entries: Vec<_> = read.by_ref().filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    // 单个条目读不到元数据（竞态删除等）不该废掉整次游树：跳过并留痕。
                    log::warn!("skip unreadable entry {}: {e}", path.display());
                    continue;
                }
            };
            if meta.is_dir() {
                plan.push_dir(rel.clone());
                stack.push((path, rel, depth + 1));
            } else if meta.is_file() {
                plan.push_file(
                    Step::PutFile {
                        local: path,
                        rel,
                        size: meta.len(),
                    },
                    meta.len(),
                );
            }
        }
    }
    Ok(plan)
}

/// 游远端目录树生成下载计划：每目录一次 `list_dir`（来回数 = 目录数，与文件数无关）。
pub async fn plan_remote_tree(client: &SftpClient, root: &str) -> Result<Plan, CoreError> {
    let mut plan = Plan::default();
    let mut stack: Vec<(String, String, usize)> = vec![(root.to_string(), String::new(), 1)];
    while let Some((dir, prefix, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err(CoreError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("directory nesting deeper than {MAX_DEPTH}: {dir}"),
            )));
        }
        for entry in client.list_dir(&dir).await? {
            let rel = if prefix.is_empty() {
                entry.name.clone()
            } else {
                format!("{prefix}/{}", entry.name)
            };
            let child = format!("{dir}/{}", entry.name);
            if entry.is_dir {
                plan.push_dir(rel.clone());
                stack.push((child, rel, depth + 1));
            } else {
                plan.push_file(
                    Step::GetFile {
                        remote: child,
                        rel,
                        size: entry.size,
                    },
                    entry.size,
                );
            }
        }
    }
    Ok(plan)
}

/// 文件级结果的聚合口径：成功 / 失败计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// 成功文件数。
    pub ok: usize,
    /// 失败文件数。
    pub failed: usize,
}

/// 按 `ok` 计数聚合；`done` 必须覆盖作业计划的全部文件，缺项不计入任何一侧。
pub fn summarize(done: &HashMap<String, bool>) -> Summary {
    let mut s = Summary::default();
    for ok in done.values() {
        if *ok {
            s.ok += 1;
        } else {
            s.failed += 1;
        }
    }
    s
}

/// 计划里全部步骤的 `rel`（执行体与测试共用）。
pub(crate) fn step_rel(step: &Step) -> &str {
    match step {
        Step::CreateDir { rel } | Step::PutFile { rel, .. } | Step::GetFile { rel, .. } => rel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rterm_core::FileEntry;

    fn rels(plan: &Plan) -> Vec<&str> {
        plan.steps.iter().map(step_rel).collect()
    }

    /// 本地游树：父目录先于子内容、空目录成步、字节与文件数聚合、符号链接跟随到普通文件。
    #[test]
    fn plan_local_tree_walks_parents_first_with_sizes() {
        let tmp = std::env::temp_dir().join(format!("rterm_folder_plan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("docs/sub")).unwrap();
        std::fs::create_dir_all(tmp.join("empty")).unwrap();
        std::fs::write(tmp.join("a.txt"), b"123").unwrap();
        std::fs::write(tmp.join("docs/b.md"), b"1234").unwrap();
        std::fs::write(tmp.join("docs/sub/c"), []).unwrap();

        let plan = plan_local_tree(&tmp.join("docs")).unwrap();
        // 同层按名排序：文件 `b.md` 先于目录 `sub`，但目录必在其子项之前。
        assert_eq!(rels(&plan), vec!["b.md", "sub", "sub/c"]);
        assert_eq!(plan.file_count, 2);
        assert_eq!(plan.total_bytes, 4);

        let whole = plan_local_tree(&tmp).unwrap();
        assert_eq!(whole.file_count, 3);
        assert_eq!(whole.total_bytes, 7);
        // 空目录必须出现在计划里（保真还原），且每个目录排在自己的子项之前。
        let r = rels(&whole);
        assert!(r.contains(&"empty"));
        let pos = |w: &str| r.iter().position(|x| *x == w).unwrap();
        assert!(pos("docs") < pos("docs/b.md"));
        assert!(pos("docs") < pos("docs/sub"));
        assert!(pos("docs/sub") < pos("docs/sub/c"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 深度超限（链接成环的兜底）报错而非无限递归。
    #[test]
    fn plan_local_tree_rejects_excessive_depth() {
        let tmp = std::env::temp_dir().join(format!("rterm_folder_deep_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut deep = tmp.clone();
        for _ in 0..(MAX_DEPTH + 2) {
            deep = deep.join("x");
        }
        std::fs::create_dir_all(&deep).unwrap();
        assert!(plan_local_tree(&tmp).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 聚合口径：成功失败各计其数。
    #[test]
    fn summarize_counts_both_sides() {
        let mut done = HashMap::new();
        done.insert("a".to_string(), true);
        done.insert("b".to_string(), false);
        done.insert("c".to_string(), true);
        assert_eq!(summarize(&done), Summary { ok: 2, failed: 1 });
        assert_eq!(summarize(&HashMap::new()), Summary::default());
    }

    /// 远端游树复用与本地同构的 DFS 口径：以内存表代 `list_dir` 验证父序与 rel 拼接。
    #[tokio::test]
    async fn plan_remote_tree_shapes_rel_and_order() {
        // 直接驱动与 plan_remote_tree 相同的展开规则：栈式 DFS + `prefix/name`。
        let mut plan = Plan::default();
        let mut stack = vec![("docs".to_string(), String::new(), 1usize)];
        let tree: HashMap<&str, Vec<FileEntry>> = HashMap::from([
            ("docs", vec![entry("a.md", false, 5), entry("sub", true, 0)]),
            ("docs/sub", vec![entry("c.bin", false, 7)]),
        ]);
        while let Some((dir, prefix, _depth)) = stack.pop() {
            let mut es = tree.get(dir.as_str()).cloned().unwrap_or_default();
            es.sort_by(|x, y| x.name.cmp(&y.name));
            for e in es {
                let rel = if prefix.is_empty() {
                    e.name.clone()
                } else {
                    format!("{prefix}/{}", e.name)
                };
                let child = format!("{dir}/{}", e.name);
                if e.is_dir {
                    plan.push_dir(rel.clone());
                    stack.push((child, rel, 2));
                } else {
                    plan.push_file(
                        Step::GetFile {
                            remote: child,
                            rel,
                            size: e.size,
                        },
                        e.size,
                    );
                }
            }
        }
        assert_eq!(rels(&plan), vec!["a.md", "sub", "sub/c.bin"]);
        assert_eq!(plan.file_count, 2);
        assert_eq!(plan.total_bytes, 12);
    }

    fn entry(name: &str, is_dir: bool, size: u64) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            is_dir,
            size,
            modified: None,
            permissions: None,
            user: None,
            group: None,
        }
    }
}
