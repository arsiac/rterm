//! 远程文件「本地编辑」模块（右键 → 下载到编辑缓存 → 外部编辑器 → 轮询回传）
//!
//! # 流程
//!
//! ```text
//! Start(右键「本地编辑」)
//!   → Probed(远端指纹探测：大小上限 + 记录 remote_fp)
//!   → 缓存无副本：EditDownload 入队 → DownloadDone → Activated
//!   → 缓存已有副本：直接 Activated（续用，不重复下载）
//! Activated = 采基线水位 + spawn 编辑器 + 起轮询流
//!   → PollStat(每 2s 一次，两次稳定且异于基准)
//!   → 指纹比对：不一致 → RequestConflict(父层弹框) → 覆盖=回传 / 取消=接受外部改动
//!               一致   → RequestUploadConfirm(父层弹框) → 回传=入队 / 取消=暂不回传
//!   → EditUpload 入队 → UploadDone(成功=更新基准并重取远端指纹)
//! ```
//!
//! # 设计约束
//!
//! - **入口仅右键菜单**：双击保持「文件=选中」的既有肌肉记忆。
//! - **轮询式改动检测**：不等编辑器进程退出——GUI 编辑器 spawn 后立即返回，等进程永远等不到
//!   保存。代价是「写一半」的瞬间可能被观测到，故用**两次稳定**观测规避。
//! - **回传前用户确认**：指纹一致分支不静默入队，先弹确认框请用户拍板；冲突框的「覆盖」
//!   本身即明确授权，不叠加二次确认。「取消」＝暂不回传（基准对齐，再次保存重新询问）。
//! - **回传旁路覆盖确认**：同名探测只挂在选择器分支；远端被他人改动的风险由回传前的
//!   指纹比对单独拦截。
//! - **核层零改动**：下载 / 上传 / 指纹全部复用 `rterm-core` 既有接口。
//! - **缓存不自动清理**：未回传的副本可能是用户改动的唯一镜像；位置在发起时经 toast 告知。

use std::collections::HashMap;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use futures::SinkExt;
use iced::Task;
use log::info;
use rterm_core::{Fingerprint, SftpClient};

use crate::app::transfer::FailureKind;
use crate::i18n::localize_error;
use crate::sftp_panel;
use crate::state::ToastKind;
use crate::t;
use std::sync::Arc;

/// 编辑会话键：`(会话 id, 远端绝对路径)`。
///
/// 同键重复发起直接复用现有会话——编辑器已开着那份副本，重复下载会把尚未回传的
/// 编辑冲掉。按会话而非按标签设键：同一会话的多个标签看到的是同一份远端文件。
pub type EditKey = (String, String);

/// 本地副本的水位：`(修改时间, 大小)`。
///
/// 只做一次 `stat`、不读内容：轮询成本必须可忽略。局限：同一水位下内容被改写检测不到。
pub(crate) type LocalStat = (SystemTime, u64);

/// 轮询间隔：2s。两次稳定观测 ⇒ 最迟约 4s 检出保存。
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// 本地编辑的大小上限：32 MiB，不进配置。
///
/// 编辑场景绝大多数为文本，阈值只是误操作护栏；超限提示改用普通下载功能（那本来就能传大文件，且带断点续传）。
pub(crate) const MAX_EDIT_BYTES: u64 = 32 * 1024 * 1024;

/// 连续回传失败达到此次数即**暂停该版本**（停止每轮重传，重新保存才再次尝试）。
///
/// 传输队列内部已有自动重试，能撑过多数瞬时抖动；走到这里的失败多半是永久性的（权限、通道已死）。
pub(crate) const MAX_UPLOAD_FAILURES: u32 = 3;

/// 模块私有状态：编辑会话表 + 轮询流取消句柄登记表。
#[derive(Default)]
pub struct State {
    /// 进行中的编辑会话，键为 [`EditKey`]。
    sessions: HashMap<EditKey, EditSession>,
    /// 编辑键 → 轮询流的取消句柄：会话终止时 abort 旧流，避免孤儿 interval 任务。
    poll_handles: HashMap<EditKey, tokio::task::AbortHandle>,
}

/// 单个文件的编辑会话。
struct EditSession {
    /// 发起编辑的标签 id：回传入队与取 SFTP 客户端都按它。
    tab_id: u64,
    /// 远端绝对路径（发起时捕获，回传时原样使用，不随面板导航漂移）。
    remote_path: String,
    /// 本地编辑缓存中的副本路径（[`edit_cache_path`] 的确定性输出）。
    local_path: PathBuf,
    /// 上一次成功回传（或初始下载）时本地副本的水位。
    baseline: LocalStat,
    /// 下载当时记录的远端指纹；回传前与现取的值比对，防覆盖外部改动。
    remote_fp: Fingerprint,
    /// 上一轮轮询观测到的水位：两次稳定判定的中间量。
    seen: Option<LocalStat>,
    /// 回传是否在途（防队列重复入队）。
    uploading: bool,
    /// 触发本轮回传时的水位；成功回传后成为新基准（不用完成时水位：上传读的是触发那一刻的内容）。
    pending_stat: Option<LocalStat>,
    /// 连续回传失败次数（达到 [`MAX_UPLOAD_FAILURES`] 暂停该版本并清零）。
    failures: u32,
    /// 「无可用通道」提示是否已发过：恢复后重置，避免断线期间每轮刷 toast。
    no_channel_notified: bool,
}

