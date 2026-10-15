//! 会话文件的导入 / 导出：仅连接配置与带凭据两种导出、对应的两种导入，四条流程在此收口。
//!
//! 带凭据通道用一次性**导出口令**派生导出密钥，把每个凭据信封在本机 DEK 与导出 DEK 之间重包裹，
//! 全程不落明文，模式 0（钥匙串随机 DEK）的机器也能导出。菜单只发意图（见 [`super::session`]），
//! 口令弹窗、文件对话框、重包裹与落盘全部归本模块。

use crate::state::ToastKind;
use crate::t;
use iced::Task;
use log::error;
use rterm_config::{
    SessionConfig, SessionStore, export_sessions, export_sessions_with_credentials,
    import_credential_export, import_sessions, inspect_sessions_file, new_id,
};
use rterm_crypto::Vault;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::task::spawn_blocking;

/// 口令流程的进度阶段。
///
/// 两条流程各自只有一次 `spawn_blocking`（派生与重包裹在同一次阻塞里做完），故只需一个忙碌态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stage {
    /// 空闲。
    #[default]
    Idle,
    /// 正在派生密钥、重包裹凭据并读写文件（Argon2id 为其中的秒级成本）。
    Working,
}

/// 带凭据导出的结果概要。
#[derive(Debug, Clone, Copy)]
pub struct ExportSummary {
    /// 导出的会话条数。
    pub exported: usize,
    /// 本机解不开而被略过的凭据条数。
    pub dropped: usize,
}

/// 导入失败的分类（决定用哪条本地化文案）。
#[derive(Debug, Clone)]
pub enum ImportError {
    /// 导出口令不正确（哨兵校验失败）。
    WrongPassphrase,
    /// 其它失败：读写、解析、KDF 参数越界等，携带底层错误文本。
    Other(String),
}

/// 模块状态：导出弹窗的开关与口令输入、导入弹窗的口令输入、两侧目标路径与进度。
#[derive(Debug, Clone, Default)]
pub struct State {
    /// 「导出会话」弹窗是否打开。
    pub export_open: bool,
    /// 弹窗内「含凭据」开关的当前状态。
    pub export_with_credentials: bool,
    /// 本次能否携带凭据：发起导出时保险库已解锁。为 `false` 时开关禁用，仍可导出连接配置。
    pub credentials_available: bool,
    /// 系统保存框已打开、等待返回值：此间禁止再次提交、取消与关窗。
    pub export_picking: bool,
    /// 带凭据导出的目标路径（保存框返回后写入，派生完成即取走）。
    pub export_path: Option<PathBuf>,
    /// 导出口令输入框。
    pub export_pass: String,
    /// 导出口令「再次输入」框。
    pub export_confirm: String,
    /// 导出弹窗错误提示。
    pub export_error: Option<String>,
    /// 「导入口令」弹窗是否打开。
    pub import_open: bool,
    /// 待导入的带凭据导出件路径。
    pub import_path: Option<PathBuf>,
    /// 导入口令输入框。
    pub import_pass: String,
    /// 导入弹窗错误提示。
    pub import_error: Option<String>,
    /// 两条流程共用的进度阶段。
    pub stage: Stage,
}

/// 模块内部消息：弹窗输入、文件对话框意图与模块自处理的异步结果。
#[derive(Clone)]
pub enum Message {
    /// 无操作占位消息（用于禁用态按钮点击）。
    Noop,
    /// 导出会话：打开导出弹窗，含不含凭据由弹窗内开关决定。
    BeginExport,
    /// 导出弹窗的「含凭据」开关变更。
    ToggleCredentials(bool),
    /// 导入会话：打开文件选择对话框（是否带凭据在选完后判定）。
    BeginImport,
    /// 导出目标文件选择结果（`None` 表示用户取消）。
    ExportFilePicked(Option<PathBuf>),
    /// 导入文件选择结果（`None` 表示用户取消）。
    ImportFilePicked(Option<PathBuf>),
    /// 导出口令输入变更。
    ExportPassInput(String),
    /// 导出口令「再次输入」变更。
    ExportConfirmInput(String),
    /// 提交导出：校验通过后经自回路去打开系统保存框。
    ExportSubmit,
    /// 打开系统保存框——单列一条消息，使测试能断言分流而不真起对话框。
    OpenExportPicker,
    /// 取消导出弹窗。
    ExportCancel,
    /// 带凭据导出完成（`Err` 携带底层错误文本）。
    ExportDone(Result<ExportSummary, String>),
    /// 导入口令输入变更。
    ImportPassInput(String),
    /// 提交导入口令：校验哨兵、把凭据重包裹为本机 DEK 并合并落盘。
    ImportSubmit,
    /// 取消导入口令弹窗。
    ImportCancel,
    /// 带凭据口令校验与重包裹完成（`Ok` 携带重包裹后的会话与略过条数）。
    ImportDone(Result<(Vec<SessionConfig>, usize), ImportError>),
}

