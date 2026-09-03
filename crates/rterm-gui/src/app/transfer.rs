//! 文件传输模块（上传 / 下载队列）

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use iced::{Subscription, Task};

use crate::app::tasks::join_path;
use crate::i18n::localize_error;
use crate::state::{ToastKind, Transfer, TransferDirection, TransferStatus};
use crate::t;
use futures::{SinkExt, StreamExt};
use rterm_core::SftpClient;
use tokio::task::AbortHandle;

/// 传输模块只读上下文：父层在路由每条消息前构造，模块据此读取当前标签的 SFTP 客户端与
/// 远端目录，但绝不写回父状态。
///
/// `client` 为 `None` 表示当前标签尚未建立 SFTP 通道，此时上传 / 下载会被拒绝（emit toast）。
pub struct Ctx {
    /// 当前活动标签 id（供「作用于活动标签」的变体定位传输记录）。
    pub tab_id: u64,
    /// 当前标签的 SFTP 客户端（上传 / 下载执行所需）。
    pub client: Option<Arc<SftpClient>>,
    /// 当前标签的远端工作目录（用于把本地文件名 / 远端名解析为绝对远端路径）。
    pub remote_dir: String,
}

/// 传输模块私有状态：每标签传输队列 + 任务 id 分配器 + 取消句柄注册表。
pub struct State {
    /// 每标签独立的传输队列（原内嵌在 `SftpView.transfers`）。
    per_tab: HashMap<u64, Vec<Transfer>>,
    /// 下一个传输任务的 id（原 `sftp::State::next_transfer_id` 搬入）。
    next_transfer_id: u64,
    /// 传输任务 id → 取消句柄（原 `sftp::State::abort_handles` 搬入）。
    abort_handles: HashMap<u64, AbortHandle>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            per_tab: HashMap::new(),
            next_transfer_id: 1,
            abort_handles: HashMap::new(),
        }
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
            Message::Upload(tab_id, paths) => {
                if paths.is_empty() {
                    return Task::none();
                }
                let Some(client) = ctx.client.clone() else {
                    return Task::done(Event::Toast(ToastKind::Error, t!("sftp.not_connected")));
                };
                for path in paths {
                    let name = match path.file_name() {
                        Some(n) => n.to_string_lossy().to_string(),
                        None => continue,
                    };
                    if name.is_empty() {
                        continue;
                    }
                    let remote = join_path(&ctx.remote_dir, &name);
                    let tid = self.next_transfer_id();
                    let transfer = Transfer {
                        id: tid,
                        direction: TransferDirection::Upload,
                        name,
                        local: path,
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
                self.start_next_transfer(tab_id)
            }
            Message::Download(tab_id, name, local) => {
                let Some(client) = ctx.client.clone() else {
                    return Task::done(Event::Toast(ToastKind::Error, t!("sftp.not_connected")));
                };
                let remote = join_path(&ctx.remote_dir, &name);
                // `local` 已是完整本地目标路径（SFTP 模块在唤起下载时已 `dir.join(name)` 拼好），
                // 此处不可再拼接一次，否则目标会变成 `…/name/name` 导致写入失败。
                self.enqueue_download(tab_id, &name, remote, local, client)
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
                // 标记完成 / 失败；上传成功需刷新当前目录以显示新文件（经父层转发给 SFTP 模块）。
                let mut relist = false;
                {
                    if let Some(tab) = self.per_tab.get_mut(&tab_id)
                        && let Some(t) = tab.iter_mut().find(|t| t.id == tid)
                    {
                        match &result {
                            Ok(()) => t.status = TransferStatus::Done,
                            Err(e) => {
                                t.status = TransferStatus::Error;
                                t.error = Some(e.clone());
                            }
                        }
                        if matches!(t.direction, TransferDirection::Upload) && result.is_ok() {
                            relist = true;
                        }
                    }
                }
                let start = self.start_next_transfer(tab_id);
                if relist {
                    // 上传成功：请求父层刷新该标签目录（父层再经 `Message::Sftp` 派发给 SFTP 模块）。
                    return Task::batch([start, Task::done(Event::RefreshDir(tab_id))]);
                }
                start
            }
            Message::TransferHandle(tid, handle) => {
                self.abort_handles.insert(tid, handle);
                Task::none()
            }
            Message::CancelTransfer(id) => {
                if let Some(handle) = self.abort_handles.remove(&id) {
                    handle.abort();
                }
                if let Some(tab) = self.per_tab.get_mut(&ctx.tab_id)
                    && let Some(t) = tab.iter_mut().find(|t| t.id == id)
                {
                    t.status = TransferStatus::Error;
                    t.error = Some("已取消".to_string());
                }
                Task::none()
            }
            Message::RetryTransfer(id) => {
                if let Some(tab) = self.per_tab.get_mut(&ctx.tab_id)
                    && let Some(t) = tab.iter_mut().find(|t| t.id == id)
                {
                    t.status = TransferStatus::Queued;
                    t.error = None;
                    t.transferred = 0;
                    t.total = 0;
                    t.speed = 0.0;
                }
                Task::none()
            }
            Message::RemoveTransfer(id) => {
                if let Some(tab) = self.per_tab.get_mut(&ctx.tab_id)
                    && let Some(pos) = tab.iter().position(|t| t.id == id)
                {
                    tab.remove(pos);
                }
                Task::none()
            }
        }
    }

    /// 启动队列中下一个「排队中」的传输（同一标签 SFTP 通道非并发安全，顺序执行）。
    ///
    /// 仅当该标签当前无「传输中」任务时才取首个 `Queued` 置为 `Active` 并拉起
    /// [`run_transfer`]；返回其任务。无客户端或无排队项时返回 `Task::none()`。
    fn start_next_transfer(&mut self, tab_id: u64) -> Task<Event> {
        let can_start = self
            .per_tab
            .get(&tab_id)
            .map(|tab| !tab.iter().any(|t| t.status == TransferStatus::Active))
            .unwrap_or(false);
        if !can_start {
            return Task::none();
        }
        let idx = self
            .per_tab
            .get(&tab_id)
            .and_then(|tab| tab.iter().position(|t| t.status == TransferStatus::Queued));
        let idx = match idx {
            Some(i) => i,
            None => return Task::none(),
        };
        let (tid, direction, local, remote, client) = {
            let tab = self.per_tab.get_mut(&tab_id).unwrap();
            let t = &mut tab[idx];
            t.status = TransferStatus::Active;
            (
                t.id,
                t.direction,
                t.local.clone(),
                t.remote.clone(),
                t.client.clone(),
            )
        };
        // 客户端缺失（已失效 / 尚未建立）时无法启动：回退为排队态，避免卡在 Active 永久阻塞队列。
        let Some(client) = client else {
            if let Some(tab) = self.per_tab.get_mut(&tab_id)
                && let Some(t) = tab.get_mut(idx)
            {
                t.status = TransferStatus::Queued;
            }
            return Task::none();
        };
        run_transfer(tab_id, tid, direction, client, local, remote)
    }

    /// 入队一个下载传输并启动队列（本地已存在同名由调用方先弹覆盖框，此处直接写入）。
    fn enqueue_download(
        &mut self,
        tab_id: u64,
        remote_name: &str,
        remote: String,
        local: PathBuf,
        client: Arc<SftpClient>,
    ) -> Task<Event> {
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
        self.start_next_transfer(tab_id)
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
    /// 上传本地文件（携带标签 id + 所选本地文件路径列表，写入当前远端目录）。
    Upload(u64, Vec<PathBuf>),
    /// 下载远端文件（携带标签 id + 远端名称 + 本地目标路径，已由 SFTP 侧拼为完整路径）。
    Download(u64, String, PathBuf),
    /// 传输进度（携带标签 id + 传输任务 id + 已传字节 + 总字节 + 瞬时速度字节/秒）。
    Progress(u64, u64, u64, u64, f64),
    /// 传输（上传 / 下载）完成（携带标签 id + 传输任务 id + 结果）。
    TransferDone(u64, u64, Result<(), String>),
    /// 后台传输 worker 的取消句柄已就绪（携带传输任务 id + 句柄）。
    TransferHandle(u64, AbortHandle),
    /// 取消某个传输任务（携带任务 id）。
    CancelTransfer(u64),
    /// 重试某个失败 / 已取消的传输（携带任务 id）。
    RetryTransfer(u64),
    /// 从列表中移除某个已完成 / 失败的传输（携带任务 id）。
    RemoveTransfer(u64),
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
                    let result = match direction {
                        TransferDirection::Upload => {
                            client.upload_with_progress(&local, &remote, cb).await
                        }
                        TransferDirection::Download => {
                            client.download_with_progress(&remote, &local, cb).await
                        }
                    };
                    result.map_err(|e| localize_error(&e))
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

            // 等 worker 得出终态；取消亦视为失败。
            let result = worker
                .await
                .unwrap_or_else(|e| Err(format!("传输任务被取消: {e}")));

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
    fn no_client_ctx(tab_id: u64) -> Ctx {
        Ctx {
            tab_id,
            client: None,
            remote_dir: "/home/user".to_string(),
        }
    }

    /// 构造一条无客户端的传输记录（测试专用：绕开「需客户端才能入队」约束，单独验证队列逻辑）。
    fn make_transfer(id: u64, direction: TransferDirection) -> Transfer {
        Transfer {
            id,
            direction,
            name: "f.txt".to_string(),
            local: PathBuf::from("/tmp/f.txt"),
            remote: "/home/user/f.txt".to_string(),
            transferred: 0,
            total: 0,
            status: TransferStatus::Queued,
            error: None,
            speed: 0.0,
            client: None,
        }
    }

    #[test]
    fn new_state_has_no_transfers() {
        let s = State::new();
        assert!(s.all_transfers().is_empty(), "新状态不应有任何传输");
    }

    #[test]
    fn upload_without_client_emits_error_toast_and_enqueues_nothing() {
        let mut s = State::new();
        let events = run_events(s.update(
            Message::Upload(7, vec![PathBuf::from("/tmp/a.txt")]),
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
        assert_eq!(t.error.as_deref(), Some("已取消"), "取消应带取消原因");
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
            Message::TransferDone(7, 1, Err("磁盘满".to_string())),
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
}