/// 模块内部消息：UI 意图 + 模块自处理的异步结果。
///
/// 由父层经 `Message::RemoteEdit` 路由进来；模块 `update` 自行消费，不外泄。
#[derive(Clone, Debug)]
pub enum Message {
    /// 从 SFTP 右键菜单发起本地编辑（标签 id + 会话 id + 远端绝对路径 + 展示名）。
    Start {
        /// 发起编辑的标签 id。
        tab_id: u64,
        /// 会话 id（编辑键的一半）。
        session_id: String,
        /// 远端绝对路径（编辑键的另一半）。
        remote_path: String,
        /// 展示用的远端文件名（toast 与冲突框用）。
        name: String,
    },
    /// 远端探测完成：大小上限判定 + 记录指纹 + 缓存副本是否已存在。
    Probed {
        /// 编辑键。
        key: EditKey,
        /// 发起编辑的标签 id。
        tab_id: u64,
        /// 展示用的远端文件名。
        name: String,
        /// 本地副本路径（已由 [`edit_cache_path`] 算定）。
        local: PathBuf,
        /// 探测结果。
        result: ProbeOutcome,
    },
    /// 轮询观测回流（键 + 本地水位；`None` = 取不到元数据）。
    PollStat {
        /// 编辑键。
        key: EditKey,
        /// 本地副本水位。
        stat: Option<LocalStat>,
    },
    /// 轮询流的取消句柄已登记（键 + 句柄）。
    PollHandle {
        /// 编辑键。
        key: EditKey,
        /// 句柄：会话终止时 `abort()`。
        handle: tokio::task::AbortHandle,
    },
    /// 副本就绪后的收尾观测（键 + 本地水位；`None` = 副本不在了）。
    ///
    /// 「续用缓存」与「下载成功」两条路都汇到这里：采基线水位、spawn 编辑器、起轮询。
    Activated {
        /// 编辑键。
        key: EditKey,
        /// 本地副本水位。
        stat: Option<LocalStat>,
    },
    /// 编辑取回（下载到缓存）完成（键 + 成败）。
    DownloadDone {
        /// 编辑键。
        key: EditKey,
        /// 是否成功。
        ok: bool,
    },
    /// 编辑回传完成（键 + 失败分类；`None` = 成功）。
    UploadDone {
        /// 编辑键。
        key: EditKey,
        /// 失败分类；取消单独识别（用户主动取消 ⇒ 暂停该版本而非计数）。
        failure: Option<FailureKind>,
    },
    /// 指纹比对结论（键 + 结论）。
    ///
    /// 不带触发水位：Proceed 分支只用来请求弹确认框，回传水位取 `session.seen`（比结论产生时更新）。
    Verified {
        /// 编辑键。
        key: EditKey,
        /// 比对结论。
        outcome: VerifyOutcome,
    },
    /// 回传成功后重取的远端指纹（键 + 指纹；`None` = 没取到，保留旧值）。
    RemoteRefreshed {
        /// 编辑键。
        key: EditKey,
        /// 重取到的远端指纹。
        fp: Option<Fingerprint>,
    },
    /// 冲突框「覆盖」：按当前副本回传（键）。
    ConflictOverwrite {
        /// 编辑键。
        key: EditKey,
    },
    /// 冲突框「取消」：接受远端外部改动为新基准（键）。
    ConflictCancel {
        /// 编辑键。
        key: EditKey,
    },
    /// 冲突取消后的基准对齐完成（键 + 本地水位 + 刷新后的远端指纹）。
    ConflictResolved {
        /// 编辑键。
        key: EditKey,
        /// 对齐用的本地水位。
        local: Option<LocalStat>,
        /// 刷新后的远端指纹（`None` = 通道不在，保留旧指纹）。
        remote_fp: Option<Fingerprint>,
    },
    /// 回传确认框「回传」：用户拍板上传这一版（键）。
    ConfirmUpload {
        /// 编辑键。
        key: EditKey,
    },
    /// 回传确认框「取消」/ Esc：暂不回传这一版（键）。
    ///
    /// 基准异步对齐到当前水位；再次保存会重新 prompt。
    ConfirmDismiss {
        /// 编辑键。
        key: EditKey,
    },
    /// 暂不回传的基准对齐完成（键 + 对齐用的本地水位）。
    ///
    /// 异步重取水位而非直接用 `seen`：框开着期间用户可能又存了盘，用旧水位对齐会让新保存立刻重新 prompt。
    DismissResolved {
        /// 编辑键。
        key: EditKey,
        /// 对齐用的本地水位。
        stat: Option<LocalStat>,
    },
    /// 结束某标签的全部编辑会话（关标签 / 删会话，父层调用）。
    EndForTab(u64),
}

/// 模块上行事件：仅通知父层，父层收到后才修改父状态或转发给其它模块。
#[derive(Clone, Debug)]
pub enum Event {
    /// 请求父层弹出 toast 通知（携带类型与文案）。
    Toast(ToastKind, String),
    /// 请求父层把远端文件下载到编辑缓存（标签 + 键 + 本地路径 + 远端绝对路径）。
    StartEditDownload {
        /// 发起编辑的标签 id。
        tab_id: u64,
        /// 编辑键。
        key: EditKey,
        /// 本地副本路径。
        local: PathBuf,
        /// 远端绝对路径（原样使用，不按活动目录拼）。
        remote: String,
    },
    /// 请求父层把本地副本回传入传输队列（标签 + 键 + 本地路径 + 远端绝对路径）。
    StartEditUpload {
        /// 发起编辑的标签 id。
        tab_id: u64,
        /// 编辑键。
        key: EditKey,
        /// 本地副本路径。
        local: PathBuf,
        /// 远端绝对路径（原样使用）。
        remote: String,
    },
    /// 请求父层弹「远端已被外部改动」冲突框（标签 + 键 + 展示名）。
    RequestConflict {
        /// 发起编辑的标签 id（对话框落在它的 SFTP 视图上）。
        tab_id: u64,
        /// 编辑键。
        key: EditKey,
        /// 展示用的远端文件名。
        name: String,
    },
    /// 请求父层弹回传确认框（标签 + 键 + 展示名）：本地有修改、指纹无冲突，
    /// 回传前请用户拍板。父层槽位被占时跳过，下一轮轮询会自然重试。
    RequestUploadConfirm {
        /// 发起编辑的标签 id（对话框落在它的 SFTP 视图上）。
        tab_id: u64,
        /// 编辑键。
        key: EditKey,
        /// 展示用的远端文件名。
        name: String,
    },
    /// 自回路：把一条模块内部消息经父层派发回 `State::update`。
    Emit(Box<Message>),
}

/// 远端探测的结果分类。
#[derive(Clone, Debug)]
pub enum ProbeOutcome {
    /// 探测成功：远端指纹 + 缓存副本是否已存在。
    Ready {
        /// 下载当时的远端指纹。
        fp: Fingerprint,
        /// 缓存里是否已有该文件的副本（续用而不重复下载）。
        cache_exists: bool,
    },
    /// 超过大小上限（[`MAX_EDIT_BYTES`]）。
    TooLarge,
    /// 探测失败（已本地化的文案）。
    Failed(String),
}

/// 回传前指纹比对的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// 远端未被外部改动：放行回传。
    Proceed,
    /// 远端已被外部改动：弹冲突框请用户定夺。
    Conflict,
}

/// 两次稳定观测的裁决结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollVerdict {
    /// 与基准一致（或观测失败）：无需动作。
    Ignore,
    /// 与上一轮观测不同：记录本轮观测，继续等待稳定。
    Observe,
    /// 连续两轮稳定且异于基准：可以回传。
    Trigger,
}

/// 父层只读上下文：各标签**此刻**的 SFTP 客户端快照 + 外部编辑器命令。
///
/// 客户端每轮现取：会话重建后旧客户端绑在已死的连接上，缓存的那份会秒失败。
pub struct Ctx {
    /// 各标签当前的 SFTP 客户端（父层每轮经 `live_clients()` 重建）。
    pub clients: HashMap<u64, Arc<SftpClient>>,
    /// 外部编辑器命令（`None` / 空 = 系统默认程序）。
    pub editor: Option<String>,
}

impl State {
    /// 构造空状态。
    pub fn new() -> Self {
        Self::default()
    }

