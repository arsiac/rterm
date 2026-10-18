//! SFTP 文件管理模块（浏览 / 导航 / 新建 / 重命名 / 删除 / 属性 / 上传下载入口）
use std::collections::HashMap;
use std::sync::Arc;

use iced::{Subscription, Task};

use crate::app::tasks::{
    apply_upload_decisions, build_conflict_rows, ensure_remote_dir, join_path, new_dir_targets,
    open_sftp_task, parent_path, resolve_and_list_task, upload_conflicts, valid_rename_name,
};
use crate::i18n::localize_error;
use crate::state::{CenterView, ConflictDecision, SftpDialog, SftpView, ToastKind};
use crate::t;
use log::warn;
use rterm_core::{FileEntry, SftpClient, SshConnection};

/// SFTP 模块私有状态：每标签文件管理视图。
#[derive(Default)]
pub struct State {
    /// 每标签独立的 SFTP 文件管理视图（原 `TerminalTab.sftp` 搬入此处）。
    per_tab: HashMap<u64, SftpView>,
}

impl State {
    /// 取某标签的 SFTP 视图（只读）。
    pub fn tab(&self, id: u64) -> Option<&SftpView> {
        self.per_tab.get(&id)
    }

    /// 取某标签的 SFTP 视图（可变）。
    pub fn tab_mut(&mut self, id: u64) -> Option<&mut SftpView> {
        self.per_tab.get_mut(&id)
    }

    /// 取当前活动标签的 SFTP 视图（只读），供渲染层使用。
    pub fn active(&self, active_tab: Option<u64>) -> Option<&SftpView> {
        active_tab.and_then(|id| self.per_tab.get(&id))
    }

    /// 确保某标签的视图存在（不存在则插入默认），返回可变引用。
    pub fn ensure(&mut self, id: u64) -> &mut SftpView {
        self.per_tab.entry(id).or_default()
    }

    /// 父层在 SFTP 通道建立后把客户端推入对应标签（下行，规则允许父写子）。
    pub fn set_connection(&mut self, tab_id: u64, client: Arc<SftpClient>) {
        self.ensure(tab_id).client = Some(client);
    }

    /// 作废某标签的 SFTP 通道（原地重连成功后调用）。
    ///
    /// 旧客户端绑在已死的连接上、任何调用秒失败：清空后视图回到未打开态，由用户重新
    /// 打开（不自动重建通道 / 续跑队列）；内联输入与对话框一并收起（指向旧目录快照）。
    pub fn invalidate(&mut self, tab_id: u64) {
        if let Some(view) = self.per_tab.get_mut(&tab_id) {
            view.session = None;
            view.client = None;
            view.busy = false;
            view.path = ".".to_string();
            view.path_input = ".".to_string();
            view.entries.clear();
            view.selected = None;
            view.hovered = None;
            view.creating_dir = None;
            view.renaming = None;
            view.dialog = None;
            view.queued_conflicts.clear();
        }
    }

    /// 查询某标签是否已建立 SFTP 会话（用于「切到文件管理自动打开」判定）。
    pub fn tab_session(&self, tab_id: u64) -> Option<String> {
        self.per_tab.get(&tab_id).and_then(|s| s.session.clone())
    }

    /// 各标签**当前**的 SFTP 客户端快照（父层在路由每条消息前交给传输模块）。
    ///
    /// 供传输模块在「启动 / 重试」时取最新客户端：传输记录里那份是入队时捕获的，会话重建后
    /// 会过期，而这里永远反映此刻可用的通道。`Arc` 克隆，代价可忽略。
    pub fn live_clients(&self) -> impl Iterator<Item = (u64, Arc<SftpClient>)> + '_ {
        self.per_tab
            .iter()
            .filter_map(|(id, view)| view.client.clone().map(|c| (*id, c)))
    }

    /// 重新列举指定标签的当前目录，结果经 `SftpListed` 回流（自回路）。
    ///
    /// 供「上传成功」后由传输模块经父层请求刷新——传输模块只发 `transfer::Event::RefreshDir`，
    /// 父层再经 `Message::Sftp` 派发到此处，避免传输模块直接写 SFTP 视图。
    pub fn refresh(&self, tab_id: u64) -> Task<Event> {
        let Some(tab) = self.per_tab.get(&tab_id) else {
            return Task::none();
        };
        let Some(client) = tab.client.clone() else {
            return Task::none();
        };
        let path = tab.path.clone();
        list_task(tab_id, client, path)
    }
}

/// 目录整树删除的操作结果分类（供汇总 toast 呈现）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemovalOutcome {
    /// 全部删除。
    Complete {
        /// 删除的文件数。
        files: usize,
        /// 删除的目录数（含根）。
        dirs: usize,
    },
    /// 尽力而为的部分成功：`failed` 条目原地留存。
    Partial {
        /// 删除的文件数。
        files: usize,
        /// 删除的目录数。
        dirs: usize,
        /// 失败条目数。
        failed: usize,
        /// 首个失败项说明（路径 + 本地化原因）。
        reason: String,
    },
    /// 守卫 / 游树环节即失败：删除尚未开始，树内没有任何损失。
    Failed(String),
}

