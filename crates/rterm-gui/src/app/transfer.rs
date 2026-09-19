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
//! # 失败重试
//!
//! 失败的分类来自核心层 [`CoreError::class`]，映射为 [`FailureKind`]。**可自动重试**的是
//! `Transient` 与 `Unknown`（网络抖动最常见的形态恰恰无法细分，不重试等于把成本转嫁给用户）；
//! `Cancelled`（用户取消）、`Permanent`（权限不足 / 路径不存在 / 磁盘满）与
//! `SessionGone`（会话已终结）只保留手动重试入口。
//!
//! [`FailureKind::SessionGone`] 单列的理由是**性质**而非程度：抖动时客户端还活着，退避后重试
//! 有可能成功；会话终结时这个 `SftpClient` 已经死了（`session closed` / `sender dropped`），
//! 重试只会立刻失败，唯一的出路是重新连接。曾把它并进 `Transient`，实测症状是「重试次数耗尽
//! 后再恢复网络仍报 session closed」—— 预算全烧在一具尸体上，行上还留着引擎原文。
//!
//! 次数取 `[transfer] retry_attempts`（默认 2、0 = 关闭），延迟为
//! `min(BASE_BACKOFF * 2^attempt, MAX_BACKOFF)`。**退避期间不占并发额度**：`WaitingRetry`
//! 既不参与调度序也不计入 `running`，否则 N 个任务同时失败会让整条队列空转最长 8 秒。
//! 到点由 [`retry_timer`] 发 [`Message::RetryDue`] 把该行转回排队态；该消息**必须幂等**——
//! 到点时若该行已不是 `WaitingRetry`（用户已取消 / 手动重试 / 移除 / 标签已关闭），直接丢弃。
//!
//! # 传输用哪个客户端
//!
//! **启动 / 重试时取该标签此刻的客户端**（`Ctx::client_for`，父层每轮从 SFTP 视图快照），
//! 而不是 [`Transfer::client`] 里入队那一刻捕获的那份 —— 后者只在标签已无客户端时兜底。
//! 理由：会话重建后同一标签会换上新通道，若继续用记录里那份，排队中的项与用户的手动重试
//! 都会打在旧会话上，而那个会话可能早就终结了。启动时还会把解析出的客户端回写进记录，
//! 使收尾阶段的远端清理也走当前通道。
//!
//! # 半成品的清理
//!
//! 清理的靶子**只可能是本模块创建的暂存文件**（见 [`staging_path`]），这正是关标签时敢对
//! 所有「留下痕迹」的行清理一次的底气。清理失败时下载侧把路径记在 `Transfer.partial` 上
//! 提示用户手动处理；上传侧只记日志（残留不致命，重传的 `create()` 会截断）。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::{Subscription, Task};

use crate::app::tasks::{ensure_remote_dir, join_path, parent_path};
use crate::i18n::localize_error;
use crate::state::{ToastKind, Transfer, TransferDirection, TransferStatus};
use crate::t;
use futures::{SinkExt, StreamExt};
use log::{debug, warn};
use rterm_config::{MAX_CONCURRENT, MIN_CONCURRENT};
use rterm_core::{CoreError, ErrorClass, SftpClient};
use std::path::Path;
use tokio::task::AbortHandle;

/// 单标签队列的条目上限：文件夹上传会把目录展开成「一文件一条」
/// （见 `app::tasks::collect_upload_items`），拖入一个十万文件的目录会瞬间产生十万条
/// `Transfer`（每条含 `String + PathBuf`）并让面板每帧全量遍历。超限时**拒绝并入并提示**，
/// 不静默丢弃，使用户知道哪些文件没进去。
const MAX_QUEUE: usize = 1000;

/// 进度上报的最小时间间隔：同一帧内只有最后一个进度值有意义，故按时间节流。
const REPORT_INTERVAL: Duration = Duration::from_millis(100);

/// 进度上报的最小字节增量（1 MiB）：与 [`REPORT_INTERVAL`] 二者满足其一即可上报。
const REPORT_BYTES: u64 = 1024 * 1024;

/// 自动重试的基础退避时长：1.5 s。
///
/// 这个值由两条实测要求夹出来，两边的约束都是硬的：
///
/// 1. **不能太小**。琥珀态的可见时长由「故障持续多久」决定，但**故障比退避还短**时，退避就是
///    全部可见时长。0.5 s 起步时反馈原话是「只显示一瞬间的琥珀色、一瞬间的重试次数」；
///    一行 11px 的「正在重试（第 1/2 次） · 尚未收到数据」要读得完，至少需要 1.5 s 量级。
/// 2. **不能太大**。退避同时也是「网络回来后多久才恢复」的延迟。2 s + 4 s（默认 2 次重试）
///    意味着最坏要等 6 s 才见分晓，反馈明确要求「不用等五秒」。
///
/// 1.5 s 起（1.5/3/6/8 s，上限 [`MAX_BACKOFF`]）两边都满足：最短可见 1.5 s 读得完，
/// 默认预算耗尽的最坏等待 1.5 + 3 = 4.5 s 在 5 s 以内。
///
/// 可见性**不再**靠拉长退避兜底：琥珀会一直挂到「第一个字节到达」（见
/// [`crate::transfer_panel`] 的 `is_retrying_without_data`），长断网时的琥珀时长由故障本身
/// 决定，与这个常量无关。
const BASE_BACKOFF: Duration = Duration::from_millis(1500);

/// 退避倍率：每次重试在上一次的基础上翻倍。
const BACKOFF_FACTOR: u32 = 2;

/// 退避上限：8 s。既避免次数较多时等待过长，也兜住「手工调大次数上限」后出现分钟级等待。
const MAX_BACKOFF: Duration = Duration::from_secs(8);

/// 删除本地半成品的重试次数（见 [`cleanup_partial`]）。
const CLEANUP_ATTEMPTS: u32 = 3;

/// 删除本地半成品的重试间隔：只在真失败时才消耗，正常路径一次即成。
const CLEANUP_RETRY_DELAY: Duration = Duration::from_millis(120);

/// 第 `attempt` 次自动重试前的退避时长（`attempt` 从 0 起）：
/// `min(BASE_BACKOFF * BACKOFF_FACTOR^attempt, MAX_BACKOFF)`。
///
/// 指数增长用 `checked_pow` + 饱和乘法兜底：次数来自配置，理论上已被裁剪，但此处不该依赖它。
fn backoff_delay(attempt: u32) -> Duration {
    let factor = BACKOFF_FACTOR.checked_pow(attempt).unwrap_or(u32::MAX);
    BASE_BACKOFF.saturating_mul(factor).min(MAX_BACKOFF)
}

/// 传输模块只读上下文：父层在路由每条消息前构造，模块据此读取当前标签的 SFTP 客户端与
/// 远端目录，以及全局传输策略（并发上限、自动重试次数），但绝不写回父状态。
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
    /// 单次传输的自动重试次数上限（`0` = 关闭自动重试）。
    ///
    /// 与 `max_concurrent` 同样**不缓存**：它在「失败那一刻」被读取，故设置界面改动后
    /// 下一次失败即用新值，无需任何重调度消息。
    pub retry_attempts: u32,
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