    /// 结束某标签的全部编辑会话：中止轮询流并移除会话。
    ///
    /// 关标签 / 删会话时由父层调用。编辑器可能还开着缓存副本，副本留在缓存里由用户手工取回。
    pub(crate) fn end_for_tab(&mut self, tab_id: u64) {
        let ended: Vec<EditKey> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.tab_id == tab_id)
            .map(|(k, _)| k.clone())
            .collect();
        for key in ended {
            if let Some(handle) = self.poll_handles.remove(&key) {
                handle.abort();
            }
            if let Some(session) = self.sessions.remove(&key) {
                info!(
                    "edit session ended (tab {tab_id}): {} (copy kept at {})",
                    session.remote_path,
                    session.local_path.display()
                );
            }
        }
    }

    /// 模块更新：只改自身 `State`；需要父层配合的事以 [`Event`] 经 `Task` 上行。
    pub fn update(&mut self, msg: Message, ctx: &Ctx) -> Task<Event> {
        match msg {
            Message::Start {
                tab_id,
                session_id,
                remote_path,
                name,
            } => self.start(tab_id, session_id, remote_path, name, ctx),
            Message::Probed {
                key,
                tab_id,
                name,
                local,
                result,
            } => self.probed(key, tab_id, name, local, result),
            Message::PollStat { key, stat } => self.poll_stat(key, stat, ctx),
            Message::PollHandle { key, handle } => {
                // 迟到的登记（会话已结束）直接 abort，不落进登记表。
                if self.sessions.contains_key(&key) {
                    self.poll_handles.insert(key, handle);
                } else {
                    handle.abort();
                }
                Task::none()
            }
            Message::Activated { key, stat } => self.activated(key, stat, ctx),
            Message::DownloadDone { key, ok } => self.download_done(key, ok, ctx),
            Message::UploadDone { key, failure } => self.upload_done(key, failure, ctx),
            Message::Verified { key, outcome } => self.verified(key, outcome),
            Message::RemoteRefreshed { key, fp } => {
                if let Some(fp) = fp
                    && let Some(session) = self.sessions.get_mut(&key)
                {
                    session.remote_fp = fp;
                }
                Task::none()
            }
            Message::ConflictOverwrite { key } => self.conflict_overwrite(key),
            Message::ConflictCancel { key } => self.conflict_cancel(key, ctx),
            Message::ConfirmUpload { key } => self.confirm_upload(key),
            Message::ConfirmDismiss { key } => self.confirm_dismiss(key),
            Message::DismissResolved { key, stat } => self.dismiss_resolved(key, stat),
            Message::ConflictResolved {
                key,
                local,
                remote_fp,
            } => {
                if let Some(session) = self.sessions.get_mut(&key) {
                    if let Some(stat) = local {
                        session.baseline = stat;
                        session.seen = None;
                    }
                    if let Some(fp) = remote_fp {
                        session.remote_fp = fp;
                    }
                    // 远端指纹没取到（通道不在）时保留旧值：下一轮保存会再弹一次冲突框，
                    // 比闷头覆盖安全。
                }
                Task::none()
            }
            Message::EndForTab(tab_id) => {
                self.end_for_tab(tab_id);
                Task::none()
            }
        }
    }

    /// 发起本地编辑：同键复用；否则探测远端（大小上限 + 指纹）。
    fn start(
        &mut self,
        tab_id: u64,
        session_id: String,
        remote_path: String,
        name: String,
        ctx: &Ctx,
    ) -> Task<Event> {
        let key: EditKey = (session_id, remote_path);
        if self.sessions.contains_key(&key) {
            // 复用：编辑器已开着那份副本。重复下载会把尚未回传的编辑冲掉。
            return Task::done(Event::Toast(
                ToastKind::Success,
                t!("remote_edit.already_editing", name => name),
            ));
        }
        let Some(client) = ctx.clients.get(&tab_id).cloned() else {
            return Task::done(Event::Toast(ToastKind::Error, t!("sftp.not_connected")));
        };
        let local = edit_cache_path(&key.0, &key.1);
        let local_probe = local.clone();
        // async 块只取 `key.1`（远端路径）会部分移动 `key`，回传消息要完整的键：
        // 预克隆一份 owned 键给闭包。
        let key_msg = key.clone();
        Task::perform(
            async move {
                match client.remote_fingerprint(&key.1).await {
                    // 上限判定按远端指纹的 size：下载前拦下，不浪费一次传输。
                    Ok(fp) if fp.len > MAX_EDIT_BYTES => ProbeOutcome::TooLarge,
                    Ok(fp) => ProbeOutcome::Ready {
                        fp,
                        cache_exists: local_probe.exists(),
                    },
                    Err(e) => ProbeOutcome::Failed(localize_error(&e)),
                }
            },
            move |result| {
                Event::Emit(Box::new(Message::Probed {
                    key: key_msg,
                    tab_id,
                    name,
                    local,
                    result,
                }))
            },
        )
    }

    /// 探测结论落地：超限 / 失败直接提示；就绪则建缓存目录、装会话，分流到
    /// 「续用缓存」或「下载」。
    fn probed(
        &mut self,
        key: EditKey,
        tab_id: u64,
        name: String,
        local: PathBuf,
        result: ProbeOutcome,
    ) -> Task<Event> {
        match result {
            ProbeOutcome::TooLarge => Task::done(Event::Toast(
                ToastKind::Error,
                t!(
                    "remote_edit.too_large",
                    name => name,
                    limit => sftp_panel::format_size(MAX_EDIT_BYTES)
                ),
            )),
            ProbeOutcome::Failed(e) => Task::done(Event::Toast(
                ToastKind::Error,
                t!("remote_edit.probe_failed", name => name, err => e),
            )),
            ProbeOutcome::Ready { fp, cache_exists } => {
                // 缓存目录先建：下载的 `.part` 暂存与最终副本都落在这里。
                if let Some(parent) = local.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    return Task::done(Event::Toast(
                        ToastKind::Error,
                        t!("remote_edit.cache_failed", err => e.to_string()),
                    ));
                }
                let session = EditSession {
                    tab_id,
                    remote_path: key.1.clone(),
                    local_path: local.clone(),
                    // 待 Activated 采到真水位；此前的轮询流尚未建立，不会被读到。
                    baseline: (SystemTime::UNIX_EPOCH, 0),
                    remote_fp: fp,
                    seen: None,
                    uploading: false,
                    pending_stat: None,
                    failures: 0,
                    no_channel_notified: false,
                };
                self.sessions.insert(key.clone(), session);
                if cache_exists {
                    // 缓存里已有该文件的副本（上次编辑的残留，含跨启动）：续用它。
                    self.activate(&key)
                } else {
                    Task::done(Event::StartEditDownload {
                        tab_id,
                        key: key.clone(),
                        local,
                        remote: key_remote(&key),
                    })
                }
            }
        }
    }

    /// 副本就绪的公共收尾：采基线水位（异步），随后 spawn 编辑器 + 起轮询流。
    fn activate(&mut self, key: &EditKey) -> Task<Event> {
        let Some(session) = self.sessions.get(key) else {
            return Task::none();
        };
        let local = session.local_path.clone();
        // 闭包须为 `'static`：预克隆 owned 键，不让方法借参溜进 Task。
        let key = key.clone();
        Task::perform(async move { local_stat(&local).await }, move |stat| {
            Event::Emit(Box::new(Message::Activated { key, stat }))
        })
    }

    /// 收尾观测落地：基线 + spawn 编辑器 + 轮询流。
    ///
    /// 副本不在了（被外部删掉）时回到下载路径重建——比把不存在的路径当基线一直轮询要诚实。
    fn activated(&mut self, key: EditKey, stat: Option<LocalStat>, ctx: &Ctx) -> Task<Event> {
        let Some(session) = self.sessions.get_mut(&key) else {
            return Task::none();
        };
        let Some(stat) = stat else {
            let (tab_id, local, remote) = (
                session.tab_id,
                session.local_path.clone(),
                session.remote_path.clone(),
            );
            return Task::done(Event::StartEditDownload {
                tab_id,
                key,
                local,
                remote,
            });
        };
        session.baseline = stat;
        let local = session.local_path.clone();
        let name = file_name_of(&session.remote_path);
        let mut tasks = Vec::new();
        // 编辑器拉不起来不终止会话：轮询继续，用户随时可以从缓存路径手动打开，
        // 之后的保存照样回传。
        match spawn_editor(ctx.editor.as_deref(), &local) {
            Ok(()) => tasks.push(Task::done(Event::Toast(
                ToastKind::Success,
                t!(
                    "remote_edit.started",
                    name => name,
                    path => local.display().to_string()
                ),
            ))),
            Err(e) => tasks.push(Task::done(Event::Toast(
                ToastKind::Error,
                t!(
                    "remote_edit.spawn_failed",
                    err => e,
                    path => local.display().to_string()
                ),
            ))),
        }
        tasks.push(poll_stream(key, local));
        Task::batch(tasks)
    }

    /// 下载完成：失败即清理会话；成功走公共收尾。
    fn download_done(&mut self, key: EditKey, ok: bool, ctx: &Ctx) -> Task<Event> {
        if !ok {
            self.forget(&key);
            return Task::done(Event::Toast(
                ToastKind::Error,
                t!("remote_edit.download_failed"),
            ));
        }
        let _ = ctx;
        self.activate(&key)
    }

    /// 轮询观测落地：两次稳定判定 → 指纹比对 → 回传确认框 / 冲突框。
    fn poll_stat(&mut self, key: EditKey, stat: Option<LocalStat>, ctx: &Ctx) -> Task<Event> {
        let Some(session) = self.sessions.get_mut(&key) else {
            return Task::none();
        };
        // 回传在途不重复触发。冲突框 / 确认框打开期间轮询仍会判定触发并再次上行
        // 弹框请求，由父层「槽位被占即跳过」去重；用户处置后基准被改写，循环自然终止。
        if session.uploading {
            return Task::none();
        }
        let (verdict, seen) = evaluate_poll(session.baseline, session.seen, stat);
        session.seen = seen;
        if verdict != PollVerdict::Trigger {
            return Task::none();
        }
        // 走到这里 `stat` 必为 `Some`（`evaluate_poll` 对 `None` 只判 Ignore），
        // 回传要用的水位已在 `session.seen`，无需再解包。
        let Some(client) = ctx.clients.get(&session.tab_id).cloned() else {
            // 无可用通道：提示一次，等通道恢复（重连后用户重开文件视图即有新客户端）。
            if !session.no_channel_notified {
                session.no_channel_notified = true;
                return Task::done(Event::Toast(
                    ToastKind::Warning,
                    t!("remote_edit.no_channel"),
                ));
            }
            return Task::none();
        };
        session.no_channel_notified = false;
        let remote_path = session.remote_path.clone();
        let remote_fp = session.remote_fp;
        Task::perform(
            async move {
                match client.remote_fingerprint(&remote_path).await {
                    // 取不到指纹也放行：上传会重建该文件，好过把用户的编辑永远憋在
                    // 缓存里；服务端的真实报错由传输队列如实呈现。
                    Ok(now) if now != remote_fp => VerifyOutcome::Conflict,
                    _ => VerifyOutcome::Proceed,
                }
            },
            move |outcome| Event::Emit(Box::new(Message::Verified { key, outcome })),
        )
    }

    /// 指纹比对结论落地：冲突 → 请求父层弹冲突框；无冲突 → 请求父弹回传确认框。
    ///
    /// 两条路都不直接回传：冲突框的「覆盖」与确认框的「回传」各自构成明确授权。
    fn verified(&mut self, key: EditKey, outcome: VerifyOutcome) -> Task<Event> {
        match outcome {
            VerifyOutcome::Proceed => {
                let Some(session) = self.sessions.get(&key) else {
                    return Task::none();
                };
                Task::done(Event::RequestUploadConfirm {
                    tab_id: session.tab_id,
                    key,
                    name: file_name_of(&session.remote_path),
                })
            }
            VerifyOutcome::Conflict => {
                let Some(session) = self.sessions.get(&key) else {
                    return Task::none();
                };
                Task::done(Event::RequestConflict {
                    tab_id: session.tab_id,
                    key,
                    name: file_name_of(&session.remote_path),
                })
            }
        }
    }

    /// 入队一次回传（置在途标记 + 记触发水位）。
    fn begin_upload(&mut self, key: &EditKey, stat: LocalStat) -> Task<Event> {
        let Some(session) = self.sessions.get_mut(key) else {
            return Task::none();
        };
        session.uploading = true;
        session.pending_stat = Some(stat);
        Task::done(Event::StartEditUpload {
            tab_id: session.tab_id,
            key: key.clone(),
            local: session.local_path.clone(),
            remote: session.remote_path.clone(),
        })
    }

    /// 回传完成：成功更新基准并重取远端指纹；失败按分类处置。
    fn upload_done(
        &mut self,
        key: EditKey,
        failure: Option<FailureKind>,
        ctx: &Ctx,
    ) -> Task<Event> {
        let Some(session) = self.sessions.get_mut(&key) else {
            return Task::none();
        };
        session.uploading = false;
        let Some(stat) = session.pending_stat.take() else {
            return Task::none();
        };
        let local = session.local_path.clone();
        match failure {
            // 成功：该版本成为新基准。远端指纹必须重取——上传把远端换成了这份内容，
            // 不刷新的话下一轮比对会拿旧指纹对上新远程，凭空弹冲突框。
            None => {
                session.baseline = stat;
                session.failures = 0;
                session.seen = None;
                let client = ctx.clients.get(&session.tab_id).cloned();
                let remote = session.remote_path.clone();
                match client {
                    Some(client) => Task::perform(
                        async move { client.remote_fingerprint(&remote).await.ok() },
                        move |fp| Event::Emit(Box::new(Message::RemoteRefreshed { key, fp })),
                    ),
                    // 通道不在：保留旧指纹。下一轮取不到指纹会直接放行上传（见
                    // `poll_stat`），不会把用户的编辑憋死。
                    None => Task::none(),
                }
            }
            // 用户取消：暂停该版本（把基准对齐到这次尝试的水位），换个说法提示。
            Some(FailureKind::Cancelled) => {
                session.baseline = stat;
                session.failures = 0;
                session.seen = None;
                Task::done(Event::Toast(
                    ToastKind::Warning,
                    t!("remote_edit.upload_canceled", path => local.display().to_string()),
                ))
            }
            // 其它失败：保留基准，下一轮自动重传；连续失败到上限暂停该版本。
            Some(_) => {
                session.failures += 1;
                if session.failures < MAX_UPLOAD_FAILURES {
                    return Task::none();
                }
                session.baseline = stat;
                session.failures = 0;
                session.seen = None;
                Task::done(Event::Toast(
                    ToastKind::Error,
                    t!("remote_edit.upload_stopped", path => local.display().to_string()),
                ))
            }
        }
    }

    /// 冲突框「覆盖」：按当前副本回传（用户已拍板，不再重取指纹）。
    fn conflict_overwrite(&mut self, key: EditKey) -> Task<Event> {
        let Some(session) = self.sessions.get(&key) else {
            return Task::none();
        };
        // 触发冲突时的稳定水位仍在 `seen`；异常缺失时退回基准（上传后基准不变，
        // 下一轮会重新判定，不会死循环）。
        let stat = session.seen.unwrap_or(session.baseline);
        self.begin_upload(&key, stat)
    }

    /// 冲突框「取消」：接受远端外部改动为新基准，同时把本地基准对齐到当前副本。
    ///
    /// 两个基准都对齐后，下一轮不会再判「改动」、也不会再弹框；用户的编辑仍在缓存
    /// 里，再保存一次（水位再变）会重新触发，届时指纹已对齐、直接传。
    fn conflict_cancel(&mut self, key: EditKey, ctx: &Ctx) -> Task<Event> {
        let Some(session) = self.sessions.get(&key) else {
            return Task::none();
        };
        let local = session.local_path.clone();
        let client = ctx.clients.get(&session.tab_id).cloned();
        let remote = session.remote_path.clone();
        Task::perform(
            async move {
                let local_stat = local_stat(&local).await;
                let remote_fp = match client {
                    Some(c) => c.remote_fingerprint(&remote).await.ok(),
                    None => None,
                };
                (local_stat, remote_fp)
            },
            move |(local, remote_fp)| {
                Event::Emit(Box::new(Message::ConflictResolved {
                    key,
                    local,
                    remote_fp,
                }))
            },
        )
    }

    /// 回传确认框「回传」：用户拍板上传这一版。
    ///
    /// 水位取 `session.seen`（最近一次稳定观测）：框开着期间轮询仍在跑，`seen` 始终最新；缺失时退回基准，下一轮重新判定。
    fn confirm_upload(&mut self, key: EditKey) -> Task<Event> {
        let Some(session) = self.sessions.get(&key) else {
            return Task::none();
        };
        let stat = session.seen.unwrap_or(session.baseline);
        self.begin_upload(&key, stat)
    }

    /// 回传确认框「取消」：暂不回传这一版。
    ///
    /// 与冲突取消同构，但不重取远端指纹——远端未被改动，指纹无需对齐，只需把本地基准对齐到
    /// 当前水位。异步重取水位（而不用 `seen`）：框开着期间用户可能又存了盘，用旧水位会让那次保存立刻重新 prompt。
    fn confirm_dismiss(&mut self, key: EditKey) -> Task<Event> {
        let Some(session) = self.sessions.get(&key) else {
            return Task::none();
        };
        let local = session.local_path.clone();
        Task::perform(async move { local_stat(&local).await }, move |stat| {
            Event::Emit(Box::new(Message::DismissResolved { key, stat }))
        })
    }

    /// 暂不回传的基准对齐落地 + 提示。
    fn dismiss_resolved(&mut self, key: EditKey, stat: Option<LocalStat>) -> Task<Event> {
        let Some(session) = self.sessions.get_mut(&key) else {
            return Task::none();
        };
        // 副本被外部删掉时只清观测记录：没有水位可对齐，下一轮观测不到文件自然不触发。
        if let Some(stat) = stat {
            session.baseline = stat;
        }
        session.seen = None;
        Task::done(Event::Toast(
            ToastKind::Warning,
            t!(
                "remote_edit.confirm_dismissed",
                path => session.local_path.display().to_string()
            ),
        ))
    }

    /// 移除会话并中止其轮询流（失败收尾用）。
    fn forget(&mut self, key: &EditKey) {
        if let Some(handle) = self.poll_handles.remove(key) {
            handle.abort();
        }
        self.sessions.remove(key);
    }
}