/// 上行事件：仅通知父层，由父层 `Message::SessionIoEvent` 分支修改父状态并落地。
///
/// 模块绝不写父状态。
#[derive(Clone)]
pub enum Event {
    /// 请求父层整体写入会话列表（导入合并 + 落盘成功后）。
    SetSessions(Vec<SessionConfig>),
    /// 请求父层弹出 toast 通知（携带类型与文案）。
    Toast(ToastKind, String),
    /// 自回路：把一条模块内部消息经父层派发回 `State::update`。
    Emit(Box<Message>),
}

/// 父层只读上下文：会话存储、当前保险库与会话列表快照。
///
/// 模块据此加解密与落盘，但**绝不写父状态**；写回一律经 [`Event`] 由父层落地。
pub struct Ctx {
    /// 会话存储（导出与落盘用）。
    pub store: Option<SessionStore>,
    /// 当前保险库（本机 DEK）。导出侧据此决定凭据开关是否可用，导入侧据此拒绝收下带凭据的导出件。
    pub vault: Option<Arc<Vault>>,
    /// 当前会话列表（导出源，以及导入合并时的既有项）。
    pub sessions: Vec<SessionConfig>,
}

impl State {
    /// 构造初始（空闲）状态。
    pub fn new() -> Self {
        Self::default()
    }

    /// 模块更新：只改自身 `State`；需要父层配合的事以 [`Event`] 经 `Task` 上行。
    pub fn update(&mut self, msg: Message, ctx: &Ctx) -> Task<Event> {
        match msg {
            Message::Noop => Task::none(),
            Message::BeginExport => {
                self.export_open = true;
                self.export_with_credentials = false;
                self.credentials_available = ctx.vault.is_some();
                self.export_pass.clear();
                self.export_confirm.clear();
                self.export_error = None;
                Task::none()
            }
            Message::ToggleCredentials(on) => {
                if !self.export_open || !self.editable() || (on && !self.credentials_available) {
                    return Task::none();
                }
                self.export_with_credentials = on;
                if !on {
                    // 表单已收起，不该在里面留一份明文口令。
                    self.export_pass.clear();
                    self.export_confirm.clear();
                }
                self.export_error = None;
                Task::none()
            }
            Message::BeginImport => pick_open_file(),
            Message::ExportFilePicked(None) => {
                // 取消保存框不算失败：弹窗与其中已填的口令原样留着，用户可重试或直接取消。
                self.export_picking = false;
                Task::none()
            }
            Message::ExportFilePicked(Some(path)) => {
                self.export_picking = false;
                if self.export_with_credentials {
                    credential_export_at_path(self, path, ctx)
                } else {
                    plain_export_at_path(self, &path, ctx)
                }
            }
            Message::ExportPassInput(v) => {
                self.export_pass = v;
                self.export_error = None;
                Task::none()
            }
            Message::ExportConfirmInput(v) => {
                self.export_confirm = v;
                self.export_error = None;
                Task::none()
            }
            Message::ExportSubmit => export_submit(self, ctx),
            Message::OpenExportPicker => {
                if !self.editable() {
                    return Task::none();
                }
                self.export_picking = true;
                pick_save_file(self)
            }
            Message::ExportCancel => {
                if !self.editable() {
                    return Task::none();
                }
                self.export_open = false;
                self.export_path = None;
                self.export_pass.clear();
                self.export_confirm.clear();
                self.export_error = None;
                Task::none()
            }
            Message::ExportDone(res) => export_done(self, res),
            Message::ImportFilePicked(None) => Task::none(),
            Message::ImportFilePicked(Some(path)) => import_file_picked(self, &path, ctx),
            Message::ImportPassInput(v) => {
                self.import_pass = v;
                self.import_error = None;
                Task::none()
            }
            Message::ImportCancel => {
                if self.stage != Stage::Idle {
                    return Task::none();
                }
                self.import_open = false;
                self.import_path = None;
                self.import_pass.clear();
                self.import_error = None;
                Task::none()
            }
            Message::ImportSubmit => import_submit(self, ctx),
            Message::ImportDone(res) => import_done(self, res, ctx),
        }
    }

