//! 会话管理模块

use crate::state::ToastKind;
use crate::t;

use iced::Task;
use iced::widget::Id;
use log::error;
use rterm_config::{AuthMethod, JumpHost, SessionConfig, SessionStore, new_id};
use rterm_crypto::Vault;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

use iced_autocomplete::{Filter, State as AcState};

/// 首次初始化会话列表时，会话总数超过该值即折叠全部命名分组。
///
/// 只在 `collapsed_groups` 尚未初始化时生效；之后以用户状态为准，不再自动重算。
pub(crate) const AUTO_COLLAPSE_THRESHOLD: usize = 15;

/// 最近连接记录的存储上限：超出后最旧的记录被丢弃。
pub(crate) const RECENT_STORE_LIMIT: usize = 20;

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
    /// 启动作业（连接成功后自动下发的命令文本）。
    StartupCommand,
    /// 第 n 跳跳板机的主机地址。
    JumpHost(usize),
    /// 第 n 跳跳板机的端口（字符串，保存时解析）。
    JumpPort(usize),
    /// 第 n 跳跳板机的登录用户名。
    JumpUsername(usize),
    /// 第 n 跳跳板机的认证方式。
    JumpAuth(usize),
    /// 第 n 跳跳板机的密码。
    JumpPassword(usize),
    /// 第 n 跳跳板机的私钥文件路径。
    JumpKeyPath(usize),
    /// 第 n 跳跳板机的私钥口令。
    JumpPassphrase(usize),
}

/// 编辑器输入框的 [`Id`]：供 `operation::focus` 精确定位焦点（iced 0.14 的输入框需显式 `id`）。
pub fn field_id(field: SessionField) -> Id {
    match field {
        SessionField::Host => Id::new("session-editor-host"),
        SessionField::Port => Id::new("session-editor-port"),
        SessionField::Username => Id::new("session-editor-username"),
        SessionField::Password => Id::new("session-editor-password"),
        SessionField::KeyPath => Id::new("session-editor-key-path"),
        SessionField::Passphrase => Id::new("session-editor-passphrase"),
        SessionField::Name => Id::new("session-editor-name"),
        SessionField::StartupCommand => Id::new("session-editor-startup-command"),
        SessionField::JumpHost(i) => Id::from(format!("session-editor-jump-{i}-host")),
        SessionField::JumpPort(i) => Id::from(format!("session-editor-jump-{i}-port")),
        SessionField::JumpUsername(i) => Id::from(format!("session-editor-jump-{i}-username")),
        SessionField::JumpPassword(i) => Id::from(format!("session-editor-jump-{i}-password")),
        SessionField::JumpKeyPath(i) => Id::from(format!("session-editor-jump-{i}-key-path")),
        SessionField::JumpPassphrase(i) => Id::from(format!("session-editor-jump-{i}-passphrase")),
        SessionField::Auth | SessionField::Group | SessionField::JumpAuth(_) => {
            Id::new("session-editor-unfocusable")
        }
    }
}

/// 把焦点落到指定输入框。
pub fn focus_field<T: Send + 'static>(field: SessionField) -> Task<T> {
    iced::widget::operation::focus(field_id(field))
}

/// 某认证方式下需渲染的凭据输入框字段：`jump` 为 `None` 表示目标主机，`Some(i)` 表示第 i 跳。
fn cred_input_fields(auth: &str, jump: Option<usize>) -> Vec<SessionField> {
    match auth {
        "publickey" => vec![
            jump.map_or(SessionField::KeyPath, SessionField::JumpKeyPath),
            jump.map_or(SessionField::Passphrase, SessionField::JumpPassphrase),
        ],
        "agent" => Vec::new(),
        _ => vec![jump.map_or(SessionField::Password, SessionField::JumpPassword)],
    }
}

/// 当前草稿中实际渲染的输入框字段（视觉顺序）：Tab 链与焦点落点共用此表。
///
/// 折叠区不渲染 ⇒ 字段不入表（输入框可聚焦，留着会让 Tab 跳进看不见的框）；
/// 分组框不入表：`iced-autocomplete` 未实现 `operate`，其内部输入框不参与焦点遍历。
pub fn visible_input_fields(draft: &EditorDraft) -> Vec<SessionField> {
    let mut fields = vec![
        SessionField::Host,
        SessionField::Port,
        SessionField::Username,
    ];
    fields.extend(cred_input_fields(&draft.auth, None));
    fields.push(SessionField::Name);
    if draft.advanced_open {
        fields.push(SessionField::StartupCommand);
        for (i, hop) in draft.jumps.iter().enumerate() {
            if !draft.expanded_jumps.contains(&i) {
                continue;
            }
            fields.push(SessionField::JumpHost(i));
            fields.push(SessionField::JumpPort(i));
            fields.push(SessionField::JumpUsername(i));
            fields.extend(cred_input_fields(&hop.auth, Some(i)));
        }
    }
    fields
}

/// 编辑器中的一跳跳板机草稿：字段以字符串保存，与 [`EditorDraft`] 同规。
#[derive(Clone)]
pub struct JumpDraft {
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
    /// 编辑已有跳板时保留的原始认证（含密文信封）；新增跳板为 `None`。
    pub(crate) orig_auth: Option<AuthMethod>,
}

impl JumpDraft {
    /// 空白跳板草稿（端口取 SSH 默认值，认证默认密码）。
    fn new() -> Self {
        Self {
            host: String::new(),
            port: rterm_config::DEFAULT_SSH_PORT.to_string(),
            username: String::new(),
            auth: "password".to_string(),
            password: String::new(),
            key_path: String::new(),
            passphrase: String::new(),
            orig_auth: None,
        }
    }

