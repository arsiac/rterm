//! GUI 消息枚举与共享类型定义。
//!
//! [`Message`] 是 iced `update` 的唯一输入，覆盖活动栏切换、会话 CRUD / 连接、
//! 终端标签与终端部件事件、中心 ↔ 右栏分隔（`pane_grid` 拖拽及窗口缩放时按比例重算），
//! 以及 SFTP 文件操作。

use crate::app::hostkey;
use crate::app::masterpw;
use crate::app::panes;
use crate::app::paste;
use crate::app::search;
use crate::app::session;
use crate::app::session_io;
use crate::app::settings;
use crate::app::sftp;
use crate::app::shortcuts;
use crate::app::snippets;
use crate::app::tabs;
use crate::app::transfer;
use crate::app::updates;
use crate::app::window;
use crate::state::CenterView;
use iced::event::Status;
use rterm_config::keybindings::Chord;
use tokio::sync::mpsc;

use crate::widget::toast::ToastId;

/// 终端尺寸变更发送端：GUI 收到本地终端 resize 后，将 `(列数, 行数)` 发往桥接任务。
pub type ResizeSender = mpsc::Sender<(u32, u32)>;

/// 所有 UI 交互与后台任务结果汇聚于此枚举。
#[derive(Clone)]
pub enum Message {
    // ===== 活动栏 / 中心视图 =====
    /// 切换中心面板显示的内容（会话管理 / 文件管理 / 传输队列）。
    SwitchCenter(CenterView),

    // ===== 会话面板（路由）=====
    /// 会话管理模块内部消息：新建 / 编辑 / 删除 / 保存 / 刷新 / 编辑字段 / 连接意图 /
    /// 分组折叠 / 悬浮高亮 / 导入导出等会话列表与编辑器的 UI 意图，由父层经此变体路由进
    /// `app::session` 模块。
    Session(session::Message),
    /// 会话管理模块上行事件，由父层收到后修改父状态（开标签连接 / 关会话标签 / 状态栏 / toast）。
    SessionEvent(session::Event),
    // ===== 会话文件导入 / 导出（路由）=====
    /// 会话文件模块内部消息：导出 / 导入的文件对话框、导出口令弹窗的输入与提交、模块自处理的
    /// 异步结果，由父层经此变体路由进 `app::session_io` 模块。
    SessionIo(session_io::Message),
    /// 会话文件模块上行事件（导入合并后的会话列表 / toast / 自回路），由父层落地。
    SessionIoEvent(session_io::Event),
    // ===== 主机密钥确认弹窗（路由）=====
    /// 主机密钥确认模块内部消息：入队请求 / 用户决定 / Esc 取消，由父层经此变体路由进模块。
    HostKey(hostkey::Message),
    /// 主机密钥确认模块上行事件，由父层收到后处理（当前模块自包含，暂无需要父层配合的事件）。
    HostKeyEvent(hostkey::Event),
    /// 关闭某条 toast 通知（携带其 [`crate::widget::toast::ToastId`]）。
    ToastDismissed(ToastId),
    /// 鼠标悬浮 / 离开某条 toast（携带 [`crate::widget::toast::ToastId`] 与是否正被悬浮），
    /// 用于暂停其自动消失计时。
    ToastHovered(ToastId, bool),
    /// 定时心跳：用于让到期 toast 自动从通知区移除（见 [`crate::app::App::subscription`]）。
    ToastTick,

    // ===== 终端标签（路由）=====
    /// 终端标签模块内部消息：标签生命周期与标签栏导航（切换 / 关闭 / 列表 dropdown / 窗口焦点 /
    /// 终端桥接与部件事件 / SSH 连接结果），由父层经此变体路由进 `app::tabs` 模块。
    Tabs(tabs::Message),
    /// 终端标签模块上行事件（写回导航态 / 拉起桥接 / 开 SFTP / 自回路），由父层收到后落地。
    TabsEvent(tabs::Event),

    // ===== 两栏布局（pane_grid 比例，路由）=====
    /// 两栏布局模块内部消息：拖拽分隔条 / 窗口缩放，由父层经此变体路由进 `app::panes` 模块。
    Panes(panes::Message),
    /// 两栏布局模块上行事件：当前为空（纯自包含几何，无需父层配合）。
    PanesEvent(panes::Event),

    // ===== 主窗口生命周期（路由）=====
    /// 主窗口模块内部消息：窗口创建 / 关闭请求 / 关闭前的最大化与尺寸查询结果，
    /// 由父层经此变体路由进 `app::window` 模块。
    Window(window::Message),
    /// 主窗口模块上行事件（查询最大化 / 尺寸、写回尺寸、关闭窗口），由父层落地。
    WindowEvent(window::Event),

    // ===== SFTP 文件管理 =====
    /// SFTP 模块内部消息：UI 意图与模块自处理的异步结果，由父层经此变体路由进模块。
    Sftp(sftp::Message),
    /// SFTP 模块上行事件，由父层收到后修改父状态。
    SftpEvent(sftp::Event),