/// 模块内部消息：UI 意图 + 模块自处理的异步结果（列举 / 写操作）。
///
/// 由父层经 `Message::Sftp` 路由进来；模块 `update` 自行消费，不外泄。
///
/// 仅 `Clone`（不 `Debug`：`SftpReady` 含 `Arc<SftpClient>`，而 `SftpClient` 未实现 `Debug`）。
#[derive(Clone)]
// 变体统一带 `Sftp` 前缀：与父层 `Message::Sftp` 路由命名一致，属刻意约定，故抑制此 lint。
#[allow(clippy::enum_variant_names)]
pub enum Message {
    /// SFTP 通道已建立（归属标签 id + 会话 id + 结果）。
    SftpReady(u64, String, Result<Arc<SftpClient>, String>),
    /// 打开某标签的文件管理（归属标签 id + 会话 id + 该标签已建立的通道 + 建通道所需的 SSH 连接）。
    ///
    /// 由父层在「切换中心视图 / 选中标签 / 会话右键打开文件管理」时派发：父层只负责挑选目标
    /// 标签与切换导航态，视图重置与「列举 / 建通道」全部在模块内完成（模块自写自身状态）。
    /// `client` 已存在则直接列举；否则用 `conn` 新建通道（结果经 [`Message::SftpReady`] 回流）。
    SftpOpenSession(
        u64,
        String,
        Option<Arc<SftpClient>>,
        Option<Arc<SshConnection>>,
    ),
    /// 进入指定目录。
    SftpCd(String),
    /// 返回上级目录。
    SftpParent,
    /// 路径输入框内容变更（携带当前文本，尚未提交）。
    SftpPathInput(String),
    /// 目录列表加载完成（携带归属标签 id + 解析后的绝对路径 + 结果）。
    SftpListed(u64, String, Result<Vec<FileEntry>, String>),
    /// 选中 SFTP 列表中的某条目（携带名称）。
    SftpSelect(String),
    /// 右键按下：把光标所在的文件条目（即当前悬浮项）标记为选中。
    ///
    /// 与 [`Message::SftpSelect`] 的差别是不携带名称——`iced_aw::ContextMenu` 会先捕获
    /// 右键事件，行内的 `on_right_press` 收不到，故改由全局监听（见 `App::subscription`）
    /// 下发本消息，选中目标在此按 `SftpView::hovered` 判定。
    SftpSelectHovered,
    /// 鼠标进入 SFTP 列表某条目（携带名称），用于渲染悬浮高亮。
    SftpEntryEnter(String),
    /// 鼠标离开 SFTP 列表某条目（携带名称），用于清除悬浮高亮。
    SftpEntryExit(String),
    /// 打开系统文件选择器以选择要上传的本地文件（可多选）。
    SftpPickUpload,
    /// 打开系统文件夹选择器以选择要上传的本地文件夹（可多选），保留目录层级上传。
    SftpPickUploadFolder,
    /// 上传选择器返回（携带上传项）：先探测目标目录的同名条目，再决定直接入队还是弹逐条决策框。
    SftpUploadPicked(Vec<(std::path::PathBuf, String)>),
    /// 探测到远端同名（携带探测时的目录、上传项、同名文件、同名目录与目录快照），请求弹逐条决策框。
    SftpUploadConflict(
        String,
        Vec<(std::path::PathBuf, String)>,
        Vec<String>,
        Vec<String>,
        HashMap<String, bool>,
    ),
    /// 修改冲突对话框中某一行的处置策略（按行序号，0 基）。
    SftpConflictDecide(usize, ConflictDecision),
    /// 把冲突对话框所有行的处置策略设为同一档（「应用到全部」）。
    SftpConflictApplyAll(ConflictDecision),
    /// 打开系统文件夹选择器以选择下载目标目录（携带远端文件名）。
    SftpPickDownload(String),
    /// 下载远端文件（携带远端名称与本地目标目录），经覆盖检查后上行给传输模块。
    SftpDownload(String, std::path::PathBuf),
    /// 打开系统文件夹选择器以选择远端**目录**的下载落点（携带远端目录名）。
    SftpPickDownloadFolder(String),
    /// 下载远端目录（携带远端名称与所选本地落点），经首层同名判定后入队一行文件夹作业。
    SftpDownloadFolder(String, std::path::PathBuf),
    /// SFTP 写操作完成（归属标签 id + 结果）。
    SftpActionDone(u64, Result<(), String>),
    /// 请求打开“删除确认”对话框（携带名称与是否为目录）。
    ///
    /// 条目类型由菜单快照随消息携带，而非在装配时重新查表：菜单项的渲染与动作出自
    /// 同一份快照，重新查表会在列表刷新后与用户看到的那一项不一致。
    /// 目录走整树删除：先弹计数确认框，Enter 或点「删除」一次即执行。
    SftpDeleteConfirm(String, bool),
    /// 目录删除前计数探测回流（携带目录名与探测结果；仅对同名弹窗生效）。
    SftpDeleteProbed(String, crate::state::DeleteProbe),
    /// 目录整树删除执行完毕（携带刷新用基准目录与结果分类）。
    SftpDeleteTreeDone(String, RemovalOutcome),
    /// 请求打开“属性”对话框（携带名称，展示文件详细信息）。
    SftpShowProperties(String),
    /// 复制远端条目的完整路径到系统剪贴板（携带完整路径）。
    SftpCopyPath(String),
    /// 复制远端条目的文件名到系统剪贴板（携带文件名，含扩展名）。
    SftpCopyName(String),
    /// 跳转到终端当前所在目录（由路由层读取活动终端 cwd 后转 [`SftpCd`] 处理，
    /// 模块自身不持有终端访问权，故此处仅作占位、不在此分支消费）。
    SftpGotoTerminalDir,
    /// 模块内部无操作占位（如文件选择器被取消，避免父层需要一个无意义的事件）。
    SftpNoop,
    /// 请求在文件列表内联进入“新建文件夹”输入态
    SftpNewDirConfirm,
    /// 内联“新建文件夹”输入框内容变更（携带当前文本）。
    SftpNewDirInput(String),
    /// 内联“新建文件夹”回车提交（为空则取消创建）。
    SftpNewDirSubmit,
    /// 请求在文件列表内联进入“重命名”输入态（携带原名，输入框预填原名）。
    SftpRenameConfirm(String),
    /// 内联“重命名”输入框内容变更（携带当前文本）。
    SftpRenameInput(String),
    /// 内联“重命名”回车提交（为空或与原名相同则取消重命名）。
    SftpRenameSubmit,
    /// F2 快捷键请求：对当前选中的 SFTP 条目触发重命名。
    SftpRenameShortcut,
    /// 确认当前打开的对话框（执行其动作）。
    SftpDialogConfirm,
    /// 取消 / 关闭当前对话框或内联输入态（Esc 与对话框取消按钮共用同一入口）。
    SftpCancelDialog,
}

/// 模块上行事件：仅通知父层，父层收到后才修改父状态。
///
/// 仅 `Clone`（不 `Debug`：含 `Box<Message>`，而 `Message` 未实现 `Debug`）。
#[derive(Clone)]
pub enum Event {
    /// 请求父层弹出 toast 通知（携带类型与文案）。
    Toast(ToastKind, String),
    /// 请求父层切换中心视图（携带目标视图）。
    NavigateTo(CenterView),
    /// 上行请求启动一个上传传输（携带所选本地文件列表，元素为 `(本地路径, 远端相对路径)`，
    /// 远端相对路径用于保留文件夹上传时的目录层级），由父层转发给传输模块。
    ///
    /// 文件选择器属于「文件管理」交互，故留在 SFTP 模块；但队列执行属传输模块，
    /// 此处只上行意图，绝不直接创建传输。
    StartUpload(Vec<(std::path::PathBuf, String)>),
    /// 上行请求启动一个下载传输（携带远端名称与本地目标路径），由父层转发给传输模块。
    StartDownload(String, std::path::PathBuf),
    /// 上行请求启动一个**目录**下载作业（携带远端名称与本地作业根完整路径）。
    StartDownloadFolder(String, std::path::PathBuf),
    /// 请求父层把键盘焦点交还终端。
    ///
    /// 内联输入态（重命名 / 新建文件夹）退出后焦点已离开文件管理器；焦点态属父层，
    /// 故由模块上行此事件、父层置 `terminal_focused`，模块绝不直接写。
    FocusTerminal,
    /// 自回路：把一条模块内部消息经父层派发回 `State::update`。
    ///
    /// 写操作（列举 / 重命名 / 删除）完成后需重新进入模块自身（如重新列举当前目录），
    /// 但模块 `update` 只能经 `Event` 上行、不能写父态；故把内部消息装进 `Emit` 上行，
    /// 父层在 `Message::SftpEvent` 分支收到后再 `self.sftp.update` 一次，形成自回路。
    Emit(Box<Message>),
}

