//! 文件传输模块（上传 / 下载队列）
//!
//! # 并发调度
//!
//! 传输按**全局并发 N** 执行：N 由 `[transfer] max_concurrent` 配置（设置界面可调，默认 3），
//! 跨标签、跨方向共享同一份额度——左侧传输面板本身就是全局聚合展示，用户的心智是
//! 「同时最多 N 个文件在动」。
//!
//! 调度器是**显式计数 + pump**而非 `tokio::Semaphore`：iced 的 `update` 是单线程消息循环，
//! 任务只能从 `update` 返回的 `Task` 启动；若把准入交给信号量，「谁先跑」由 tokio 调度决定，
//! UI 的 `Queued` / `Active` 会与实际在跑的任务产生时序缝隙，也无法单测。
//!
//! # 同一 SFTP 客户端为什么可以并发
//!
//! 历史注释（本模块与 `state.rs`）曾断言「同一标签的 SFTP 通道非并发安全，故顺序执行」，
//! 该前提与 `russh-sftp` 2.4.0 的实现不符：
//!
//! - `SftpSession { session: Arc<RawSftpSession>, .. }`，全部操作方法签名均为 `&self`；
//! - `RawSftpSession` 由「单写任务 + `Arc<DashMap<request_id, oneshot>>` + `AtomicU32`
//!   请求 id」构成，响应由读循环按 id 派发回各自的等待者 —— **多请求在途互不串包**，
//!   这正是 SFTP 协议本身的多路复用语义；
//! - 单个 `File` 内部还有 `write_acks` 队列，按 `limits@openssh.com` 的
//!   `max_concurrent_writes` 做写流水线。
//!
//! 故 `Arc<SftpClient>` 可被 N 个传输任务同时持有并调用。需要如实认知的是收益上限：
//! OpenSSH 的 `sftp-server` 是每通道一个进程、按序处理请求，单通道的并发是「隐藏往返延迟」
//! 而非服务端并行；真正的服务端并行需要通道池（未实现）。
//!
//! # 调度不变量
//!
//! - **I1**：`running.len() <= max_concurrent`（`max_concurrent` 由父层在**路由每条消息前**
//!   经 `Ctx` 注入，绝不缓存进本模块 `State`，否则滑块消息到达那一刻会读到旧值，
//!   表现为「拖了没反应」）。
//! - **I2**：`running` 的移除**只发生在 `TransferDone` 处理里**（唯一释放点）。取消路径
//!   不得提前摘除——`abort()` 只中止内层 tokio worker，外层 `Task::stream` 仍会在
//!   `worker.await` 上醒来并发出 `TransferDone`，在那里统一回收额度。唯一例外见
//!   [`Message::TabClosed`]（标签已消失，不再有事件送回来）。
//! - **I3**：同一 `tid` 任一时刻最多一个在跑的 worker，故只有「不在 `running` 中」的排队项
//!   才能被启动；否则旧 worker 迟到的 `TransferDone` 会错误释放新 worker 的额度（见
//!   [`State::next_queued`] 的过滤条件与 [`State::admit`]）。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use iced::{Subscription, Task};

use crate::app::tasks::{ensure_remote_dir, join_path, parent_path};
use crate::i18n::localize_error;
use crate::state::{ToastKind, Transfer, TransferDirection, TransferStatus};
use crate::t;
use futures::{SinkExt, StreamExt};
use log::warn;
use rterm_config::{MAX_CONCURRENT, MIN_CONCURRENT};
use rterm_core::{CoreError, ErrorClass, SftpClient};
use std::path::Path;
use tokio::task::AbortHandle;

/// 单标签队列的条目上限：文件夹上传会把目录展开成「一文件一条」
/// （见 `app::tasks::collect_upload_items`），拖入一个十万文件的目录会瞬间产生十万条
/// `Transfer`（每条含 `String + PathBuf`）并让面板每帧全量遍历。超限时**拒绝并入并提示**，
/// 不静默丢弃，使用户知道哪些文件没进去。
const MAX_QUEUE: usize = 1000;

/// 传输模块只读上下文：父层在路由每条消息前构造，模块据此读取当前标签的 SFTP 客户端与
/// 远端目录，但绝不写回父状态。
///
/// `client` 为 `None` 表示当前标签尚未建立 SFTP 通道，此时上传 / 下载会被拒绝（emit toast）。
pub struct Ctx {
    /// 当前活动标签 id（供「作用于活动标签」的变体定位传输记录）。
    pub tab_id: u64,
    /// 当前标签的 SFTP 客户端（上传 / 下载执行所需）。
    pub client: Option<Arc<SftpClient>>,
    /// 各标签**当前**的 SFTP 客户端快照（父层每轮重建）。
    ///
    /// 与 [`Ctx::client`] 的分工：后者只服务「入队」这类作用于活动标签的动作，前者服务
    /// 「启动 / 重试」——那条路径必须按传输**自己的标签**取客户端（面板跨标签聚合，非活动标签
    /// 的排队项同样要能跑）。且取的是此刻的客户端，而不是记录里入队时捕获的那份：会话重建后
    /// 同一标签会换上新通道，旧记录因此能用上新客户端（见设计文档 §19）。
    pub clients: HashMap<u64, Arc<SftpClient>>,
    /// 当前标签的远端工作目录（用于把本地文件名 / 远端名解析为绝对远端路径）。
    pub remote_dir: String,
    /// 全局最大并发传输数（唯一真相在 `AppConfig`，由父层在路由每条消息前注入）。
    ///
    /// 刻意**不**缓存进 [`State`]：见模块文档的不变量 I1。
    pub max_concurrent: usize,
}

impl Ctx {
    /// 某标签此刻可用的 SFTP 客户端（无则 `None`）。
    ///
    /// 这是启动 / 重试路径取客户端的**唯一**入口：优先当前，缺了才由调用方回退到记录里
    /// 入队时捕获的那一份（回退只为兼容，正常路径下二者是同一个）。
    pub fn client_for(&self, tab_id: u64) -> Option<Arc<SftpClient>> {
        self.clients.get(&tab_id).cloned()
    }
}

/// 传输模块私有状态：每标签传输队列 + 任务 id 分配器 + 取消句柄注册表。
pub struct State {
    /// 每标签独立的传输队列（原内嵌在 `SftpView.transfers`）。
    ///
    /// 保持「每标签一条队列」而非单一全局队列：渲染层 `all_transfers()` 与「取消 / 移除作用于
    /// 活动标签」的既有语义都建立在它之上。跨标签的公平序由单调的 `Transfer.id` 给出（见
    /// [`State::next_queued`]），无需额外的排序结构。
    per_tab: HashMap<u64, Vec<Transfer>>,
    /// 下一个传输任务的 id（原 `sftp::State::next_transfer_id` 搬入）。
    ///
    /// 同时充当全局排队序：id 单调递增，故「id 最小者优先」等价于「入队最早者优先」。
    next_transfer_id: u64,
    /// 传输任务 id → 取消句柄（原 `sftp::State::abort_handles` 搬入）。
    abort_handles: HashMap<u64, AbortHandle>,
    /// 正在运行的传输任务 id 集合，其基数即当前占用的并发额度（见模块文档的不变量 I1/I2）。
    running: HashSet<u64>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            per_tab: HashMap::new(),
            next_transfer_id: 1,
            abort_handles: HashMap::new(),
            running: HashSet::new(),
        }
    }
}