/// 轮询流：每 [`POLL_INTERVAL`] 采一次本地副本水位，上行 `PollStat`。
///
/// 与传输模块的 worker 同构：内层 tokio 任务经 cloned sender 上行，流只登记取消句柄并等待结束。
fn poll_stream(key: EditKey, local: PathBuf) -> Task<Event> {
    Task::stream(iced::stream::channel(
        64,
        move |mut output: futures::channel::mpsc::Sender<Event>| async move {
            let worker = {
                let mut tx = output.clone();
                let key = key.clone();
                tokio::spawn(async move {
                    let mut ticker = tokio::time::interval(POLL_INTERVAL);
                    // 延迟补拍而非突发补拍：任务被卡住后不应连珠炮式补上多次观测。
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        ticker.tick().await;
                        let stat = local_stat(&local).await;
                        if tx
                            .send(Event::Emit(Box::new(Message::PollStat {
                                key: key.clone(),
                                stat,
                            })))
                            .await
                            .is_err()
                        {
                            // 流已断（Task 被丢弃）：退出循环，不留孤儿任务。
                            break;
                        }
                    }
                })
            };
            let _ = output
                .send(Event::Emit(Box::new(Message::PollHandle {
                    key,
                    handle: worker.abort_handle(),
                })))
                .await;
            let _ = worker.await;
        },
    ))
}