impl State {
    /// 模块更新：只改自身 `State`；需要父层配合的事以 [`Event`] 经 `Task` 上行。
    ///
    /// `active_tab` 为当前活动标签 id（父层传入），供「作用于活动标签」的变体定位视图。
    pub fn update(&mut self, msg: Message, active_tab: u64) -> Task<Event> {
        match msg {
            Message::SftpReady(tab_id, _id, result) => {
                let tab = self.ensure(tab_id);
                tab.busy = false;
                match result {
                    // 通道建立成功：存入 client 并立即列举当前目录，否则列表永远为空。
                    Ok(c) => {
                        tab.client = Some(c.clone());
                        tab.busy = true;
                        list_task(tab_id, c, ".".to_string())
                    }
                    // 通道建立失败：上报错误而非静默留空，避免用户只见空白列表。
                    Err(e) => {
                        tab.client = None;
                        Task::done(Event::Toast(
                            ToastKind::Error,
                            format!("{}: {e}", t!("app.list_failed")),
                        ))
                    }
                }
            }
            Message::SftpOpenSession(tab_id, session_id, client, conn) => {
                let tab = self.ensure(tab_id);
                tab.session = Some(session_id.clone());
                tab.path = ".".to_string();
                tab.path_input = tab.path.clone();
                tab.creating_dir = None;
                tab.renaming = None;
                tab.entries.clear();
                match (client, conn) {
                    // 通道已存在：直接列举（会先解析为绝对路径）。
                    (Some(c), _) => {
                        self.ensure(tab_id).busy = true;
                        list_task(tab_id, c, ".".to_string())
                    }
                    // 无通道：先建立 SFTP 通道，结果经 `SftpReady` 回流。
                    (None, Some(conn)) => {
                        self.ensure(tab_id).busy = true;
                        Task::perform(open_sftp_task(conn), move |res| {
                            Event::Emit(Box::new(Message::SftpReady(
                                tab_id,
                                session_id,
                                res.map_err(|e| localize_error(&e)),
                            )))
                        })
                    }
                    // 既无通道又无连接：父层已在派发前拦截并提示，此处仅复位视图。
                    (None, None) => Task::none(),
                }
            }
            Message::SftpListed(tab_id, abs, result) => {
                let mut err: Option<String> = None;
                if let Some(tab) = self.per_tab.get_mut(&tab_id) {
                    tab.path = abs.clone();
                    tab.path_input = abs;
                    match result {
                        Ok(entries) => tab.entries = entries,
                        Err(e) => err = Some(e),
                    }
                    tab.busy = false;
                } else if let Err(e) = result {
                    err = Some(e);
                }
                match err {
                    Some(e) => Task::done(Event::Toast(
                        ToastKind::Error,
                        format!("{}: {e}", t!("app.list_failed")),
                    )),
                    None => Task::none(),
                }
            }
            Message::SftpCd(path) => {
                let client = self.per_tab.get(&active_tab).and_then(|t| t.client.clone());
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    tab.path = path.clone();
                    tab.path_input = path.clone();
                    tab.entries.clear();
                }
                match client {
                    Some(c) => list_task(active_tab, c, path),
                    None => Task::done(Event::Toast(ToastKind::Error, t!("sftp.not_connected"))),
                }
            }
            Message::SftpParent => {
                let client = self.per_tab.get(&active_tab).and_then(|t| t.client.clone());
                let parent = self
                    .per_tab
                    .get(&active_tab)
                    .map(|t| parent_path(&t.path))
                    .unwrap_or_else(|| ".".to_string());
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    tab.path = parent.clone();
                    tab.path_input = parent.clone();
                    tab.entries.clear();
                }
                match client {
                    Some(c) => list_task(active_tab, c, parent),
                    None => Task::done(Event::Toast(ToastKind::Error, t!("sftp.not_connected"))),
                }
            }
            Message::SftpPathInput(v) => {
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    tab.path_input = v;
                }
                Task::none()
            }
            Message::SftpSelect(name) => {
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    tab.selected = Some(name);
                }
                Task::none()
            }
            Message::SftpSelectHovered => {
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    // 右键瞬间快照命中条目，供菜单内容使用（“..” / 空白处 → None）；
                    // 只在此处更新，使已打开的菜单不随之后的鼠标移动而改变。
                    let target = tab.hovered.as_deref().and_then(|name| {
                        tab.entries
                            .iter()
                            .find(|e| e.name == name)
                            .map(|e| (e.name.clone(), e.is_dir))
                    });
                    if let Some(name) = tab.hovered.clone() {
                        // 悬浮项即光标下的条目；未在条目上悬浮（右击空白处 / 终端）时保持原选中不变。
                        tab.selected = Some(name);
                    }
                    tab.context_target = target;
                }
                Task::none()
            }
            Message::SftpEntryEnter(name) => {
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    tab.hovered = Some(name);
                }
                Task::none()
            }
            Message::SftpEntryExit(_name) => {
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    tab.hovered = None;
                }
                Task::none()
            }
            Message::SftpNewDirConfirm => {
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    tab.creating_dir = Some(t!("sftp.new_dir_name").to_string());
                }
                Task::none()
            }
            Message::SftpNewDirInput(v) => {
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    tab.creating_dir = Some(v);
                }
                Task::none()
            }
            Message::SftpNewDirSubmit => {
                let (client, name, path) = match self.per_tab.get_mut(&active_tab) {
                    Some(tab) => {
                        let name = tab.creating_dir.take();
                        (tab.client.clone(), name, tab.path.clone())
                    }
                    None => return Task::none(),
                };
                let Some(name) = name else {
                    return Task::none();
                };
                if name.trim().is_empty() {
                    return Task::none();
                }
                let Some(client) = client else {
                    return Task::done(Event::Toast(ToastKind::Error, t!("sftp.not_connected")));
                };
                let Some((parent, target)) = new_dir_targets(&path, &name) else {
                    return Task::done(Event::Toast(ToastKind::Error, t!("sftp.dir_name_invalid")));
                };
                Task::perform(
                    async move {
                        // 父级逐级补齐，末级单独创建：重名不在本地吞成成功，仍交服务端定夺。
                        ensure_remote_dir(&client, &parent).await?;
                        client.create_dir(&target).await
                    },
                    move |res| match res {
                        Ok(()) => Event::Emit(Box::new(Message::SftpCd(path))),
                        Err(e) => Event::Toast(
                            ToastKind::Error,
                            format!("{}: {}", t!("sftp.mkdir_failed"), localize_error(&e)),
                        ),
                    },
                )
            }
            Message::SftpRenameConfirm(from) => {
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    tab.renaming = Some((from.clone(), from));
                }
                Task::none()
            }
            Message::SftpRenameInput(v) => {
                if let Some(tab) = self.per_tab.get_mut(&active_tab)
                    && let Some(r) = tab.renaming.as_mut()
                {
                    r.1 = v;
                }
                Task::none()
            }
            Message::SftpRenameSubmit => {
                let (client, rename, path) = match self.per_tab.get_mut(&active_tab) {
                    Some(tab) => {
                        let renaming = tab.renaming.take();
                        (tab.client.clone(), renaming, tab.path.clone())
                    }
                    None => return Task::none(),
                };
                let Some((from, cur)) = rename else {
                    return Task::none();
                };
                if cur.trim().is_empty() || cur == from {
                    return Task::none();
                }
                let Some(client) = client else {
                    return Task::done(Event::Toast(ToastKind::Error, t!("sftp.not_connected")));
                };
                if !valid_rename_name(cur.trim()) {
                    return Task::done(Event::Toast(
                        ToastKind::Error,
                        t!("sftp.rename_name_invalid"),
                    ));
                }
                let from_path = join_path(&path, &from);
                let to_path = join_path(&path, &cur);
                Task::perform(
                    async move {
                        client
                            .rename(&from_path, &to_path)
                            .await
                            .map_err(|e| localize_error(&e))
                    },
                    move |res| match res {
                        Ok(()) => Event::Emit(Box::new(Message::SftpCd(path))),
                        Err(e) => Event::Toast(
                            ToastKind::Error,
                            format!("{}: {e}", t!("sftp.rename_failed")),
                        ),
                    },
                )
            }
            Message::SftpRenameShortcut => {
                let name = self
                    .per_tab
                    .get(&active_tab)
                    .and_then(|t| t.selected.clone());
                if let Some(name) = name
                    && let Some(tab) = self.per_tab.get_mut(&active_tab)
                {
                    tab.renaming = Some((name.clone(), name));
                }
                Task::none()
            }
            Message::SftpCancelDialog => {
                // 取消按优先级：内联重命名 > 内联新建文件夹 > 模态对话框（与 Esc 同入口）。
                let Some(tab) = self.per_tab.get_mut(&active_tab) else {
                    return Task::none();
                };
                if tab.renaming.take().is_some() || tab.creating_dir.take().is_some() {
                    // 退出内联输入态：焦点已离开文件管理器，交还终端（焦点态在父层）。
                    Task::done(Event::FocusTerminal)
                } else if let Some(SftpDialog::UploadConflicts { .. }) = tab.dialog.take() {
                    // 取消一批冲突＝该批整批不入队；排队的下一批（若有）接上弹出。
                    tab.dialog = tab.queued_conflicts.pop_front();
                    Task::none()
                } else {
                    tab.dialog = None;
                    Task::none()
                }
            }
            Message::SftpDialogConfirm => {
                let dialog = match self.per_tab.get_mut(&active_tab) {
                    Some(tab) => tab.dialog.take(),
                    None => return Task::none(),
                };
                let Some(dialog) = dialog else {
                    return Task::none();
                };
                match dialog {
                    SftpDialog::DeleteFile { name } => {
                        let (client, base_path) = match self.per_tab.get(&active_tab) {
                            Some(tab) => (tab.client.clone(), tab.path.clone()),
                            None => {
                                return Task::done(Event::Toast(
                                    ToastKind::Error,
                                    t!("sftp.not_connected"),
                                ));
                            }
                        };
                        let Some(client) = client else {
                            return Task::done(Event::Toast(
                                ToastKind::Error,
                                t!("sftp.not_connected"),
                            ));
                        };
                        let full = join_path(&base_path, &name);
                        Task::perform(
                            async move {
                                client
                                    .remove_file(&full)
                                    .await
                                    .map_err(|e| localize_error(&e))
                            },
                            move |res| match res {
                                Ok(()) => Event::Emit(Box::new(Message::SftpCd(base_path))),
                                Err(e) => Event::Toast(
                                    ToastKind::Error,
                                    format!("{}: {e}", t!("sftp.delete_failed")),
                                ),
                            },
                        )
                    }
                    SftpDialog::DeleteTree { name, .. } => {
                        let (client, base_path) = match self.per_tab.get(&active_tab) {
                            Some(tab) => (tab.client.clone(), tab.path.clone()),
                            None => {
                                return Task::done(Event::Toast(
                                    ToastKind::Error,
                                    t!("sftp.not_connected"),
                                ));
                            }
                        };
                        let Some(client) = client else {
                            return Task::done(Event::Toast(
                                ToastKind::Error,
                                t!("sftp.not_connected"),
                            ));
                        };
                        let full = join_path(&base_path, &name);
                        Task::perform(
                            async move {
                                match client.remove_dir_recursive(&full).await {
                                    Ok(report) if report.errors.is_empty() => {
                                        RemovalOutcome::Complete {
                                            files: report.files,
                                            dirs: report.dirs,
                                        }
                                    }
                                    Ok(report) => {
                                        // 汇总只点名第一个失败项：尽力而为语义下其余失败同理。
                                        let reason = report
                                            .errors
                                            .first()
                                            .map(|(p, e)| format!("{p}: {}", localize_error(e)))
                                            .unwrap_or_default();
                                        RemovalOutcome::Partial {
                                            files: report.files,
                                            dirs: report.dirs,
                                            failed: report.errors.len(),
                                            reason,
                                        }
                                    }
                                    Err(e) => RemovalOutcome::Failed(localize_error(&e)),
                                }
                            },
                            move |outcome| {
                                Event::Emit(Box::new(Message::SftpDeleteTreeDone(
                                    base_path, outcome,
                                )))
                            },
                        )
                    }
                    // 下载覆盖确认：单文件行上行普通下载；目录行上行文件夹作业（同一框形，措辞见渲染侧）。
                    SftpDialog::OverwriteDownload {
                        name,
                        local,
                        is_folder,
                        ..
                    } => Task::done(if is_folder {
                        Event::StartDownloadFolder(name, local)
                    } else {
                        Event::StartDownload(name, local)
                    }),
                    // 上传冲突逐条决策：按各行策略展开成最终入队项；全部跳过则不入队。
                    // 处理完这批再弹出排队里的下一批。
                    SftpDialog::UploadConflicts { items, rows } => {
                        if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                            tab.dialog = tab.queued_conflicts.pop_front();
                        }
                        let kept = apply_upload_decisions(&items, &rows);
                        if kept.is_empty() {
                            Task::none()
                        } else {
                            Task::done(Event::StartUpload(kept))
                        }
                    }
                    // 属性框为只读信息框，确认仅关闭。
                    SftpDialog::Properties { .. } => Task::none(),
                }
            }
            Message::SftpDeleteConfirm(name, is_dir) => {
                if !is_dir {
                    if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                        tab.dialog = Some(crate::state::SftpDialog::DeleteFile { name });
                    }
                    return Task::none();
                }
                let (client, base_path) = match self.per_tab.get(&active_tab) {
                    Some(tab) => (tab.client.clone(), tab.path.clone()),
                    None => {
                        return Task::done(Event::Toast(
                            ToastKind::Error,
                            t!("sftp.not_connected"),
                        ));
                    }
                };
                let Some(client) = client else {
                    return Task::done(Event::Toast(ToastKind::Error, t!("sftp.not_connected")));
                };
                // 先开「计数中」弹窗再发探测：计数是整树列举，慢网络上不该让弹窗缺席。
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    tab.dialog = Some(crate::state::SftpDialog::DeleteTree {
                        name: name.clone(),
                        probe: crate::state::DeleteProbe::Counting,
                    });
                }
                let full = join_path(&base_path, &name);
                Task::perform(
                    async move {
                        crate::app::folder_job::plan_remote_tree(&client, &full)
                            .await
                            .map(|plan| {
                                // 目录步 = 全部后代目录（不含根），正是提示里的「子目录数」。
                                let dirs = plan
                                    .steps
                                    .iter()
                                    .filter(|s| {
                                        matches!(s, crate::app::folder_job::Step::CreateDir { .. })
                                    })
                                    .count();
                                crate::state::DeleteProbe::Counts {
                                    files: plan.file_count,
                                    dirs,
                                    bytes: plan.total_bytes,
                                }
                            })
                            .unwrap_or(crate::state::DeleteProbe::Unknown)
                    },
                    move |probe| Event::Emit(Box::new(Message::SftpDeleteProbed(name, probe))),
                )
            }
            Message::SftpDeleteProbed(name, probe) => {
                if let Some(tab) = self.per_tab.get_mut(&active_tab)
                    && let Some(crate::state::SftpDialog::DeleteTree {
                        name: cur,
                        probe: p,
                        ..
                    }) = &mut tab.dialog
                {
                    // 只认同名弹窗：期间可能已取消、或改开了另一个目录的删除框。
                    if *cur == name {
                        *p = probe;
                    }
                }
                Task::none()
            }
            Message::SftpDeleteTreeDone(base_path, outcome) => {
                let toast = match outcome {
                    RemovalOutcome::Complete { files, dirs } => Event::Toast(
                        ToastKind::Success,
                        t!("sftp.deleted_tree", files => files, dirs => dirs),
                    ),
                    RemovalOutcome::Partial {
                        files,
                        dirs,
                        failed,
                        reason,
                    } => Event::Toast(
                        ToastKind::Error,
                        t!(
                            "sftp.delete_partial",
                            files => files,
                            dirs => dirs,
                            failed => failed,
                            reason => reason
                        ),
                    ),
                    RemovalOutcome::Failed(e) => Event::Toast(
                        ToastKind::Error,
                        format!("{}: {e}", t!("sftp.delete_failed")),
                    ),
                };
                // 三种结局都重扫目录：删除进行过多则树已变、少则远端可能已被他方改动，
                // 列表都不能留在旧快照。
                Task::batch([
                    Task::done(toast),
                    Task::done(Event::Emit(Box::new(Message::SftpCd(base_path)))),
                ])
            }
            Message::SftpShowProperties(name) => {
                // 组装 Properties 对话框：从当前目录条目里取出该名称的元数据。
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    let entry = tab.entries.iter().find(|e| e.name == name).cloned();
                    if let Some(entry) = entry {
                        let path = join_path(&tab.path, &name);
                        tab.dialog = Some(SftpDialog::Properties { entry, path });
                    }
                }
                Task::none()
            }
            Message::SftpCopyPath(path) => {
                // 文档约定：把完整路径写入系统剪贴板，并提示成功。
                Task::batch([
                    iced::clipboard::write::<Event>(path),
                    Task::done(Event::Toast(ToastKind::Success, t!("app.copied_path"))),
                ])
            }
            Message::SftpCopyName(name) => {
                // 与复制路径同构：把文件名写入系统剪贴板，并提示成功。
                Task::batch([
                    iced::clipboard::write::<Event>(name),
                    Task::done(Event::Toast(ToastKind::Success, t!("app.copied_name"))),
                ])
            }
            Message::SftpNoop => Task::none(),
            // 占位：实际跳转由路由层读取活动终端 cwd 后转 `SftpCd` 完成，模块不消费。
            Message::SftpGotoTerminalDir => Task::none(),
            Message::SftpPickUpload => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title(t!("sftp.upload_title"))
                        .pick_files()
                        .await
                        .map(|files| {
                            files
                                .into_iter()
                                .map(|f| {
                                    let p = f.path().to_path_buf();
                                    let rel = p
                                        .file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_default();
                                    (p, rel)
                                })
                                .collect::<Vec<(std::path::PathBuf, String)>>()
                        })
                },
                |items| match items {
                    // 选择结果先回模块做同名探测，由那边决定直接入队还是请用户确认。
                    Some(items) => Event::Emit(Box::new(Message::SftpUploadPicked(items))),
                    None => Event::Emit(Box::new(Message::SftpNoop)),
                },
            ),
            // 文件夹上传：每个所选文件夹只产生一个根条目（相对路径 = 目录名），一棵树占队列一行，
            // 树内展开由传输模块的作业执行体完成。
            Message::SftpPickUploadFolder => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title(t!("sftp.upload_folder_title"))
                        .pick_folders()
                        .await
                        .map(|folders| {
                            folders
                                .into_iter()
                                .filter_map(|f| {
                                    let p = f.path().to_path_buf();
                                    let name = p
                                        .file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .filter(|n| !n.is_empty())?;
                                    Some((p, name))
                                })
                                .collect::<Vec<(std::path::PathBuf, String)>>()
                        })
                },
                |items| match items {
                    Some(items) => Event::Emit(Box::new(Message::SftpUploadPicked(items))),
                    None => Event::Emit(Box::new(Message::SftpNoop)),
                },
            ),
            Message::SftpUploadPicked(items) => {
                let (client, path, listed) = match self.per_tab.get(&active_tab) {
                    Some(tab) => {
                        // 快照只取名字与类型：万一要顶上，整条元数据都用不上。
                        let listed: Vec<(String, bool)> = tab
                            .entries
                            .iter()
                            .map(|e| (e.name.clone(), e.is_dir))
                            .collect();
                        (tab.client.clone(), tab.path.clone(), listed)
                    }
                    None => return Task::done(Event::StartUpload(items)),
                };
                let Some(client) = client else {
                    return Task::done(Event::StartUpload(items));
                };
                // 入队前探一次目标目录；列举失败退回上面那份快照——探测本身不该挡住上传。
                Task::perform(
                    async move {
                        let existing: HashMap<String, bool> = match client.list_dir(&path).await {
                            Ok(entries) => {
                                entries.into_iter().map(|e| (e.name, e.is_dir)).collect()
                            }
                            Err(e) => {
                                warn!("upload conflict probe failed for {path}: {e}");
                                listed.into_iter().collect()
                            }
                        };
                        (path, items, existing)
                    },
                    |(path, items, existing)| {
                        let rels: Vec<&str> = items.iter().map(|(_, rel)| rel.as_str()).collect();
                        let (files, dirs) = upload_conflicts(&rels, &existing);
                        if files.is_empty() && dirs.is_empty() {
                            return Event::StartUpload(items);
                        }
                        Event::Emit(Box::new(Message::SftpUploadConflict(
                            path, items, files, dirs, existing,
                        )))
                    },
                )
            }
            Message::SftpUploadConflict(path, items, files, dirs, existing) => {
                // 结论只对「仍是那个目录」的视图成立：视图已切走或已不在，就按老行为直接入队。
                let Some(tab) = self.per_tab.get_mut(&active_tab) else {
                    return Task::done(Event::StartUpload(items));
                };
                if tab.path != path {
                    return Task::done(Event::StartUpload(items));
                }
                let dialog = SftpDialog::UploadConflicts {
                    items,
                    rows: build_conflict_rows(&files, &dirs, &existing),
                };
                // 上一批还没处理完：排队，不覆盖正在显示的对话框（否则前一批被静默丢弃）。
                if matches!(tab.dialog, Some(SftpDialog::UploadConflicts { .. })) {
                    tab.queued_conflicts.push_back(dialog);
                } else {
                    tab.dialog = Some(dialog);
                }
                Task::none()
            }
            Message::SftpConflictDecide(index, decision) => {
                if let Some(SftpDialog::UploadConflicts { rows, .. }) = self
                    .per_tab
                    .get_mut(&active_tab)
                    .and_then(|tab| tab.dialog.as_mut())
                    && let Some(row) = rows.get_mut(index)
                {
                    row.decision = decision;
                }
                Task::none()
            }
            Message::SftpConflictApplyAll(decision) => {
                if let Some(SftpDialog::UploadConflicts { rows, .. }) = self
                    .per_tab
                    .get_mut(&active_tab)
                    .and_then(|tab| tab.dialog.as_mut())
                {
                    for row in rows.iter_mut() {
                        row.decision = decision;
                    }
                }
                Task::none()
            }
            Message::SftpPickDownload(name) => self.pick_download_dir(active_tab, name, {
                |name, dir| Message::SftpDownload(name, dir)
            }),
            Message::SftpPickDownloadFolder(name) => {
                self.pick_download_dir(active_tab, name, |name, dir| {
                    Message::SftpDownloadFolder(name, dir)
                })
            }
            Message::SftpDownload(name, dir) => {
                // 记住本次选定的目录，作为下次选择器的起始位置（字段语义见 `SftpView::download_dir`）。
                self.remember_download_dir(active_tab, &dir);
                let local = dir.join(&name);
                // 本地已存在同名文件：先弹覆盖确认框，确认后再上行给传输模块创建下载传输。
                if local.exists() {
                    if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                        tab.dialog = Some(SftpDialog::OverwriteDownload {
                            name: name.clone(),
                            local: local.clone(),
                            transfer_id: 0,
                            is_folder: false,
                        });
                    }
                    Task::none()
                } else {
                    Task::done(Event::StartDownload(name, local))
                }
            }
            Message::SftpDownloadFolder(name, dir) => {
                self.remember_download_dir(active_tab, &dir);
                let local = dir.join(&name);
                // 首层判定只问落点根：同名真目录合并一次确认后整树覆盖其中同名文件，
                // 子树逐文件不再问——与上传侧首层三档决策对称。
                match classify_folder_download_target(&local) {
                    FolderDownloadTarget::Free => {
                        Task::done(Event::StartDownloadFolder(name, local))
                    }
                    FolderDownloadTarget::Merge => {
                        if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                            tab.dialog = Some(SftpDialog::OverwriteDownload {
                                name,
                                local,
                                transfer_id: 0,
                                is_folder: true,
                            });
                        }
                        Task::none()
                    }
                    FolderDownloadTarget::Blocked => Task::done(Event::Toast(
                        ToastKind::Error,
                        t!("sftp.download_folder_blocked", name => name),
                    )),
                }
            }
            Message::SftpActionDone(tab_id, _result) => {
                if let Some(tab) = self.per_tab.get_mut(&tab_id) {
                    tab.busy = false;
                }
                Task::none()
            }
        }
    }

    /// 下载选择器公共骨架：从上次记住的落点打开目录选择器，结果经 `on_picked` 回发自回路。
    ///
    /// 起始目录取「上次选择的下载目录」，首次为系统下载目录（`~/Downloads`）——不传
    /// `set_directory` 时 rfd 用平台默认值（通常是家目录），与用户预期不符。
    fn pick_download_dir(
        &self,
        active_tab: u64,
        name: String,
        on_picked: fn(String, std::path::PathBuf) -> Message,
    ) -> Task<Event> {
        let start = self
            .per_tab
            .get(&active_tab)
            .map(|tab| tab.download_dir.clone())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| {
                rterm_config::paths::download_dir()
                    .to_string_lossy()
                    .to_string()
            });
        Task::perform(
            async move {
                rfd::AsyncFileDialog::new()
                    .set_title(t!("sftp.download_title"))
                    .set_directory(&start)
                    .pick_folder()
                    .await
                    .map(|handle| handle.path().to_path_buf())
            },
            move |dir| match dir {
                Some(dir) => Event::Emit(Box::new(on_picked(name, dir))),
                None => Event::Emit(Box::new(Message::SftpNoop)),
            },
        )
    }

    /// 记住本次选定的本地落点，作为下次下载选择器的起始位置（字段语义见 `SftpView::download_dir`）。
    fn remember_download_dir(&mut self, active_tab: u64, dir: &std::path::Path) {
        if let Some(tab) = self.per_tab.get_mut(&active_tab) {
            tab.download_dir = dir.to_string_lossy().to_string();
        }
    }

    /// 订阅：当前为占位（内部异步结果暂由父层 `open_files` 的任务产生，
    /// 后续把列举 / 写操作流式逻辑迁入此处并 `map` 为 `Message`）。
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }
}

