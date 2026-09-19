//! GUI 内部状态类型定义。
//!
//! 这些类型保存于 [`App`](crate::app::App) 中，驱动三栏布局与各类面板的交互。

use crate::message::ResizeSender;
use crate::widget::term::Terminal;
use rterm_core::{
    ConnectionStatus, FileEntry, HostKeyPrompt, HostKeyReply, SftpClient, SshConnection,
};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// 中心面板可显示的内容类型（由最左侧活动栏切换）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CenterView {
    /// 会话管理列表。
    Sessions,
    /// SFTP 文件管理。
    Files,
    /// 传输队列视图（上传 / 下载，聚合所有标签），与 Sessions / Files 并列由活动栏切换。
    Transfers,
}

/// 终端当前目录（cwd）共享容器别名，与 `rterm_core::terminal_bridge::CwdTracker` 同构，
/// 供 GUI 侧按标签持有并在桥接创建时传给核心层。
pub type TerminalTabCwd = Option<Arc<Mutex<Option<String>>>>;

/// 终端标签页。
///
/// 连接状态与 SSH / SFTP 句柄全部按标签各自持有：同一会话开多个标签时，各标签独立
/// 走一遍握手与桥接，互不共享状态。若把状态挂到会话上，开新标签会把已连接标签一起
/// 改回「连接中」，任一标签断线也会让同会话其余标签一起变红。
pub struct TerminalTab {
    /// 标签唯一标识，同时作为终端部件的 id。
    pub id: u64,
    /// 该标签所属的会话 id。
    pub session_id: String,
    /// 该标签自己的连接状态机取值。
    pub status: ConnectionStatus,
    /// 连接失败原因（仅本标签）。
    pub error: Option<String>,
    /// 该标签独占的 SSH 连接（每个标签页独立建立，支持同一会话多标签并行）。
    pub conn: Option<Arc<SshConnection>>,
    /// 内嵌终端组件（`crate::widget::term`；桥接就绪前为 `None`，必须驻留主线程）。
    pub terminal: Option<Terminal>,
    /// 窗口尺寸变更发送端（桥接任务据此下发 window-change）。
    pub resize_tx: Option<ResizeSender>,
    /// 桥接断开标志：关标签 / 关窗口时置位，通知核心层 pump 任务尽快退出，
    /// 释放服务端管道句柄（否则后台线程与进程残留）。
    pub disconnect: Option<Arc<AtomicBool>>,
    /// 终端当前工作目录（cwd）：由核心层桥接 pump 扫描 OSC 7 序列实时写入，
    /// 文件管理「进入终端目录」按钮读取它跳转到对应远端目录。多标签各自独立，
    /// 故按标签持有（同一会话开多标签时各标签 cwd 互不串）。
    pub cwd: Arc<Mutex<Option<String>>>,
    /// 标签标题（默认取会话名）。
    pub title: String,
}

/// 待确认的主机密钥弹窗（连接握手暂停期间挂起，渲染与决策均取队首）。
pub struct HostKeyPromptState {
    /// 发起连接的标签 id（关标签时据此清理对应的悬挂弹窗；按标签而非会话清理，
    /// 否则关闭同会话的其它标签会连带拒绝本标签挂起的确认）。
    pub tab_id: u64,
    /// 弹窗展示的密钥信息。
    pub prompt: HostKeyPrompt,
    /// 用户决定句柄（回复后握手继续；丢弃不回复视为拒绝）。
    pub reply: HostKeyReply,
}

/// SFTP 模态对话框类型。
#[derive(Debug, Clone)]
pub enum SftpDialog {
    /// 删除确认（携带名称与是否目录）。
    Delete {
        /// 待删除条目名称。
        name: String,
        /// 是否为目录（决定递归删除与图标）。
        is_dir: bool,
    },
    /// 下载覆盖确认（携带远端名称、本地目标完整路径与对应传输任务 id）。
    OverwriteDownload {
        /// 远端条目名称。
        name: String,
        /// 本地目标完整路径。
        local: std::path::PathBuf,
        /// 对应的传输任务 id（确认后继续 / 跳过）。
        transfer_id: u64,
    },
    /// 文件属性展示（携带条目与完整远端路径，只读信息框）。
    Properties {
        /// 展示的文件条目元数据。
        entry: FileEntry,
        /// 完整远端路径。
        path: String,
    },
}

/// 顶部反馈横幅的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// 成功（绿色，定时自动消失）。
    Success,
    /// 错误（红色，定时自动消失）。
    Error,
    /// 警告（琥珀色，定时自动消失）：钥匙串丢失 / 凭据可能失效等。
    Warning,
}

/// 传输方向（上传 / 下载），用于左侧传输面板的图标与排序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// 本地 → 远端。
    Upload,
    /// 远端 → 本地。
    Download,
}