    // ===== 文件传输（上传 / 下载队列）=====
    /// 传输模块内部消息：上传 / 下载意图与模块自处理的进度 / 完成结果，由父层经此变体路由进模块。
    Transfer(transfer::Message),
    /// 传输模块上行事件（toast / 刷新目录 / 自回路），由父层收到后修改父状态或转发给其它模块。
    TransferEvent(transfer::Event),

    // ===== 主密码模块（路由）=====
    /// 主密码模块内部消息：设置 / 解锁 / 更改流程的 UI 意图与模块自处理的异步结果，
    /// 由父层经此变体路由进 `app::masterpw` 模块。
    MasterPw(masterpw::Message),
    /// 主密码模块上行事件，由父层收到后修改父状态（vault / sessions / config / toast）。
    MasterPwEvent(masterpw::Event),

    // ===== 终端右键菜单 =====
    /// 菜单「复制」：把活动终端的选区文本写入系统剪贴板。
    TerminalCopy,
    /// 菜单「粘贴」：异步读取系统剪贴板，结果经 [`Message::TerminalPasteLoaded`] 回流。
    TerminalPaste,
    /// 系统剪贴板读取结果：`Some` 时按括号粘贴模式写入活动终端，`None`（读取失败 / 空）忽略。
    TerminalPasteLoaded(Option<String>),

    // ===== 杂项 =====
    /// 无操作占位消息（用于禁用态按钮点击）。
    Noop,

    // ===== 活动栏设置菜单 =====
    /// 展开 / 收起活动栏底部设置入口的下拉菜单（菜单项：设置 / 快捷键）；
    /// 点外与 Esc 亦经其 `on_dismiss` 收起。
    ToggleSettingsMenu,

    // ===== 设置弹窗模块（路由）=====
    /// 设置弹窗模块内部消息：各设置项的 UI 意图（含「切换弹窗」「切换分类」「打开日志目录」），
    /// 由父层经此变体路由进 `app::settings` 模块。
    Settings(settings::Message),
    /// 设置弹窗模块上行事件，由父层收到后写回 `AppConfig` 并落盘（模块绝不写父状态）。
    SettingsEvent(settings::Event),

    // ===== 快捷键弹窗模块（路由）=====
    /// 快捷键弹窗模块内部消息：弹窗开关等 UI 意图，由父层经此变体路由进 `app::shortcuts`。
    Shortcuts(shortcuts::Message),
    /// 快捷键弹窗模块上行事件，由父层收到后落地（模块绝不写父状态）。
    ShortcutsEvent(shortcuts::Event),

    // ===== 粘贴确认模块（路由）=====
    /// 粘贴确认模块内部消息：粘贴请求与弹窗决策（确认 / 取消 / 本次不再提醒），
    /// 由父层经此变体路由进 `app::paste` 模块。
    Paste(paste::Message),
    /// 粘贴确认模块上行事件：放行的粘贴交父层编码并写入终端。
    PasteEvent(paste::Event),

    // ===== 终端内搜索（路由）=====
    /// 终端内搜索模块内部消息：搜索条开关 / 查询 / 上下跳转 / 两个匹配开关，
    /// 由父层经此变体路由进 `app::search` 模块。
    Search(search::Message),
    /// 终端内搜索模块上行事件：后端搜索命令与焦点意图，由父层落地到目标终端。
    SearchEvent(search::Event),

    // ===== 命令片段（路由）=====
    /// 命令片段模块内部消息：片段列表与编辑弹窗的 UI 意图、发送当前 / 发送全部，
    /// 由父层经此变体路由进 `app::snippets` 模块。
    Snippets(snippets::Message),
    /// 命令片段模块上行事件（写入终端 / toast / 状态栏），由父层收到后落地。
    SnippetsEvent(snippets::Event),

    // ===== 更新检查（路由）=====
    /// 更新检查模块内部消息：启动自动检查 / 立即检查 / 检查结果 / 打开发布页 / 关闭横幅，
    /// 由父层经此变体路由进 `app::updates` 模块。
    Updates(updates::Message),
    /// 更新检查模块上行事件（写回「上次检查时间戳」），由父层收到后写回 `AppConfig` 并落盘。
    UpdatesEvent(updates::Event),
    /// 候选按键按下：由全局按键监听归一为和弦送来（常规态为「带修饰键或具名键」，
    /// 录制态含裸字符键），在 `update` 里查键位表分发（Esc 属上下文键，另作处理）。
    AppKey {
        /// 归一化后的和弦（逻辑修饰键 + 按键）。
        chord: Chord,
        /// 该事件是否已被部件捕获（模态遮罩 / 终端拦下的保留键），用于避免重复派发。
        status: Status,
    },
    /// 鼠标右键按下：把光标所在的会话 / 文件 / 常用命令行标记为选中，使右键菜单作用于哪一条可见。
    ///
    /// 由全局监听下发（`App::subscription`）而非行内 `on_right_press`——`iced_aw::ContextMenu`
    /// 会先捕获右键事件，行内处理收不到；选中谁再由各模块按自身悬浮态判定。
    RightPress,
}
