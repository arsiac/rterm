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
    /// 排队中：等待同标签当前传输结束（SFTP 通道非并发安全，单标签顺序执行）。
    Queued,
    /// 传输中。
    Active,
    /// 已完成。
    Done,
    /// 失败 / 已取消。
    Error,
}

/// 单个 SFTP 文件传输任务（上传 / 下载）的进度与状态。
///
/// 同一标签的 SFTP 客户端非并发安全，故单标签内传输顺序执行；左侧面板聚合所有标签的
/// 传输。状态变更由 `Message::Progress` / `Message::TransferDone` 驱动；瞬时速度随进度消息
/// 由 `run_transfer` 的 `stream` 任务按真实 I/O 间隔估算并携带（核心层只回传累计字节，
/// 不提供速率），UI 在上游速度基础上做滑动平均后用于显示与 ETA 估算。
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
    /// 当前状态（排队 / 传输中 / 完成 / 失败）。
    pub status: TransferStatus,
    /// 错误信息（失败 / 取消时存在）。
    pub error: Option<String>,
    /// 瞬时速度（字节/秒），由进度消息按真实 I/O 间隔估算后携带，UI 做滑动平均用于显示与 ETA。
    pub speed: f64,
    /// 执行该传输所用的 SFTP 客户端（按标签独占，随传输一并保存，避免跨标签共用同一客户端）。
    ///
    /// 与 [`SftpView::client`] 同源：入队时由父层经上下文注入当前标签的客户端，
    /// 使传输模块无需在每次启动时反向查询父状态。为 `Option` 仅用于「客户端已失效 /
    /// 尚未建立」的边界情形（以及无需真实连接的单元测试），正常入队时恒为 `Some`。
    pub client: Option<std::sync::Arc<rterm_core::SftpClient>>,
}

/// SFTP 文件管理视图的临时状态。
pub struct SftpView {
    /// 当前正在管理的会话 id（未打开则为 `None`）。
    pub session: Option<String>,
    /// 该标签独占的 SFTP 客户端（每个标签各自基于自己的 SSH 连接开一条通道，
    /// 避免多标签共用同一客户端造成的请求错乱与并发不安全）。
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
    /// 是否有 SFTP 写操作进行中（用于禁用按钮 / 提示）。
    pub busy: bool,
    /// 当前打开的模态对话框（`None` 表示无）：删除确认 / 下载覆盖确认 / 文件属性。
    pub dialog: Option<SftpDialog>,
    /// 上次选择的下载目录：用作下载目录选择器的起始目录，首次取系统下载目录。
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
            busy: false,
            dialog: None,
            download_dir: dirs::download_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string()),
        }
    }
}

impl SftpView {
    /// 构造空的文件管理视图。
    pub fn new() -> Self {
        Self::default()
    }
}