/// 传输状态机，驱动左侧传输面板的图标与可操作按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferStatus {
    /// 排队中：等待全局并发额度空出（并发上限见 `[transfer] max_concurrent`，默认 3）。
    Queued,
    /// 传输中。
    Active,
    /// 等待自动重试：上一次尝试以瞬时故障告终，正在指数退避。
    ///
    /// **刻意独立于 [`Self::Queued`]**：退避期间不占用并发额度（否则 N 个任务同时失败会让
    /// 整个队列空转最长 8 秒），也不参与调度序，故不能复用「排队中」——那会让面板把它算进
    /// 排队数、并让调度器误以为它可以立即启动。到点后由 `Message::RetryDue` 转回 `Queued`。
    WaitingRetry,
    /// 已完成。
    Done,
    /// 失败 / 已取消（含自动重试次数耗尽）。
    Error,
}

/// 单个 SFTP 文件传输任务（上传 / 下载）的进度与状态。
///
/// 传输按「全局并发 N」调度（跨标签、跨方向共享同一份额度），同一条 SFTP 通道可同时承载
/// 多个请求——russh-sftp 的会话内部按请求 id 多路复用回执，`SftpSession` 的操作全为 `&self`，
/// 故多个传输任务可同时持有同一客户端（详见 `app::transfer` 的调度器说明）。左侧面板聚合
/// 所有标签的传输。状态变更由 `Message::Progress` / `Message::TransferDone` 驱动；瞬时速度随
/// 进度消息由 `run_transfer` 的 `stream` 任务按真实 I/O 间隔估算并携带（核心层只回传累计字节，
/// 不提供速率），UI 在上游速度基础上做滑动平均后用于显示与 ETA 估算。失败后按
/// `[transfer] retry_attempts` 指数退避自动重试，**下一轮从上次落盘的字节继续**（源端指纹未变
/// 时，见 `Transfer::resume`）；半成品只在取消 / 移除 / 关标签时清理，下载的失败行刻意保留它
/// 作为续传起点（详见 `app::transfer` 的重试、续传与清理说明）。
#[derive(Clone)]
pub struct Transfer {
    /// 传输唯一标识，用于取消 / 重试 / 移除消息路由。
    pub id: u64,
    /// 传输方向。
    pub direction: TransferDirection,
    /// 显示名称（文件名）。
    pub name: String,
    /// 本地路径：上传为源文件，下载为目标文件。
    pub local: std::path::PathBuf,
    /// 远端绝对路径。
    pub remote: String,
    /// 已传输字节数。
    pub transferred: u64,
    /// 总字节数（已知时显示进度百分比，为 0 表示未知总量）。
    pub total: u64,
    /// 当前状态（排队 / 等待重试 / 传输中 / 完成 / 失败）。
    pub status: TransferStatus,
    /// 已安排的自动重试次数（0 表示尚未自动重试过）。
    ///
    /// 每进入一次 [`TransferStatus::WaitingRetry`] 自增；手动重试会把计数清零（视为用户重新开始，
    /// 故手动重试次数不限）。它同时是重试预算的消耗量（`attempts < retry_attempts`）。
    ///
    /// **只在「等待重试 / 最终失败」两态显示**（分母为 `[transfer] retry_attempts`）：重试一旦
    /// 成功、行重新跑起来，就该回到干净的样子——否则一次网络抖动会留下永久徽标（实测反馈）。
    /// 故该字段在成功路径上**不清零**，面板只是不显示它。
    pub attempts: u32,
    /// 下次自动重试的最早时刻（仅 [`TransferStatus::WaitingRetry`] 期间为 `Some`）。
    ///
    /// 面板用它渲染倒计时；调度器**不**依赖它（`WaitingRetry` 本身就不参与调度），
    /// 到点由退避定时器发 `Message::RetryDue` 转回排队态。
    pub not_before: Option<Instant>,
    /// 半成品文件路径（仅当「该清理而清理失败」时为 `Some`）。
    ///
    /// 清理只发生在**取消、移除行与关标签**三处（下载的 `.part` 暂存 / 上传的远端残留），
    /// 失败时把路径留在这里提示用户手动处理。**下载的失败行不在清理之列**：它刻意保留 `.part`
    /// 作为续传的起点（见 `app::transfer` 的「最终失败时暂存的去留」），行上「继续下载」按钮
    /// 就是冲它去的。上传侧的清理失败只记日志（残留不致命，重传的 `create()` 会截断），
    /// 不进此字段。
    ///
    /// 关标签时发起的清理没有行可承载提示，失败时改弹 toast（见 `app::transfer`）。
    pub partial: Option<String>,
    /// 错误信息（失败 / 取消时存在）。
    pub error: Option<String>,
    /// 瞬时速度（字节/秒），由进度消息按真实 I/O 间隔估算后携带，UI 做滑动平均用于显示与 ETA。
    pub speed: f64,
    /// 该传输**入队时**捕获的 SFTP 客户端（回退用；权威值始终是标签此刻的客户端）。
    ///
    /// 启动 / 重试时优先取父层经 `Ctx::client_for` 注入的「该标签当前客户端」，仅当标签此刻
    /// 拿不到客户端时才回退到这一份（见 `app::transfer` 模块文档「传输用哪个客户端」）。
    /// 会话重建后同一标签会换上新通道，若启动路径直接用这份记录，排队项与手动重试都会打在
    /// 旧会话上。为 `Option` 仅用于「客户端已失效 / 尚未建立」的边界情形（以及无需真实连接的
    /// 单元测试），正常入队时恒为 `Some`。
    pub client: Option<std::sync::Arc<rterm_core::SftpClient>>,
    /// 续传状态（上一轮记录的源端指纹 + 该轮起始偏移），`None` = 尚无可用半成品。
    ///
    /// 由每次尝试开始时的 `Message::AttemptStarted` 写入，是下一轮「能不能接着写」的**唯一**
    /// 校验基准（判定见 `app::transfer::resume_offset`）。排队中 / 从未跑过 / 已收尾（`Done`）
    /// 时为 `None`；失败与被取消**不清**它——那正是续传要用的东西。
    pub resume: Option<ResumeState>,
    /// 该行的暂存文件里是**完整数据**（下载内容已落全、只是改名失败）。
    ///
    /// 语义是「任何清理路径都必须跳过它」：它不是半成品，而是用户唯一的一份数据。由
    /// `Failure::keep_staging` 在最终失败时落到这里，供关标签 / 移除等清理路径判定。
    pub keep_staging: bool,
}