    /// 由已有跳板机构造草稿（凭据留空、原始信封暂存，用于编辑）。
    fn from_config(hop: &JumpHost) -> Self {
        let (auth, key_path) = auth_draft_fields(&hop.auth);
        Self {
            host: hop.host.clone(),
            port: hop.port.to_string(),
            username: hop.username.clone(),
            auth,
            password: String::new(),
            key_path,
            passphrase: String::new(),
            orig_auth: Some(hop.auth.clone()),
        }
    }
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
    /// 启动作业（连接成功后自动下发的命令文本，空串表示不执行）。
    pub startup_command: String,
    /// 跳板链草稿（由外到内，与保存后的 [`SessionConfig::jumps`] 同序）。
    pub jumps: Vec<JumpDraft>,
    /// 端口转发规则（编辑器暂不提供编辑入口，打开 / 保存原样携带，
    /// 避免保存会话时静默丢弃规则；管理入口见 `app::forwards`）。
    pub forwards: Vec<rterm_config::ForwardRule>,
    /// 是否收藏：编辑任意字段都须原样带回，否则会静默清掉收藏。
    pub favorite: bool,
    /// 分组字段的自动补全状态（候选池 + 当前文本），由打开编辑器时按现有会话分组填充。
    ///
    /// 用 [`Rc`] 包裹以规避 `iced_autocomplete::State` 的 `Clone` 不确定性与生命周期：
    /// 其内可变经由 `&self` 的 `RefCell` 生效，故 `Rc` 克隆后视图与更新仍共享同一份。
    pub group_ac: Rc<AcState<String>>,
    /// 保存校验失败信息（显示在弹窗内），修改任意字段时清除。
    pub error: Option<String>,
    /// 字段级校验错误：`(字段, 文案)`，就地标红显示，页脚只显示计数。
    pub errors: Vec<(SessionField, String)>,
    /// 「高级选项」折叠区是否展开（启动作业与跳板机）。草稿内的临时 UI 态，不落盘。
    pub advanced_open: bool,
    /// 已展开的跳板机卡片下标集合。
    pub expanded_jumps: BTreeSet<usize>,
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
            startup_command: String::new(),
            jumps: Vec::new(),
            forwards: Vec::new(),
            favorite: false,
            group_ac: Rc::new(AcState::new(std::iter::empty::<String>(), Filter::None)),
            error: None,
            errors: Vec::new(),
            advanced_open: false,
            expanded_jumps: BTreeSet::new(),
            orig_auth: None,
        }
    }

    /// 是否为「新建会话」草稿：新建草稿不含原始认证（`orig_auth` 为 `None`），
    /// 编辑草稿由 [`Self::from_config`] 构造、恒持有原始认证。
    ///
    /// 注意不能用 `id.is_empty()` 判断：[`Self::new`] 在构造时就分配了非空 id。
    pub fn is_new(&self) -> bool {
        self.orig_auth.is_none()
    }

    /// 由会话配置构造草稿（用于「编辑」）。
    ///
    /// 凭据字段在编辑器内留空（占位「保持不变」），原始信封暂存于 `Self::orig_auth`；
    /// 仅当用户键入新凭据时才重新加密。
    pub fn from_config(cfg: &SessionConfig) -> Self {
        let (auth, key_path) = auth_draft_fields(&cfg.auth);
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
            startup_command: cfg.startup_command.clone().unwrap_or_default(),
            jumps: cfg.jumps.iter().map(JumpDraft::from_config).collect(),
            forwards: cfg.forwards.clone(),
            favorite: cfg.favorite,
            group_ac: Rc::new(AcState::new(std::iter::empty::<String>(), Filter::None)),
            error: None,
            errors: Vec::new(),
            advanced_open: false,
            expanded_jumps: BTreeSet::new(),
            orig_auth: Some(cfg.auth.clone()),
        }
    }

    /// 由草稿构建会话配置：校验主机必填、端口为非 0 数字；
    /// 名称选填，为空时默认 `user@host`。失败返回错误字符串。
    ///
    /// `vault` 用于把键入的明文凭据加密为信封；凭据字段留空时沿用 `Self::orig_auth`
    /// 中已有的信封（编辑场景），实现「不改动即保持不变」。
    pub fn to_config(&self, vault: &Vault) -> Result<SessionConfig, String> {
        if let Some((_, msg)) = self.validate().first() {
            return Err(msg.clone());
        }
        // 端口已在 validate 中确认可解析且非 0。
        let port = self
            .port
            .parse::<u16>()
            .unwrap_or(rterm_config::DEFAULT_SSH_PORT);
        let name = if self.name.trim().is_empty() {
            format!("{}@{}", self.username.trim(), self.host.trim())
        } else {
            self.name.clone()
        };
        let auth = build_auth(
            &self.auth,
            &self.password,
            &self.key_path,
            &self.passphrase,
            self.orig_auth.as_ref(),
            vault,
        );
        let jumps = self.build_jumps(vault);
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
            startup_command: if self.startup_command.trim().is_empty() {
                None
            } else {
                Some(self.startup_command.clone())
            },
            jumps,
            forwards: self.forwards.clone(),
            favorite: self.favorite,
        })
    }

    /// 收齐全部校验失败字段：`(字段, 文案)`，按主机 → 端口 → 各跳板机的顺序。
    ///
    /// 规则与文案沿用既有校验，区别仅在于一次给出全部失败项。
    pub fn validate(&self) -> Vec<(SessionField, String)> {
        let mut errors = Vec::new();
        if self.host.trim().is_empty() {
            errors.push((SessionField::Host, t!("session.host_empty")));
        }
        match self.port.parse::<u16>() {
            Err(_) => errors.push((SessionField::Port, t!("session.port_invalid"))),
            Ok(0) => errors.push((SessionField::Port, t!("session.port_zero"))),
            Ok(_) => {}
        }
        for (i, hop) in self.jumps.iter().enumerate() {
            let index = i + 1;
            if hop.host.trim().is_empty() {
                errors.push((
                    SessionField::JumpHost(i),
                    t!("session.jump_host_empty", index => index),
                ));
            }
            // 端口留空取默认值 22；填了则必须是非 0 数字。
            if !hop.port.trim().is_empty()
                && hop.port.parse::<u16>().ok().filter(|p| *p != 0).is_none()
            {
                errors.push((
                    SessionField::JumpPort(i),
                    t!("session.jump_port_invalid", index => index),
                ));
            }
        }
        errors
    }

    /// 由跳板草稿构建跳板链（由外到内）：端口留空取 SSH 默认值。
    ///
    /// 主机与端口的合法性已由 [`Self::validate`] 先行判定，此处只做取值。
    fn build_jumps(&self, vault: &Vault) -> Vec<JumpHost> {
        self.jumps
            .iter()
            .map(|hop| JumpHost {
                host: hop.host.clone(),
                port: hop
                    .port
                    .trim()
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .unwrap_or(rterm_config::DEFAULT_SSH_PORT),
                username: hop.username.clone(),
                auth: build_auth(
                    &hop.auth,
                    &hop.password,
                    &hop.key_path,
                    &hop.passphrase,
                    hop.orig_auth.as_ref(),
                    vault,
                ),
            })
            .collect()
    }
}

/// 由认证方式拆出编辑器草稿的两个字段（认证选择值 + 私钥路径）。
fn auth_draft_fields(auth: &AuthMethod) -> (String, String) {
    match auth {
        AuthMethod::Password { .. } => ("password".into(), String::new()),
        AuthMethod::PublicKey { key_path, .. } => {
            ("publickey".into(), key_path.to_string_lossy().to_string())
        }
        AuthMethod::Agent => ("agent".into(), String::new()),
    }
}