/// 一次传输作业的完整参数。
///
/// 由 [`State::admit`] 产出时是「启动参数」（[`State::pump`] 据此拉起 worker），
/// 由 [`State::job_of`] 产出时是「收尾参数」（[`cleanup_partial`] 据此删除半成品）。
/// 两者字段完全重合——都需要「谁、往哪、用哪个客户端」——故共用同一结构，避免两份平行定义。
struct Job {
    /// 所属标签 id（事件回传时用于定位队列）。
    tab_id: u64,
    /// 传输任务 id。
    tid: u64,
    /// 传输方向。
    direction: TransferDirection,
    /// 本地路径（上传源 / 下载目标）。
    local: PathBuf,
    /// 远端绝对路径。
    remote: String,
    /// 执行该传输的客户端；`None` 只可能出现在测试用宽松判定下（生产判定要求其存在）。
    client: Option<Arc<SftpClient>>,
}

/// 由一条传输记录 + 其所属标签构造 [`Job`]。
fn job_from(tab_id: u64, t: &Transfer) -> Job {
    Job {
        tab_id,
        tid: t.id,
        direction: t.direction,
        local: t.local.clone(),
        remote: t.remote.clone(),
        client: t.client.clone(),
    }
}
impl State {
    /// 构造空状态。
    pub fn new() -> Self {
        Self::default()
    }

    /// 聚合所有标签的传输任务（引用，生命周期随 `&self`），供左侧传输面板渲染。
    pub fn all_transfers(&self) -> Vec<&Transfer> {
        self.per_tab.values().flat_map(|v| v.iter()).collect()
    }

    /// 分配下一个传输任务 id。
    fn next_transfer_id(&mut self) -> u64 {
        let id = self.next_transfer_id;
        self.next_transfer_id += 1;
        id
    }

    /// 模块更新：只改自身 `State`；需要父层配合的事以 [`Event`] 经 `Task` 上行。
    ///
    /// `ctx` 为父层传入的只读上下文（当前标签 id + SFTP 客户端 + 远端目录），供「作用于活动
    /// 标签」的变体定位 / 取客户端；模块据此执行但不写父态。
    pub fn update(&mut self, msg: Message, ctx: &Ctx) -> Task<Event> {
        match msg {
            Message::Upload(tab_id, items) => {
                if items.is_empty() {
                    return Task::none();
                }
                let Some(client) = ctx.client.clone() else {
                    return Task::done(Event::Toast(ToastKind::Error, t!("sftp.not_connected")));
                };
                let mut overflow = false;
                for (local, rel) in items {
                    // 队列容量保护：超限即拒绝剩余项并提示，避免十万文件目录把面板与内存拖垮。
                    if !self.has_queue_capacity(tab_id) {
                        overflow = true;
                        break;
                    }
                    let name = match Path::new(&local).file_name() {
                        Some(n) => n.to_string_lossy().to_string(),
                        None => continue,
                    };
                    if name.is_empty() {
                        continue;
                    }
                    // 远端相对路径保留目录层级（文件夹上传），再拼到当前远端目录下。
                    let remote = join_path(&ctx.remote_dir, &rel);
                    let tid = self.next_transfer_id();
                    let transfer = Transfer {
                        id: tid,
                        direction: TransferDirection::Upload,
                        name,
                        local,
                        remote,
                        transferred: 0,
                        total: 0,
                        status: TransferStatus::Queued,
                        error: None,
                        speed: 0.0,
                        client: Some(client.clone()),
                    };
                    self.per_tab.entry(tab_id).or_default().push(transfer);
                }
                let pump = self.pump(ctx);
                if overflow {
                    return Task::batch([
                        pump,
                        Task::done(Event::Toast(ToastKind::Error, t!("transfer.queue_full"))),
                    ]);
                }
                pump
            }
            Message::Download(tab_id, name, local) => {
                let Some(client) = ctx.client.clone() else {
                    return Task::done(Event::Toast(ToastKind::Error, t!("sftp.not_connected")));
                };
                let remote = join_path(&ctx.remote_dir, &name);
                // `local` 已是完整本地目标路径（SFTP 模块在唤起下载时已 `dir.join(name)` 拼好），
                // 此处不可再拼接一次，否则目标会变成 `…/name/name` 导致写入失败。
                self.enqueue_download(tab_id, &name, remote, local, client, ctx)
            }
            Message::Progress(tab_id, tid, transferred, total, speed) => {
                if let Some(tab) = self.per_tab.get_mut(&tab_id)
                    && let Some(t) = tab.iter_mut().find(|t| t.id == tid)
                {
                    t.transferred = transferred;
                    t.total = total;
                    t.speed = speed;
                }
                Task::none()
            }
            Message::TransferDone(tab_id, tid, result) => {
                // 唯一的额度释放点（不变量 I2）：无论成功、失败还是被取消，都在此回收，
                // 故取消路径不得提前 `running.remove`，否则额度会被释放两次。
                self.running.remove(&tid);
                self.abort_handles.remove(&tid);
                // 标记完成 / 失败；上传成功需刷新当前目录以显示新文件（经父层转发给 SFTP 模块）。
                let mut relist = false;
                if let Some(tab) = self.per_tab.get_mut(&tab_id)
                    && let Some(t) = tab.iter_mut().find(|t| t.id == tid)
                    // 仅在仍在 `Active` 时落地结果：若该行已被用户手动重试置回 `Queued`，
                    // 这条迟到的结果只应释放额度，不该把排队态改回失败态（否则重试被吞掉）。
                    && t.status == TransferStatus::Active
                {
                    match result {
                        Ok(()) => {
                            t.status = TransferStatus::Done;
                            t.error = None;
                            if matches!(t.direction, TransferDirection::Upload) {
                                relist = true;
                            }
                        }
                        Err(f) => {
                            warn!("transfer {tid} failed ({:?}): {}", f.kind, f.message);
                            t.error = Some(f.message);
                            t.status = TransferStatus::Error;
                        }
                    }
                }
                let mut tasks = vec![self.pump(ctx)];
                if relist {
                    // 上传成功：请求父层刷新该标签目录（父层再经 `Message::Sftp` 派发给 SFTP 模块）。
                    tasks.push(Task::done(Event::RefreshDir(tab_id)));
                }
                Task::batch(tasks)
            }
            Message::TransferHandle(tid, handle) => {
                self.abort_handles.insert(tid, handle);
                Task::none()
            }
            Message::CancelTransfer(id) => {
                // 「取消」按行所处状态分两种，处置方式不同，不能一律改状态：
                // 1. 有 worker 在跑：只 `abort()`，状态留给随后的 `TransferDone` —— 在那里统一
                //    「落地为取消 + 归还额度」（不变量 I2）；
                // 2. 排队中：从未启动，在此直接落地。
                // 按 id 全局查找：面板跨标签聚合展示，非活动标签的行同样要能被取消。
                let was_running = self.running.contains(&id);
                if let Some(handle) = self.abort_handles.remove(&id) {
                    handle.abort();
                }
                if was_running {
                    return Task::none();
                }
                let status = self.find(id).map(|t| t.status);
                // 只对「排队中 / 等待重试」的行落地。`Done` / `Error` 的行本就不该有取消按钮，
                // 但消息可能迟到（连点 / 与服务端竞态），此时把已完成的任务改写成「已取消」
                // 是货真价实的错误显示，故在此收口。
                // 按 id 全局查找：面板跨标签聚合展示，非活动标签的行同样要能被取消。
                if matches!(status, Some(TransferStatus::Queued))
                    && let Some(t) = self.find_mut(id)
                {
                    t.status = TransferStatus::Error;
                    t.error = Some(t!("app.canceled"));
                }
                Task::none()
            }
            Message::RetryTransfer(id) => {
                // 手动重试 = 重新开始：回到排队态、清零进度，并立即补位。
                if let Some(t) = self.find_mut(id) {
                    t.status = TransferStatus::Queued;
                    t.error = None;
                    t.transferred = 0;
                    t.total = 0;
                    t.speed = 0.0;
                }
                // 若该行仍在运行（用户在取消后立刻点了重试），`next_queued` 会因 I3 跳过它，
                // 待其 `TransferDone` 释放额度后再由那次 pump 补位启动。
                self.pump(ctx)
            }
            Message::RemoveTransfer(id) => {
                // `Active` 项不得直接删除：删掉记录后 worker 仍在跑，其 `TransferDone` 将无处
                // 落地，`running` 里的额度也会永久漂移。面板本就不给 `Active` 行提供删除按钮，
                // 此处仅作防御。
                let status = self.find(id).map(|t| t.status);
                let Some(status) = status else {
                    return Task::none();
                };
                if status == TransferStatus::Active {
                    return Task::none();
                }
                if let Some(tab) = self.per_tab.get_mut(&ctx.tab_id)
                    && let Some(pos) = tab.iter().position(|t| t.id == id)
                {
                    tab.remove(pos);
                }
                Task::none()
            }
            Message::ConcurrencyChanged => {
                // 并发上限变更（设置界面拖动滑块）：只重新调度——调大即刻补位，
                // 调小不打断在跑任务，只停止补位、待其完成后自然收敛。
                self.pump(ctx)
            }
            Message::TabClosed(tab_id) => {
                // 关标签必须一并清理传输态：`per_tab` 中的僵尸行会一直挂在全局面板上，
                // 而 `running` 中的僵尸 id 会**永久**占用并发额度（串行实现下只表现为多几行
                // 记录，改成全局计数后就成了可用额度的泄漏）。
                //
                // 这是不变量 I2「只在 TransferDone 释放额度」的唯一例外：标签已消失，
                // 其任务不会再有任何事件送回来。迟到的 `TransferDone` 只会再 remove 一次
                // 已不存在的 id（幂等），不会重新占额度。
                let rows = self.per_tab.remove(&tab_id).unwrap_or_default();
                for t in &rows {
                    if let Some(handle) = self.abort_handles.remove(&t.id) {
                        handle.abort();
                    }
                    self.running.remove(&t.id);
                }
                let tasks = vec![self.pump(ctx)];
                Task::batch(tasks)
            }
            Message::OpenContainingFolder(local) => {
                // 携带完整本地路径而非传输 id：避免现有「仅活动标签可操作」的限制（面板跨标签
                // 聚合展示，非活动标签的传输同样应能定位）。
                if let Err(e) = open::that(containing_folder(&local)) {
                    warn!("failed to open containing folder: {e}");
                }
                Task::none()
            }
        }
    }

