//! 会话管理模块

use crate::state::ToastKind;
use crate::t;

use iced::Task;
use log::error;
use rterm_config::{SessionConfig, SessionStore, export_sessions, import_sessions, new_id};
use rterm_crypto::Vault;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

/// 会话编辑器可编辑的字段，供 [`Message::EditorField`] 区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionField {
    /// 显示名称。
    Name,
    /// 主机地址。
    Host,
    /// 端口（字符串，保存时解析）。
    Port,
    /// 登录用户名。
    Username,
    /// 认证方式（取值为 `password` / `publickey` / `agent`）。
    Auth,
    /// 密码。
    Password,
    /// 私钥文件路径。
    KeyPath,
    /// 私钥口令。
    Passphrase,
    /// 分组名。
    Group,
}

/// 会话编辑器草稿：所有字段以字符串保存，避免 `text_input` 借用在渲染后失效。
///
/// 保存时再解析为 [`SessionConfig`]。
#[derive(Clone)]
pub struct EditorDraft {
    /// 会话唯一标识。
    pub id: String,
    /// 显示名称。
    pub name: String,
    /// 主机地址。
    pub host: String,
    /// 端口（字符串形式）。
    pub port: String,
    /// 登录用户名。
    pub username: String,
    /// 认证方式（`password` / `publickey` / `agent`）。
    pub auth: String,
    /// 密码。
    pub password: String,
    /// 私钥文件路径。
    pub key_path: String,
    /// 私钥口令。
    pub passphrase: String,
    /// 分组名。
    pub group: String,
    /// 保存校验失败信息（显示在弹窗内），修改任意字段时清除。
    pub error: Option<String>,
    /// 编辑已有会话时保留的原始认证（含密文信封）；新建会话为 `None`。
    /// 凭据字段留空保存时沿用此处信封，避免误将空串当新凭据加密。
    pub(crate) orig_auth: Option<rterm_config::AuthMethod>,
}

impl EditorDraft {
    /// 构造空白草稿（用于「新建会话」）。
    pub fn new() -> Self {
        Self {
            id: new_id(),
            name: String::new(),
            host: String::new(),
            port: rterm_config::DEFAULT_SSH_PORT.to_string(),
            username: String::new(),
            auth: "password".to_string(),
            password: String::new(),
            key_path: String::new(),
            passphrase: String::new(),
            group: String::new(),
            error: None,
            orig_auth: None,
        }
    }

    /// 由会话配置构造草稿（用于「编辑」）。
    ///
    /// 凭据字段在编辑器内留空（占位「保持不变」），原始信封暂存于 `Self::orig_auth`；
    /// 仅当用户键入新凭据时才重新加密。
    pub fn from_config(cfg: &SessionConfig) -> Self {
        let (auth, key_path) = match &cfg.auth {
            rterm_config::AuthMethod::Password { .. } => ("password".into(), String::new()),
            rterm_config::AuthMethod::PublicKey { key_path, .. } => {
                ("publickey".into(), key_path.to_string_lossy().to_string())
            }
            rterm_config::AuthMethod::Agent => ("agent".into(), String::new()),
        };
        Self {
            id: cfg.id.clone(),
            name: cfg.name.clone(),
            host: cfg.host.clone(),
            port: cfg.port.to_string(),
            username: cfg.username.clone(),
            auth,
            password: String::new(),
            key_path,
            passphrase: String::new(),
            group: cfg.group.clone().unwrap_or_default(),
            error: None,
            orig_auth: Some(cfg.auth.clone()),
        }
    }