/// 由认证选择（`password` / `publickey` / `agent`）+ 凭据明文构建认证方式。
///
/// 凭据字段留空时沿用 `orig` 中已有的信封，实现编辑场景的「不改动即保持不变」。
/// 目标主机与每一跳跳板机共用本函数。
fn build_auth(
    choice: &str,
    password: &str,
    key_path: &str,
    passphrase: &str,
    orig: Option<&AuthMethod>,
    vault: &Vault,
) -> AuthMethod {
    match choice {
        "publickey" => {
            let passphrase = if !passphrase.is_empty() {
                Some(vault.encrypt(passphrase))
            } else if let Some(AuthMethod::PublicKey { passphrase, .. }) = orig {
                passphrase.clone()
            } else {
                None
            };
            AuthMethod::PublicKey {
                key_path: key_path.into(),
                passphrase,
            }
        }
        "agent" => AuthMethod::Agent,
        _ => {
            let password = if !password.is_empty() {
                Some(vault.encrypt(password))
            } else if let Some(AuthMethod::Password { password }) = orig {
                password.clone()
            } else {
                // 凭据留空且无原值：标记为「未设置密码」，连接前需在编辑中补填。
                None
            };
            AuthMethod::Password { password }
        }
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
    pub collapsed_groups: BTreeSet<String>,
    /// 工具栏「更多」 dropdown 是否展开。
    pub show_more_menu: bool,
    /// 会话列表过滤词：对名称 / 主机 / 用户名 / 分组做大小写不敏感子串匹配，空串 = 全量。
    pub filter: String,
    /// 会话编辑器中的草稿（无则未编辑）。
    pub editor: Option<EditorDraft>,
    /// 编辑器内当前聚焦的输入框（`None` 表示无输入框聚焦）。
    ///
    /// iced 0.14 的输入框在 Esc 时自行失焦并吞掉该键，应用层据此判断「Esc 只失焦、
    /// 再按一次才关弹窗」的两段式行为。
    pub focused_field: Option<SessionField>,
    /// 待确认删除的会话 id（`None` 表示当前未弹出删除确认框）。
    pub pending_delete: Option<String>,
}

impl State {
    /// 用初始存储、会话列表与已解析的分组折叠态构造模块状态。
    pub fn new(
        store: Option<SessionStore>,
        sessions: Vec<SessionConfig>,
        collapsed_groups: BTreeSet<String>,
    ) -> Self {
        Self {
            store,
            sessions,
            hovered_session: None,
            selected_session: None,
            collapsed_groups,
            show_more_menu: false,
            filter: String::new(),
            editor: None,
            focused_field: None,
            pending_delete: None,
        }
    }

    /// Tab 链推进：按可见字段顺序移动焦点，走到末尾后回到首个（反向同理）。
    ///
    /// 折叠区不渲染 ⇒ 其字段不在链内；当前焦点不在链内（如落在按钮上）时，
    /// 正向取首个、反向取末个。
    pub fn step_focus(&self, forward: bool) -> Option<SessionField> {
        let draft = self.editor.as_ref()?;
        let fields = visible_input_fields(draft);
        if fields.is_empty() {
            return None;
        }
        let current = self
            .focused_field
            .and_then(|f| fields.iter().position(|c| *c == f));
        let index = match current {
            Some(i) if forward => (i + 1) % fields.len(),
            Some(i) => (i + fields.len() - 1) % fields.len(),
            None if forward => 0,
            None => fields.len() - 1,
        };
        fields.get(index).copied()
    }

    /// 持久化会话列表：失败仅记录日志（保险库未就绪则跳过）。返回错误文案（若有），
    /// 供调用方经 [`Event::Status`] 上行提示。
    fn save(&mut self, ctx: &Ctx) -> Option<String> {
        let store = self.store.as_ref()?;
        let Some(vault) = ctx.vault.as_ref() else {
            error!("vault not ready, cannot save session");
            return Some(t!("app.vault_locked"));
        };
        let header = vault.header();
        match store.save(&self.sessions, header) {
            Ok(()) => None,
            Err(e) => {
                error!("failed to save session: {e}");
                Some(t!("app.save_failed", err => e))
            }
        }
    }

    /// 展开某会话所属分组（若处于折叠态）：刚被操作过的会话必须可见。
    ///
    /// 返回上行事件，仅在确有分组由折叠转为展开时才有。
    fn expand_group_of(&mut self, id: &str) -> Option<Event> {
        let key = self
            .sessions
            .iter()
            .find(|s| s.id == id)?
            .group
            .clone()
            .unwrap_or_default();
        self.collapsed_groups
            .remove(&key)
            .then(|| Event::CollapsedGroupsChanged(self.collapsed_groups.clone()))
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
                .map_err(|e| t!("app.save_failed", err => e))?;
        }
        Ok(())
    }

    /// 写入某会话的端口转发规则并落盘（存储不可用时仅改内存态，不落盘）。
    ///
    /// 会话不存在（并发中被删除等）视为成功：无内存可改，也无从落盘。
    pub fn set_forwards(
        &mut self,
        session_id: &str,
        forwards: Vec<rterm_config::ForwardRule>,
        header: &rterm_crypto::CryptoHeader,
    ) -> Result<(), String> {
        let Some(cfg) = self.sessions.iter_mut().find(|s| s.id == session_id) else {
            return Ok(());
        };
        cfg.forwards = forwards;
        if let Some(store) = self.store.as_ref() {
            store
                .save(&self.sessions, header)
                .map_err(|e| t!("app.save_failed", err => e))?;
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
    /// 复制已有会话（携带会话 id）：副本名称追加数字后缀，凭据信封原样克隆。
    DuplicateSession(String),
    /// 请求删除会话（携带会话 id）：仅置待确认状态并弹出确认框，不直接删除。
    RequestDeleteSession(String),
    /// 确认删除会话：从待确认状态取出 id 执行实际删除。
    ConfirmDeleteSession,
    /// 取消删除会话：撤销待确认状态并关闭确认框。
    CancelDeleteSession,
    /// 保存当前编辑器中的会话。
    SaveSession,
    /// 取消编辑。
    CancelEdit,
    /// 从持久化存储重新加载会话列表（丢弃未保存于存储的仅内存变更）。
    RefreshSessions,
    /// 编辑器字段变更（字段类型 + 新值）。
    EditorField(SessionField, String),
    /// 展开 / 折叠「高级选项」区（启动作业与跳板机）。
    ToggleAdvanced,
    /// 展开 / 折叠第 n 台跳板机卡片。
    ToggleJump(usize),
    /// 把焦点落到指定字段（Tab 链推进、认证方式切换后的落点）。
    FocusField(SessionField),
    /// 在跳板链末尾追加一跳空白跳板机。
    AddJump,
    /// 移除第 n 跳跳板机。
    RemoveJump(usize),
    /// 打开系统文件选择器以选取私钥文件（结果经 `Event::Emit` 自回路回填 KeyPath 字段）。
    PickKeyFile,
    /// 私钥文件选择结果（`None` 表示用户取消对话框）。
    KeyFilePicked(Option<String>),
    /// 打开文件选择器以选取第 n 跳跳板机的私钥文件（结果经 `Event::Emit` 自回路回填该跳）。
    PickJumpKeyFile(usize),
    /// 跳板机私钥文件选择结果（携带跳下标；`None` 表示用户取消对话框）。
    JumpKeyFilePicked(usize, Option<String>),
    /// 折叠 / 展开某分组（携带分组键：空串表示「未分组」区块）。
    ToggleGroup(String),
    /// 折叠全部分组（含「未分组」区块）。
    CollapseAllGroups,
    /// 展开全部分组。
    ExpandAllGroups,
    /// 切换收藏态（携带会话 id）：翻转 `favorite` 并落盘。
    ToggleFavorite(String),
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
    /// 切换工具栏「更多」dropdown显隐。
    ToggleMoreMenu,
    /// 会话列表过滤词变更（携带输入框最新文本）。
    FilterChanged(String),
    /// 「更多」菜单：导出会话。收起菜单后经 `Event::SessionFile` 交父层转给
    /// [`super::session_io`]——是否携带凭据由该模块的弹窗开关决定，落盘也不在本模块。
    ExportSessions,
    /// 「更多」菜单：导入会话（同上；文件是否带凭据由 `session_io` 读文件标记后判定）。
    ImportSessions,
}

/// 「更多」菜单里的会话文件操作意图。
///
/// 会话面板的视图只能生产 [`Message`]（`layout` 统一 `.map(Message::Session)`），故菜单项在此
/// 收口为一条上行事件，由父层翻译成 [`super::session_io`] 的消息——模块间不互相直接派发消息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFlow {
    /// 导入会话。
    Import,
    /// 导出会话（是否携带凭据由导出弹窗的开关决定）。
    Export,
}

/// 上行事件：仅通知父层，由父层 `Message::SessionEvent` 分支修改父状态并落地。
///
/// 模块绝不写父状态；开标签 / 关标签 / 状态栏 / toast 一律经对应事件由父层落地。
#[derive(Clone)]
pub enum Event {
    /// 发起连接（携带会话 id）：父层据此开标签并异步连接。
    Connect(String),
    /// 分组的折叠态发生变化（携带完整的折叠分组键集合）：父层据此写回配置并落盘。
    CollapsedGroupsChanged(BTreeSet<String>),
    /// 打开某会话的文件管理（携带会话 id）：父层据此打开对应标签的 SFTP 面板。
    OpenFiles(String),
    /// 某会话被删除（携带会话 id）：父层关闭该会话的全部终端标签。
    SessionDeleted(String),
    /// 写回状态栏提示（携带文案，`None` 表示清除）。
    Status(Option<String>),
    /// 弹 toast 通知（携带类型与文案，如导入 / 导出结果）。
    Toast(ToastKind, String),
    /// 会话文件操作意图：父层据此转交 [`super::session_io`]（文件对话框、口令弹窗、加解密与落盘）。
    SessionFile(FileFlow),
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
                let mut draft = EditorDraft::new();
                draft.group_ac = Rc::new(group_ac_state(&draft.group, &self.sessions));
                self.editor = Some(draft);
                self.focused_field = Some(SessionField::Host);
                focus_field::<Event>(SessionField::Host)
            }
            Message::EditSession(id) => {
                if let Some(cfg) = self.sessions.iter().find(|s| s.id == id).cloned() {
                    let mut draft = EditorDraft::from_config(&cfg);
                    draft.group_ac = Rc::new(group_ac_state(&draft.group, &self.sessions));
                    self.editor = Some(draft);
                    self.focused_field = Some(SessionField::Host);
                    focus_field::<Event>(SessionField::Host)
                } else {
                    Task::none()
                }
            }
            Message::DuplicateSession(id) => {
                let Some(src) = self.sessions.iter().find(|s| s.id == id).cloned() else {
                    return Task::none();
                };
                // 保险库未就绪时先行守卫：否则只落内存副本，落盘被跳过、刷新后凭空消失。
                if ctx.vault.is_none() {
                    return Task::done(Event::Status(Some(t!("app.vault_locked"))));
                }
                let copy = duplicated(&src, &self.sessions);
                let name = copy.name.clone();
                let id = copy.id.clone();
                self.selected_session = Some(copy.id.clone());
                self.sessions.push(copy);
                // 落盘失败时以错误覆盖成功提示，避免报告一个并未持久化的副本。
                let status = match self.save(ctx) {
                    Some(e) => e,
                    None => t!("app.session_duplicated", name => name),
                };
                // 副本必须可见：其分组处于折叠态时展开并持久化。
                let mut tasks = vec![Task::done(Event::Status(Some(status)))];
                if let Some(e) = self.expand_group_of(&id) {
                    tasks.push(Task::done(e));
                }
                Task::batch(tasks)
            }
            Message::RequestDeleteSession(id) => {
                self.pending_delete = Some(id);
                Task::none()
            }
            Message::ConfirmDeleteSession => {
                let id = match self.pending_delete.take() {
                    Some(id) => id,
                    None => return Task::none(),
                };
                // 被删会话若正处选中/悬浮态，清理残留指针（原删除分支缺失此步）。
                if self.selected_session.as_deref() == Some(id.as_str()) {
                    self.selected_session = None;
                }
                if self.hovered_session.as_deref() == Some(id.as_str()) {
                    self.hovered_session = None;
                }
                // 从列表移除会话；标签关闭由父层经 `Event::SessionDeleted` 处理。
                self.sessions.retain(|s| s.id != id);
                let mut tasks = vec![Task::done(Event::SessionDeleted(id))];
                if let Some(e) = self.save(ctx) {
                    tasks.push(Task::done(Event::Status(Some(e))));
                }
                Task::batch(tasks)
            }
            Message::CancelDeleteSession => {
                self.pending_delete = None;
                Task::none()
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
                    // 先收齐全部校验失败字段：就地标红 + 页脚计数，焦点跳到首个出错字段。
                    let errors = draft.validate();
                    if !errors.is_empty() {
                        let first = errors[0].0;
                        let mut next = EditorDraft {
                            error: None,
                            errors,
                            ..draft
                        };
                        // 出错字段若在收起的跳板机卡片里，先展开它——否则错误既看不见也聚焦不到。
                        if let SessionField::JumpHost(i) | SessionField::JumpPort(i) = first {
                            next.advanced_open = true;
                            next.expanded_jumps.insert(i);
                        }
                        self.editor = Some(next);
                        self.focused_field = Some(first);
                        return focus_field::<Event>(first);
                    }
                    match draft
                        .to_config(vault)
                        .and_then(|cfg| self.store_session(cfg, &header))
                    {
                        Ok(()) => {
                            // 刚保存的会话必须可见：其分组处于折叠态时展开并持久化。
                            let mut tasks =
                                vec![Task::done(Event::Status(Some(t!("app.session_saved"))))];
                            if let Some(e) = self.expand_group_of(&draft.id) {
                                tasks.push(Task::done(e));
                            }
                            self.focused_field = None;
                            Task::batch(tasks)
                        }
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
                self.focused_field = None;
                Task::none()
            }
            Message::RefreshSessions => {
                // 入口在「更多」下拉菜单内，触发后收起菜单。
                self.show_more_menu = false;
                // 重新从存储加载会话列表；存储不可用则仅保留当前内存列表。
                if let Some(store) = self.store.as_ref() {
                    self.sessions = store.load().unwrap_or_else(|e| {
                        error!("failed to reload sessions: {e}");
                        self.sessions.clone()
                    });
                }
                Task::none()
            }
            Message::EditorField(field, value) => {
                // 认证方式切换后把焦点落到对应凭据框（agent 无凭据框，焦点留在分段控件）。
                let landing = match field {
                    SessionField::Auth => cred_input_fields(&value, None).first().copied(),
                    SessionField::JumpAuth(i) => {
                        cred_input_fields(&value, Some(i)).first().copied()
                    }
                    _ => None,
                };
                if let Some(draft) = self.editor.as_mut() {
                    apply_editor_field(draft, field, value);
                    draft.error = None;
                    // 该字段的错误就地消解（页脚计数随之减少）。
                    draft.errors.retain(|(f, _)| *f != field);
                }
                // 正在输入 ⇒ 焦点在该输入框上。
                self.focused_field = landing.or(Some(field));
                match landing {
                    Some(f) => focus_field::<Event>(f),
                    None => Task::none(),
                }
            }
            Message::ToggleAdvanced => {
                if let Some(draft) = self.editor.as_mut() {
                    draft.advanced_open = !draft.advanced_open;
                }
                Task::none()
            }
            Message::ToggleJump(index) => {
                // `remove` 返回 true 表示原本已展开 ⇒ 转为收起；否则插入 ⇒ 展开。
                if let Some(draft) = self.editor.as_mut()
                    && !draft.expanded_jumps.remove(&index)
                {
                    draft.expanded_jumps.insert(index);
                }
                Task::none()
            }
            Message::FocusField(field) => {
                self.focused_field = Some(field);
                focus_field::<Event>(field)
            }
            Message::AddJump => {
                if let Some(draft) = self.editor.as_mut() {
                    draft.jumps.push(JumpDraft::new());
                    // 新增后自动展开该张卡片。
                    let index = draft.jumps.len() - 1;
                    draft.expanded_jumps.insert(index);
                    draft.error = None;
                }
                Task::none()
            }
            Message::RemoveJump(index) => {
                if let Some(draft) = self.editor.as_mut()
                    && index < draft.jumps.len()
                {
                    draft.jumps.remove(index);
                    draft.error = None;
                    // 下标前移一位，展开态随之平移（被移除的那张不再展开）。
                    draft.expanded_jumps = draft
                        .expanded_jumps
                        .iter()
                        .filter_map(|i| match i.cmp(&index) {
                            std::cmp::Ordering::Equal => None,
                            std::cmp::Ordering::Greater => Some(i - 1),
                            std::cmp::Ordering::Less => Some(*i),
                        })
                        .collect();
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
            Message::PickJumpKeyFile(index) => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .pick_file()
                        .await
                        .map(|f| f.path().to_string_lossy().to_string())
                },
                move |path| Event::Emit(Box::new(Message::JumpKeyFilePicked(index, path))),
            ),
            Message::JumpKeyFilePicked(index, path) => {
                // 文件选择器回程：跳可能在对话框打开期间被移除，故按当下下标取、越界即忽略。
                if let Some(draft) = self.editor.as_mut()
                    && let Some(p) = path
                    && let Some(hop) = draft.jumps.get_mut(index)
                {
                    hop.key_path = p;
                    draft.error = None;
                }
                Task::none()
            }
            Message::ToggleGroup(key) => {
                // 集合中存即折叠：点击在折叠 / 展开间切换，并上行持久化意图。
                if !self.collapsed_groups.remove(&key) {
                    self.collapsed_groups.insert(key);
                }
                Task::done(Event::CollapsedGroupsChanged(self.collapsed_groups.clone()))
            }
            Message::CollapseAllGroups => {
                self.collapsed_groups = group_keys(&self.sessions);
                Task::done(Event::CollapsedGroupsChanged(self.collapsed_groups.clone()))
            }
            Message::ExpandAllGroups => {
                self.collapsed_groups.clear();
                Task::done(Event::CollapsedGroupsChanged(BTreeSet::new()))
            }
            Message::ToggleFavorite(id) => {
                // 翻转收藏态并落盘；会话已被删除则忽略。
                let Some(cfg) = self.sessions.iter_mut().find(|s| s.id == id) else {
                    return Task::none();
                };
                cfg.favorite = !cfg.favorite;
                match self.save(ctx) {
                    Some(err) => Task::done(Event::Status(Some(err))),
                    None => Task::none(),
                }
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
                let mut tasks = vec![Task::done(Event::Connect(id.clone()))];
                // 刚连接的会话必须可见：其分组折叠时在此展开并持久化。
                if let Some(e) = self.expand_group_of(&id) {
                    tasks.push(Task::done(e));
                }
                Task::batch(tasks)
            }
            Message::OpenFiles(id) => {
                // 右键「打开文件管理」：打开 SFTP 由父层经 `Event::OpenFiles` 处理。
                Task::done(Event::OpenFiles(id))
            }
            Message::ToggleMoreMenu => {
                self.show_more_menu = !self.show_more_menu;
                Task::none()
            }
            Message::FilterChanged(query) => {
                // 过滤后原悬浮项可能被隐藏，残留高亮会指向不存在的行，顺手清掉。
                self.hovered_session = None;
                self.filter = query;
                Task::none()
            }
            Message::ExportSessions => {
                self.show_more_menu = false;
                Task::done(Event::SessionFile(FileFlow::Export))
            }
            Message::ImportSessions => {
                self.show_more_menu = false;
                Task::done(Event::SessionFile(FileFlow::Import))
            }
        }
    }
}