    /// 启动队列中下一个「排队中」的传输（同一标签 SFTP 通道非并发安全，顺序执行）。
    ///
    /// 这是唯一的调度入口——入队、完成、重试、并发数变更、标签关闭都汇到这里，故额度永远由
    /// 当前额度与队列现状推导得出，不需要额外的记账。`max_concurrent` 取自 `ctx`（父层在路由
    /// 本消息前重建），因此在设置界面拖动滑块后，同一条消息链就能读到新值并立即补位。
    fn pump(&mut self, ctx: &Ctx) -> Task<Event> {
        // 启动条件的判定放在 `admit` 的实参里：客户端缺失的项本轮跳过（而非阻塞整个调度），
        // 这样一个已断连标签的残留队列不会冻住其它标签的队列。
        //
        // 判定按**该传输自己的标签**取客户端（面板是跨标签聚合的，活动标签的客户端对别的标签
        // 无效），并优先取此刻的那一份：会话重建后同一标签换了新客户端，旧记录也就跟着复活。
        let starts = self.admit(ctx.max_concurrent, |tab_id, t| {
            ctx.client_for(tab_id).is_some() || t.client.is_some()
        });
        let mut tasks = Vec::with_capacity(starts.len());
        for s in starts {
            // 优先当前客户端；记录里那份只在标签已经拿不到客户端时兜底（见 `Ctx::client_for`）。
            if let Some(client) = ctx.client_for(s.tab_id).or(s.client) {
                // 回写进去：收尾时的远端清理（删上传留下的半个文件）也走 `Transfer.client`，
                // 让它同样用上当前通道，而不是入队时的那份可能已死的。
                if let Some(t) = self.find_mut(s.tid) {
                    t.client = Some(client.clone());
                }
                tasks.push(run_transfer(
                    s.tab_id,
                    s.tid,
                    s.direction,
                    client,
                    s.local,
                    s.remote,
                ));
            }
        }
        Task::batch(tasks)
    }

    /// 按额度把队列中的可启动项置为 `Active` 并占用槽位，返回其启动参数。
    ///
    /// `startable` 判定一个排队项本轮能否启动，入参为该传输所属标签 id 与记录本身（客户端
    /// 按标签解析，见 [`State::pump`]）；不可启动者记入本轮黑名单后**跳过**（而不是终止本轮
    /// 循环），使后续候选仍有机会补位。返回值的顺序即启动顺序。
    ///
    /// 除调用方给的判定之外，这里还**自带**不变量 I4：写靶已被占用的候选一律跳过。它属于调度
    /// 规则而非业务条件，故写在调度器里——两个 worker 交叠写同一个暂存文件的产物是拼接垃圾，
    fn admit(&mut self, limit: usize, startable: impl Fn(u64, &Transfer) -> bool) -> Vec<Job> {
        let limit = limit.clamp(MIN_CONCURRENT, MAX_CONCURRENT);
        let mut starts = Vec::new();
        let mut skipped: HashSet<u64> = HashSet::new();
        while self.running.len() < limit {
            let Some((tab_id, tid)) = self.next_queued(&skipped) else {
                break;
            };
            let Some(start) = self.take_startable(tid, tab_id, &startable) else {
                // 不可启动（如客户端缺失）：本轮跳过，继续找下一个候选。
                skipped.insert(tid);
                continue;
            };
            self.running.insert(tid);
            starts.push(start);
        }
        starts
    }

    /// 当前 `Active` 行占用的写靶集合（不变量 I4 的初始占用）。
    fn next_queued(&self, skipped: &HashSet<u64>) -> Option<(u64, u64)> {
        self.per_tab
            .iter()
            .flat_map(|(tab_id, queue)| queue.iter().map(move |t| (*tab_id, t)))
            .filter(|(_, t)| {
                t.status == TransferStatus::Queued
                    && !self.running.contains(&t.id)
                    && !skipped.contains(&t.id)
            })
            .min_by_key(|(_, t)| t.id)
            .map(|(tab_id, t)| (tab_id, t.id))
    }

    /// 若该排队项满足启动条件，则置为 `Active` 并取出启动参数；否则返回 `None`（不动状态）。
    fn take_startable(
        &mut self,
        tid: u64,
        tab_id: u64,
        startable: &impl Fn(u64, &Transfer) -> bool,
    ) -> Option<Job> {
        let job = {
            let t = self.find(tid)?;
            if !startable(tab_id, t) {
                return None;
            }
            job_from(tab_id, t)
        };
        if let Some(t) = self.find_mut(tid) {
            t.status = TransferStatus::Active;
        }
        Some(job)
    }

    /// 该标签的队列是否还能容纳新条目（容量保护，见 [`MAX_QUEUE`]）。
    fn has_queue_capacity(&self, tab_id: u64) -> bool {
        self.per_tab
            .get(&tab_id)
            .map(|queue| queue.len() < MAX_QUEUE)
            .unwrap_or(true)
    }