    /// 由草稿构建会话配置：校验主机必填、端口为非 0 数字；
    /// 名称选填，为空时默认 `user@host`。失败返回错误字符串。
    ///
    /// `vault` 用于把键入的明文凭据加密为信封；凭据字段留空时沿用 `Self::orig_auth`
    /// 中已有的信封（编辑场景），实现「不改动即保持不变」。
    pub fn to_config(&self, vault: &Vault) -> Result<SessionConfig, String> {
        if self.host.trim().is_empty() {
            return Err(t!("session.host_empty"));
        }
        let port = self
            .port
            .parse::<u16>()
            .map_err(|_| t!("session.port_invalid"))?;
        if port == 0 {
            return Err(t!("session.port_zero"));
        }
        let name = if self.name.trim().is_empty() {
            format!("{}@{}", self.username.trim(), self.host.trim())
        } else {
            self.name.clone()
        };
        let auth = match self.auth.as_str() {
            "publickey" => {
                let passphrase = if !self.passphrase.is_empty() {
                    Some(vault.encrypt(&self.passphrase))
                } else if let Some(rterm_config::AuthMethod::PublicKey { passphrase, .. }) =
                    &self.orig_auth
                {
                    passphrase.clone()
                } else {
                    None
                };
                rterm_config::AuthMethod::PublicKey {
                    key_path: self.key_path.clone().into(),
                    passphrase,
                }
            }
            "agent" => rterm_config::AuthMethod::Agent,
            _ => {
                let password = if !self.password.is_empty() {
                    Some(vault.encrypt(&self.password))
                } else if let Some(rterm_config::AuthMethod::Password { password }) =
                    &self.orig_auth
                {
                    password.clone()
                } else {
                    // 凭据留空且无原值：标记为「未设置密码」，连接前需在编辑中补填。
                    None
                };
                rterm_config::AuthMethod::Password { password }
            }
        };
        Ok(SessionConfig {
            id: self.id.clone(),
            name,
            host: self.host.clone(),
            port,
            username: self.username.clone(),
            auth,
            group: if self.group.is_empty() {
                None
            } else {
                Some(self.group.clone())
            },
        })
    }
}

/// 模块状态：会话列表与编辑器的 UI 私有字段。
///
/// 原散落在 `App` 上的 `store` / `sessions` / `hovered_session` / `collapsed_groups` / `editor`
/// 统一收归此处。模块 `update` 只改自身 `State`；跨父层写回一律经 [`Event`]。
#[derive(Clone)]
pub struct State {
    /// 会话配置持久化器（若初始化失败则为 `None`，此时保存会被跳过）。
    pub store: Option<SessionStore>,
    /// 已保存的全部会话配置。
    pub sessions: Vec<SessionConfig>,
    /// 当前被鼠标悬浮的会话 id（`None` 表示无悬浮），用于渲染会话列表行的悬浮高亮背景。
    pub hovered_session: Option<String>,
    /// 当前选中的会话 id（`None` 表示无选中），用于渲染会话列表行的选中背景。
    ///
    /// 右键唤出某会话的右键菜单时按 [`Message::SessionSelectHovered`] 设置，
    /// 使菜单作用于哪一条在视觉上可辨。
    pub selected_session: Option<String>,
    /// 被折叠的分组键集合（空串表示「未分组」区块），点击分组头在集合内增删以切换展开态。
    pub collapsed_groups: HashSet<String>,
    /// 会话编辑器中的草稿（无则未编辑）。
    pub editor: Option<EditorDraft>,
}

impl State {
    /// 用初始存储与会话列表构造模块状态。
    pub fn new(store: Option<SessionStore>, sessions: Vec<SessionConfig>) -> Self {
        Self {
            store,
            sessions,
            hovered_session: None,
            selected_session: None,
            collapsed_groups: HashSet::new(),
            editor: None,
        }
    }

    /// 持久化会话列表：失败仅记录日志（保险库未就绪则跳过）。返回错误文案（若有），
    /// 供调用方经 [`Event::Status`] 上行提示。
    fn save(&mut self, ctx: &Ctx) -> Option<String> {
        let store = self.store.as_ref()?;
        let Some(vault) = ctx.vault.as_ref() else {
            error!("保险库未就绪，无法保存会话");
            return Some(t!("app.vault_locked"));
        };
        let header = vault.header();
        match store.save(&self.sessions, header) {
            Ok(()) => None,
            Err(e) => {
                error!("保存会话失败: {e}");
                Some(t!("app.save_failed", err => e))
            }
        }
    }

