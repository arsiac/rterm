//! SFTP 文件管理模块（浏览 / 导航 / 新建 / 重命名 / 删除 / 属性 / 上传下载入口）
use std::collections::HashMap;
use std::sync::Arc;

use iced::{Subscription, Task};

use crate::app::tasks::{join_path, open_sftp_task, parent_path, resolve_and_list_task};
use crate::i18n::localize_error;
use crate::state::{CenterView, SftpDialog, SftpView, ToastKind};
use crate::t;
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

    /// 查询某标签是否已建立 SFTP 会话（用于「切到文件管理自动打开」判定）。
    pub fn tab_session(&self, tab_id: u64) -> Option<String> {
        self.per_tab.get(&tab_id).and_then(|s| s.session.clone())
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
    /// 打开系统文件夹选择器以选择下载目标目录（携带远端文件名）。
    SftpPickDownload(String),
    /// 下载远端文件（携带远端名称与本地目标目录），经覆盖检查后上行给传输模块。
    SftpDownload(String, std::path::PathBuf),
    /// SFTP 写操作完成（归属标签 id + 结果）。
    SftpActionDone(u64, Result<(), String>),
    /// 请求打开“删除确认”对话框（携带名称）。
    SftpDeleteConfirm(String),
    /// 请求打开“属性”对话框（携带名称，展示文件详细信息）。
    SftpShowProperties(String),
    /// 复制远端条目的完整路径到系统剪贴板（携带完整路径）。
    SftpCopyPath(String),
    /// 跳转到终端当前所在目录（由路由层读取活动终端 cwd 后转 [`SftpCd`] 处理，
    /// 模块自身不持有终端访问权，故此处仅作占位、不在此分支消费）。
    SftpGotoTerminalDir,
    /// 模块内部无操作占位（如文件选择器被取消，避免父层需要一个无意义的事件）。
    SftpNoop,
    /// 请求在文件列表内联进入“新建文件夹”输入态（默认名称 “New Folder”）。
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
                            format!("{}: {e}", t!("sftp.list_failed")),
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
                        format!("{}: {e}", t!("sftp.list_failed")),
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
                if let Some(tab) = self.per_tab.get_mut(&active_tab)
                    && let Some(name) = tab.hovered.clone()
                {
                    // 悬浮项即光标下的条目；未在条目上悬浮（右击空白处 / 终端）时保持原选中不变。
                    tab.selected = Some(name);
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
                    tab.creating_dir = Some("New Folder".to_string());
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
                let new_path = join_path(&path, &name);
                Task::perform(
                    async move {
                        client
                            .create_dir(&new_path)
                            .await
                            .map_err(|e| localize_error(&e))
                    },
                    move |res| match res {
                        Ok(()) => Event::Emit(Box::new(Message::SftpCd(path))),
                        Err(e) => Event::Toast(
                            ToastKind::Error,
                            format!("{}: {e}", t!("sftp.mkdir_failed")),
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
                    SftpDialog::Delete { name, is_dir } => {
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
                                let res = if is_dir {
                                    client.remove_dir(&full).await
                                } else {
                                    client.remove_file(&full).await
                                };
                                res.map_err(|e| localize_error(&e))
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
                    // 下载覆盖确认：按对话框中记录的本地路径与远端名称上行给传输模块创建下载传输。
                    SftpDialog::OverwriteDownload { name, local, .. } => {
                        Task::done(Event::StartDownload(name, local))
                    }
                    // 属性框为只读信息框，确认仅关闭。
                    SftpDialog::Properties { .. } => Task::none(),
                }
            }
            Message::SftpDeleteConfirm(name) => {
                if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                    tab.dialog = Some(crate::state::SftpDialog::Delete {
                        name,
                        is_dir: false,
                    });
                }
                Task::none()
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
                    Task::done(Event::Toast(ToastKind::Success, "已复制路径".to_string())),
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
                    Some(items) => Event::StartUpload(items),
                    None => Event::Emit(Box::new(Message::SftpNoop)),
                },
            ),
            // 文件夹上传：选一个或多个本地文件夹，递归展平为带层级相对路径的上传项后上行。
            Message::SftpPickUploadFolder => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title(t!("sftp.upload_folder_title"))
                        .pick_folders()
                        .await
                        .map(|folders| {
                            folders
                                .into_iter()
                                .flat_map(|f| crate::app::tasks::collect_upload_items(f.path()))
                                .collect::<Vec<(std::path::PathBuf, String)>>()
                        })
                },
                |items| match items {
                    Some(items) => Event::StartUpload(items),
                    None => Event::Emit(Box::new(Message::SftpNoop)),
                },
            ),
            Message::SftpPickDownload(name) => Task::perform(
                async move {
                    rfd::AsyncFileDialog::new()
                        .set_title(t!("sftp.download_title"))
                        .pick_folder()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                move |dir| match dir {
                    Some(dir) => {
                        Event::Emit(Box::new(Message::SftpDownload(name, dir.to_path_buf())))
                    }
                    None => Event::Emit(Box::new(Message::SftpNoop)),
                },
            ),
            Message::SftpDownload(name, dir) => {
                let local = dir.join(&name);
                // 本地已存在同名文件：先弹覆盖确认框，确认后再上行给传输模块创建下载传输。
                if local.exists() {
                    if let Some(tab) = self.per_tab.get_mut(&active_tab) {
                        tab.dialog = Some(SftpDialog::OverwriteDownload {
                            name: name.clone(),
                            local: local.clone(),
                            transfer_id: 0,
                        });
                    }
                    Task::none()
                } else {
                    Task::done(Event::StartDownload(name, local))
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