/// 按配置拉起编辑器：空配置用系统默认程序，否则按空白符拆分取程序 + 参数，
/// 副本路径追加为末参。仅 spawn、不等待进程退出（改动检测是轮询式的）。
fn spawn_editor(editor: Option<&str>, local: &Path) -> Result<(), String> {
    match parse_editor_command(editor) {
        None => open::that(local).map_err(|e| e.to_string()),
        Some((program, args)) => {
            let mut cmd = std::process::Command::new(program);
            cmd.args(args).arg(local);
            cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
        }
    }
}

/// 解析编辑器命令：`None` / 空串 → `None`（系统默认程序）；否则按空白符拆成 `(程序, 参数)`。
///
/// 刻意不解析引号：拆分规则越简单，用户越能预测（设置项的帮助文案已注明）。
pub(crate) fn parse_editor_command(editor: Option<&str>) -> Option<(String, Vec<String>)> {
    let text = editor?.trim();
    if text.is_empty() {
        return None;
    }
    let mut parts = text.split_whitespace();
    let program = parts.next()?.to_string();
    let args = parts.map(str::to_string).collect();
    Some((program, args))
}

/// 轮询观测裁决：连续两轮水位相同、且与基准不同才回传。
///
/// 单轮观测只更新 `seen`：编辑器「写临时文件 + rename」天然原子，直接覆盖写的不是，两次稳定是漏过「写一半」的最低成本手段。
pub(crate) fn evaluate_poll(
    baseline: LocalStat,
    seen: Option<LocalStat>,
    now: Option<LocalStat>,
) -> (PollVerdict, Option<LocalStat>) {
    match now {
        // 取不到元数据（副本被删？）：不动作，保留上一轮观测。
        None => (PollVerdict::Ignore, seen),
        // 与基准一致：无改动，观测记录清零（免得陈旧观测在下次改动时凑成「稳定」）。
        Some(now) if now == baseline => (PollVerdict::Ignore, None),
        Some(now) if seen == Some(now) => (PollVerdict::Trigger, Some(now)),
        Some(now) => (PollVerdict::Observe, Some(now)),
    }
}