/// 传输模块私有状态：每标签传输队列 + 任务 id 分配器 + 取消句柄注册表 + 运行中任务集合。
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
                        attempts: 0,
                        not_before: None,
                        partial: None,
                        error: None,
                        speed: 0.0,
                        client: Some(client.clone()),
                        keep_staging: false,
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
                    // **第一个字节到达 = 这次故障结束了**：琥珀态（等待重试 / 重试中尚无数据）
                    // 到此为止，上一轮的失败原因也随之收走——它的使命是回答「为什么在重试」，
                    // 现在数据在流，这一行不再是「出了问题」的行。
                    //
                    // 必须限定 `Active`：worker 被中止后可能还有一条迟到的进度回来，那时行已是
                    // `Error`，抹掉它的失败原因会让用户无从判断。`transferred > 0` 则把核心层在
                    // 每次尝试开头发的那个 `(0, total)` 排除在外（那正是「尚无数据」的信号）。
                    if t.status == TransferStatus::Active && transferred > 0 {
                        t.error = None;
                    }
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
                // 需自动重试时记下「退避时长 + 作业参数」；最终失败时改记「待清理的作业」。
                let mut retry: Option<(Duration, u64)> = None;
                let mut cleanup: Option<Job> = None;
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
                            t.not_before = None;
                            t.partial = None;
                            // 成功即暂存已被用到终点（下载的 `.part` 已改名到目标）：清掉「这里面
                            // 是完整数据」的标记，不让它跟着一条已完成记录继续走。
                            t.keep_staging = false;
                            if matches!(t.direction, TransferDirection::Upload) {
                                relist = true;
                            }
                        }
                        Err(f) => {
                            warn!("transfer {tid} failed ({:?}): {}", f.kind, f.message);
                            t.error = Some(f.message);
                            if f.kind.is_retryable() && t.attempts < ctx.retry_attempts {
                                // 退避期间**不占额度**（关键设计）：上面已经 `running.remove`，
                                // 该行接下来进入 `WaitingRetry` 也不参与调度，额度立刻让给排队项；
                                // 否则 N 个任务同时失败会让整条队列空转最长 `MAX_BACKOFF`。
                                let delay = backoff_delay(t.attempts);
                                t.attempts += 1;
                                t.not_before = Some(Instant::now() + delay);
                                t.status = TransferStatus::WaitingRetry;
                                retry = Some((delay, tid));
                            } else {
                                // 不可重试或次数耗尽：落地为失败。
                                t.status = TransferStatus::Error;
                                t.not_before = None;
                                t.partial = None;
                                // 「暂存里是完整数据」这件事要落到行上：关标签 / 移除行等清理
                                // 路径据此跳过它（见 `leaves_partial_on_disk`）。
                                t.keep_staging = f.keep_staging;
                                // 让「继续」退化成重下整个文件（见模块文档「最终失败时暂存的去留」）。
                                // 上传侧不在保留之列：它失败留下的是一份被截断的**真名**远端文件
                                // （阶段 B 才会改成远端 `.part` 暂存），留着会被误认为「文件在这儿」。
                                // 取消则两侧都清理：用户明确不要了。
                                let keep = f.keep_staging
                                    || (t.direction == TransferDirection::Download
                                        && f.kind != FailureKind::Cancelled);
                                if !keep {
                                    cleanup = Some(job_from(tab_id, t));
                                }
                            }
                        }
                    }
                }
                let mut tasks = vec![self.pump(ctx)];
                if let Some(job) = cleanup {
                    tasks.push(cleanup_partial(job));
                }
                if let Some((delay, tid)) = retry {
                    tasks.push(retry_timer(delay, tid));
                }
                if relist {
                    // 上传成功：请求父层刷新该标签目录（父层再经 `Message::Sftp` 派发给 SFTP 模块）。
                    tasks.push(Task::done(Event::RefreshDir(tab_id)));
                }
                Task::batch(tasks)
            }
            Message::TransferHandle(tid, handle) => {
                // 标签已关闭 / 额度已归还（标签关闭时清理过）的 worker 不该再登记句柄：
                // 直接中止，避免孤儿 worker 空跑到连接自然断开。
                if self.running.contains(&tid) {
                    self.abort_handles.insert(tid, handle);
                } else {
                    handle.abort();
                }
                Task::none()
            }
            Message::CancelTransfer(id) => {
                // 「取消」按行所处状态分三种，处置方式不同，不能一律改状态：
                // 1. 有 worker 在跑：只 `abort()`，状态留给随后的 `TransferDone`——
                //    在那里统一「落地为取消 + 归还额度 + 清理半成品」（不变量 I2）；
                // 2. 等待重试中：既无 worker 也无额度，须在此直接落地；但**磁盘上有半成品**
                //    （上一轮真跑过），故同时触发清理；
                // 3. 排队中：从未启动，本地 / 远端都没有半成品，**不可清理**——那会删掉用户
                //    原有的同名文件。
                let was_running = self.running.contains(&id);
                if let Some(handle) = self.abort_handles.remove(&id) {
                    handle.abort();
                }
                if was_running {
                    return Task::none();
                }
                let status = self.find(id).map(|t| t.status);
                let cleanup = self
                    .job_of(id)
                    .filter(|_| self.find(id).is_some_and(leaves_partial_on_disk));
                // 只对「排队中 / 等待重试」的行落地。`Done` / `Error` 的行本就不该有取消按钮，
                // 但消息可能迟到（连点 / 与服务端竞态），此时把已完成的任务改写成「已取消」
                // 是货真价实的错误显示，故在此收口。
                // 按 id 全局查找：面板跨标签聚合展示，非活动标签的行同样要能被取消。
                if matches!(
                    status,
                    Some(TransferStatus::Queued | TransferStatus::WaitingRetry)
                ) && let Some(t) = self.find_mut(id)
                {
                    t.status = TransferStatus::Error;
                    t.error = Some(t!("app.canceled"));
                    t.not_before = None;
                    t.partial = None;
                }
                match cleanup {
                    Some(job) => cleanup_partial(job),
                    None => Task::none(),
                }
            }
            Message::RetryTransfer(id) => {
                // 手动重试 = 重新开始：回到排队态、清零进度，并立即补位（与自动重试走同一条调度路径）。
                if let Some(t) = self.find_mut(id) {
                    t.status = TransferStatus::Queued;
                    t.attempts = 0;
                    t.not_before = None;
                    t.partial = None;
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
                    debug!("refused to remove a running transfer: {id}");
                    return Task::none();
                }
                let cleanup = self
                    .job_of(id)
                    .filter(|_| self.find(id).is_some_and(leaves_partial_on_disk));
                self.remove(id);
                let mut tasks = vec![self.pump(ctx)];
                if let Some(job) = cleanup {
                    tasks.push(cleanup_partial(job));
                }
                Task::batch(tasks)
            }
            Message::RetryDue(tid) => {
                // 退避到点：**必须幂等**。到点时该行可能已不是 `WaitingRetry`（用户已取消 /
                // 手动重试 / 移除，或所属标签已关闭），那说明这次重试早已作废，直接丢弃——
                // 绝不能无条件把它推回排队态（会把用户刚取消的任务又重新跑起来）。
                if !self
                    .find(tid)
                    .is_some_and(|t| t.status == TransferStatus::WaitingRetry)
                {
                    debug!("dropped a stale retry timer for transfer {tid}");
                    return Task::none();
                }
                if let Some(t) = self.find_mut(tid) {
                    t.status = TransferStatus::Queued;
                    t.not_before = None;
                    t.speed = 0.0;
                }
                self.pump(ctx)
            }
            Message::CleanupDone(tid, partial) => {
                // 半成品清理的异步结果：仅清理失败时带路径，用于提示用户手动处理。
                match self.find_mut(tid) {
                    Some(t) => t.partial = partial,
                    // 行已不存在——关标签时发起的清理，或用户在清理期间点了移除。此时面板上没有
                    // 能承载提示的行，改用 toast：否则「有文件没删掉」这件事会完全无声。
                    None => {
                        if let Some(path) = partial {
                            return Task::done(Event::Toast(
                                ToastKind::Error,
                                t!("transfer.partial_left", path => path),
                            ));
                        }
                    }
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
                //
                // 在磁盘上留有痕迹的行（在跑 / 等待重试）在关标签即放弃该
                // 传输时都要清理——否则用户下次打开下载目录会看到一堆 `.part`。
                let rows = self.per_tab.remove(&tab_id).unwrap_or_default();
                // 先把「清理所需参数」取出来（后面 `rows` 会被消费），再逐行归还额度。
                let cleanups: Vec<Job> = rows
                    .iter()
                    .filter(|t| leaves_partial_on_disk(t))
                    .map(|t| job_from(tab_id, t))
                    .collect();
                for t in &rows {
                    if let Some(handle) = self.abort_handles.remove(&t.id) {
                        handle.abort();
                    }
                    self.running.remove(&t.id);
                }
                let mut tasks = vec![self.pump(ctx)];
                tasks.extend(cleanups.into_iter().map(cleanup_partial));
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

    /// 重新调度：在并发额度允许的范围内，按全局 FIFO 拉起尽可能多的排队项。
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

    /// 该传输所属标签 id（跨标签查找；id 全局唯一，故可用于「非活动标签的行也要能操作」）。
    fn tab_of(&self, tid: u64) -> Option<u64> {
        self.per_tab
            .iter()
            .find(|(_, queue)| queue.iter().any(|t| t.id == tid))
            .map(|(tab_id, _)| *tab_id)
    }

    /// 按 id 构造该传输的收尾参数（清理半成品用）；记录已不存在时返回 `None`。
    ///
    /// 与 [`take_startable`](Self::take_startable) 的区别：不改状态、不要求客户端存在，
    /// 只把「删哪个文件、用哪个客户端」取出来交给 [`cleanup_partial`]。
    fn job_of(&self, tid: u64) -> Option<Job> {
        let tab_id = self.tab_of(tid)?;
        Some(job_from(tab_id, self.find(tid)?))
    }

    /// 按 id 从所属队列中删除该传输（跨标签）；删掉返回 `true`。
    fn remove(&mut self, tid: u64) -> bool {
        for queue in self.per_tab.values_mut() {
            if let Some(pos) = queue.iter().position(|t| t.id == tid) {
                queue.remove(pos);
                return true;
            }
        }
        false
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
            attempts: 0,
            not_before: None,
            partial: None,
            error: None,
            speed: 0.0,
            client: Some(client),
            keep_staging: false,
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
    /// 重试某个失败 / 已取消的传输（携带任务 id）。手动重试会把自动重试计数清零。
    RetryTransfer(u64),
    /// 自动重试的退避到点（携带任务 id）：把该行从「等待重试」转回排队态。
    ///
    /// 由 [`retry_timer`] 定时发回；**必须幂等**——到点时该行可能已被取消 / 手动重试 / 移除。
    RetryDue(u64),
    /// 半成品文件清理结束（携带任务 id + 清理失败时留下的路径）。
    CleanupDone(u64, Option<String>),
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
    /// 收尾时**不得删除**暂存文件。
    ///
    /// 目前只有一种来源：下载内容已完整落盘、但改名到目标路径失败（见 [`finalize_download`]）。
    /// 此时暂存文件里是用户唯一的一份数据，把它当作半成品删掉等于毁掉一次成功的下载——
    /// 文案里已带上路径，让用户自己改名即可。
    pub keep_staging: bool,
}

impl FailureKind {
    /// 是否值得自动重试。
    ///
    /// `Unknown` 也纳入：实测中最常见的失败是网络抖动，而抖动常表现为无法细分的形态
    /// （如 `UnexpectedBehavior("sender dropped")`），不重试等于把成本转嫁给用户；
    /// 代价是可能在永久性错误上多试几次——有指数退避兜底，开销可接受。
    /// [`SessionGone`](Self::SessionGone) 明确排除：那具尸体不会因为多等几秒就活过来。
    fn is_retryable(self) -> bool {
        matches!(self, Self::Transient | Self::Unknown)
    }
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
            keep_staging: false,
        }
    }

    /// 用户取消 / worker 被中止导致的失败。
    fn cancelled(message: String) -> Self {
        Self {
            kind: FailureKind::Cancelled,
            message,
            keep_staging: false,
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
fn staging_path(local: &Path) -> PathBuf {
    let mut staged = local.as_os_str().to_os_string();
    staged.push(".part");
    PathBuf::from(staged)
}
///   故要清理；上传侧在失败那一刻已删过远端残留，这里再判一次是空操作（删除失败只记日志）。
///
/// `keep_staging` 的行**一概为假**：那时暂存里是**完整数据**（下载已落全、只是改名失败），
/// 是用户唯一的一份，任何清理路径都必须跳过——宁可留一个文件，也不能毁掉一次成功的下载。
fn leaves_partial_on_disk(t: &Transfer) -> bool {
    if t.keep_staging {
        return false;
    }
    match t.status {
        TransferStatus::Active | TransferStatus::WaitingRetry => true,
        TransferStatus::Error => t.direction == TransferDirection::Download,
        TransferStatus::Queued | TransferStatus::Done => false,
    }
}

/// 下载完成后把暂存文件改名到目标路径。
///
/// 目标已存在时先删再改名：Windows 的 `rename` 不会覆盖已存在的目标（Unix 会），而用户
/// 此前已在覆盖确认框里明确同意覆盖，故删除是有授权的。
///
/// 改名失败时调用方**必须保留**暂存文件（见 [`Failure::keep_staging`]）——能走到这里说明
/// 内容已经完整落盘，只是名字不对，删掉就等于毁掉一次成功的下载。
async fn finalize_download(staging: &Path, target: &Path) -> std::io::Result<()> {
    match tokio::fs::rename(staging, target).await {
        Ok(()) => Ok(()),
        Err(first) => {
            if tokio::fs::metadata(target).await.is_err() {
                return Err(first);
            }
            tokio::fs::remove_file(target).await?;
            tokio::fs::rename(staging, target).await
        }
    }
}

/// 退避定时器：`delay` 之后发一条 [`Message::RetryDue`] 回到模块自身。
///
/// 刻意**不新增订阅**：复用既有的 `Event::Emit` 自回路，`RetryDue` 与 `Progress` /
/// `TransferDone` 走同一条父层转发路径，模块不需要为此持有任何计时状态。
///
/// **`sleep` 必须写在 `async` 块内部**：`State::update` 在 iced 的事件循环里同步执行，那里
/// 没有 tokio 运行时上下文，`tokio::time::sleep(..)` 在**构造时**就要向运行时取计时器句柄，
/// 外层构造会直接 panic（`there is no reactor running`）。放进 `async` 块则推迟到 future
/// 被轮询时构造——那时已运行在 iced 的 tokio 执行器里（`run_transfer` 里的 `tokio::spawn`
/// 同理，故它写在 channel 闭包内部而非 `update` 里）。
fn retry_timer(delay: Duration, tid: u64) -> Task<Event> {
    Task::perform(async move { tokio::time::sleep(delay).await }, move |()| {
        Event::Emit(Box::new(Message::RetryDue(tid)))
    })
}

/// 清理取消 / 移除 / 关标签之后留下的半成品文件，结果经 [`Message::CleanupDone`] 回到模块自身。
///
/// 起点，由行上的「继续下载」接着用；只有取消、移除行、关标签这三类「这条传输到此为止」的
/// 路径才清理（见模块文档「最终失败时暂存的去留」）。
///
/// - **上传**：删除远端残留。残留本身不致命（重传的 `create()` 会截断），故删除失败只记日志；
/// - **下载**：删除本地 `.part` 暂存文件（[`staging_path`]）。半个本地文件若以真实文件名留下，
///   会被用户误认为「已下载完成」，是真正危险的一侧，故确实要删。删除前先确认它是**文件**再删
///   ——失败可能发生在 `File::create` 之前（如远端 `open` 被拒），此时磁盘上根本没有暂存文件，
///   若不加判定就会误删同名目录。删除失败时把路径回报给界面（`Transfer.partial`）提示用户
///   手动处理。
///
/// 清理靶子只可能是本模块创建的暂存文件（从不是用户原有的文件）：这正是关标签时敢对所有
/// 「留下痕迹」的行清理一次的底气，见 [`Message::TabClosed`]。
///
/// 本函数**只应在「该任务确实跑过一轮」时调用**（排队中被取消的任务从未碰过任何文件），
fn cleanup_partial(job: Job) -> Task<Event> {
    let tid = job.tid;
    Task::perform(
        async move {
            match job.direction {
                TransferDirection::Upload => {
                    if let Some(client) = &job.client
                        && let Err(e) = client.remove_file(&job.remote).await
                    {
                        debug!("failed to remove partial remote file {}: {e}", job.remote);
                    }
                    None
                }
                TransferDirection::Download => {
                    let staged = staging_path(&job.local);
                    // 先确认它是**普通文件**：失败可能发生在 `File::create` 之前（如远端 `open`
                    // 被拒），此时磁盘上根本没有暂存文件；若不加判定就会把同名目录交给 `remove_file`。
                    let is_file = tokio::fs::metadata(&staged)
                        .await
                        .map(|m| m.is_file())
                        .unwrap_or(false);
                    if !is_file {
                        return None;
                    }
                    // 重试几次再判失败：关标签时发起的清理会与「worker 正在被 `abort`」重叠，
                    // 而 Windows 删除仍被打开的文件会失败（Unix 不会）。多试几次即可覆盖那个窗口，
                    // 仍失败才如实把路径回报给用户手动处理。
                    let mut error = String::new();
                    for attempt in 0..CLEANUP_ATTEMPTS {
                        match tokio::fs::remove_file(&staged).await {
                            Ok(()) => return None,
                            Err(e) => error = e.to_string(),
                        }
                        if attempt + 1 < CLEANUP_ATTEMPTS {
                            tokio::time::sleep(CLEANUP_RETRY_DELAY).await;
                        }
                    }
                    debug!(
                        "failed to remove partial local file {}: {error}",
                        staged.display()
                    );
                    Some(staged.display().to_string())
                }
            }
        },
        move |partial| Event::Emit(Box::new(Message::CleanupDone(tid, partial))),
    )
}

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
            //
            // 速度 EMA 在**每次**回调时更新（保精度、保平滑），但只在满足节流条件时才真正跨线程
            // 上报：核心层每写满 64 KiB 就回调一次，并发 N 后消息量会 ×N，而其中绝大多数是同一帧内
            // 被覆盖掉的中间值。终值必须放行，否则进度条会停在 99%（完成事件依赖通道关闭后发出）。
            let mut last = Instant::now();
            let mut last_bytes = 0u64;
            let mut speed_ema = 0.0f64;
            let mut last_report = last;
            let mut reported_bytes = 0u64;
            let mut first = true;
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

                let due = first
                    || transferred == total
                    || now.saturating_duration_since(last_report) >= REPORT_INTERVAL
                    || transferred.saturating_sub(reported_bytes) >= REPORT_BYTES;
                if !due {
                    return;
                }
                first = false;
                last_report = now;
                reported_bytes = transferred;
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
                        TransferDirection::Download => {
                            // 先写同目录的 `.part` 暂存文件，成功后再改名到目标路径（浏览器同款）：
                            // 「下载到一半的文件」因此不会以真实文件名出现。暂存文件也是清理的靶子，
                            // 而它只可能由本模块创建（见 `staging_path`）。
                            let staged = staging_path(&local);
                            match client.download_with_progress(&remote, &staged, cb).await {
                                Ok(()) => match finalize_download(&staged, &local).await {
                                    Ok(()) => Ok(()),
                                    Err(e) => {
                                        // 内容已完整落盘、只是改不了名：保留暂存文件并把路径告诉
                                        // 用户，绝不能当作半成品删掉（那是用户唯一的一份数据）。
                                        warn!(
                                            "downloaded {remote} but failed to rename {} -> {}: {e}",
                                            staged.display(),
                                            local.display()
                                        );
                                        Err(Failure {
                                            kind: FailureKind::Permanent,
                                            message: t!(
                                                "transfer.rename_failed",
                                                path => staged.display().to_string()
                                            ),
                                            keep_staging: true,
                                        })
                                    }
                                },
                                Err(e) => Err(Failure::from_core(&e)),
                            }
                        }
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
    use rterm_core::CoreErrorKind;
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
            retry_attempts: 2,
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
            attempts: 0,
            not_before: None,
            partial: None,
            error: None,
            speed: 0.0,
            client: None,
            keep_staging: false,
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
                    keep_staging: false,
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

    /// 造一个指定分类的失败（文案在测试里不重要）。
    fn failure(kind: FailureKind) -> Failure {
        Failure {
            kind,
            message: format!("{kind:?}"),
            keep_staging: false,
        }
    }

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
    fn backoff_grows_exponentially_and_is_capped() {
        // 首次退避 1.5s：行上的「正在重试（第 n/N 次）」要读得完，且最坏等待须在 5s 以内。
        assert_eq!(backoff_delay(0), BASE_BACKOFF, "首次重试等 1.5s");
        assert_eq!(backoff_delay(1), Duration::from_secs(3));
        assert_eq!(backoff_delay(2), Duration::from_secs(6));
        assert_eq!(backoff_delay(3), MAX_BACKOFF, "第 4 次起触顶（12s → 8s）");
        assert_eq!(backoff_delay(4), MAX_BACKOFF);
        assert_eq!(
            backoff_delay(u32::MAX),
            MAX_BACKOFF,
            "次数极大时不得溢出（配置虽已裁剪，调度器不该依赖它）"
        );
    }

    #[test]
    fn transient_failure_schedules_a_retry_instead_of_failing() {
        let mut s = State::new();
        enqueue_with_status(&mut s, 7, 1, TransferStatus::Active);
        let ctx = no_client_ctx(7);
        let started = Instant::now();

        // 只断言状态：丢弃返回的任务，退避定时器不启动。
        dispatch(
            &mut s,
            Message::TransferDone(7, 1, Err(failure(FailureKind::Transient))),
            &ctx,
        );

        let t = s.find(1).unwrap();
        assert_eq!(
            t.status,
            TransferStatus::WaitingRetry,
            "瞬时故障应进入等待重试而不是直接失败"
        );
        assert_eq!(t.attempts, 1, "第一次重试的计数应为 1");
        let deadline = t.not_before.expect("等待重试必须带下次重试时刻");
        let left = deadline.saturating_duration_since(started);
        assert!(
            (BASE_BACKOFF..MAX_BACKOFF).contains(&left),
            "首次退避应为 1.5s 量级，实际 {left:?}"
        );
        assert_eq!(s.running.len(), 0, "退避期间不占并发额度");
    }

    #[test]
    fn unknown_failure_is_also_retried() {
        let mut s = State::new();
        enqueue_with_status(&mut s, 7, 1, TransferStatus::Active);

        dispatch(
            &mut s,
            Message::TransferDone(7, 1, Err(failure(FailureKind::Unknown))),
            &no_client_ctx(7),
        );

        assert_eq!(
            status_of(&s, 1),
            Some(TransferStatus::WaitingRetry),
            "无法细分的抖动也值得重试（实测里最常见的就是它）"
        );
    }

    #[test]
    fn permanent_and_cancelled_failures_are_never_retried() {
        for kind in [
            FailureKind::Permanent,
            FailureKind::Cancelled,
            // 会话终结同样不重试：那具尸体不会因为多等几秒就活过来（设计文档 §19）。
            FailureKind::SessionGone,
        ] {
            let mut s = State::new();
            enqueue_with_status(&mut s, 7, 1, TransferStatus::Active);

            dispatch(
                &mut s,
                Message::TransferDone(7, 1, Err(failure(kind))),
                &no_client_ctx(7),
            );

            let t = s.find(1).unwrap();
            assert_eq!(t.status, TransferStatus::Error, "{kind:?} 不该自动重试");
            assert_eq!(t.attempts, 0, "{kind:?} 不该消耗重试计数");
            assert!(t.not_before.is_none(), "{kind:?} 不该留下退避时刻");
        }
    }

    #[test]
    fn a_dead_session_is_classified_as_such_and_explained_instead_of_echoed() {
        // 第五轮实测：重试耗尽后再恢复网络，行上仍报 `session closed`。那条文案来自
        // `RawSftpSession::send`，含义是「这个客户端已经死了」，必须与网络抖动分开 ——
        // 分开后既不再空烧重试预算，行上给的也不再是引擎术语。
        let e = CoreError::sftp(
            CoreErrorKind::ReadRemote,
            std::io::Error::other("session closed"),
        );
        let f = Failure::from_core(&e);

        assert_eq!(f.kind, FailureKind::SessionGone);
        assert!(!f.kind.is_retryable(), "会话已终结不该自动重试");
        // 断言「不含引擎原文」而不是「等于某句中文」：文案本身要能随语言改。
        assert!(
            !f.message.contains("session closed"),
            "不该把引擎原文甩给用户：{}",
            f.message
        );
        assert!(!f.message.is_empty());
    }

    #[test]
    fn a_scenario_prefix_still_applies_to_a_dead_session() {
        // 建目录失败这条路径此前是把 `{e}` 直接拼进文案的，会话终结时会漏出引擎原文；
        // 现在与其它失败走同一个构造器，规则一致（分类识别 + 不泄漏术语）。
        let e = CoreError::sftp(
            CoreErrorKind::CreateDir,
            std::io::Error::other("session closed"),
        );
        let f = Failure::core_prefixed("创建远端目录失败".to_string(), &e);

        assert_eq!(f.kind, FailureKind::SessionGone);
        assert!(
            f.message.starts_with("创建远端目录失败"),
            "场景前缀要保留：{}",
            f.message
        );
        assert!(
            !f.message.contains("session closed"),
            "会话终结的文案不该带引擎原文：{}",
            f.message
        );
    }

    #[test]
    fn zero_retry_budget_disables_automatic_retry() {
        let mut s = State::new();
        enqueue_with_status(&mut s, 7, 1, TransferStatus::Active);
        let mut ctx = no_client_ctx(7);
        ctx.retry_attempts = 0;

        dispatch(
            &mut s,
            Message::TransferDone(7, 1, Err(failure(FailureKind::Transient))),
            &ctx,
        );

        assert_eq!(
            status_of(&s, 1),
            Some(TransferStatus::Error),
            "retry_attempts = 0 即关闭自动重试"
        );
    }

    #[test]
    fn auto_retry_stops_once_the_budget_is_exhausted() {
        let mut s = State::new();
        // 已用满 2 次自动重试（attempts = 2）后又失败一次：必须落地为失败。
        let mut t = make_transfer(1, TransferDirection::Upload);
        t.status = TransferStatus::Active;
        t.attempts = 2;
        s.per_tab.entry(7).or_default().push(t);

        dispatch(
            &mut s,
            Message::TransferDone(7, 1, Err(failure(FailureKind::Transient))),
            &no_client_ctx(7),
        );

        let t = s.find(1).unwrap();
        assert_eq!(t.status, TransferStatus::Error, "次数耗尽后应失败");
        assert_eq!(t.attempts, 2, "不再自增");
        assert!(t.not_before.is_none());
    }

    #[test]
    fn retry_budget_is_read_per_failure_from_the_context() {
        // 上限经 `Ctx` 注入（不缓存进 State）：同样的失败消息，上限为 3 时应继续重试。
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Upload);
        t.status = TransferStatus::Active;
        t.attempts = 2;
        s.per_tab.entry(7).or_default().push(t);
        let mut ctx = no_client_ctx(7);
        ctx.retry_attempts = 3;

        dispatch(
            &mut s,
            Message::TransferDone(7, 1, Err(failure(FailureKind::Transient))),
            &ctx,
        );

        let t = s.find(1).unwrap();
        assert_eq!(t.status, TransferStatus::WaitingRetry);
        assert_eq!(t.attempts, 3);
        // 第三次重试的退避：1.5s × 2² = 6s。
        assert_eq!(backoff_delay(2), Duration::from_secs(6));
    }

    #[test]
    fn waiting_retry_does_not_block_the_queue() {
        let mut s = State::new();
        enqueue_with_status(&mut s, 7, 1, TransferStatus::WaitingRetry);
        enqueue_with_status(&mut s, 7, 2, TransferStatus::Queued);

        // 额度只有 1：等待重试的项不参与调度，后一条应拿到这个额度，
        // 而不是让整条队列跟着它的退避一起空转。
        let started = s.admit(1, |_, _| true);

        assert_eq!(started.iter().map(|j| j.tid).collect::<Vec<_>>(), vec![2]);
        assert_eq!(status_of(&s, 1), Some(TransferStatus::WaitingRetry));
    }

    #[test]
    fn a_transient_failure_comes_back_as_retry_due_and_is_promoted() {
        let mut s = State::new();
        enqueue_with_status(&mut s, 7, 1, TransferStatus::Active);

        // 真跑一次定时器（首次退避 [`BASE_BACKOFF`]，故这条用例真的等 1.5s）：验证
        // 「TransferDone → 定时器 → RetryDue」这条回路。
        let events = run_events(s.update(
            Message::TransferDone(7, 1, Err(failure(FailureKind::Transient))),
            &no_client_ctx(7),
        ));
        let retry_due = events.into_iter().find_map(|e| match e {
            Event::Emit(m) => match *m {
                Message::RetryDue(tid) => Some(tid),
                _ => None,
            },
            _ => None,
        });
        assert_eq!(retry_due, Some(1), "退避到点应回一条 RetryDue");

        // **失败原因刻意保留**：接下来那一段仍是琥珀态（重试中尚无数据），行上要能回答
        // 「为什么在重试」；它由首个数据到达时的 `Progress` 收走。
        let _ = run_events(s.update(Message::RetryDue(1), &no_client_ctx(7)));
        let t = s.find(1).unwrap();
        assert_eq!(t.status, TransferStatus::Queued);
        assert!(t.not_before.is_none());
        assert_eq!(t.transferred, 0);
        assert_eq!(
            t.error.as_deref(),
            Some("Transient"),
            "失败原因要留到首个数据到达为止"
        );
    }

    #[test]
    fn the_first_byte_ends_the_retry_state_and_drops_the_failure_reason() {
        // 「第一个字节到达 = 这次故障结束」的落地：核心层在每次尝试读写循环**之前**先回调一次
        // `(0, total)`（`core::sftp::copy_with_progress`），随后每写完一块回调 `(n, total)`。
        // 故 `transferred == 0` 就是「尚无数据」的事实依据，面板据此把该行保持为琥珀
        // （见 `crate::transfer_panel::is_retrying_without_data`）。
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Download);
        t.status = TransferStatus::Active;
        t.attempts = 1;
        t.error = Some("连接已断开".to_string());
        s.per_tab.entry(7).or_default().push(t);

        // 尝试开头那个 `(0, total)`：不能把失败原因清掉，否则琥珀态当场失去解释。
        let _ = run_events(s.update(Message::Progress(7, 1, 0, 4096, 0.0), &no_client_ctx(7)));
        let t = s.find(1).unwrap();
        assert_eq!(t.transferred, 0, "尚无数据仍应保持 0");
        assert_eq!(
            t.error.as_deref(),
            Some("连接已断开"),
            "尚无数据时不得清失败原因"
        );

        // 第一块真的写下来了：琥珀态结束，失败原因随之收走。
        let _ = run_events(s.update(Message::Progress(7, 1, 65536, 4096, 1.0), &no_client_ctx(7)));
        let t = s.find(1).unwrap();
        assert_eq!(t.transferred, 65536);
        assert!(t.error.is_none(), "数据已到，这一行不再是「出了问题」的行");
    }

    #[test]
    fn a_late_progress_never_wipes_the_reason_of_a_failed_row() {
        // worker 被中止后可能还有一条迟到的进度回来（取消 / 关闭标签）。那时行已是 `Error`，
        // 若不加 `status == Active` 的限定，它会把失败原因抹掉，用户就无从判断发生了什么。
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Download);
        t.status = TransferStatus::Error;
        t.error = Some("权限不足".to_string());
        s.per_tab.entry(7).or_default().push(t);

        let _ = run_events(s.update(Message::Progress(7, 1, 512, 4096, 1.0), &no_client_ctx(7)));

        let t = s.find(1).unwrap();
        assert_eq!(
            t.error.as_deref(),
            Some("权限不足"),
            "迟到的进度不得抹掉失败原因"
        );
    }

    #[test]
    fn retry_due_is_ignored_when_the_row_is_no_longer_waiting() {
        // 到点前用户取消：状态已是 `Error`，迟到的 RetryDue 必须被丢弃，
        // 否则用户刚取消的任务会被重新跑起来。
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Upload);
        t.status = TransferStatus::WaitingRetry;
        t.attempts = 1;
        t.not_before = Some(Instant::now());
        s.per_tab.entry(7).or_default().push(t);

        dispatch(&mut s, Message::CancelTransfer(1), &no_client_ctx(7));
        assert_eq!(status_of(&s, 1), Some(TransferStatus::Error));

        let _ = run_events(s.update(Message::RetryDue(1), &no_client_ctx(7)));
        assert_eq!(
            status_of(&s, 1),
            Some(TransferStatus::Error),
            "迟到的 RetryDue 不得把已取消的传输推回排队"
        );

        // 该行被移除后到达的 RetryDue 同样只是无害忽略（不得 panic）。
        dispatch(&mut s, Message::RemoveTransfer(1), &no_client_ctx(7));
        assert!(s.find(1).is_none());
        let _ = run_events(s.update(Message::RetryDue(1), &no_client_ctx(7)));
    }

    #[test]
    fn retry_due_after_tab_closed_is_a_no_op() {
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Upload);
        t.status = TransferStatus::WaitingRetry;
        s.per_tab.entry(7).or_default().push(t);

        dispatch(&mut s, Message::TabClosed(7), &no_client_ctx(7));
        let _ = run_events(s.update(Message::RetryDue(1), &no_client_ctx(7)));

        assert!(!s.per_tab.contains_key(&7), "关标签后不得被迟到消息重建");
        assert!(s.running.is_empty());
    }

    #[test]
    fn manual_retry_resets_the_automatic_retry_counter() {
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Upload);
        t.status = TransferStatus::WaitingRetry;
        t.attempts = 2;
        t.not_before = Some(Instant::now());
        s.per_tab.entry(7).or_default().push(t);

        dispatch(&mut s, Message::RetryTransfer(1), &no_client_ctx(7));

        let t = s.find(1).unwrap();
        assert_eq!(t.status, TransferStatus::Queued);
        assert_eq!(t.attempts, 0, "手动重试视为重新开始，故手动重试次数不限");
        assert!(t.not_before.is_none());
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

    #[test]
    fn cancelling_a_queued_transfer_does_not_trigger_cleanup() {
        // 排队中从未启动，磁盘上**没有**半成品；若对此触发清理，会删掉用户原有的同名文件。
        let mut s = State::new();
        enqueue_with_status(&mut s, 7, 1, TransferStatus::Queued);

        let events = run_events(s.update(Message::CancelTransfer(1), &no_client_ctx(7)));

        assert_eq!(status_of(&s, 1), Some(TransferStatus::Error));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::Emit(m) if matches!(**m, Message::CleanupDone(..)))),
            "排队中的取消不该产生任何清理动作"
        );
    }

    #[test]
    fn cancelling_a_transfer_awaiting_retry_triggers_cleanup() {
        // 等待重试的项已经跑过一轮，磁盘上有半成品：放弃它时必须清理。
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Download);
        t.status = TransferStatus::WaitingRetry;
        t.not_before = Some(Instant::now());
        s.per_tab.entry(7).or_default().push(t);

        let events = run_events(s.update(Message::CancelTransfer(1), &no_client_ctx(7)));

        // 该测试记录的本地路径 `/tmp/f.txt` 并不存在，故清理走的是「无需清理」的成功路径，
        // 但 CleanupDone 一定会回到模块（证明清理确实被触发）。
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Emit(m) if matches!(**m, Message::CleanupDone(1, _)))),
            "等待重试中的取消应触发半成品清理"
        );
        assert_eq!(status_of(&s, 1), Some(TransferStatus::Error));
    }

    #[test]
    fn removing_a_transfer_awaiting_retry_triggers_cleanup() {
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Download);
        t.status = TransferStatus::WaitingRetry;
        s.per_tab.entry(7).or_default().push(t);

        let events = run_events(s.update(Message::RemoveTransfer(1), &no_client_ctx(7)));

        assert!(s.find(1).is_none());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Emit(m) if matches!(**m, Message::CleanupDone(1, _)))),
            "移除等待重试中的项同样应清理半成品"
        );
    }

    #[test]
    fn cleanup_done_records_the_path_only_when_it_failed() {
        let mut s = State::new();
        enqueue_with_status(&mut s, 7, 1, TransferStatus::Error);

        dispatch(
            &mut s,
            Message::CleanupDone(1, Some("/tmp/half.bin".to_string())),
            &no_client_ctx(7),
        );
        assert_eq!(s.find(1).unwrap().partial.as_deref(), Some("/tmp/half.bin"));

        // 清理成功的回报（None）不留下任何痕迹；对已不存在的 id 回报也只是忽略。
        dispatch(&mut s, Message::CleanupDone(1, None), &no_client_ctx(7));
        assert!(s.find(1).unwrap().partial.is_none());
        dispatch(
            &mut s,
            Message::CleanupDone(99, Some("/tmp/x".to_string())),
            &no_client_ctx(7),
        );
    }

    /// 阶段二的清理会真的删本地文件，故用专属临时目录独立验证（不碰 `paths` 沙箱）。
    fn cleanup_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rterm_transfer_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("应能创建临时目录");
        dir
    }

    #[test]
    fn cleanup_removes_only_the_staged_download() {
        let dir = cleanup_dir("cleanup_dl");
        // 目标名的文件可能是**用户原有的**（下载前已存在、用户在覆盖确认框里同意覆盖），
        // 暂存文件才是本次下载写下的半个文件。清理只准动后者——这正是 `.part` 暂存的意义。
        let target = dir.join("half.bin");
        std::fs::write(&target, b"user data").expect("应能写入用户原有文件");
        let staged = staging_path(&target);
        std::fs::write(&staged, b"half").expect("应能写入半成品");

        let events = run_events(cleanup_partial(Job {
            tab_id: 7,
            tid: 1,
            direction: TransferDirection::Download,
            local: target.clone(),
            remote: "/remote/half.bin".to_string(),
            client: None,
        }));

        assert!(!staged.exists(), "暂存的半成品必须被删除");
        assert!(target.exists(), "同名目标文件是用户的，绝不能碰");
        assert_eq!(
            std::fs::read(&target).expect("应能读回"),
            b"user data",
            "用户原有文件的内容不得被改动"
        );
        assert!(
            matches!(&events[..], [Event::Emit(m)] if matches!(**m, Message::CleanupDone(1, None))),
            "清理成功应回报 None（无残留路径）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_never_touches_a_directory_or_a_missing_file() {
        let dir = cleanup_dir("cleanup_guard");
        // 删除是破坏性操作，宁可漏删不可误删：暂存路径上若恰好是个**目录**（或压根不存在），
        // 清理必须原地放弃，而不是把目录交给 `remove_file` 硬删。
        let not_a_file = dir.join("not_a_file");
        let staged_dir = staging_path(&not_a_file);
        std::fs::create_dir_all(&staged_dir).expect("应能创建子目录");
        let missing = dir.join("missing.bin");

        for local in [not_a_file.clone(), missing.clone()] {
            let _ = run_events(cleanup_partial(Job {
                tab_id: 7,
                tid: 1,
                direction: TransferDirection::Download,
                local: local.clone(),
                remote: "/remote/x".to_string(),
                client: None,
            }));
        }

        assert!(staged_dir.is_dir(), "同名目录不得被清理掉");
        assert!(
            !staging_path(&missing).exists(),
            "不存在的暂存文件保持不存在"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_skips_the_remote_side_when_the_client_is_gone() {
        // 上传侧的清理需要活着的客户端；没有客户端时只记日志，不得 panic、也不回报残留
        // （上传残留不致命：重传的 `create()` 会截断）。
        let events = run_events(cleanup_partial(Job {
            tab_id: 7,
            tid: 1,
            direction: TransferDirection::Upload,
            local: PathBuf::from("/tmp/f.txt"),
            remote: "/home/user/f.txt".to_string(),
            client: None,
        }));

        assert!(
            matches!(&events[..], [Event::Emit(m)] if matches!(**m, Message::CleanupDone(1, None)))
        );
    }

    #[test]
    fn a_final_failure_starts_a_cleanup() {
        // 不可重试的失败要清理半成品（否则半个本地文件会一直躺着）。
        let mut s = State::new();
        enqueue_with_status(&mut s, 7, 1, TransferStatus::Active);

        let events = run_events(s.update(
            Message::TransferDone(7, 1, Err(failure(FailureKind::Permanent))),
            &no_client_ctx(7),
        ));

        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Emit(m) if matches!(**m, Message::CleanupDone(1, _)))),
            "最终失败应触发半成品清理"
        );
    }

    #[test]
    fn a_failed_download_keeps_its_partial_but_a_failed_upload_cleans_up() {
        // 它要救的那类故障，失败那一刻才是用户想接着下的时刻）；上传失败仍清理，因为它留下的是
        // 半个**真名**远端文件，用户会以为「文件在这儿」。
        let mut s = State::new();
        enqueue_with_status(&mut s, 7, 1, TransferStatus::Active); // Upload
        let mut dl = make_transfer(2, TransferDirection::Download);
        dl.status = TransferStatus::Active;
        s.per_tab.entry(7).or_default().push(dl);

        let dl_events = run_events(s.update(
            Message::TransferDone(7, 2, Err(failure(FailureKind::SessionGone))),
            &no_client_ctx(7),
        ));
        assert_eq!(s.find(2).map(|t| t.status), Some(TransferStatus::Error));
        assert!(
            !dl_events
                .iter()
                .any(|e| matches!(e, Event::Emit(m) if matches!(**m, Message::CleanupDone(2, _)))),
            "下载失败必须保留暂存，否则「继续」会退化成重下整个文件"
        );

        let up_events = run_events(s.update(
            Message::TransferDone(7, 1, Err(failure(FailureKind::SessionGone))),
            &no_client_ctx(7),
        ));
        assert_eq!(s.find(1).map(|t| t.status), Some(TransferStatus::Error));
        assert!(
            up_events
                .iter()
                .any(|e| matches!(e, Event::Emit(m) if matches!(**m, Message::CleanupDone(1, _)))),
            "上传失败要清掉远端残留，不能留下半个真名文件"
        );
    }

    #[test]
    fn cancelling_a_download_still_cleans_up() {
        // 「保留」只针对**失败**：取消是用户明确不要了，两侧都清理。
        let mut s = State::new();
        let mut dl = make_transfer(1, TransferDirection::Download);
        dl.status = TransferStatus::Active;
        s.per_tab.entry(7).or_default().push(dl);

        let events = run_events(s.update(
            Message::TransferDone(7, 1, Err(failure(FailureKind::Cancelled))),
            &no_client_ctx(7),
        ));

        assert_eq!(s.find(1).map(|t| t.status), Some(TransferStatus::Error));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Emit(m) if matches!(**m, Message::CleanupDone(1, _)))),
            "取消的下载要清理暂存"
        );
    }

    #[test]
    fn a_failed_rename_marks_the_row_so_its_data_is_never_cleaned_up() {
        // 改名失败 = 内容已完整落盘。`keep_staging` 必须落到**行上**（而不只是这一次的失败值），
        // 否则随后的关标签 / 移除会把用户唯一的一份数据当半成品删掉。
        let mut s = State::new();
        let mut t = make_transfer(1, TransferDirection::Download);
        t.status = TransferStatus::Active;
        s.per_tab.entry(7).or_default().push(t);

        let events = run_events(s.update(
            Message::TransferDone(
                7,
                1,
                Err(Failure {
                    kind: FailureKind::Permanent,
                    message: "rename failed".to_string(),
                    keep_staging: true,
                }),
            ),
            &no_client_ctx(7),
        ));

        let t = s.find(1).expect("行应仍在");
        assert!(t.keep_staging, "完整数据的标记必须落到行上");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::Emit(m) if matches!(**m, Message::CleanupDone(1, _)))),
            "完整数据不得被当成半成品删掉"
        );
    }

    #[test]
    fn leaves_partial_on_disk_covers_exactly_the_states_that_own_a_file() {
        let dl = |status| {
            let mut t = make_transfer(1, TransferDirection::Download);
            t.status = status;
            t
        };
        assert!(leaves_partial_on_disk(&dl(TransferStatus::Active)));
        assert!(leaves_partial_on_disk(&dl(TransferStatus::WaitingRetry)));
        assert!(leaves_partial_on_disk(&dl(TransferStatus::Error)));
        // 排队中从未碰过文件；完成态的暂存已改名到目标。
        assert!(!leaves_partial_on_disk(&dl(TransferStatus::Queued)));
        assert!(!leaves_partial_on_disk(&dl(TransferStatus::Done)));

        // 上传失败留下的是半个真名文件，在失败那一刻就被清理了，故不算「留有痕迹」。
        let mut up = make_transfer(1, TransferDirection::Upload);
        up.status = TransferStatus::Error;
        assert!(!leaves_partial_on_disk(&up), "上传失败已清理完毕");
        let mut up_waiting = make_transfer(1, TransferDirection::Upload);
        up_waiting.status = TransferStatus::WaitingRetry;
        assert!(
            leaves_partial_on_disk(&up_waiting),
            "等待重试的上传：远端还留着上一轮写了一半的文件，关标签时要清理"
        );

        // 完整数据（改名失败）在任何状态下都不清理：它比状态优先。
        let mut keep = dl(TransferStatus::Error);
        keep.keep_staging = true;
        assert!(!leaves_partial_on_disk(&keep));
        let mut keep_waiting = dl(TransferStatus::WaitingRetry);
        keep_waiting.keep_staging = true;
        assert!(!leaves_partial_on_disk(&keep_waiting));
    }

    #[test]
    fn staging_path_appends_a_suffix_without_dropping_extensions() {
        // 用 `with_extension` 会把 archive.tar.gz 变成 archive.tar.part（丢一层后缀），
        // 改回真名时无从还原，故必须是纯追加。
        assert_eq!(
            staging_path(Path::new("/tmp/archive.tar.gz")),
            PathBuf::from("/tmp/archive.tar.gz.part")
        );
        assert_eq!(
            staging_path(Path::new("/tmp/no_ext")),
            PathBuf::from("/tmp/no_ext.part")
        );
    }

    #[test]
    fn finalize_download_moves_the_staged_file_into_place() {
        let dir = cleanup_dir("finalize_dl");
        let target = dir.join("archive.tar.gz");
        let staged = staging_path(&target);
        std::fs::write(&staged, b"complete payload").expect("应能写入暂存文件");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("应能建立测试运行时");
        rt.block_on(finalize_download(&staged, &target))
            .expect("同目录改名应成功");

        assert!(!staged.exists(), "改名后暂存文件不应再存在");
        assert_eq!(
            std::fs::read(&target).expect("应能读回目标文件"),
            b"complete payload"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finalize_download_overwrites_an_existing_target() {
        // 用户在覆盖确认框里已同意覆盖；Windows 的 `rename` 不会覆盖已存在的目标，
        // 故先删再改名，保证三平台行为一致。
        let dir = cleanup_dir("finalize_over");
        let target = dir.join("f.bin");
        let staged = staging_path(&target);
        std::fs::write(&target, b"old").expect("应能写入旧文件");
        std::fs::write(&staged, b"new").expect("应能写入暂存文件");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("应能建立测试运行时");
        rt.block_on(finalize_download(&staged, &target))
            .expect("覆盖改名应成功");

        assert_eq!(std::fs::read(&target).expect("应能读回"), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_download_that_failed_to_rename_keeps_its_staged_file() {
        // 内容已完整落盘、只是改不了名：暂存文件是用户唯一的一份数据，绝不能当作半成品删掉。
        let mut s = State::new();
        enqueue_with_status(&mut s, 7, 1, TransferStatus::Active);

        let events = run_events(s.update(
            Message::TransferDone(
                7,
                1,
                Err(Failure {
                    kind: FailureKind::Permanent,
                    message: "rename failed: /tmp/f.bin.part".to_string(),
                    keep_staging: true,
                }),
            ),
            &no_client_ctx(7),
        ));

        assert_eq!(status_of(&s, 1), Some(TransferStatus::Error));
        assert!(
            s.find(1)
                .unwrap()
                .error
                .as_deref()
                .unwrap()
                .contains(".part"),
            "失败文案必须带上暂存文件路径，用户才知道去哪找"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::Emit(m) if matches!(**m, Message::CleanupDone(..)))),
            "保留暂存文件时不得发起清理"
        );
    }

    #[test]
    fn tab_close_cleans_up_rows_that_left_files_behind() {
        // F：关标签即放弃该标签的全部传输，留下半成品的行（在跑 / 等待重试）要清理掉。
        // 排队中（没碰过磁盘）与已完成（暂存文件已改名）的行不产生清理动作。
        let mut s = State::new();
        enqueue_with_status(&mut s, 7, 1, TransferStatus::Active);
        enqueue_with_status(&mut s, 7, 2, TransferStatus::WaitingRetry);
        enqueue_with_status(&mut s, 7, 3, TransferStatus::Queued);
        enqueue_with_status(&mut s, 7, 4, TransferStatus::Done);
        enqueue_with_status(&mut s, 7, 5, TransferStatus::Error);
        s.running.insert(1);

        let events = run_events(s.update(Message::TabClosed(7), &no_client_ctx(7)));

        let cleaned: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                Event::Emit(m) => match &**m {
                    Message::CleanupDone(tid, _) => Some(*tid),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(
            cleaned,
            vec![1, 2],
            "只应清理「在跑 / 等待重试」这两条留下痕迹的行"
        );
        assert!(s.all_transfers().is_empty(), "关标签后不应留下任何僵尸行");
        assert!(s.running.is_empty(), "关标签必须归还全部并发额度");
    }
}