/// 列举某标签的指定远端路径：解析为绝对路径并取回条目，结果经 `SftpListed` 回流（自回路）。
///
/// 路径相对 / “.” 均可，由 [`resolve_and_list_task`] 在核心层走 REALPATH 展开。
fn list_task(tab_id: u64, client: Arc<SftpClient>, path: String) -> Task<Event> {
    Task::perform(
        resolve_and_list_task(client, path.clone()),
        move |res| match res {
            Ok((abs, entries)) => {
                Event::Emit(Box::new(Message::SftpListed(tab_id, abs, Ok(entries))))
            }
            Err(e) => Event::Emit(Box::new(Message::SftpListed(
                tab_id,
                path,
                Err(localize_error(&e)),
            ))),
        },
    )
}

/// 目录下载的首层同名判定结果（只看落点根自身，不游子树）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FolderDownloadTarget {
    /// 落点无同名条目：直接入队作业。
    Free,
    /// 同名**真目录**：整树合并写一次确认，确认即覆盖其中同名文件。
    Merge,
    /// 同名文件或符号链接：挡住建目录，拒绝并提示用户先处理。
    Blocked,
}

/// 判据用 `symlink_metadata`（不跟随链接）：指向目录的符号链接同样算 `Blocked`——
/// 写穿它会把数据落到用户所选落点之外，不能代为合并。
pub(crate) fn classify_folder_download_target(local: &std::path::Path) -> FolderDownloadTarget {
    match std::fs::symlink_metadata(local) {
        Err(_) => FolderDownloadTarget::Free,
        Ok(meta) if meta.is_dir() => FolderDownloadTarget::Merge,
        Ok(_) => FolderDownloadTarget::Blocked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 目录下载首层判定三态：不存在 = Free；真目录 = Merge；普通文件与符号链接都是 Blocked。
    #[test]
    fn classify_folder_download_target_three_states() {
        let base = std::env::temp_dir().join(format!("rterm_fdl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        assert_eq!(
            classify_folder_download_target(&base.join("none")),
            FolderDownloadTarget::Free
        );
        std::fs::create_dir(base.join("realdir")).unwrap();
        assert_eq!(
            classify_folder_download_target(&base.join("realdir")),
            FolderDownloadTarget::Merge
        );
        std::fs::write(base.join("file"), b"x").unwrap();
        assert_eq!(
            classify_folder_download_target(&base.join("file")),
            FolderDownloadTarget::Blocked
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("realdir"), base.join("link")).unwrap();
            assert_eq!(
                classify_folder_download_target(&base.join("link")),
                FolderDownloadTarget::Blocked,
                "链接指向目录也不放行：写穿链接会逃出用户选的落点"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 删除入口按菜单快照携带的条目类型分流：文件直接装配确认框；目录要先探测计数，
    /// 无通道时不弹（计数无从谈起）。装配处曾把类型硬编码为 `false`，使目录删除永远发到
    /// `remove_file` 上、必然失败。
    #[test]
    fn delete_confirm_routes_by_the_entry_kind_from_the_menu() {
        let mut state = State::default();
        state.ensure(7);

        let _ = state.update(Message::SftpDeleteConfirm("docs".to_string(), true), 7);
        assert!(
            state.tab(7).and_then(|tab| tab.dialog.as_ref()).is_none(),
            "无通道时目录删除不应弹窗"
        );

        let _ = state.update(
            Message::SftpDeleteConfirm("notes.txt".to_string(), false),
            7,
        );
        match state.tab(7).and_then(|tab| tab.dialog.as_ref()) {
            Some(SftpDialog::DeleteFile { name }) => assert_eq!(name, "notes.txt"),
            other => panic!("期望单文件删除确认弹窗，实际 {other:?}"),
        }
    }

    /// 计数探测回流只更新同名弹窗：乱序 / 错名的迟到结果不得盖掉当前弹窗的探测态。
    #[test]
    fn delete_tree_probe_updates_only_the_matching_dialog() {
        let mut state = State::default();
        state.ensure(7);
        {
            let tab = state.tab_mut(7).unwrap();
            tab.dialog = Some(SftpDialog::DeleteTree {
                name: "docs".to_string(),
                probe: crate::state::DeleteProbe::Counting,
            });
        }

        let _ = state.update(
            Message::SftpDeleteProbed(
                "docs".to_string(),
                crate::state::DeleteProbe::Counts {
                    files: 2,
                    dirs: 1,
                    bytes: 70,
                },
            ),
            7,
        );
        match state.tab(7).and_then(|tab| tab.dialog.as_ref()) {
            Some(SftpDialog::DeleteTree {
                probe: crate::state::DeleteProbe::Counts { files, dirs, bytes },
                ..
            }) => {
                assert_eq!((*files, *dirs, *bytes), (2, 1, 70));
            }
            other => panic!("探测结果应写回弹窗，实际 {other:?}"),
        }

        // 迟到的异名结果被忽略：弹窗仍持上一次的计数。
        let _ = state.update(
            Message::SftpDeleteProbed("other".to_string(), crate::state::DeleteProbe::Unknown),
            7,
        );
        assert!(
            matches!(
                state.tab(7).and_then(|tab| tab.dialog.as_ref()),
                Some(SftpDialog::DeleteTree {
                    probe: crate::state::DeleteProbe::Counts { .. },
                    ..
                })
            ),
            "异名探测结果不得改写当前弹窗"
        );
    }

    /// 目录删除确认一次即放行：弹窗被消费、进入执行分支（单测无通道，执行以
    /// not_connected 收尾）；计数探测尚未回流的弹窗同样可确认。
    #[test]
    fn delete_tree_confirm_executes_in_one_step() {
        let mut state = State::default();
        state.ensure(7);
        {
            let tab = state.tab_mut(7).unwrap();
            tab.dialog = Some(SftpDialog::DeleteTree {
                name: "docs".to_string(),
                probe: crate::state::DeleteProbe::Counting,
            });
        }

        let _ = state.update(Message::SftpDialogConfirm, 7);
        assert!(
            state.tab(7).and_then(|tab| tab.dialog.as_ref()).is_none(),
            "确认必须直接消费弹窗进入执行，不得有任何二次解锁环节"
        );
    }

    /// 作废通道：会话标记 / 目录 / 列表 / 选中 / 内联输入与对话框全部清空，视图回到
    /// 未打开态（`client` 无法在单测中构造，与 `session` 同处清空）。
    #[test]
    fn invalidate_resets_the_tab_view() {
        let mut state = State::default();
        {
            let view = state.ensure(7);
            view.session = Some("s1".to_string());
            view.path = "/var/log".to_string();
            view.path_input = "/var/log".to_string();
            view.entries.push(rterm_core::FileEntry {
                name: "syslog".to_string(),
                is_dir: false,
                size: 42,
                modified: None,
                permissions: None,
                user: None,
                group: None,
            });
            view.selected = Some("syslog".to_string());
            view.hovered = Some("syslog".to_string());
            view.creating_dir = Some("New Folder".to_string());
            view.renaming = Some(("a".to_string(), "b".to_string()));
            view.dialog = Some(SftpDialog::DeleteFile {
                name: "syslog".to_string(),
            });
            view.busy = true;
        }

        state.invalidate(7);

        let view = state.tab(7).expect("视图仍在，只是被复位");
        assert!(view.session.is_none());
        assert!(view.client.is_none());
        assert!(!view.busy);
        assert_eq!(view.path, ".");
        assert_eq!(view.path_input, ".");
        assert!(view.entries.is_empty());
        assert!(view.selected.is_none());
        assert!(view.hovered.is_none());
        assert!(view.creating_dir.is_none());
        assert!(view.renaming.is_none());
        assert!(view.dialog.is_none());
        assert!(view.queued_conflicts.is_empty());

        // 作废不存在的标签只是无操作。
        state.invalidate(404);
    }

    /// 造一批「与现有条目同名」的冲突探测结论：单文件 `name` 相对路径，目录快照里就有它。
    fn conflict_batch(name: &str) -> Message {
        Message::SftpUploadConflict(
            ".".to_string(),
            vec![(
                std::path::PathBuf::from(format!("/l/{name}")),
                name.to_string(),
            )],
            vec![name.to_string()],
            Vec::new(),
            std::collections::HashMap::from([(name.to_string(), false)]),
        )
    }

    /// 连点两批上传时，后一批必须排队而不是覆盖正在显示的对话框。
    #[test]
    fn second_conflict_batch_queues_behind_the_first() {
        let mut state = State::default();
        state.ensure(7);

        let _ = state.update(conflict_batch("a.txt"), 7);
        let _ = state.update(conflict_batch("b.txt"), 7);

        let view = state.tab(7).expect("视图存在");
        match view.dialog.as_ref() {
            Some(SftpDialog::UploadConflicts { rows, .. }) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].name, "a.txt");
            }
            other => panic!("期望第一批冲突对话框，实际 {other:?}"),
        }
        assert_eq!(view.queued_conflicts.len(), 1);
    }

    /// 确认与取消都要弹出排队中的下一批；取消不把该批入队（上行事件在父层，这里只验状态推进）。
    #[test]
    fn finishing_a_conflict_batch_promotes_the_next_queued_one() {
        let mut state = State::default();
        state.ensure(7);
        let _ = state.update(conflict_batch("a.txt"), 7);
        let _ = state.update(conflict_batch("b.txt"), 7);

        let _ = state.update(Message::SftpDialogConfirm, 7);
        let view = state.tab(7).expect("视图存在");
        match view.dialog.as_ref() {
            Some(SftpDialog::UploadConflicts { rows, .. }) => {
                assert_eq!(rows[0].name, "b.txt", "确认后应弹出第二批");
            }
            other => panic!("期望第二批冲突对话框，实际 {other:?}"),
        }
        assert!(view.queued_conflicts.is_empty());

        let _ = state.update(Message::SftpCancelDialog, 7);
        assert!(state.tab(7).expect("视图存在").dialog.is_none());
    }

    /// 「应用到全部」把当前对话框的每一行都设为同一策略，逐行改判互不串台。
    #[test]
    fn apply_all_and_per_row_decisions_write_through_rows() {
        let two = Message::SftpUploadConflict(
            ".".to_string(),
            vec![
                (std::path::PathBuf::from("/l/a.txt"), "a.txt".to_string()),
                (std::path::PathBuf::from("/l/b.txt"), "b.txt".to_string()),
            ],
            vec!["a.txt".to_string(), "b.txt".to_string()],
            Vec::new(),
            std::collections::HashMap::from([
                ("a.txt".to_string(), false),
                ("b.txt".to_string(), false),
            ]),
        );
        let mut state = State::default();
        state.ensure(9);
        let _ = state.update(two, 9);

        let _ = state.update(Message::SftpConflictApplyAll(ConflictDecision::Skip), 9);
        let rows_of = |s: &State| match s.tab(9).and_then(|t| t.dialog.as_ref()) {
            Some(SftpDialog::UploadConflicts { rows, .. }) => rows.clone(),
            other => panic!("期望冲突对话框，实际 {other:?}"),
        };
        assert!(
            rows_of(&state)
                .iter()
                .all(|r| r.decision == ConflictDecision::Skip)
        );

        let _ = state.update(Message::SftpConflictDecide(0, ConflictDecision::Rename), 9);
        let rows = rows_of(&state);
        assert_eq!(rows[0].decision, ConflictDecision::Rename);
        assert_eq!(rows[1].decision, ConflictDecision::Skip);
    }
}