/// 一次传输的续传状态：下一轮尝试的校验基准。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeState {
    /// 上一轮记录下来的源端指纹（大小 + 修改时间），用于确认「还是同一个文件」。
    pub source: rterm_core::Fingerprint,
    /// 上一轮尝试的起始偏移（= 该轮开始时暂存文件的长度）。
    pub offset: u64,
}

/// SFTP 文件管理视图的临时状态。
pub struct SftpView {
    /// 当前正在管理的会话 id（未打开则为 `None`）。
    pub session: Option<String>,
    /// 该标签独占的 SFTP 客户端（每个标签各自基于自己的 SSH 连接开一条通道，
    /// 使各标签的文件上下文互不干扰）。
    ///
    /// 同一客户端**可以**被多个传输任务并发调用：russh-sftp 的会话按请求 id 多路复用回执，
    /// 并发安全由协议层保证（详见 `app::transfer` 模块文档）。
    pub client: Option<Arc<SftpClient>>,
    /// 当前远端路径。
    pub path: String,
    /// 路径输入框的暂存内容（编辑中、尚未提交，回车后写入 [`Self::path`]）。
    pub path_input: String,
    /// 内联“新建文件夹”输入态：为 `Some(text)` 时表示正在列表内新建，
    /// `text` 为输入框当前内容（进入时默认 “New Folder”），为 `None` 表示未新建。
    pub creating_dir: Option<String>,
    /// 内联“重命名”输入态：为 `Some((原名, 当前文本))` 时表示正在重命名该条目，
    /// 当前文本进入时默认等于原名，为 `None` 表示未重命名。
    pub renaming: Option<(String, String)>,
    /// 当前目录条目列表。
    pub entries: Vec<FileEntry>,
    /// 当前选中的条目名称。
    pub selected: Option<String>,
    /// 当前被鼠标悬浮的条目名称（`None` 表示无悬浮），用于渲染行的悬浮高亮背景。
    pub hovered: Option<String>,
    /// 右键菜单的上下文目标：右键按下瞬间命中的条目 `(名称, 是否目录)`，
    /// `None` 表示空白处（或 “..” 合成项）。仅在右键时快照，使已打开的菜单
    /// 在鼠标移动后保持稳定，不随 [`Self::hovered`] 改变。
    pub context_target: Option<(String, bool)>,
    /// 是否有 SFTP 写操作进行中（用于禁用按钮 / 提示）。
    pub busy: bool,
    /// 当前打开的模态对话框（`None` 表示无）：删除确认 / 下载覆盖确认 / 文件属性。
    pub dialog: Option<SftpDialog>,
    /// 上次选择的下载目录：用作下载目录选择器的起始目录，首次取系统下载目录
    /// （`~/Downloads`，经 [`rterm_config::paths::download_dir`] 解析并逐级回退）。
    ///
    /// 不是「下载目标未指定时的回落」——`Message::SftpDownload` 总是自带目标目录。
    pub download_dir: String,
}

impl Default for SftpView {
    /// 返回 `SftpView` 默认状态（无进行中传输、默认下载目录、无错误）。
    fn default() -> Self {
        Self {
            session: None,
            client: None,
            path: ".".to_string(),
            path_input: String::new(),
            creating_dir: None,
            renaming: None,
            entries: Vec::new(),
            selected: None,
            hovered: None,
            context_target: None,
            busy: false,
            dialog: None,
            download_dir: rterm_config::paths::download_dir()
                .to_string_lossy()
                .to_string(),
        }
    }
}

impl SftpView {
    /// 构造空的文件管理视图。
    pub fn new() -> Self {
        Self::default()
    }
}