/// 编辑缓存路径：`<remote_edit_dir>/<会话>/<远端树镜像>`。
///
/// 镜像而非哈希目录：用户在缓存里能直接看出哪个文件是哪台机器上的哪个路径；输出对同一编辑键确定（续用缓存才成立）。
pub(crate) fn edit_cache_path(session_id: &str, remote_path: &str) -> PathBuf {
    let root = rterm_config::paths::remote_edit_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("rterm-remote-edit"));
    let mut path = root.join(sanitize_segment(session_id));
    for seg in remote_path.split('/').filter(|s| !s.is_empty()) {
        path = path.join(sanitize_segment(seg));
    }
    path
}

/// 净化单个路径分量：分隔符 / 控制字符 / `.` / `..` 一律替换为 `_`，其余原样保留
/// （CJK 文件名是常见形态）；超长段截断并接 8 位十六进制后缀，避开文件系统单分量上限。
fn sanitize_segment(seg: &str) -> String {
    /// 分量长度上限：远低于各文件系统的 255 字节限制，留足截断后缀。
    const MAX_LEN: usize = 100;
    /// 截断后缀长度（`-` + 8 位十六进制）。
    const SUFFIX_LEN: usize = 9;

    let cleaned: String = if seg == "." || seg == ".." {
        "_".to_string()
    } else {
        seg.chars()
            .map(|c| match c {
                '/' | '\\' | '\0' => '_',
                c if c.is_control() => '_',
                c => c,
            })
            .collect()
    };
    if cleaned.len() <= MAX_LEN {
        return cleaned;
    }
    // 净化后的串只含 ASCII 之外的合法字符与 `_`；按字节截断前先确认边界安全——
    // 非法字符已被替换，多字节字符仍可能存在，故按 char 边界截。
    let mut out: String = cleaned.chars().take(MAX_LEN - SUFFIX_LEN).collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&cleaned, &mut hasher);
    // 只取低 32 位：`{:08x}` 对更宽的值会吐出 9~16 位，把总长顶出 MAX_LEN
    // （实测 u64 全宽时分量 108 > 100）。32 位对缓存目录的命名消歧绰绰有余。
    out.push('-');
    out.push_str(&format!("{:08x}", hasher.finish() as u32));
    out
}