    /// 按 id 查找传输记录（跨标签；id 全局唯一，故无需限定标签）。
    fn find(&self, tid: u64) -> Option<&Transfer> {
        self.per_tab
            .values()
            .flat_map(|queue| queue.iter())
            .find(|t| t.id == tid)
    }

    /// 按 id 取传输记录的可变引用（跨标签）。
    fn find_mut(&mut self, tid: u64) -> Option<&mut Transfer> {
        self.per_tab
            .values_mut()
            .flat_map(|queue| queue.iter_mut())
            .find(|t| t.id == tid)
    }

    /// 入队一个下载传输并启动调度（本地已存在同名由调用方先弹覆盖框，此处直接写入）。
    fn enqueue_download(
        &mut self,
        tab_id: u64,
        remote_name: &str,
        remote: String,
        local: PathBuf,
        client: Arc<SftpClient>,
        ctx: &Ctx,
    ) -> Task<Event> {
        if !self.has_queue_capacity(tab_id) {
            return Task::batch([
                self.pump(ctx),
                Task::done(Event::Toast(ToastKind::Error, t!("transfer.queue_full"))),
            ]);
        }
        let name = remote_name.to_string();
        let tid = self.next_transfer_id();
        let transfer = Transfer {
            id: tid,
            direction: TransferDirection::Download,
            name,
            local,
            remote,
            transferred: 0,
            total: 0,
            status: TransferStatus::Queued,
            error: None,
            speed: 0.0,
            client: Some(client),
        };
        self.per_tab.entry(tab_id).or_default().push(transfer);
        self.pump(ctx)
    }

    /// 订阅：当前为占位（内部异步结果由 `run_transfer` 的流式任务产生，
    /// 后续把进度 / 完成流式逻辑迁入此处并 `map` 为 `Message`）。
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }
}

/// 模块内部消息：上传 / 下载意图 + 模块自处理的异步进度 / 完成结果。
///
/// 由父层经 `Message::Transfer` 路由进来；模块 `update` 自行消费，不外泄。
///
/// 仅 `Clone`（不 `Debug`：`TransferHandle` 含 `AbortHandle`，而 `AbortHandle` 未实现 `Debug`）。
#[derive(Clone)]
// 变体统一带 `Transfer` 语义前缀：与父层 `Message::Transfer` 路由命名一致，属刻意约定，故抑制此 lint。
#[allow(clippy::enum_variant_names)]
pub enum Message {
    /// 上传本地文件 / 文件夹（携带标签 id + 上传项列表，每项为 `(本地路径, 远端相对路径)`；
    /// 文件夹上传时远端相对路径保留目录层级，由本模块在执行时按需创建远端父目录）。
    Upload(u64, Vec<(PathBuf, String)>),
    /// 下载远端文件（携带标签 id + 远端名称 + 本地目标路径，已由 SFTP 侧拼为完整路径）。
    Download(u64, String, PathBuf),
    /// 传输进度（携带标签 id + 传输任务 id + 已传字节 + 总字节 + 瞬时速度字节/秒）。
    Progress(u64, u64, u64, u64, f64),
    /// 传输（上传 / 下载）完成（携带标签 id + 传输任务 id + 结果）。
    TransferDone(u64, u64, Result<(), Failure>),
    /// 后台传输 worker 的取消句柄已就绪（携带传输任务 id + 句柄）。
    TransferHandle(u64, AbortHandle),
    /// 取消某个传输任务（携带任务 id）。
    CancelTransfer(u64),
    /// 重试某个失败 / 已取消的传输（携带任务 id）。
    RetryTransfer(u64),
    /// 半成品文件清理结束（携带任务 id + 清理失败时留下的路径）。
    /// 从列表中移除某个已完成 / 失败的传输（携带任务 id）。
    RemoveTransfer(u64),
    /// 全局最大并发传输数发生变更：重新调度（调大即刻补位，调小不打断在跑任务）。
    ConcurrencyChanged,
    /// 某标签被关闭（携带标签 id）：中止其全部在跑任务并清空其队列，
    /// 避免僵尸记录与并发额度的永久泄漏。这是「额度只在 `TransferDone` 释放」的唯一例外。
    TabClosed(u64),
    /// 用系统文件管理器打开某个传输对应的本地文件所在文件夹（携带完整本地路径）。
    OpenContainingFolder(PathBuf),
}

/// 传输失败的分类，供上层判断「是否值得自动重试」。
///
/// 与错误文案分离：文案要按当前界面语言本地化，而分类是稳定语义，不能被翻译。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// 用户取消 / worker 被中止：永不自动重试。
    Cancelled,
    /// 承载这次传输的会话已终结：重试同一个客户端永远会立刻失败，**不**自动重试。
    ///
    /// 与 [`Transient`](Self::Transient) 的区别是性质而非程度：抖动时客户端还活着，退避后重试
    /// 有可能成功；会话终结时客户端已死，重试只是把预算烧在尸体上（每次零耗时，只有退避在等）。
    /// 用户要做的不是「等它自己好」，而是重新连接 —— 故行上的文案也必须说这件事。
    SessionGone,
    /// 瞬时故障（网络抖动、超时、通道断开）：自动重试有意义。
    Transient,
    /// 永久故障（权限不足、路径不存在、磁盘满）：只提供手动重试入口。
    Permanent,
    /// 无法判定：当前按可重试处理（实测中最常见的抖动本就无法细分）。
    Unknown,
}

/// 一次传输失败：稳定的分类 + 按当前语言本地化后的文案。
#[derive(Debug, Clone)]
pub struct Failure {
    /// 失败分类（决定是否自动重试）。
    pub kind: FailureKind,
    /// 用户可见的失败文案（已按当前界面语言本地化）。
    pub message: String,
}

impl Failure {
    /// 由核心层错误构造：分类下钻到核心层 [`CoreError::class`]，文案交给 `localize_error`。
    fn from_core(e: &CoreError) -> Self {
        Self::core(None, e)
    }

    /// 由核心层错误构造，并给文案加上场景前缀（如「创建远端目录失败」）。
    fn core_prefixed(prefix: String, e: &CoreError) -> Self {
        Self::core(Some(prefix), e)
    }

    /// 由核心层错误构造失败的共同实现。
    ///
    /// 会话终结时的文案**刻意不带引擎原文**：`session closed` 对用户没有信息量，它要说的是
    /// 「连接没了，重连再试」，故整句替换为 [`errors.session_gone`]；原始错误改记日志，
    /// 排查时照样看得到（这条路径此前是把引擎原文直接甩给用户看的）。
    fn core(prefix: Option<String>, e: &CoreError) -> Self {
        let kind = kind_of(e);
        let detail = if kind == FailureKind::SessionGone {
            warn!("transfer failed, session is gone: {e}");
            t!("errors.session_gone")
        } else {
            localize_error(e)
        };
        Self {
            kind,
            message: match prefix {
                Some(p) => format!("{p}: {detail}"),
                None => detail,
            },
        }
    }

    /// 用户取消 / worker 被中止导致的失败。
    fn cancelled(message: String) -> Self {
        Self {
            kind: FailureKind::Cancelled,
            message,
        }
    }
}