    /// 导出弹窗是否处于「可改输入、可提交、可关窗」的状态：既没在选路径，也没在派生。
    fn editable(&self) -> bool {
        self.stage == Stage::Idle && !self.export_picking
    }
}

/// 打开「另存为」对话框，结果经 `Event::Emit` 自回路流入 [`Message::ExportFilePicked`]。
fn pick_save_file(state: &mut State) -> Task<Event> {
    let title = t!("session.export").to_string();
    let file_name = if state.export_with_credentials {
        "rterm-sessions-with-credentials.toml"
    } else {
        "rterm-sessions.toml"
    };
    Task::perform(
        async move {
            rfd::AsyncFileDialog::new()
                .set_title(&title)
                .set_file_name(file_name)
                .add_filter("TOML", &["toml"])
                .save_file()
                .await
                .map(|f| f.path().to_path_buf())
        },
        |path| Event::Emit(Box::new(Message::ExportFilePicked(path))),
    )
}

/// 打开文件选择对话框，结果经 `Event::Emit` 自回路流入 [`Message::ImportFilePicked`]。
fn pick_open_file() -> Task<Event> {
    let title = t!("session.import").to_string();
    Task::perform(
        async move {
            rfd::AsyncFileDialog::new()
                .set_title(&title)
                .add_filter("TOML", &["toml"])
                .pick_file()
                .await
                .map(|f| f.path().to_path_buf())
        },
        |path| Event::Emit(Box::new(Message::ImportFilePicked(path))),
    )
}

/// 选定导入文件后分流：仅连接配置直接入库；带凭据者索要口令；裸拷贝的本机配置拒绝。
fn import_file_picked(state: &mut State, path: &Path, ctx: &Ctx) -> Task<Event> {
    let meta = match inspect_sessions_file(path) {
        Ok(meta) => meta,
        Err(e) => {
            return Task::done(Event::Toast(
                ToastKind::Error,
                t!("app.import_failed", err => e).to_string(),
            ));
        }
    };
    if meta.with_credentials {
        // 凭据要重包裹成本机 DEK，保险库未就绪时无处可去——先提示，别让用户白输一遍口令。
        if ctx.vault.is_none() {
            return Task::done(Event::Toast(
                ToastKind::Error,
                t!("app.vault_locked").to_string(),
            ));
        }
        state.import_path = Some(path.to_path_buf());
        state.import_pass.clear();
        state.import_error = None;
        state.import_open = true;
        return Task::none();
    }
    if !meta.is_export_file && meta.has_crypto_header {
        // 别机器的 sessions.toml 被直接拷来：其信封由对方本机 DEK 加密，收下只会得到一堆解不了的
        // 凭据，也等于诱导用户拿主密码去解一份并非导出的文件。明确拒绝。
        return Task::done(Event::Toast(
            ToastKind::Error,
            t!("cred.not_export_file").to_string(),
        ));
    }
    // 带标记的仅连接导出件，或无标记的旧导出件：凭据一律由 `import_sessions` 剥离。
    import_plain_sessions(path, ctx)
}

/// 仅连接配置通道入库：追加 + `id` 冲突重分配，然后落盘。
fn import_plain_sessions(path: &Path, ctx: &Ctx) -> Task<Event> {
    match import_sessions(path) {
        Ok(incoming) => {
            let count = incoming.len();
            commit_imported(incoming, count, 0, ctx)
        }
        Err(e) => Task::done(Event::Toast(
            ToastKind::Error,
            t!("app.import_failed", err => e).to_string(),
        )),
    }
}

/// 把导入的会话并入现有列表并落盘，成功则上行合并结果与提示。
///
/// 落盘用的是**本机**保险库的头：带凭据通道的信封已被重包裹为本机 DEK，仅连接通道本就没有凭据。
fn commit_imported(
    incoming: Vec<SessionConfig>,
    imported: usize,
    dropped: usize,
    ctx: &Ctx,
) -> Task<Event> {
    let Some(store) = ctx.store.as_ref() else {
        return Task::done(Event::Toast(
            ToastKind::Error,
            t!("masterpw.no_store").to_string(),
        ));
    };
    let Some(vault) = ctx.vault.as_ref() else {
        return Task::done(Event::Toast(
            ToastKind::Error,
            t!("app.vault_locked").to_string(),
        ));
    };
    let merged = merge_imported(&ctx.sessions, incoming);
    if let Err(e) = store.save(&merged, vault.header()) {
        error!("failed to write sessions after import: {e}");
        return Task::done(Event::Toast(
            ToastKind::Error,
            t!("app.import_failed", err => e).to_string(),
        ));
    }
    let message = if dropped == 0 {
        t!("app.imported", count => imported)
    } else {
        t!("cred.imported_dropped", count => imported, dropped => dropped)
    };
    Task::batch(vec![
        Task::done(Event::SetSessions(merged)),
        Task::done(Event::Toast(ToastKind::Success, message.to_string())),
    ])
}