/// 取本地副本的水位；文件不在 / 元数据读不到时为 `None`。
async fn local_stat(local: &Path) -> Option<LocalStat> {
    let meta = tokio::fs::metadata(local).await.ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// 从远端绝对路径取展示用文件名（无分段时退回全路径）。
fn file_name_of(remote_path: &str) -> String {
    remote_path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(remote_path)
        .to_string()
}

/// 编辑键里的远端路径（装配下载 / 回传事件时取用）。
fn key_remote(key: &EditKey) -> String {
    key.1.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个确定的水位（用于裁决表驱动）。
    fn stat(secs: u64, len: u64) -> LocalStat {
        (SystemTime::UNIX_EPOCH + Duration::from_secs(secs), len)
    }

    /// 缓存路径对同一编辑键确定：续用缓存才成立。
    #[test]
    fn cache_path_is_deterministic_per_key() {
        let a = edit_cache_path("sess-1", "/home/u/notes.txt");
        let b = edit_cache_path("sess-1", "/home/u/notes.txt");
        assert_eq!(a, b);
        assert!(a.ends_with("sess-1/home/u/notes.txt"), "{a:?}");
    }

    /// 不同键（不同会话 / 不同远端文件）路径互异。
    #[test]
    fn cache_path_distinguishes_keys() {
        let a = edit_cache_path("sess-1", "/home/u/notes.txt");
        let b = edit_cache_path("sess-2", "/home/u/notes.txt");
        let c = edit_cache_path("sess-1", "/home/u/other.txt");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    /// 会话 id 与远端路径里的 traversal 段必须被中性化：结果永远以缓存根开头
    /// （负向对照：把 sanitize 撤掉，本断言立刻失败——路径会逃出根）。
    #[test]
    fn cache_path_neutralizes_traversal() {
        let root = rterm_config::paths::remote_edit_dir()
            .unwrap_or_else(|| std::env::temp_dir().join("rterm-remote-edit"));
        for key in [
            ("../evil".to_string(), "/a/b.txt".to_string()),
            ("sess".to_string(), "/a/../../b.txt".to_string()),
            ("sess".to_string(), "/a/..\x00b.txt".to_string()),
        ] {
            let path = edit_cache_path(&key.0, &key.1);
            assert!(
                path.starts_with(&root),
                "缓存路径不得逃出根：{path:?}（根 {root:?}）"
            );
            assert!(
                !path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir)),
                "不得含 .. 分量：{path:?}"
            );
        }
    }

    /// 超长分量被截断但保持互异（不同长名的前 91 字符相同也不撞车）。
    #[test]
    fn cache_path_truncates_long_segments_without_collision() {
        let long = "x".repeat(300);
        let a = edit_cache_path("sess", &format!("/{long}a"));
        let b = edit_cache_path("sess", &format!("/{long}b"));
        assert_ne!(a, b);
        let seg = a
            .components()
            .next_back()
            .unwrap()
            .as_os_str()
            .to_string_lossy();
        assert!(seg.len() <= 100, "分量超长：{seg}");
    }

    /// 两次稳定判定：单轮观测不触发；连续两轮相同且异于基准才触发。
    #[test]
    fn poll_requires_two_stable_observations() {
        let base = stat(100, 10);
        // 第一轮：异于基准 → 只记录观测。
        let (verdict, seen) = evaluate_poll(base, None, Some(stat(200, 20)));
        assert_eq!(verdict, PollVerdict::Observe);
        assert_eq!(seen, Some(stat(200, 20)));
        // 第二轮：与上一轮相同 → 触发。
        let (verdict, _) = evaluate_poll(base, seen, Some(stat(200, 20)));
        assert_eq!(verdict, PollVerdict::Trigger);
        // 负向对照：两轮之间水位又变（编辑器还在写）→ 不触发。
        let (verdict, seen) = evaluate_poll(base, Some(stat(200, 20)), Some(stat(300, 30)));
        assert_eq!(verdict, PollVerdict::Observe);
        assert_eq!(seen, Some(stat(300, 30)));
    }

    /// 回到基准水位：判 Ignore 且观测记录清零（负向对照：不清零的话，
    /// 陈旧观测会在下次改动时与首轮观测凑成「稳定」而漏过一轮）。
    #[test]
    fn poll_back_to_baseline_resets_observation() {
        let base = stat(100, 10);
        let (verdict, seen) = evaluate_poll(base, None, Some(stat(200, 20)));
        assert_eq!(verdict, PollVerdict::Observe);
        // 水位回到基准：Ignor + 观测清零。
        let (verdict, seen) = evaluate_poll(base, seen, Some(base));
        assert_eq!(verdict, PollVerdict::Ignore);
        assert_eq!(seen, None);
        // 再改动：又要两轮才触发。
        let (verdict, seen) = evaluate_poll(base, None, Some(stat(200, 20)));
        assert_eq!(verdict, PollVerdict::Observe);
        let (verdict, _) = evaluate_poll(base, seen, Some(stat(200, 20)));
        assert_eq!(verdict, PollVerdict::Trigger);
    }

    /// 与基准一致（含从未改动）：Ignor，不触发。
    #[test]
    fn poll_unchanged_is_ignored() {
        let base = stat(100, 10);
        let (verdict, seen) = evaluate_poll(base, None, Some(base));
        assert_eq!(verdict, PollVerdict::Ignore);
        assert_eq!(seen, None);
        let (verdict, _) = evaluate_poll(base, Some(base), Some(base));
        assert_eq!(verdict, PollVerdict::Ignore);
    }

    /// 观测失败（副本不在）：不动作，保留上一轮观测（下一轮补采）。
    #[test]
    fn poll_missing_stat_keeps_previous_observation() {
        let base = stat(100, 10);
        let (verdict, seen) = evaluate_poll(base, Some(stat(200, 20)), None);
        assert_eq!(verdict, PollVerdict::Ignore);
        assert_eq!(seen, Some(stat(200, 20)));
        // 副本回来后与保留的观测相同 → 立即触发（该水位早已稳定）。
        let (verdict, _) = evaluate_poll(base, seen, Some(stat(200, 20)));
        assert_eq!(verdict, PollVerdict::Trigger);
    }

    /// 编辑器命令解析：空 / 纯空白 → None；否则按空白符拆成程序 + 参数。
    #[test]
    fn editor_command_splits_on_whitespace() {
        assert_eq!(parse_editor_command(None), None);
        assert_eq!(parse_editor_command(Some("")), None);
        assert_eq!(parse_editor_command(Some("   ")), None);
        assert_eq!(
            parse_editor_command(Some("code --wait")),
            Some(("code".to_string(), vec!["--wait".to_string()]))
        );
        assert_eq!(
            parse_editor_command(Some("  gvim -f  ")),
            Some(("gvim".to_string(), vec!["-f".to_string()]))
        );
        assert_eq!(
            parse_editor_command(Some("notepad")),
            Some(("notepad".to_string(), Vec::new()))
        );
        // 引号不识别：原样进参数（帮助文案已注明，此处锁住该语义）。
        assert_eq!(
            parse_editor_command(Some("code \"/path with space/x\"")),
            Some((
                "code".to_string(),
                vec![
                    "\"/path".to_string(),
                    "with".to_string(),
                    "space/x\"".to_string()
                ]
            ))
        );
    }

    /// 同键重复发起复用现有会话：不重复下载、不产生第二个会话。
    #[test]
    fn repeated_start_reuses_the_existing_session() {
        let mut state = State::new();
        let ctx = Ctx {
            clients: HashMap::new(),
            editor: None,
        };
        let key: EditKey = ("sess-1".to_string(), "/home/u/notes.txt".to_string());
        // 预置一个在途会话（无客户端也能过 contains_key 复用判定）。
        state.sessions.insert(
            key.clone(),
            EditSession {
                tab_id: 7,
                remote_path: key.1.clone(),
                local_path: PathBuf::from("/tmp/cache/notes.txt"),
                baseline: stat(100, 10),
                remote_fp: Fingerprint {
                    len: 10,
                    modified: None,
                },
                seen: None,
                uploading: false,
                pending_stat: None,
                failures: 0,
                no_channel_notified: false,
            },
        );

        let task = state.update(
            Message::Start {
                tab_id: 7,
                session_id: key.0.clone(),
                remote_path: key.1.clone(),
                name: "notes.txt".to_string(),
            },
            &ctx,
        );
        // 复用路径只弹提示，不上行下载 / 回传 / 冲突请求。
        let events = collect_events(task);
        assert_eq!(events.len(), 1, "复用只应产出一条 toast：{events:?}");
        assert!(
            matches!(&events[0], Event::Toast(ToastKind::Success, _)),
            "复用提示应为成功态 toast：{events:?}"
        );
        assert_eq!(state.sessions.len(), 1, "不得产生第二个会话");
    }

    /// 无通道时发起编辑：提示后不装会话（负向对照：不拦的话探测会打在没有
    /// 客户端的标签上）。
    #[test]
    fn start_without_channel_is_rejected() {
        let mut state = State::new();
        let ctx = Ctx {
            clients: HashMap::new(),
            editor: None,
        };
        let task = state.update(
            Message::Start {
                tab_id: 7,
                session_id: "sess-1".to_string(),
                remote_path: "/home/u/notes.txt".to_string(),
                name: "notes.txt".to_string(),
            },
            &ctx,
        );
        let events = collect_events(task);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Toast(ToastKind::Error, _))),
            "无通道应报错：{events:?}"
        );
        assert!(state.sessions.is_empty(), "无通道不得装会话");
    }

    /// 关标签终止该标签的会话并中止轮询流；其它标签的会话不受影响。
    #[test]
    fn end_for_tab_terminates_only_that_tab() {
        let mut state = State::new();
        let mk = |tab_id: u64, name: &str| {
            (
                ("sess-1".to_string(), format!("/home/u/{name}")),
                EditSession {
                    tab_id,
                    remote_path: format!("/home/u/{name}"),
                    local_path: PathBuf::from(format!("/tmp/cache/{name}")),
                    baseline: stat(100, 10),
                    remote_fp: Fingerprint {
                        len: 10,
                        modified: None,
                    },
                    seen: None,
                    uploading: false,
                    pending_stat: None,
                    failures: 0,
                    no_channel_notified: false,
                },
            )
        };
        let (ka, sa) = mk(7, "a.txt");
        state.sessions.insert(ka, sa);
        let (kb, sb) = mk(8, "b.txt");
        state.sessions.insert(kb, sb);
        state.end_for_tab(7);
        assert_eq!(state.sessions.len(), 1, "只终止 tab 7 的会话");
        assert!(state.sessions.keys().all(|(_, p)| p.ends_with("b.txt")));
    }

    /// 下载失败：清理会话，不残留轮询流登记。
    #[test]
    fn download_failure_removes_the_session() {
        let mut state = State::new();
        let ctx = Ctx {
            clients: HashMap::new(),
            editor: None,
        };
        let key: EditKey = ("sess-1".to_string(), "/home/u/notes.txt".to_string());
        state.sessions.insert(
            key.clone(),
            EditSession {
                tab_id: 7,
                remote_path: key.1.clone(),
                local_path: PathBuf::from("/tmp/cache/notes.txt"),
                baseline: stat(100, 10),
                remote_fp: Fingerprint {
                    len: 10,
                    modified: None,
                },
                seen: None,
                uploading: false,
                pending_stat: None,
                failures: 0,
                no_channel_notified: false,
            },
        );
        let _ = state.update(Message::DownloadDone { key, ok: false }, &ctx);
        assert!(state.sessions.is_empty(), "下载失败应清理会话");
    }

    /// 造一个「已激活、正在 watch」的会话（`seen` 由调用方给，模拟轮询观测）。
    fn watching_session(key: &EditKey, seen: Option<LocalStat>) -> EditSession {
        EditSession {
            tab_id: 7,
            remote_path: key.1.clone(),
            local_path: PathBuf::from("/tmp/cache/notes.txt"),
            baseline: stat(100, 10),
            remote_fp: Fingerprint {
                len: 10,
                modified: None,
            },
            seen,
            uploading: false,
            pending_stat: None,
            failures: 0,
            no_channel_notified: false,
        }
    }

    fn notes_key() -> EditKey {
        ("sess-1".to_string(), "/home/u/notes.txt".to_string())
    }

    /// 指纹一致分支：只请求父层弹确认框，**不**静默入队回传（撤掉确认、改回直接
    /// `begin_upload` 时本断言立刻失败）。
    #[test]
    fn verified_proceed_asks_instead_of_uploading() {
        let mut state = State::new();
        let ctx = Ctx {
            clients: HashMap::new(),
            editor: None,
        };
        let key = notes_key();
        state
            .sessions
            .insert(key.clone(), watching_session(&key, Some(stat(200, 20))));

        let events = collect_events(state.update(
            Message::Verified {
                key: key.clone(),
                outcome: VerifyOutcome::Proceed,
            },
            &ctx,
        ));

        assert_eq!(events.len(), 1, "指纹一致只应产出一条事件：{events:?}");
        assert!(
            matches!(
                &events[0],
                Event::RequestUploadConfirm { tab_id: 7, key: k, name } if *k == key && name == "notes.txt"
            ),
            "指纹一致只应请求弹确认框：{events:?}"
        );
        let session = &state.sessions[&key];
        assert!(!session.uploading, "未获用户确认不得置在途");
        assert_eq!(session.pending_stat, None, "未获用户确认不得记触发水位");
    }

    /// 确认框「回传」：按最近一次稳定观测入队，置在途标记。
    #[test]
    fn confirm_upload_starts_the_transfer() {
        let mut state = State::new();
        let ctx = Ctx {
            clients: HashMap::new(),
            editor: None,
        };
        let key = notes_key();
        state
            .sessions
            .insert(key.clone(), watching_session(&key, Some(stat(200, 20))));

        let events =
            collect_events(state.update(Message::ConfirmUpload { key: key.clone() }, &ctx));

        assert_eq!(events.len(), 1, "确认后只应产出一条事件：{events:?}");
        assert!(
            matches!(
                &events[0],
                Event::StartEditUpload { tab_id: 7, key: k, local, remote }
                    if *k == key
                        && local == &PathBuf::from("/tmp/cache/notes.txt")
                        && remote == "/home/u/notes.txt"
            ),
            "确认后应原样入队回传：{events:?}"
        );
        let session = &state.sessions[&key];
        assert!(session.uploading, "确认后应置在途");
        assert_eq!(
            session.pending_stat,
            Some(stat(200, 20)),
            "触发水位应取最近一次稳定观测"
        );
    }

    /// 确认框「取消」：基准对齐到当前水位，该版本不再触发；且有一次明确提示。
    #[test]
    fn dismiss_resolved_silences_the_version() {
        let mut state = State::new();
        let ctx = Ctx {
            clients: HashMap::new(),
            editor: None,
        };
        let key = notes_key();
        state
            .sessions
            .insert(key.clone(), watching_session(&key, Some(stat(200, 20))));

        // 对齐落地：基准换成当前水位 + 提示（警告态：用户要知道这版不会自动传了）。
        let events = collect_events(state.update(
            Message::DismissResolved {
                key: key.clone(),
                stat: Some(stat(200, 20)),
            },
            &ctx,
        ));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Toast(ToastKind::Warning, _))),
            "暂不回传应有提示：{events:?}"
        );
        let session = &state.sessions[&key];
        assert_eq!(session.baseline, stat(200, 20), "基准应对齐到当前水位");
        assert_eq!(session.seen, None, "观测记录应清掉");

        // 同一水位再来一轮：判 Ignore，不再上行任何请求（否则取消毫无意义）。
        let events = collect_events(state.update(
            Message::PollStat {
                key: key.clone(),
                stat: Some(stat(200, 20)),
            },
            &ctx,
        ));
        assert!(events.is_empty(), "已暂不回传的版本不得再触发：{events:?}");

        // 再次保存（水位再变）：重新走两轮稳定 → 又可以 prompt（负向对照：
        // 若对齐用了陈旧的 `seen`，这次新保存会被漏掉）。
        let _ = state.update(
            Message::PollStat {
                key: key.clone(),
                stat: Some(stat(300, 30)),
            },
            &ctx,
        );
        let events = collect_events(state.update(
            Message::PollStat {
                key,
                stat: Some(stat(300, 30)),
            },
            &ctx,
        ));
        // 第二圈稳定后本应上行指纹比对（无通道 → 只出不入的 toast 路径），
        // 这里只断言「没有被上一行的对齐堵死」：有事件产出即说明循环重新打开。
        assert!(!events.is_empty(), "新保存应重新打开判定循环：{events:?}");
    }

    /// 冲突分支不变：Verified{Conflict} 仍请求冲突框；冲突框「覆盖」直接回传，
    /// **不过**确认框（覆盖本身即授权，不叠加二次确认）。
    #[test]
    fn conflict_branch_unchanged() {
        let mut state = State::new();
        let ctx = Ctx {
            clients: HashMap::new(),
            editor: None,
        };
        let key = notes_key();
        state
            .sessions
            .insert(key.clone(), watching_session(&key, Some(stat(200, 20))));

        let events = collect_events(state.update(
            Message::Verified {
                key: key.clone(),
                outcome: VerifyOutcome::Conflict,
            },
            &ctx,
        ));
        assert_eq!(events.len(), 1, "冲突只应产出一条事件：{events:?}");
        assert!(
            matches!(
                &events[0],
                Event::RequestConflict { tab_id: 7, key: k, name } if *k == key && name == "notes.txt"
            ),
            "指纹不一致仍应请求冲突框：{events:?}"
        );

        // 用户拍板「覆盖」：直接入队，中间不插确认框。
        let events =
            collect_events(state.update(Message::ConflictOverwrite { key: key.clone() }, &ctx));
        assert_eq!(events.len(), 1, "覆盖只应产出一条事件：{events:?}");
        assert!(
            matches!(
                &events[0],
                Event::StartEditUpload { tab_id: 7, key: k, local, remote }
                    if *k == key
                        && local == &PathBuf::from("/tmp/cache/notes.txt")
                        && remote == "/home/u/notes.txt"
            ),
            "冲突框覆盖应直接回传、不过确认框：{events:?}"
        );
    }

    /// 把 `Task<Event>` 跑在临时运行时里并收集产出的事件（与传输模块的
    /// `run_events` 同手法：`Task` 经 `into_stream` 转为产出流逐条收集）。
    fn collect_events(task: Task<Event>) -> Vec<Event> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("临时 tokio 运行时");
        runtime.block_on(async move {
            let mut stream = match iced_runtime::task::into_stream(task) {
                Some(s) => s,
                None => return Vec::new(),
            };
            let mut out = Vec::new();
            while let Some(action) = futures::StreamExt::next(&mut stream).await {
                if let iced_runtime::Action::Output(msg) = action {
                    out.push(msg);
                }
            }
            out
        })
    }
}