/// 把核心层的可重试性分类映射为本模块的 [`FailureKind`]。
fn kind_of(e: &CoreError) -> FailureKind {
    match e.class() {
        ErrorClass::Transient => FailureKind::Transient,
        ErrorClass::Permanent => FailureKind::Permanent,
        ErrorClass::SessionGone => FailureKind::SessionGone,
        ErrorClass::Unknowable => FailureKind::Unknown,
    }
}
/// 模块上行事件：仅通知父层，父层收到后才修改父状态或转发给其它模块。
///
/// 仅 `Clone`（不 `Debug`：含 `Box<Message>`，而 `Message` 未实现 `Debug`）。
#[derive(Clone)]
pub enum Event {
    /// 请求父层弹出 toast 通知（携带类型与文案）。
    Toast(ToastKind, String),
    /// 上传成功：请求父层刷新对应标签的 SFTP 目录（携带标签 id），由父层转发给 SFTP 模块。
    RefreshDir(u64),
    /// 自回路：把一条模块内部消息经父层派发回 `State::update`。
    ///
    /// 写操作（进度 / 传输完成）完成后需重新进入模块自身，但模块 `update` 只能经 `Event` 上行、
    /// 不能写父态；故把内部消息装进 `Emit` 上行，父层在 `Message::TransferEvent` 分支收到后再
    /// `self.transfer.update` 一次，形成自回路。
    Emit(Box<Message>),
}

/// 返回本地文件的所在文件夹；文件无父目录时回退为该路径本身（此时通常已是目录）。
fn containing_folder(local: &Path) -> PathBuf {
    local
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| local.to_path_buf())
}