/// 现有列表与导入项合并：`id` 已存在者为新项重分配 `id`，不覆盖原会话。
fn merge_imported(existing: &[SessionConfig], incoming: Vec<SessionConfig>) -> Vec<SessionConfig> {
    let mut merged = existing.to_vec();
    for mut session in incoming {
        // 逐个比对已增长的列表，故导入项内部的重复 `id` 也会被重分配。
        if merged.iter().any(|e| e.id == session.id) {
            session.id = new_id();
        }
        merged.push(session);
    }
    merged
}

/// 提交导出：只做即时校验，通过后才发 [`Message::OpenExportPicker`]——不合法就不该让用户
/// 面对一个白点的系统对话框。
fn export_submit(state: &mut State, ctx: &Ctx) -> Task<Event> {
    if !state.editable() {
        return Task::none();
    }
    if state.export_with_credentials {
        if state.export_pass.is_empty() {
            state.export_error = Some(t!("cred.empty"));
            return Task::none();
        }
        if state.export_pass != state.export_confirm {
            state.export_error = Some(t!("cred.mismatch"));
            return Task::none();
        }
        // 兜底：弹窗开着期间保险库可能已失效（改主密码 / 重新锁定）。
        if ctx.vault.is_none() {
            state.export_error = Some(t!("app.vault_locked"));
            return Task::none();
        }
    }
    Task::done(Event::Emit(Box::new(Message::OpenExportPicker)))
}

/// 仅连接配置导出：落盘即完成，弹窗随之关闭。
fn plain_export_at_path(state: &mut State, path: &Path, ctx: &Ctx) -> Task<Event> {
    state.export_open = false;
    state.export_error = None;
    match export_sessions(path, &ctx.sessions) {
        Ok(()) => Task::done(Event::Toast(
            ToastKind::Success,
            t!(
                "app.exported",
                count => ctx.sessions.len(),
                path => path.display()
            )
            .to_string(),
        )),
        Err(e) => Task::done(Event::Toast(
            ToastKind::Error,
            t!("app.export_failed", err => e).to_string(),
        )),
    }
}

/// 带凭据导出：口令已校验、路径已选定，一次 `spawn_blocking` 完成派生 + 重包裹 + 写盘。
fn credential_export_at_path(state: &mut State, path: PathBuf, ctx: &Ctx) -> Task<Event> {
    let Some(local) = ctx.vault.clone() else {
        state.export_error = Some(t!("app.vault_locked"));
        return Task::none();
    };
    let exported = ctx.sessions.len();
    let sessions = ctx.sessions.clone();
    // 口令用 `take` 交出所有权，避免弹窗字段里再留一份副本。
    let passphrase = std::mem::take(&mut state.export_pass);
    state.export_confirm.clear();
    state.export_path = Some(path.clone());
    state.stage = Stage::Working;
    Task::perform(
        async move {
            spawn_blocking(move || {
                let export = Vault::for_export(&passphrase);
                let dropped = export_sessions_with_credentials(&path, &sessions, &local, &export)
                    .map_err(|e| e.to_string())?;
                Ok(ExportSummary { exported, dropped })
            })
            .await
            .unwrap()
        },
        |res| Event::Emit(Box::new(Message::ExportDone(res))),
    )
}

/// 带凭据导出完成：关窗、清空口令输入，给出含略过条数的结果提示。
fn export_done(state: &mut State, res: Result<ExportSummary, String>) -> Task<Event> {
    state.stage = Stage::Idle;
    state.export_open = false;
    let path = state.export_path.take();
    state.export_pass.clear();
    state.export_confirm.clear();
    state.export_error = None;

    let Some(path) = path else {
        return Task::none();
    };
    match res {
        Ok(summary) => {
            let message = if summary.dropped == 0 {
                t!(
                    "cred.exported",
                    count => summary.exported,
                    path => path.display()
                )
            } else {
                t!(
                    "cred.exported_dropped",
                    count => summary.exported,
                    path => path.display(),
                    dropped => summary.dropped
                )
            };
            Task::done(Event::Toast(ToastKind::Success, message.to_string()))
        }
        Err(e) => Task::done(Event::Toast(
            ToastKind::Error,
            t!("app.export_failed", err => e).to_string(),
        )),
    }
}