/// 按过滤词筛选会话：对名称 / 主机 / 用户名 / 分组做大小写不敏感的子串匹配。
///
/// 过滤词先 `trim`，空串返回全量；返回引用切片避免逐帧克隆。纯展示过滤，不触碰磁盘。
pub(crate) fn filtered_sessions<'a>(
    sessions: &'a [SessionConfig],
    query: &str,
) -> Vec<&'a SessionConfig> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return sessions.iter().collect();
    }
    sessions
        .iter()
        .filter(|s| {
            s.name.to_lowercase().contains(&query)
                || s.host.to_lowercase().contains(&query)
                || s.username.to_lowercase().contains(&query)
                || s.group
                    .as_deref()
                    .is_some_and(|g| g.to_lowercase().contains(&query))
        })
        .collect()
}

/// 当前会话列表涉及的全部分组键：命名分组 + 存在未分组会话时的空串键。
fn group_keys(sessions: &[SessionConfig]) -> BTreeSet<String> {
    let mut keys: BTreeSet<String> = sessions.iter().filter_map(|s| s.group.clone()).collect();
    if sessions.iter().any(|s| s.group.is_none()) {
        keys.insert(String::new());
    }
    keys
}

/// 解析分组折叠态：未初始化时按会话总数给出默认值，已初始化时修剪失效分组键。
///
/// 首启且总会话数超过 [`AUTO_COLLAPSE_THRESHOLD`] 时折叠全部命名分组；「未分组」区块
/// 恒默认展开。返回折叠集合与「是否需要回写配置」。
pub(crate) fn resolve_collapsed_groups(
    sessions: &[SessionConfig],
    stored: Option<&[String]>,
) -> (BTreeSet<String>, bool) {
    let named_groups: BTreeSet<String> = sessions.iter().filter_map(|s| s.group.clone()).collect();
    let has_ungrouped = sessions.iter().any(|s| s.group.is_none());

    match stored {
        None => {
            let initial = if sessions.len() > AUTO_COLLAPSE_THRESHOLD {
                named_groups
            } else {
                BTreeSet::new()
            };
            (initial, true)
        }
        Some(keys) => {
            let stored_set: BTreeSet<String> = keys.iter().cloned().collect();
            let pruned: BTreeSet<String> = stored_set
                .iter()
                .filter(|key| {
                    if key.is_empty() {
                        has_ungrouped
                    } else {
                        named_groups.contains(*key)
                    }
                })
                .cloned()
                .collect();
            // 重复键会被集合去重，故长度变化也算需要回写。
            let changed = pruned != stored_set || keys.len() != stored_set.len();
            (pruned, changed)
        }
    }
}