/// 下载的暂存文件路径：与目标同目录、追加 `.part` 后缀（`archive.tar.gz` → `archive.tar.gz.part`）。
///
/// 刻意用 `OsString` 追加而非 `with_extension`：后者会把 `archive.tar.gz` 变成
/// `archive.tar.part`，丢掉一层后缀，改回真名时无从还原。同目录是关键——跨文件系统的
/// `rename` 会失败（用户可能把下载目录放在另一个挂载点上）。
/// 运行单个传输任务（上传 / 下载），以 `Task::stream` 把进度与完成事件流回流模块。
///
/// - 在独立 tokio 任务里调用核心层 `upload_with_progress` / `download_with_progress`，
///   进度回调经 mpsc 通道回传；
/// - 流任务按真实 I/O 间隔估算瞬时速度，逐条发射 `Progress`；
/// - 启动时先发射 `TransferHandle` 以登记取消句柄，结束发射 `TransferDone`。
///
/// 每个事件都包成 `Event::Emit(Box<Message>)`，由父层派发回 `State::update`，
/// 从而保证「子模块只经 Event 通信、不持有 `&mut App`」的架构约束。
fn run_transfer(
    tab_id: u64,
    tid: u64,
    direction: TransferDirection,
    client: Arc<SftpClient>,
    local: PathBuf,
    remote: String,
) -> Task<Event> {
    Task::stream(iced::stream::channel(
        64,
        move |mut output: futures::channel::mpsc::Sender<Event>| async move {
            // 进度通道：核心层同步回调（FnMut）产生的瞬时进度经 mpsc 回传给流任务，
            // 由流任务逐条包成 `Event::Emit(Message::Progress)` 上行，避免子模块直接写父态。
            let (mut prog_tx, mut prog_rx) = futures::channel::mpsc::channel::<(u64, u64, f64)>(64);

            // 进度回调：核心层只回传累计字节，瞬时速度在此按真实 I/O 间隔估算（EMA 平滑），
            // 经通道回传后由 UI 直接展示与估算 ETA。注意 `prog_tx` 作为唯一发送端被此闭包捕获、
            // 随 worker 任务结束（闭包丢弃）而释放，从而 `prog_rx` 必然关闭、下方转发循环必然退出——
            // 这是刻意设计：避免「原始发送端滞留外层作用域导致通道不关闭、TransferDone 永不发出」的
            // 死锁（那会让前序传输卡在 Active，进而阻塞其后所有排队传输，表现为「上传一直排队」）。
            let mut last = Instant::now();
            let mut last_bytes = 0u64;
            let mut speed_ema = 0.0f64;
            let cb = move |_name: &str, transferred: u64, total: u64| {
                let now = Instant::now();
                let dt = now.saturating_duration_since(last).as_secs_f64();
                let inst = if dt > 1e-6 {
                    (transferred.saturating_sub(last_bytes)) as f64 / dt
                } else {
                    speed_ema
                };
                // 指数滑动平均抑制瞬时抖动，读数更平滑。
                speed_ema = speed_ema * 0.7 + inst * 0.3;
                last = now;
                last_bytes = transferred;
                let _ = prog_tx.try_send((transferred, total, speed_ema));
            };

            // 在独立 tokio 任务里跑真实上传 / 下载。
            let worker = {
                let client = client.clone();
                let local = local.clone();
                let remote = remote.clone();
                let cb = cb;
                tokio::spawn(async move {
                    // 两个分支都直接产出 `Result<(), Failure>`：错误分类必须在跨界前保留，
                    // UI 要据此判断能否重试，故不能只传本地化文案。
                    match direction {
                        TransferDirection::Upload => {
                            // 文件夹上传时 `remote` 含子目录层级，先递归确保远端父目录存在，
                            // 否则 `upload_with_progress` 会因目标目录不存在而失败。
                            let parent = parent_path(&remote);
                            if !parent.is_empty()
                                && parent != remote
                                && let Err(e) = ensure_remote_dir(&client, &parent).await
                            {
                                // 建目录失败同样带分类：远端权限不足不该被反复重试，
                                // 会话终结同样该直接停下并给出「重连再试」（走共享的构造器，
                                // 文案规则与其它失败完全一致）。
                                return Err(Failure::core_prefixed(t!("sftp.mkdir_failed"), &e));
                            }

                            client
                                .upload_with_progress(&local, &remote, cb)
                                .await
                                .map_err(|e| Failure::from_core(&e))
                        }
                        TransferDirection::Download => client
                            .download_with_progress(&remote, &local, cb)
                            .await
                            .map_err(|e| Failure::from_core(&e)),
                    }
                })
            };

            // 登记取消句柄：父层据此可中止该 worker（见 `Message::CancelTransfer`）。
            let handle = worker.abort_handle();
            let _ = output
                .send(Event::Emit(Box::new(Message::TransferHandle(tid, handle))))
                .await;

            // 进度转发：逐条把进度通道的每一项包成 `Progress` 上行；worker 结束后随 `cb`
            // 丢弃唯一发送端、`prog_rx` 关闭，循环自然退出，随后才发完成事件——故进度的终值
            // 不会丢失，且 `TransferDone` 必然发出（队列得以继续推进）。
            let mut out_progress = output.clone();
            while let Some((transferred, total, speed)) = prog_rx.next().await {
                let _ = out_progress
                    .send(Event::Emit(Box::new(Message::Progress(
                        tab_id,
                        tid,
                        transferred,
                        total,
                        speed,
                    ))))
                    .await;
            }

            // 等 worker 得出终态。这里的 `Err` 只可能来自「worker 被 abort」（用户取消 / 关标签）
            // 或 worker 自身 panic——两者都不该自动重试，故统一归为「取消」分类；原始原因（含
            // panic 信息）留在日志里，界面文案对用户保持一致（三种取消入口同一句话）。
            let result = worker.await.unwrap_or_else(|e| {
                warn!("transfer {tid} worker ended abnormally: {e}");
                Err(Failure::cancelled(t!("app.canceled")))
            });

            let _ = output
                .send(Event::Emit(Box::new(Message::TransferDone(
                    tab_id, tid, result,
                ))))
                .await;
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::path::PathBuf;

    /// 在临时 tokio 运行时里把 `Task<Event>` 跑完并收集产出的事件（与 `masterpw` 的
    /// `run_events` 同源，只是 `Event` 落在此模块）。
    fn run_events(task: iced::Task<Event>) -> Vec<Event> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let mut stream = match iced_runtime::task::into_stream(task) {
                Some(s) => s,
                None => return Vec::new(),
            };
            let mut out = Vec::new();
            while let Some(action) = stream.next().await {
                if let iced_runtime::Action::Output(msg) = action {
                    out.push(msg);
                }
            }
            out
        })
    }

    /// 当前标签无 SFTP 通道的只读上下文（上传 / 下载应被拒并弹 toast）。
    ///
    /// 自动重试次数取默认值 2；需要验证「关闭自动重试」的用例自行覆盖该字段。
    /// `clients` 为空与 `client: None` 一致（无任何标签有通道），故启动判定仍会跳过所有项。
    fn no_client_ctx(tab_id: u64) -> Ctx {
        Ctx {
            tab_id,
            client: None,
            clients: HashMap::new(),
            remote_dir: "/home/user".to_string(),
            max_concurrent: 3,
        }
    }

    /// 构造一条无客户端的传输记录。
    ///
    /// 测试无法构造真实的 [`SftpClient`]（需要活的 SSH 通道，`SftpSession::new` 会先做版本握手），
    /// 故一律以 `client: None` 入队；凡需要真正「启动」的调度用例都直接调 [`State::admit`] 并传入
    /// 宽松判定 `|_, _| true`，绕开客户端要求、只验证额度与队列逻辑（`pump` 与 `admit` 的差别
    /// 仅在于启动判定与拉起 worker）。**代价**：`pump` 里「优先取标签当前客户端」这条规则无法
    /// 用单测覆盖，只能靠代码审查与手工验收（见设计文档 §19.4）。
    fn make_transfer(id: u64, direction: TransferDirection) -> Transfer {
        Transfer {
            id,
            direction,
            name: format!("f{id}.txt"),
            // 路径按 id 唯一：除了专门验 I4 的用例，任何调度用例都不该被「所有测试记录恰好指向
            // 同一个文件」干扰——那会让同靶互斥把它们全判成冲突，只有第一条能启动。
            local: PathBuf::from(format!("/tmp/f{id}.txt")),
            remote: format!("/home/user/f{id}.txt"),
            transferred: 0,
            total: 0,
            status: TransferStatus::Queued,
            error: None,
            speed: 0.0,
            client: None,
        }
    }

    /// 把若干条传输记录入队到指定标签，返回其 id 序列。
    fn enqueue(s: &mut State, tab_id: u64, ids: &[u64]) -> Vec<u64> {
        for &id in ids {
            s.per_tab
                .entry(tab_id)
                .or_default()
                .push(make_transfer(id, TransferDirection::Upload));
        }
        ids.to_vec()
    }

    /// 统计某状态下（可跨标签）的传输条数。
    fn count_status(s: &State, status: TransferStatus) -> usize {
        s.all_transfers()
            .iter()
            .filter(|t| t.status == status)
            .count()
    }

    /// 取某条传输的当前状态（跨标签查找）。
    fn status_of(s: &State, tid: u64) -> Option<TransferStatus> {
        s.find(tid).map(|t| t.status)
    }

    #[test]
    fn new_state_has_no_transfers() {
        let s = State::new();
        assert!(s.all_transfers().is_empty(), "新状态不应有任何传输");
        assert!(s.running.is_empty(), "新状态不应占用任何并发额度");
    }

    #[test]
    fn upload_without_client_emits_error_toast_and_enqueues_nothing() {
        let mut s = State::new();
        let events = run_events(s.update(
            Message::Upload(7, vec![(PathBuf::from("/tmp/a.txt"), "a.txt".to_string())]),
            &no_client_ctx(7),
        ));
        assert_eq!(events.len(), 1, "缺少客户端应只产出一个错误 toast");
        assert!(matches!(events[0], Event::Toast(ToastKind::Error, _)));
        assert!(s.all_transfers().is_empty(), "无客户端不应入队任何传输");
    }

    #[test]
    fn download_without_client_emits_error_toast_and_enqueues_nothing() {
        let mut s = State::new();
        let events = run_events(s.update(
            Message::Download(7, "a.txt".to_string(), PathBuf::from("/tmp/dl")),
            &no_client_ctx(7),
        ));
        assert_eq!(events.len(), 1, "缺少客户端应只产出一个错误 toast");
        assert!(matches!(events[0], Event::Toast(ToastKind::Error, _)));
        assert!(s.all_transfers().is_empty(), "无客户端不应入队任何传输");
    }

    #[test]
    fn cancel_marks_transfer_error_with_reason() {
        let mut s = State::new();
        s.per_tab
            .entry(7)
            .or_default()
            .push(make_transfer(1, TransferDirection::Upload));
        let _ = run_events(s.update(Message::CancelTransfer(1), &no_client_ctx(7)));

        let t = s
            .per_tab
            .get(&7)
            .unwrap()
            .iter()
            .find(|t| t.id == 1)
            .unwrap();
        assert_eq!(t.status, TransferStatus::Error, "取消应置为失败态");
        assert_eq!(
            t.error.as_deref(),
            Some(t!("app.canceled").as_str()),
            "取消应带取消原因"
        );
    }

    #[test]
    fn retry_resets_failed_transfer_to_queued() {
        let mut s = State::new();
        s.per_tab
            .entry(7)
            .or_default()
            .push(make_transfer(1, TransferDirection::Upload));
        let _ = run_events(s.update(Message::CancelTransfer(1), &no_client_ctx(7)));
        let _ = run_events(s.update(Message::RetryTransfer(1), &no_client_ctx(7)));

        let t = s
            .per_tab
            .get(&7)
            .unwrap()
            .iter()
            .find(|t| t.id == 1)
            .unwrap();
        assert_eq!(t.status, TransferStatus::Queued, "重试应回到排队态");
        assert!(t.error.is_none(), "重试应清除错误信息");
        assert_eq!(t.transferred, 0);
    }

    #[test]
    fn remove_deletes_transfer_from_queue() {
        let mut s = State::new();
        s.per_tab
            .entry(7)
            .or_default()
            .push(make_transfer(1, TransferDirection::Download));
        let _ = run_events(s.update(Message::RemoveTransfer(1), &no_client_ctx(7)));

        assert!(s.per_tab.get(&7).unwrap().is_empty(), "移除后队列应为空");
    }

    #[test]
    fn containing_folder_returns_parent_directory() {
        assert_eq!(
            containing_folder(Path::new("/home/user/dl/f.txt")),
            PathBuf::from("/home/user/dl"),
            "应返回文件的父目录"
        );
        assert_eq!(
            containing_folder(Path::new("/")),
            PathBuf::from("/"),
            "根目录无父目录时应回退为自身"
        );
    }

    #[test]
    fn progress_updates_transferred_total_and_speed() {
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Upload);
        t.status = TransferStatus::Active;
        s.per_tab.entry(7).or_default().push(t);

        let _ = run_events(s.update(Message::Progress(7, 1, 512, 1024, 12.5), &no_client_ctx(7)));

        let t = s
            .per_tab
            .get(&7)
            .unwrap()
            .iter()
            .find(|t| t.id == 1)
            .unwrap();
        assert_eq!(t.transferred, 512);
        assert_eq!(t.total, 1024);
        assert_eq!(t.speed, 12.5);
    }

    #[test]
    fn upload_done_success_emits_refresh_dir() {
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Upload);
        t.status = TransferStatus::Active;
        s.per_tab.entry(7).or_default().push(t);

        let events = run_events(s.update(Message::TransferDone(7, 1, Ok(())), &no_client_ctx(7)));
        assert!(
            events.iter().any(|e| matches!(e, Event::RefreshDir(7))),
            "上传成功应请求父层刷新目录"
        );
        let t = s
            .per_tab
            .get(&7)
            .unwrap()
            .iter()
            .find(|t| t.id == 1)
            .unwrap();
        assert_eq!(t.status, TransferStatus::Done, "上传成功应置为完成态");
    }

    #[test]
    fn download_done_success_does_not_emit_refresh_dir() {
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Download);
        t.status = TransferStatus::Active;
        s.per_tab.entry(7).or_default().push(t);

        let events = run_events(s.update(Message::TransferDone(7, 1, Ok(())), &no_client_ctx(7)));
        assert!(
            !events.iter().any(|e| matches!(e, Event::RefreshDir(_))),
            "下载成功不应触发刷新目录"
        );
        let t = s
            .per_tab
            .get(&7)
            .unwrap()
            .iter()
            .find(|t| t.id == 1)
            .unwrap();
        assert_eq!(t.status, TransferStatus::Done);
    }

    #[test]
    fn transfer_done_error_marks_failure_and_keeps_error() {
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Upload);
        t.status = TransferStatus::Active;
        s.per_tab.entry(7).or_default().push(t);

        let events = run_events(s.update(
            Message::TransferDone(
                7,
                1,
                Err(Failure {
                    kind: FailureKind::Permanent,
                    message: "磁盘满".to_string(),
                }),
            ),
            &no_client_ctx(7),
        ));
        assert!(
            !events.iter().any(|e| matches!(e, Event::RefreshDir(_))),
            "失败不应刷新目录"
        );
        let t = s
            .per_tab
            .get(&7)
            .unwrap()
            .iter()
            .find(|t| t.id == 1)
            .unwrap();
        assert_eq!(t.status, TransferStatus::Error);
        assert_eq!(t.error.as_deref(), Some("磁盘满"));
    }

    // ===== 并发调度器 =====

    #[test]
    fn admit_starts_up_to_limit_and_leaves_rest_queued() {
        let mut s = State::new();
        enqueue(&mut s, 7, &[1, 2, 3, 4, 5]);

        let started = s.admit(3, |_, _| true);

        assert_eq!(
            started.iter().map(|x| x.tid).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "应按 id 升序启动前 3 条"
        );
        assert_eq!(count_status(&s, TransferStatus::Active), 3);
        assert_eq!(count_status(&s, TransferStatus::Queued), 2);
        assert_eq!(s.running.len(), 3, "运行集合应恰好等于并发上限");
    }

    #[test]
    fn finishing_a_transfer_frees_a_slot_for_the_next_queued() {
        let mut s = State::new();
        enqueue(&mut s, 7, &[1, 2, 3, 4]);
        s.admit(3, |_, _| true);

        // 完成事件是唯一的额度释放点（不变量 I2），随后同一分支补位。
        let _ = run_events(s.update(Message::TransferDone(7, 1, Ok(())), &no_client_ctx(7)));
        assert_eq!(s.running.len(), 2, "完成后应释放一个额度");
        // `update` 内部的 pump 因测试记录没有客户端而无法真正启动，故显式再调度一次验证补位。
        let started = s.admit(3, |_, _| true);
        assert_eq!(started.iter().map(|x| x.tid).collect::<Vec<_>>(), vec![4]);
        assert_eq!(s.running.len(), 3, "补位后额度应重新占满");
        assert_eq!(status_of(&s, 4), Some(TransferStatus::Active));
    }

    #[test]
    fn raising_the_limit_backfills_immediately() {
        let mut s = State::new();
        enqueue(&mut s, 7, &[1, 2, 3, 4, 5]);
        s.admit(3, |_, _| true);
        assert_eq!(s.running.len(), 3);

        // 并发上限 3 → 5：同一帧内立即补上 2 条。
        let started = s.admit(5, |_, _| true);
        assert_eq!(
            started.iter().map(|x| x.tid).collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert_eq!(s.running.len(), 5);
        assert_eq!(count_status(&s, TransferStatus::Queued), 0);
    }

    #[test]
    fn lowering_the_limit_does_not_interrupt_running_transfers() {
        let mut s = State::new();
        enqueue(&mut s, 7, &[1, 2, 3, 4, 5]);
        s.admit(5, |_, _| true);
        assert_eq!(s.running.len(), 5);

        // 上限 5 → 2：不打断在跑任务（不 abort、不改状态），也不补位。
        let started = s.admit(2, |_, _| true);
        assert!(started.is_empty(), "额度已满（2 < 5）时不应再启动任何任务");
        assert_eq!(
            count_status(&s, TransferStatus::Active),
            5,
            "在跑任务不受影响"
        );

        // 随任务陆续完成，收敛到新上限：完成 3 条后只剩 2 条在跑。
        for tid in [1, 2, 3] {
            let _ = run_events(s.update(Message::TransferDone(7, tid, Ok(())), &no_client_ctx(7)));
        }
        assert_eq!(s.running.len(), 2, "应自然收敛到新上限");
    }

    #[test]
    fn admitted_transfers_follow_global_fifo_across_tabs() {
        let mut s = State::new();
        // 两个标签交错入队：id 单调递增即等于入队序，启动序不应受 HashMap 迭代序影响。
        s.per_tab
            .entry(7)
            .or_default()
            .push(make_transfer(1, TransferDirection::Upload));
        s.per_tab
            .entry(9)
            .or_default()
            .push(make_transfer(2, TransferDirection::Download));
        s.per_tab
            .entry(7)
            .or_default()
            .push(make_transfer(3, TransferDirection::Upload));
        s.per_tab
            .entry(9)
            .or_default()
            .push(make_transfer(4, TransferDirection::Download));

        let started = s.admit(2, |_, _| true);

        assert_eq!(
            started.iter().map(|x| x.tid).collect::<Vec<_>>(),
            vec![1, 2],
            "跨标签应按 id 升序（全局 FIFO），且各自归属正确的标签"
        );
        assert_eq!(started[0].tab_id, 7);
        assert_eq!(started[1].tab_id, 9);
    }

    #[test]
    fn unstartable_transfer_is_skipped_without_blocking_others() {
        let mut s = State::new();
        enqueue(&mut s, 7, &[1, 2, 3]);

        // 视为第 1 条不具备启动条件（如客户端缺失）：它应保持排队，后两条照常启动，
        // 且循环不会因它而死转。
        let started = s.admit(3, |_, t| t.id != 1);

        assert_eq!(
            started.iter().map(|x| x.tid).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(
            status_of(&s, 1),
            Some(TransferStatus::Queued),
            "不可启动项应保持排队态"
        );
        assert_eq!(s.running.len(), 2);
    }

    #[test]
    fn limit_is_clamped_to_the_supported_range() {
        let mut s = State::new();
        enqueue(&mut s, 7, &[1, 2, 3]);

        // 0 与超上限都取自配置，理论上已被裁剪；此处验证调度器自身的兜底。
        let _ = s.admit(0, |_, _| true);
        assert_eq!(s.running.len(), MIN_CONCURRENT, "下限兜底为 1");

        let started = s.admit(usize::MAX, |_, _| true);
        assert_eq!(s.running.len(), 3, "上限受队列长度限制");
        assert_eq!(started.len(), 2);
    }

    #[test]
    fn cancel_releases_the_slot_exactly_once() {
        let mut s = State::new();
        enqueue(&mut s, 7, &[1]);
        s.admit(3, |_, _| true);
        assert_eq!(s.running.len(), 1);

        // 取消**只**中止 worker，不在此处改状态、也不动 `running`：abort 之后外层 stream 任务
        // 会立刻醒来发出 `TransferDone`，由那个唯一释放点统一落地「失败 + 归还额度 + 清理半成品」。
        let _ = run_events(s.update(Message::CancelTransfer(1), &no_client_ctx(7)));
        assert_eq!(
            s.running.len(),
            1,
            "取消路径不得提前释放额度（否则 TransferDone 会重复释放）"
        );
        assert_eq!(
            status_of(&s, 1),
            Some(TransferStatus::Active),
            "在跑的传输由随后的 TransferDone 落地，本处刻意不改状态"
        );

        let _ = run_events(s.update(
            Message::TransferDone(7, 1, Err(Failure::cancelled("canceled".to_string()))),
            &no_client_ctx(7),
        ));
        assert_eq!(s.running.len(), 0, "取消后应恰好释放一次");
        assert_eq!(status_of(&s, 1), Some(TransferStatus::Error));
    }

    #[test]
    fn late_result_does_not_clobber_a_manual_retry() {
        let mut s = State::new();
        enqueue(&mut s, 7, &[1]);
        s.admit(3, |_, _| true);

        // 用户取消 → 立刻重试：该行回到排队态，但旧 worker 尚未收尾（仍在 running 中）。
        let _ = run_events(s.update(Message::CancelTransfer(1), &no_client_ctx(7)));
        let _ = run_events(s.update(Message::RetryTransfer(1), &no_client_ctx(7)));
        assert_eq!(status_of(&s, 1), Some(TransferStatus::Queued));
        assert_eq!(s.running.len(), 1, "旧 worker 仍在跑，额度未释放");

        // 旧 worker 的迟到结果只应释放额度，不得把排队态改回失败态。
        let _ = run_events(s.update(
            Message::TransferDone(7, 1, Err(Failure::cancelled("canceled".to_string()))),
            &no_client_ctx(7),
        ));
        assert_eq!(
            status_of(&s, 1),
            Some(TransferStatus::Queued),
            "迟到的结果不得吞掉用户的重试"
        );
        assert_eq!(s.running.len(), 0);

        // 额度已释放，重试立即生效。
        let started = s.admit(3, |_, _| true);
        assert_eq!(started.iter().map(|x| x.tid).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn retry_waits_for_the_old_worker_to_release_its_slot() {
        let mut s = State::new();
        enqueue(&mut s, 7, &[1, 2]);
        s.admit(1, |_, _| true);
        assert_eq!(s.running.len(), 1);

        // 第 1 条仍在跑，第 2 条不得越过它启动（不变量 I3：同一 tid 不得有两个 worker）。
        let _ = run_events(s.update(Message::RetryTransfer(2), &no_client_ctx(7)));
        assert_eq!(status_of(&s, 2), Some(TransferStatus::Queued));
        assert_eq!(count_status(&s, TransferStatus::Active), 1);
    }

    #[test]
    fn removing_a_running_transfer_is_refused() {
        let mut s = State::new();
        enqueue(&mut s, 7, &[1]);
        s.admit(3, |_, _| true);

        let _ = run_events(s.update(Message::RemoveTransfer(1), &no_client_ctx(7)));

        assert!(
            status_of(&s, 1).is_some(),
            "运行中的传输不得被直接移除（否则 worker 成为孤儿且额度永久漂移）"
        );
        assert_eq!(s.running.len(), 1);
    }

    #[test]
    fn tab_closed_clears_its_queue_and_reclaims_slots() {
        let mut s = State::new();
        enqueue(&mut s, 7, &[1, 2]);
        enqueue(&mut s, 9, &[3]);
        s.admit(3, |_, _| true);
        assert_eq!(s.running.len(), 3);

        let _ = run_events(s.update(Message::TabClosed(7), &no_client_ctx(7)));

        assert!(
            !s.per_tab.contains_key(&7),
            "关标签应清空该标签的传输队列，避免僵尸行留在全局面板上"
        );
        assert!(
            !s.running.contains(&1) && !s.running.contains(&2),
            "该标签的额度必须归还，否则会永久降低全局并发度"
        );
        assert_eq!(s.running.len(), 1, "其它标签的在跑任务不受影响");
        assert_eq!(status_of(&s, 3), Some(TransferStatus::Active));
    }

    #[test]
    fn clearing_a_closed_tab_does_not_resurrect_a_queued_transfer() {
        let mut s = State::new();
        enqueue(&mut s, 7, &[1]);
        let _ = run_events(s.update(Message::TabClosed(7), &no_client_ctx(7)));

        // 标签消失后迟到的完成事件只应无害地忽略（不 panic、不重建队列）。
        let _ = run_events(s.update(Message::TransferDone(7, 1, Ok(())), &no_client_ctx(7)));
        assert!(!s.per_tab.contains_key(&7));
        assert!(s.running.is_empty());
    }

    #[test]
    fn queue_capacity_is_bounded() {
        let mut s = State::new();
        let ids: Vec<u64> = (1..=MAX_QUEUE as u64).collect();
        enqueue(&mut s, 7, &ids);

        assert!(!s.has_queue_capacity(7), "达到上限后不应再接受新条目");
        assert!(s.has_queue_capacity(9), "其它标签的容量互不影响");
    }

    // ===== 自动重试与半成品清理 =====

    /// 派发一条消息但**不驱动**返回的任务。
    ///
    /// 自动重试的退避定时器会真的等满首轮退避（[`BASE_BACKOFF`] = 1.5 s，测试里没有可用的时间
    /// 加速），故凡只断言状态机的用例都走这里——返回的任务被丢弃即不会被轮询，定时器自然不会启动。
    /// 需要观察上行事件（进展 / 重试到点）的用例仍用 [`run_events`]。
    fn dispatch(s: &mut State, msg: Message, ctx: &Ctx) {
        drop(s.update(msg, ctx));
    }

    /// 入队一条处于指定状态的传输。
    fn enqueue_with_status(s: &mut State, tab_id: u64, id: u64, status: TransferStatus) {
        let mut t = make_transfer(id, TransferDirection::Upload);
        t.status = status;
        s.per_tab.entry(tab_id).or_default().push(t);
    }

    #[test]
    fn cancel_operates_on_a_row_of_any_tab() {
        // 面板跨标签聚合展示：非活动标签的行也必须能被取消 / 重试 / 移除，
        // 否则按钮点了没反应（这些操作不依赖 `ctx.tab_id`）。
        let mut s = State::new();
        enqueue_with_status(&mut s, 9, 1, TransferStatus::Queued);

        dispatch(&mut s, Message::CancelTransfer(1), &no_client_ctx(7));

        assert_eq!(status_of(&s, 1), Some(TransferStatus::Error));
    }

    #[test]
    fn cancelling_a_settled_transfer_leaves_it_untouched() {
        // 迟到的取消消息不得把「已完成」改写成「已取消」，也不得覆盖失败原因。
        let mut s = State::new();
        enqueue_with_status(&mut s, 7, 1, TransferStatus::Done);
        enqueue_with_status(&mut s, 7, 2, TransferStatus::Error);
        s.find_mut(2).unwrap().error = Some("磁盘满".to_string());

        dispatch(&mut s, Message::CancelTransfer(1), &no_client_ctx(7));
        dispatch(&mut s, Message::CancelTransfer(2), &no_client_ctx(7));

        assert_eq!(status_of(&s, 1), Some(TransferStatus::Done));
        assert_eq!(status_of(&s, 2), Some(TransferStatus::Error));
        assert_eq!(s.find(2).unwrap().error.as_deref(), Some("磁盘满"));
    }
}