/// 提交导入口令：把「读盘 + 派生 + 哨兵校验 + 重包裹」放进同一次阻塞调用，
/// 使校验用的头与被解的信封来自同一次读取。
fn import_submit(state: &mut State, ctx: &Ctx) -> Task<Event> {
    if state.stage != Stage::Idle {
        return Task::none();
    }
    if state.import_pass.is_empty() {
        state.import_error = Some(t!("cred.empty"));
        return Task::none();
    }
    let Some(path) = state.import_path.clone() else {
        state.import_error = Some(t!("masterpw.no_store"));
        return Task::none();
    };
    let Some(local) = ctx.vault.clone() else {
        state.import_error = Some(t!("app.vault_locked"));
        return Task::none();
    };
    let passphrase = std::mem::take(&mut state.import_pass);
    state.stage = Stage::Working;
    Task::perform(
        async move {
            spawn_blocking(move || {
                // 口令错在哨兵处即失败：此时不产出任何会话，本机会话列表分毫未动。
                let (incoming, dropped) = import_credential_export(&path, &passphrase, &local)
                    .map_err(|e| match e {
                        rterm_config::ConfigError::Crypto(rterm_crypto::CryptoError::Verify) => {
                            ImportError::WrongPassphrase
                        }
                        other => ImportError::Other(other.to_string()),
                    })?;
                Ok((incoming, dropped))
            })
            .await
            .unwrap()
        },
        |res| Event::Emit(Box::new(Message::ImportDone(res))),
    )
}