    /// 写入单条会话配置：覆盖同名 id 或追加，并落盘（字段校验已在
    /// [`EditorDraft::to_config`] 完成，本函数不重复校验）。
    fn store_session(
        &mut self,
        cfg: SessionConfig,
        header: &rterm_crypto::CryptoHeader,
    ) -> Result<(), String> {
        // 覆盖同名 id 或追加。
        if let Some(existing) = self.sessions.iter_mut().find(|s| s.id == cfg.id) {
            *existing = cfg.clone();
        } else {
            self.sessions.push(cfg.clone());
        }
        if let Some(store) = self.store.as_ref() {
            store
                .save(&self.sessions, header)
                .map_err(|e| format!("保存失败: {e}"))?;
        }
        Ok(())
    }
}

/// 模块内部消息：会话列表与编辑器的 UI 意图。
///
/// 父层经 `Message::Session` 路由进来，模块 `update` 自行消费，不外泄。
#[derive(Clone)]
pub enum Message {
    /// 新建会话（打开空白编辑器）。
    NewSession,
    /// 编辑已有会话（携带会话 id）。
    EditSession(String),
    /// 删除会话（携带会话 id）。
    DeleteSession(String),
    /// 保存当前编辑器中的会话。
    SaveSession,
    /// 取消编辑。
    CancelEdit,
    /// 从持久化存储重新加载会话列表（丢弃未保存于存储的仅内存变更）。
    RefreshSessions,
    /// 编辑器字段变更（字段类型 + 新值）。
    EditorField(SessionField, String),
    /// 打开系统文件选择器以选取私钥文件（结果经 `Event::Emit` 自回路回填 KeyPath 字段）。
    PickKeyFile,
    /// 私钥文件选择结果（`None` 表示用户取消对话框）。
    KeyFilePicked(Option<String>),
    /// 折叠 / 展开某分组（携带分组键：空串表示「未分组」区块）。
    ToggleGroup(String),
    /// 左键按下选中会话（携带会话 id）。
    SessionSelect(String),
    /// 鼠标进入会话列表某一项（携带会话 id），用于渲染悬浮高亮。
    SessionEnter(String),
    /// 鼠标离开会话列表某一项（携带会话 id），用于清除悬浮高亮。
    SessionExit(String),
    /// 右键按下：把光标所在的会话（即当前悬浮项）标记为选中。
    ///
    /// 不携带 id——`iced_aw::ContextMenu` 会先捕获右键事件，行内的 `on_right_press`
    /// 收不到，故改由全局监听（见 `App::subscription`）下发本消息，选中目标在此按
    /// [`State::hovered_session`] 判定。
    SessionSelectHovered,
    /// 发起连接（携带会话 id）：经 `Event::Connect` 上行，由父层开标签并连接。
    ConnectSession(String),
    /// 打开某会话的文件管理（携带会话 id），右键菜单「打开文件管理」触发：
    /// 经 `Event::OpenFiles` 上行，由父层打开对应标签的 SFTP。
    OpenFiles(String),
    /// 打开系统文件选择器以指定导出目标文件（结果经 `Event::Emit` 自回路流入
    /// [`Message::ExportSessionsToFile]`）。
    ExportSessions,
    /// 导出到所选文件（携带路径，`None` 表示用户取消对话框）。
    ExportSessionsToFile(Option<PathBuf>),
    /// 打开系统文件选择器以选取导入文件（结果经 `Event::Emit` 自回路流入
    /// [`Message::ImportSessionsFromFile`]。
    ImportSessions,
    /// 从所选文件导入会话（携带路径，`None` 表示用户取消对话框）。
    ImportSessionsFromFile(Option<PathBuf>),
}

/// 上行事件：仅通知父层，由父层 `Message::SessionEvent` 分支修改父状态并落地。
///
/// 模块绝不写父状态；开标签 / 关标签 / 状态栏 / toast 一律经对应事件由父层落地。
#[derive(Clone)]
pub enum Event {
    /// 发起连接（携带会话 id）：父层据此开标签并异步连接。
    Connect(String),
    /// 打开某会话的文件管理（携带会话 id）：父层据此打开对应标签的 SFTP 面板。
    OpenFiles(String),
    /// 某会话被删除（携带会话 id）：父层关闭该会话的全部终端标签。
    SessionDeleted(String),
    /// 写回状态栏提示（携带文案，`None` 表示清除）。
    Status(Option<String>),
    /// 弹 toast 通知（携带类型与文案，如导入 / 导出结果）。
    Toast(ToastKind, String),
    /// 自回路：把内部消息派发回本模块自身，形成「文件对话框完成 → 回填字段 / 导入导出」闭环。
    Emit(Box<Message>),
}