/// 构造分组字段的自动补全状态：候选为现有会话去重后的分组名，初始文本取草稿当前分组。
///
/// 过滤采用大小写不敏感子串匹配——中文分组名直接「包含」即可命中，英文则忽略大小写；
/// 自由输入不存在于候选中的新分组名仍会被照常提交（这正是 autocomplete 相对 combo_box 的语义）。
fn group_ac_state(initial: &str, sessions: &[SessionConfig]) -> AcState<String> {
    let mut groups: Vec<String> = sessions.iter().filter_map(|s| s.group.clone()).collect();
    groups.sort();
    groups.dedup();
    AcState::with_value(
        groups,
        initial.to_string(),
        Filter::Custom(Box::new(|opt: &String, q: &str| {
            opt.to_lowercase().contains(&q.to_lowercase())
        })),
    )
}

/// 由现有会话生成副本：换新 id，名称追加「空格 + 数字」后缀（后缀不含可翻译词，
/// 故无需为其准备文案），其余字段——含凭据密文信封——原样克隆。
///
/// 信封与源同属当前保险库 DEK，克隆即可解出同一凭据，故不必解密再加密。
fn duplicated(src: &SessionConfig, existing: &[SessionConfig]) -> SessionConfig {
    let mut n = 2;
    let mut name = format!("{} {n}", src.name);
    while existing.iter().any(|s| s.name == name) {
        n += 1;
        name = format!("{} {n}", src.name);
    }
    SessionConfig {
        id: new_id(),
        name,
        // 复制出的条目不继承收藏：收藏针对具体条目，不是连接配置。
        favorite: false,
        ..src.clone()
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
        SessionField::StartupCommand => draft.startup_command = value,
        // 跳板字段带下标：删除某跳后旧下标可能失效，故一律越界静默忽略。
        SessionField::JumpHost(i) => {
            if let Some(j) = draft.jumps.get_mut(i) {
                j.host = value;
            }
        }
        SessionField::JumpPort(i) => {
            if let Some(j) = draft.jumps.get_mut(i) {
                j.port = value;
            }
        }
        SessionField::JumpUsername(i) => {
            if let Some(j) = draft.jumps.get_mut(i) {
                j.username = value;
            }
        }
        SessionField::JumpAuth(i) => {
            if let Some(j) = draft.jumps.get_mut(i) {
                j.auth = value;
            }
        }
        SessionField::JumpPassword(i) => {
            if let Some(j) = draft.jumps.get_mut(i) {
                j.password = value;
            }
        }
        SessionField::JumpKeyPath(i) => {
            if let Some(j) = draft.jumps.get_mut(i) {
                j.key_path = value;
            }
        }
        SessionField::JumpPassphrase(i) => {
            if let Some(j) = draft.jumps.get_mut(i) {
                j.passphrase = value;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rterm_config::AuthMethod;

    /// 构造一份可编辑的草稿：主机 / 端口已填，其余留空。
    fn draft() -> EditorDraft {
        let mut d = EditorDraft::new();
        d.host = "example.com".into();
        d.port = "22".into();
        d
    }

    /// 构造一台跳板机草稿（主机 / 端口可指定）。
    fn jump(host: &str, port: &str) -> JumpDraft {
        let mut j = JumpDraft::new();
        j.host = host.into();
        j.port = port.into();
        j
    }

    /// 校验错误应一次收齐全部失败字段，而不是遇到第一个即返回。
    #[test]
    fn validate_collects_every_failing_field() {
        let mut d = EditorDraft::new();
        d.port = "0".into();
        d.jumps.push(jump("", "abc"));
        d.jumps.push(jump("host2", "70000"));

        let errors = d.validate();

        let fields: Vec<SessionField> = errors.iter().map(|(f, _)| *f).collect();
        assert_eq!(
            fields,
            vec![
                SessionField::Host,
                SessionField::Port,
                SessionField::JumpHost(0),
                SessionField::JumpPort(0),
                SessionField::JumpPort(1),
            ],
            "错误应按主机 → 端口 → 各跳板机顺序收齐"
        );
    }

    /// 空端口取默认值 22，不算错误；非 0 数字才算。
    #[test]
    fn validate_treats_an_empty_jump_port_as_default() {
        let mut d = draft();
        d.jumps.push(jump("host2", ""));

        assert!(d.validate().is_empty(), "空端口应取默认值而非报错");
    }

    /// 折叠区不渲染 ⇒ 其输入框不参与 Tab 链。
    #[test]
    fn visible_fields_exclude_the_collapsed_advanced_section() {
        let mut d = draft();
        d.jumps.push(jump("host2", "22"));

        let closed = visible_input_fields(&d);
        assert!(!closed.contains(&SessionField::StartupCommand));
        assert!(!closed.contains(&SessionField::JumpHost(0)));

        d.advanced_open = true;
        let opened = visible_input_fields(&d);
        assert!(opened.contains(&SessionField::StartupCommand));
        assert!(
            !opened.contains(&SessionField::JumpHost(0)),
            "跳板机卡片自身折叠时其输入框仍不进 Tab 链"
        );

        d.expanded_jumps.insert(0);
        let expanded = visible_input_fields(&d);
        assert!(expanded.contains(&SessionField::JumpHost(0)));
    }

    /// Tab 链顺序：主机 → 端口 → 用户名 → 凭据 → 名称（凭据随认证方式变化）。
    #[test]
    fn visible_fields_follow_the_documented_tab_order() {
        let mut d = draft();

        assert_eq!(
            visible_input_fields(&d),
            vec![
                SessionField::Host,
                SessionField::Port,
                SessionField::Username,
                SessionField::Password,
                SessionField::Name,
            ]
        );

        d.auth = "publickey".into();
        assert_eq!(
            visible_input_fields(&d),
            vec![
                SessionField::Host,
                SessionField::Port,
                SessionField::Username,
                SessionField::KeyPath,
                SessionField::Passphrase,
                SessionField::Name,
            ]
        );

        d.auth = "agent".into();
        assert!(!visible_input_fields(&d).contains(&SessionField::Password));
    }

    /// Tab 推进到末尾后回到首个，反向同理；当前焦点不在链内时正向取首个。
    ///
    /// `step_focus` 只算出下一个落点，不自行推进追踪焦点——推进由 [`Message::FocusField`] 落地。
    #[test]
    fn step_focus_cycles_and_wraps() {
        let mut state = State::new(None, Vec::new(), BTreeSet::new());
        state.editor = Some(draft());
        state.focused_field = Some(SessionField::Host);

        assert_eq!(state.step_focus(true), Some(SessionField::Port));
        assert_eq!(
            state.step_focus(false),
            Some(SessionField::Name),
            "从首个反向应取末位"
        );

        state.focused_field = Some(SessionField::Name);
        assert_eq!(
            state.step_focus(true),
            Some(SessionField::Host),
            "末位之后应回到首个"
        );

        state.focused_field = None;
        assert_eq!(state.step_focus(true), Some(SessionField::Host));
        assert_eq!(
            state.step_focus(false),
            Some(SessionField::Name),
            "无当前焦点时反向应取末位"
        );
    }

    /// 移除一台跳板机后，展开态下标应整体前移，避免展开错位。
    #[test]
    fn removing_a_jump_shifts_the_expanded_indices() {
        let mut state = State::new(None, Vec::new(), BTreeSet::new());
        let mut d = draft();
        d.jumps.push(jump("h1", "22"));
        d.jumps.push(jump("h2", "22"));
        d.jumps.push(jump("h3", "22"));
        d.expanded_jumps = BTreeSet::from([0, 2]);
        state.editor = Some(d);

        let _ = state.update(Message::RemoveJump(1), &Ctx { vault: None });

        let draft = state.editor.as_ref().unwrap();
        assert_eq!(draft.expanded_jumps, BTreeSet::from([0, 1]));
    }

    /// 新增跳板机后自动展开该张卡片。
    #[test]
    fn adding_a_jump_expands_it() {
        let mut state = State::new(None, Vec::new(), BTreeSet::new());
        state.editor = Some(draft());

        let _ = state.update(Message::AddJump, &Ctx { vault: None });

        let draft = state.editor.as_ref().unwrap();
        assert_eq!(draft.expanded_jumps, BTreeSet::from([0]));
    }

    /// 校验失败时，出错字段所在的跳板机卡片与高级选项区应自动展开（否则错误既看不见也聚焦不到）。
    #[test]
    fn a_failing_jump_is_expanded_before_focus_moves_to_it() {
        let mut state = State::new(None, Vec::new(), BTreeSet::new());
        let mut d = draft();
        d.jumps.push(jump("", "22"));
        state.editor = Some(d);
        let vault = Arc::new(Vault::new("test-master"));

        let _ = state.update(Message::SaveSession, &Ctx { vault: Some(vault) });

        let draft = state.editor.as_ref().unwrap();
        assert!(draft.advanced_open, "出错字段在折叠区内，须先展开高级选项");
        assert_eq!(draft.expanded_jumps, BTreeSet::from([0]));
        assert_eq!(state.focused_field, Some(SessionField::JumpHost(0)));
    }

    /// 构造测试用会话配置（名称与认证方式可变，其余字段固定）。
    fn cfg(name: &str, auth: AuthMethod) -> SessionConfig {
        SessionConfig {
            id: new_id(),
            name: name.to_string(),
            host: "example.com".into(),
            port: 2222,
            username: "root".into(),
            auth,
            group: Some("prod".into()),
            jumps: Vec::new(),
            startup_command: None,
            forwards: Vec::new(),
            favorite: false,
        }
    }

    /// 构造指定名称 / 主机 / 用户名 / 分组的会话（其余字段固定），供过滤用例使用。
    fn s(name: &str, host: &str, user: &str, group: Option<&str>) -> SessionConfig {
        SessionConfig {
            id: new_id(),
            name: name.into(),
            host: host.into(),
            port: 22,
            username: user.into(),
            auth: AuthMethod::Agent,
            group: group.map(str::to_string),
            jumps: Vec::new(),
            startup_command: None,
            forwards: Vec::new(),
            favorite: false,
        }
    }

    #[test]
    fn filter_matches_across_fields_case_insensitively() {
        let sessions = vec![
            s("Web Server", "web-01.example.com", "alice", Some("Prod")),
            s("Database", "db-01.example.com", "bob", Some("Staging")),
            s("Bastion", "bastion.example.com", "ops", None),
        ];

        // 名称、主机、用户名、分组任一字段命中即可。
        assert_eq!(filtered_sessions(&sessions, "web").len(), 1);
        assert_eq!(filtered_sessions(&sessions, "example.com").len(), 3);
        assert_eq!(filtered_sessions(&sessions, "bob").len(), 1);
        assert_eq!(filtered_sessions(&sessions, "prod").len(), 1);
        // 大小写不敏感，前后空白被裁掉。
        assert_eq!(filtered_sessions(&sessions, "WEB SERVER").len(), 1);
        assert_eq!(filtered_sessions(&sessions, "  prod  ").len(), 1);
    }

    #[test]
    fn filter_empty_query_returns_all_and_no_match_returns_empty() {
        let sessions = vec![s("a", "h1", "u", None), s("b", "h2", "u", None)];

        assert_eq!(filtered_sessions(&sessions, "").len(), 2);
        assert_eq!(filtered_sessions(&sessions, "   ").len(), 2);
        assert!(filtered_sessions(&sessions, "nope").is_empty());
    }

    #[test]
    fn duplicate_assigns_new_id_and_keeps_fields() {
        let vault = Vault::new("test-master");
        let src = cfg(
            "web",
            AuthMethod::Password {
                password: Some(vault.encrypt("secret")),
            },
        );

        let copy = duplicated(&src, std::slice::from_ref(&src));

        assert_ne!(copy.id, src.id);
        assert_eq!(copy.name, "web 2");
        assert_eq!(copy.host, src.host);
        assert_eq!(copy.port, src.port);
        assert_eq!(copy.username, src.username);
        assert_eq!(copy.group, src.group);
        // 信封原样克隆：同一 DEK 下仍解出源的凭据，无需重新加密。
        match (&src.auth, &copy.auth) {
            (
                AuthMethod::Password {
                    password: Some(src_env),
                },
                AuthMethod::Password {
                    password: Some(copy_env),
                },
            ) => assert_eq!(src_env, copy_env),
            _ => panic!("凭据信封应被原样保留"),
        }
    }

    #[test]
    fn duplicate_skips_taken_numbers() {
        let src = cfg("web", AuthMethod::Agent);
        let sessions = vec![
            src.clone(),
            cfg("web 2", AuthMethod::Agent),
            cfg("web 3", AuthMethod::Agent),
        ];

        assert_eq!(duplicated(&src, &sessions).name, "web 4");
    }

    #[test]
    fn duplicate_of_credential_less_session_stays_credential_less() {
        let src = cfg("bare", AuthMethod::Password { password: None });

        let copy = duplicated(&src, &[]);

        assert_eq!(copy.name, "bare 2");
        assert_eq!(copy.auth, AuthMethod::Password { password: None });
    }

    #[test]
    fn jump_chain_round_trips_through_the_editor() {
        let vault = Vault::new("test-master");
        let src = SessionConfig {
            jumps: vec![
                JumpHost {
                    host: "bastion-outer".into(),
                    port: 2222,
                    username: "outer".into(),
                    auth: AuthMethod::Password {
                        password: Some(vault.encrypt("outer-pw")),
                    },
                },
                JumpHost {
                    host: "bastion-inner".into(),
                    port: rterm_config::DEFAULT_SSH_PORT,
                    username: "inner".into(),
                    auth: AuthMethod::PublicKey {
                        key_path: "/keys/id_ed25519".into(),
                        passphrase: None,
                    },
                },
            ],
            ..cfg("web", AuthMethod::Password { password: None })
        };

        // 打开编辑器再保存（凭据留空 → 沿用原信封），跳板链必须逐字往返。
        let draft = EditorDraft::from_config(&src);
        let out = draft.to_config(&vault).expect("round trip must succeed");

        assert_eq!(out.jumps, src.jumps);
    }

    #[test]
    fn jump_credentials_are_encrypted_from_typed_plaintext() {
        let vault = Vault::new("test-master");
        let mut draft = EditorDraft::from_config(&cfg("web", AuthMethod::Agent));
        draft.jumps.push(JumpDraft {
            host: "bastion".into(),
            port: String::new(),
            username: "ops".into(),
            auth: "password".into(),
            password: "typed-secret".into(),
            key_path: String::new(),
            passphrase: String::new(),
            orig_auth: None,
        });

        let out = draft.to_config(&vault).expect("save must succeed");

        assert_eq!(out.jumps.len(), 1);
        let hop = &out.jumps[0];
        assert_eq!(hop.port, rterm_config::DEFAULT_SSH_PORT, "空端口应取默认值");
        match &hop.auth {
            AuthMethod::Password {
                password: Some(env),
            } => {
                assert_eq!(
                    vault.decrypt(env).expect("decrypt").as_str(),
                    "typed-secret"
                );
            }
            _ => panic!("键入的密码应被加密成信封"),
        }
    }

    #[test]
    fn a_jump_without_a_host_is_rejected() {
        let vault = Vault::new("test-master");
        let mut draft = EditorDraft::from_config(&cfg("web", AuthMethod::Agent));
        draft.jumps.push(JumpDraft::new());

        assert!(
            draft.to_config(&vault).is_err(),
            "空主机的跳板机应被校验拦下"
        );
    }

    #[test]
    fn resolve_collapsed_groups_collapses_named_groups_for_large_first_run() {
        let mut sessions = Vec::new();
        for i in 0..8 {
            sessions.push(s(&format!("p{i}"), "h", "u", Some("prod")));
        }
        for i in 0..7 {
            sessions.push(s(&format!("s{i}"), "h", "u", Some("staging")));
        }
        sessions.push(s("ungrouped", "h", "u", None));
        assert_eq!(sessions.len(), AUTO_COLLAPSE_THRESHOLD + 1);

        let (collapsed, changed) = resolve_collapsed_groups(&sessions, None);

        assert!(changed, "首启初始化必须回写配置");
        assert_eq!(
            collapsed.into_iter().collect::<Vec<_>>(),
            vec!["prod".to_string(), "staging".to_string()]
        );
    }

    #[test]
    fn resolve_collapsed_groups_expands_named_groups_at_threshold() {
        let sessions: Vec<SessionConfig> = (0..AUTO_COLLAPSE_THRESHOLD)
            .map(|i| s(&format!("s{i}"), "h", "u", Some("prod")))
            .collect();

        let (collapsed, changed) = resolve_collapsed_groups(&sessions, None);

        assert!(changed, "首启即使结果为空也必须标记初始化");
        assert!(collapsed.is_empty(), "未超阈值时命名分组应默认展开");
    }

    #[test]
    fn resolve_collapsed_groups_keeps_user_state() {
        let sessions = vec![
            s("a", "h", "u", Some("prod")),
            s("b", "h", "u", Some("staging")),
        ];
        let stored = vec!["prod".to_string()];

        let (collapsed, changed) = resolve_collapsed_groups(&sessions, Some(&stored));

        assert!(!changed, "现存键无需回写");
        assert_eq!(collapsed.into_iter().collect::<Vec<_>>(), stored);
    }

    #[test]
    fn resolve_collapsed_groups_prunes_stale_keys() {
        let sessions = vec![s("a", "h", "u", Some("prod"))];
        let stored = vec!["prod".to_string(), "gone".to_string(), String::new()];

        let (collapsed, changed) = resolve_collapsed_groups(&sessions, Some(&stored));

        assert!(changed, "失效键与无对应会话的未分组键都应被修剪");
        assert_eq!(
            collapsed.into_iter().collect::<Vec<_>>(),
            vec!["prod".to_string()]
        );
    }

    #[test]
    fn resolve_collapsed_groups_keeps_ungrouped_key_when_present() {
        let sessions = vec![s("a", "h", "u", None)];
        let stored = vec![String::new()];

        let (collapsed, changed) = resolve_collapsed_groups(&sessions, Some(&stored));

        assert!(!changed);
        assert!(collapsed.contains(""), "存在未分组会话时应保留未分组折叠键");
    }

    #[test]
    fn toggle_group_flips_state_without_touching_other_groups() {
        let mut state = State::new(None, Vec::new(), BTreeSet::new());
        let ctx = Ctx { vault: None };

        let _ = state.update(Message::ToggleGroup("prod".into()), &ctx);
        let _ = state.update(Message::ToggleGroup("staging".into()), &ctx);
        assert!(state.collapsed_groups.contains("prod"));
        assert!(state.collapsed_groups.contains("staging"));

        let _ = state.update(Message::ToggleGroup("prod".into()), &ctx);
        assert!(!state.collapsed_groups.contains("prod"));
        assert!(state.collapsed_groups.contains("staging"));
    }

    #[test]
    fn collapse_all_and_expand_all_toggle_every_group() {
        let sessions = vec![
            s("a", "h", "u", Some("prod")),
            s("b", "h", "u", Some("staging")),
            s("c", "h", "u", None),
        ];
        let mut state = State::new(None, sessions, BTreeSet::new());
        let ctx = Ctx { vault: None };

        let _ = state.update(Message::CollapseAllGroups, &ctx);
        assert_eq!(
            state.collapsed_groups,
            BTreeSet::from(["prod".to_string(), "staging".to_string(), String::new()])
        );

        let _ = state.update(Message::ExpandAllGroups, &ctx);
        assert!(state.collapsed_groups.is_empty());
    }

    #[test]
    fn toggle_favorite_flips_the_flag() {
        let session = s("web", "h", "u", Some("prod"));
        let id = session.id.clone();
        let mut state = State::new(None, vec![session], BTreeSet::new());
        let ctx = Ctx { vault: None };

        let _ = state.update(Message::ToggleFavorite(id.clone()), &ctx);
        assert!(state.sessions[0].favorite);

        let _ = state.update(Message::ToggleFavorite(id), &ctx);
        assert!(!state.sessions[0].favorite);
    }

    #[test]
    fn toggle_favorite_ignores_unknown_session() {
        let mut state = State::new(
            None,
            vec![s("web", "h", "u", Some("prod"))],
            BTreeSet::new(),
        );
        let ctx = Ctx { vault: None };

        let _ = state.update(Message::ToggleFavorite("gone".into()), &ctx);

        assert!(!state.sessions[0].favorite);
    }

    #[test]
    fn connecting_expands_the_collapsed_group_of_that_session() {
        let session = s("web", "h", "u", Some("prod"));
        let id = session.id.clone();
        let mut state = State::new(None, vec![session], BTreeSet::from(["prod".to_string()]));
        let ctx = Ctx { vault: None };

        let _ = state.update(Message::ConnectSession(id), &ctx);

        assert!(
            !state.collapsed_groups.contains("prod"),
            "刚连接的会话所在分组应自动展开"
        );
    }

    #[test]
    fn connecting_leaves_other_groups_collapsed() {
        let session = s("web", "h", "u", Some("prod"));
        let id = session.id.clone();
        let mut state = State::new(
            None,
            vec![session],
            BTreeSet::from(["prod".to_string(), "staging".to_string()]),
        );
        let ctx = Ctx { vault: None };

        let _ = state.update(Message::ConnectSession(id), &ctx);

        assert!(state.collapsed_groups.contains("staging"));
    }
}