/// 口令校验与重包裹完成：失败留在弹窗内；成功则关窗、清空口令并合并落盘。
fn import_done(
    state: &mut State,
    res: Result<(Vec<SessionConfig>, usize), ImportError>,
    ctx: &Ctx,
) -> Task<Event> {
    state.stage = Stage::Idle;
    match res {
        Ok((incoming, dropped)) => {
            state.import_open = false;
            state.import_path = None;
            state.import_pass.clear();
            state.import_error = None;
            let imported = incoming.len();
            commit_imported(incoming, imported, dropped, ctx)
        }
        // 口令错与其它失败都停留在弹窗、不改动任何会话；提示语须点明「不是主密码」。
        Err(ImportError::WrongPassphrase) => {
            state.import_error = Some(t!("cred.wrong_pass"));
            Task::none()
        }
        Err(ImportError::Other(e)) => {
            state.import_error = Some(t!("app.import_failed", err => e));
            Task::none()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::STATE_ROOT_LOCK as STATE_LOCK;
    use futures::StreamExt;
    use rterm_config::{AuthMethod, new_id};

    /// 在临时 tokio 运行时里跑完 `Task<Event>` 并收集事件。
    ///
    /// 重包裹跑在 `spawn_blocking` 上，必须有真实运行时上下文，否则 panic。
    fn run_events(task: Task<Event>) -> Vec<Event> {
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

    /// 父层替身：按 `events::apply_session_io_event` 的规则落地事件，并驱动自回路。
    struct Parent {
        state: State,
        ctx: Ctx,
        toasts: Vec<(ToastKind, String)>,
    }

    impl Parent {
        fn new(vault: Option<Arc<Vault>>, sessions: Vec<SessionConfig>) -> Self {
            Self {
                state: State::new(),
                ctx: Ctx {
                    store: SessionStore::new().ok(),
                    vault,
                    sessions,
                },
                toasts: Vec::new(),
            }
        }

        /// 派发一条消息并把事件落地，返回自回路的内层消息（父层会逐条再投递）。
        fn send(&mut self, msg: Message) -> Vec<Message> {
            let mut emits = Vec::new();
            for e in run_events(self.state.update(msg, &self.ctx)) {
                match e {
                    Event::SetSessions(sessions) => self.ctx.sessions = sessions,
                    Event::Toast(kind, text) => self.toasts.push((kind, text)),
                    Event::Emit(inner) => emits.push(*inner),
                }
            }
            emits
        }

        /// 走完一条消息引发的整条自回路（含 `spawn_blocking` 的完成消息）。
        /// 派发 [`Message::ExportSubmit`] 须改走 [`submit_and_pick`]：其自回路会真起系统保存框。
        fn drain(&mut self, msg: Message) {
            let mut queue = self.send(msg);
            while let Some(next) = queue.pop() {
                queue.extend(self.send(next));
            }
        }

        fn ids(&self) -> Vec<String> {
            self.ctx.sessions.iter().map(|s| s.id.clone()).collect()
        }
    }

    /// 把状态根重定向到独立临时目录，绝不碰真实 `~/.config/rterm`。
    fn use_temp_state_root(tag: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("rterm_sio_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        rterm_config::paths::set_test_root(Some(tmp.clone()));
        tmp
    }

    /// 构造带口令凭据的会话。
    fn session_with_password(vault: &Vault, name: &str, secret: &str) -> SessionConfig {
        SessionConfig {
            id: new_id(),
            name: name.to_string(),
            host: "example.com".into(),
            port: 22,
            username: "root".into(),
            auth: AuthMethod::Password {
                password: Some(vault.encrypt(secret)),
            },
            group: None,
            jumps: Vec::new(),
        }
    }

    /// 打开导出弹窗并按需拨动凭据开关。
    fn open_export_dialog(parent: &mut Parent, credentials: bool) {
        parent.drain(Message::BeginExport);
        parent.drain(Message::ToggleCredentials(credentials));
    }

    /// 提交导出并直接投保存框的返回值：`send` 收下 `OpenExportPicker` 而不回投，
    /// 回投会在测试里真起一个系统对话框。
    fn submit_and_pick(parent: &mut Parent, path: &Path) {
        let emits = parent.send(Message::ExportSubmit);
        assert!(
            emits.iter().all(|m| matches!(m, Message::OpenExportPicker)),
            "校验通过后只该发出一条开保存框的自回路消息"
        );
        parent.drain(Message::ExportFilePicked(Some(path.to_path_buf())));
    }

    /// 走完带凭据导出：开弹窗 → 拨开关 → 输口令 → 提交并选路径。
    fn drive_export(parent: &mut Parent, path: &Path, passphrase: &str) {
        open_export_dialog(parent, true);
        parent.drain(Message::ExportPassInput(passphrase.to_string()));
        parent.drain(Message::ExportConfirmInput(passphrase.to_string()));
        submit_and_pick(parent, path);
    }

    /// 保险库未解锁：弹窗照常打开，但凭据开关拨不上去；仅连接配置的导出不受影响。
    #[test]
    fn locked_vault_disables_the_credentials_toggle_only() {
        let _guard = STATE_LOCK.lock().unwrap();
        let tmp = use_temp_state_root("no_vault");
        let mut parent = Parent::new(None, Vec::new());

        parent.drain(Message::BeginExport);

        assert!(parent.state.export_open, "导出弹窗应照常打开");
        assert!(
            !parent.state.credentials_available,
            "未解锁时凭据开关应不可用"
        );
        assert!(parent.toasts.is_empty(), "不该再用 toast 拦下整个导出");

        parent.drain(Message::ToggleCredentials(true));
        assert!(
            !parent.state.export_with_credentials,
            "未解锁时开关拨不上去"
        );

        let target = tmp.join("plain.toml");
        submit_and_pick(&mut parent, &target);
        assert!(target.exists(), "未解锁也应能导出仅连接配置");
        assert!(!parent.state.export_open, "导出完成应关窗");
    }

    /// 口令不合法时不发保存框；一致了才发，且选路进行中重复提交无效。
    #[test]
    fn export_submit_waits_for_a_valid_passphrase_before_opening_the_picker() {
        let _guard = STATE_LOCK.lock().unwrap();
        let _tmp = use_temp_state_root("validate");
        let vault = Arc::new(Vault::new("local-master"));
        let mut parent = Parent::new(Some(vault), Vec::new());
        open_export_dialog(&mut parent, true);

        let emits = parent.send(Message::ExportSubmit);
        assert!(emits.is_empty(), "空口令不该发起保存框");
        assert_eq!(
            parent.state.export_error.as_deref(),
            Some(t!("cred.empty").as_str())
        );

        parent.drain(Message::ExportPassInput("alpha".into()));
        parent.drain(Message::ExportConfirmInput("beta".into()));
        let emits = parent.send(Message::ExportSubmit);
        assert!(emits.is_empty(), "两次不一致不该发起保存框");
        assert_eq!(
            parent.state.export_error.as_deref(),
            Some(t!("cred.mismatch").as_str())
        );
        assert_eq!(parent.state.stage, Stage::Idle, "校验失败不应进入忙碌态");
        assert!(parent.state.export_open, "校验失败应留在弹窗内");

        parent.drain(Message::ExportConfirmInput("alpha".into()));
        let emits = parent.send(Message::ExportSubmit);
        assert_eq!(emits.len(), 1, "校验通过后应恰好发出一条自回路消息");
        assert!(matches!(emits[0], Message::OpenExportPicker));
        assert!(
            !parent.state.export_picking,
            "拨闸门的是 `OpenExportPicker` 本身"
        );

        parent.state.export_picking = true;
        assert!(
            parent.send(Message::ExportSubmit).is_empty(),
            "选路进行中不应再发一条"
        );
    }

    /// 弹窗开着期间保险库失效：提交被兜底拦下，不白起保存框。
    #[test]
    fn vault_lost_while_the_dialog_is_open_blocks_the_submit() {
        let _guard = STATE_LOCK.lock().unwrap();
        let _tmp = use_temp_state_root("vault_gone");
        let mut parent = Parent::new(Some(Arc::new(Vault::new("local-master"))), Vec::new());
        open_export_dialog(&mut parent, true);
        parent.drain(Message::ExportPassInput("alpha".into()));
        parent.drain(Message::ExportConfirmInput("alpha".into()));
        parent.ctx.vault = None;

        let emits = parent.send(Message::ExportSubmit);

        assert!(emits.is_empty(), "无密钥可重包裹时不该起保存框");
        assert_eq!(
            parent.state.export_error.as_deref(),
            Some(t!("app.vault_locked").as_str())
        );
        assert!(parent.state.export_open, "应留在弹窗内告知用户");
    }

    /// 收起凭据表单即清空两个口令输入框。
    #[test]
    fn toggle_off_clears_the_passphrase_fields() {
        let _guard = STATE_LOCK.lock().unwrap();
        let _tmp = use_temp_state_root("toggle_off");
        let mut parent = Parent::new(Some(Arc::new(Vault::new("local-master"))), Vec::new());
        open_export_dialog(&mut parent, true);
        parent.drain(Message::ExportPassInput("alpha".into()));
        parent.drain(Message::ExportConfirmInput("alpha".into()));

        parent.drain(Message::ToggleCredentials(false));

        assert!(parent.state.export_pass.is_empty(), "收起的表单不该留口令");
        assert!(parent.state.export_confirm.is_empty());
    }

    /// 保存框取消：弹窗与已填口令原样留着，不算失败、无提示；闸门解除后取消弹窗才生效。
    #[test]
    fn picker_cancel_leaves_the_dialog_and_its_inputs() {
        let _guard = STATE_LOCK.lock().unwrap();
        let _tmp = use_temp_state_root("pick_cancel");
        let mut parent = Parent::new(Some(Arc::new(Vault::new("local-master"))), Vec::new());
        open_export_dialog(&mut parent, true);
        parent.drain(Message::ExportPassInput("alpha".into()));
        parent.drain(Message::ExportConfirmInput("alpha".into()));
        parent.send(Message::ExportSubmit);
        parent.state.export_picking = true;

        parent.drain(Message::ExportCancel);
        assert!(parent.state.export_open, "选路期间取消不该关窗");

        parent.drain(Message::ExportFilePicked(None));

        assert!(parent.state.export_open, "取消保存框不该关弹窗");
        assert_eq!(parent.state.export_pass, "alpha", "已填口令不该被清空");
        assert!(!parent.state.export_picking, "返回后应解除选路闸门");
        assert!(parent.toasts.is_empty(), "取消不该有提示");

        parent.drain(Message::ExportCancel);
        assert!(!parent.state.export_open, "闸门解除后取消应生效");
    }

    /// 端到端：带凭据导出后导入，凭据被重包裹回本机密钥，`id` 冲突者重分配而非覆盖。
    #[test]
    fn credential_export_and_import_round_trip_rewraps_to_local_dek() {
        let _guard = STATE_LOCK.lock().unwrap();
        let tmp = use_temp_state_root("round_trip");
        let vault = Arc::new(Vault::new("local-master"));
        let sessions = vec![session_with_password(&vault, "web", "s3cret")];
        let local_id = sessions[0].id.clone();
        let mut parent = Parent::new(Some(vault.clone()), sessions);
        let target = tmp.join("export.toml");

        drive_export(&mut parent, &target, "export-pass");

        assert!(target.exists(), "导出应落盘");
        assert!(!parent.state.export_open, "导出完成后应关窗");
        assert!(
            parent.state.export_pass.is_empty() && parent.state.export_confirm.is_empty(),
            "导出完成应清空口令输入"
        );

        parent.drain(Message::ImportFilePicked(Some(target.clone())));
        assert!(parent.state.import_open, "带凭据导出件应索要导入口令");
        parent.drain(Message::ImportPassInput("export-pass".into()));
        parent.drain(Message::ImportSubmit);

        assert_eq!(parent.ctx.sessions.len(), 2, "导入应追加而非覆盖");
        assert!(!parent.state.import_open, "导入成功后应关窗");
        let imported = parent
            .ctx
            .sessions
            .iter()
            .find(|s| s.id != local_id)
            .expect("导入项");
        assert_ne!(imported.id, local_id, "同 id 的导入项须被重分配 id");
        let AuthMethod::Password {
            password: Some(env),
        } = &imported.auth
        else {
            panic!("导入项应保留口令凭据");
        };
        assert!(
            vault.decrypt(env).is_ok(),
            "导入的凭据必须已被重包裹为本机密钥"
        );
    }

    /// 导入口令错误：哨兵即失败，弹窗停留、本机会话列表分毫未动。
    #[test]
    fn wrong_import_passphrase_keeps_the_dialog_and_the_local_list() {
        let _guard = STATE_LOCK.lock().unwrap();
        let tmp = use_temp_state_root("wrong_pass");
        let vault = Arc::new(Vault::new("local-master"));
        let mut parent = Parent::new(
            Some(vault.clone()),
            vec![session_with_password(&vault, "db", "topsecret")],
        );
        let target = tmp.join("export.toml");
        drive_export(&mut parent, &target, "right-pass");
        let ids_before = parent.ids();

        parent.drain(Message::ImportFilePicked(Some(target)));
        parent.drain(Message::ImportPassInput("wrong-pass".into()));
        parent.drain(Message::ImportSubmit);

        assert!(parent.state.import_open, "口令错误应停留在弹窗内");
        assert_eq!(
            parent.state.import_error.as_deref(),
            Some(t!("cred.wrong_pass").as_str())
        );
        assert_eq!(parent.ids(), ids_before, "本机会话列表不应改变");
        assert!(
            t!("cred.wrong_pass").contains("master password")
                || t!("cred.notice_master_password").contains("主密码"),
            "提示须点明这不是主密码"
        );
    }

    /// 裸拷贝的 `sessions.toml`（无导出标记却带加密头）在选完文件即被拒绝，不引导用户输口令。
    #[test]
    fn bare_sessions_toml_copy_is_refused_at_file_pick() {
        let _guard = STATE_LOCK.lock().unwrap();
        let tmp = use_temp_state_root("bare_copy");
        let vault = Arc::new(Vault::new("local-master"));
        let sessions = vec![session_with_password(&vault, "web", "s3cret")];
        let mut parent = Parent::new(Some(vault.clone()), sessions.clone());
        // 造一份「本机形制」的会话文件（带加密头、无导出标记），模拟被直接拷来的他机 sessions.toml。
        let store = parent.ctx.store.as_ref().expect("store");
        store.save(&sessions, vault.header()).expect("落盘");
        let copy = tmp.join("foreign.toml");
        std::fs::copy(store.path(), &copy).expect("复制");
        let ids_before = parent.ids();

        parent.drain(Message::ImportFilePicked(Some(copy)));

        assert!(!parent.state.import_open, "裸拷贝不应进入口令弹窗");
        assert_eq!(
            parent.toasts.last().expect("应有提示").1,
            t!("cred.not_export_file")
        );
        assert_eq!(parent.ids(), ids_before, "拒绝时不应改动本机会话");
    }

    /// 开关关着的导出：不索要口令，导出的文件里凭据被剥离。
    #[test]
    fn plain_export_and_import_strips_credentials() {
        let _guard = STATE_LOCK.lock().unwrap();
        let tmp = use_temp_state_root("plain");
        let vault = Arc::new(Vault::new("local-master"));
        let mut parent = Parent::new(
            Some(vault.clone()),
            vec![session_with_password(&vault, "web", "pw")],
        );
        let target = tmp.join("plain.toml");

        open_export_dialog(&mut parent, false);
        submit_and_pick(&mut parent, &target);

        assert!(target.exists());
        assert!(!parent.state.export_open, "仅连接配置导出不需要口令");

        parent.drain(Message::ImportFilePicked(Some(target)));
        assert!(!parent.state.import_open, "仅连接配置导入不应弹窗");
        assert_eq!(parent.ctx.sessions.len(), 2);
        let AuthMethod::Password { password } = &parent.ctx.sessions[1].auth else {
            panic!("导入项应仍为口令形制");
        };
        assert!(password.is_none(), "导入的仅连接配置不应携带凭据信封");
    }
}