/// 父层只读上下文：当前保险库（保存加密所需），供模块读取，不写回。
pub struct Ctx {
    /// 当前凭据保险库（可能为 `None`：未解锁时）。读取用，写回经 [`Event`]。
    pub vault: Option<Arc<Vault>>,
}

impl State {
    /// 模块更新：只改自身 `State`；需要父层配合的事以 [`Event`] 经 `Task` 上行。
    ///
    /// `ctx` 为父层传入的只读上下文（当前保险库），模块据此加密保存凭据，但**绝不写父状态**；
    /// 写回一律经对应 [`Event`] 由父层落地。
    pub fn update(&mut self, msg: Message, ctx: &Ctx) -> Task<Event> {
        match msg {
            Message::NewSession => {
                self.editor = Some(EditorDraft::new());
                Task::none()
            }
            Message::EditSession(id) => {
                if let Some(cfg) = self.sessions.iter().find(|s| s.id == id).cloned() {
                    self.editor = Some(EditorDraft::from_config(&cfg));
                }
                Task::none()
            }
            Message::DeleteSession(id) => {
                // 从列表移除会话；标签关闭由父层经 `Event::SessionDeleted` 处理。
                self.sessions.retain(|s| s.id != id);
                let mut tasks = vec![Task::done(Event::SessionDeleted(id))];
                if let Some(e) = self.save(ctx) {
                    tasks.push(Task::done(Event::Status(Some(e))));
                }
                Task::batch(tasks)
            }
            Message::SaveSession => {
                // 凭据信封必须由保险库加密为明文后再落盘（core 层不持有主密钥）。
                let Some(vault) = ctx.vault.as_ref() else {
                    // 理论上主密码弹窗会阻断到此；保险起见仍做守卫：就地标注错误，不破坏已填内容。
                    if let Some(draft) = self.editor.as_mut() {
                        draft.error = Some(t!("app.vault_locked"));
                    }
                    return Task::none();
                };
                let header = vault.header().clone();
                if let Some(draft) = self.editor.take() {
                    match draft
                        .to_config(vault)
                        .and_then(|cfg| self.store_session(cfg, &header))
                    {
                        Ok(()) => Task::done(Event::Status(Some(t!("app.session_saved")))),
                        // 校验失败：不关闭弹窗，保留已填内容，错误显示在弹窗内。
                        Err(e) => {
                            self.editor = Some(EditorDraft {
                                error: Some(e),
                                ..draft
                            });
                            Task::none()
                        }
                    }
                } else {
                    Task::none()
                }
            }
            Message::CancelEdit => {
                self.editor = None;
                Task::none()
            }
            Message::RefreshSessions => {
                // 重新从存储加载会话列表；存储不可用则仅保留当前内存列表。
                if let Some(store) = self.store.as_ref() {
                    self.sessions = store.load().unwrap_or_else(|e| {
                        error!("重新加载会话失败: {e}");
                        self.sessions.clone()
                    });
                }
                Task::none()
            }
            Message::EditorField(field, value) => {
                if let Some(draft) = self.editor.as_mut() {
                    apply_editor_field(draft, field, value);
                    draft.error = None;
                }
                Task::none()
            }
            Message::PickKeyFile => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .pick_file()
                        .await
                        .map(|f| f.path().to_string_lossy().to_string())
                },
                |path| Event::Emit(Box::new(Message::KeyFilePicked(path))),
            ),
            Message::KeyFilePicked(path) => {
                // 文件选择器回程：仅当确有选中且编辑器仍打开时回填 KeyPath。
                if let (Some(p), Some(draft)) = (path, self.editor.as_mut()) {
                    draft.key_path = p;
                    draft.error = None;
                }
                Task::none()
            }
            Message::ToggleGroup(key) => {
                // 集合中存即折叠：点击在折叠 / 展开间切换。
                if !self.collapsed_groups.remove(&key) {
                    self.collapsed_groups.insert(key);
                }
                Task::none()
            }
            Message::SessionSelect(id) => {
                self.selected_session = Some(id);
                Task::none()
            }
            Message::SessionEnter(id) => {
                // 仅在悬浮项变化时更新，避免重复重绘。
                if self.hovered_session.as_deref() != Some(id.as_str()) {
                    self.hovered_session = Some(id);
                }
                Task::none()
            }
            Message::SessionExit(id) => {
                // 仅当离开的正是当前悬浮项时才清除，避免与相邻项的进入事件竞争。
                if self.hovered_session.as_deref() == Some(id.as_str()) {
                    self.hovered_session = None;
                }
                Task::none()
            }
            Message::SessionSelectHovered => {
                // 悬浮项即光标下的会话；未在列表项上悬浮（右击空白处 / 终端）时保持原选中不变。
                if let Some(id) = self.hovered_session.clone() {
                    self.selected_session = Some(id);
                }
                Task::none()
            }
            Message::ConnectSession(id) => {
                // 双击即发连接意图；开标签与异步连接由父层经 `Event::Connect` 处理。
                Task::done(Event::Connect(id))
            }
            Message::OpenFiles(id) => {
                // 右键「打开文件管理」：打开 SFTP 由父层经 `Event::OpenFiles` 处理。
                Task::done(Event::OpenFiles(id))
            }
            Message::ExportSessions => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title(t!("session.export"))
                        .set_file_name("rterm-sessions.toml")
                        .add_filter("TOML", &["toml"])
                        .save_file()
                        .await
                        .map(|f| f.path().to_path_buf())
                },
                |path| Event::Emit(Box::new(Message::ExportSessionsToFile(path))),
            ),
            Message::ExportSessionsToFile(path) => {
                if let Some(p) = path {
                    // 导出仅含连接配置、不含凭证，无需保险库，故不要求 vault 就绪。
                    match export_sessions(&p, &self.sessions) {
                        Ok(()) => Task::done(Event::Toast(
                            ToastKind::Success,
                            t!("app.exported", count => self.sessions.len(), path => p.display()),
                        )),
                        Err(e) => Task::done(Event::Toast(
                            ToastKind::Error,
                            t!("app.export_failed", err => e),
                        )),
                    }
                } else {
                    Task::none()
                }
            }
            Message::ImportSessions => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title(t!("session.import"))
                        .add_filter("TOML", &["toml"])
                        .pick_file()
                        .await
                        .map(|f| f.path().to_path_buf())
                },
                |path| Event::Emit(Box::new(Message::ImportSessionsFromFile(path))),
            ),
            Message::ImportSessionsFromFile(path) => {
                if let Some(p) = path {
                    match import_sessions(&p) {
                        Ok(imported) => {
                            let mut count = 0usize;
                            for mut s in imported {
                                // 与现有 id 冲突则重分配新 id，避免覆盖原会话。
                                if self.sessions.iter().any(|e| e.id == s.id) {
                                    s.id = new_id();
                                }
                                self.sessions.push(s);
                                count += 1;
                            }
                            self.save(ctx);
                            Task::done(Event::Toast(
                                ToastKind::Success,
                                t!("app.imported", count => count),
                            ))
                        }
                        Err(e) => Task::done(Event::Toast(
                            ToastKind::Error,
                            t!("app.import_failed", err => e),
                        )),
                    }
                } else {
                    Task::none()
                }
            }
        }
    }
}

/// 将编辑器字段变更应用到草稿。
fn apply_editor_field(draft: &mut EditorDraft, field: SessionField, value: String) {
    match field {
        SessionField::Name => draft.name = value,
        SessionField::Host => draft.host = value,
        SessionField::Port => draft.port = value,
        SessionField::Username => draft.username = value,
        SessionField::Auth => draft.auth = value,
        SessionField::Password => draft.password = value,
        SessionField::KeyPath => draft.key_path = value,
        SessionField::Passphrase => draft.passphrase = value,
        SessionField::Group => draft.group = value,
    }
}
