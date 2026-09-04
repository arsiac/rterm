//! 主密码设置 / 解锁模块

use crate::state::ToastKind;
use crate::t;
use iced::Task;
use log::{error, warn};
use rterm_config::{AuthMethod, SessionConfig, SessionStore};
use rterm_crypto::Vault;
use std::sync::Arc;
use tokio::task::spawn_blocking;

/// 异步两阶段进度（按钮文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpwStage {
    /// 空闲。
    Idle,
    /// 正在派生密钥（Argon2id，最耗时）。
    Deriving,
    /// 正在重加密凭据并写盘。
    Reencrypting,
}

/// 模块状态：主密码流程的全部 UI 字段（原 `App` 上的 `mpw_*`）。
#[derive(Debug, Clone)]
pub struct State {
    /// 主密码输入框内容（设置 / 解锁共用）。
    pub input: String,
    /// 「再次输入」框内容。
    pub confirm: String,
    /// 「我已牢记主密码」勾选状态。
    pub memorized: bool,
    /// 错误提示（无则 `None`）。
    pub error: Option<String>,
    /// 两阶段异步进度。
    pub stage: MpwStage,
    /// 是否处于「设置主密码」流程（模式 0 → 1）。
    pub setup: bool,
    /// 是否打开「更改主密码」弹窗。
    pub change_open: bool,
    /// 更改弹窗「当前主密码」框。
    pub change_current: String,
    /// 更改弹窗「新主密码」框。
    pub change_new: String,
    /// 更改弹窗「确认新主密码」框。
    pub change_new_confirm: String,
    /// 更改弹窗「我已牢记」勾选。
    pub change_memorized: bool,
    /// 更改弹窗错误提示。
    pub change_error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    /// 构造初始（空闲）状态：对应 `App::new` 中原本的 `mpw_*` 初始化。
    pub fn new() -> Self {
        Self {
            input: String::new(),
            confirm: String::new(),
            memorized: false,
            error: None,
            stage: MpwStage::Idle,
            setup: false,
            change_open: false,
            change_current: String::new(),
            change_new: String::new(),
            change_new_confirm: String::new(),
            change_memorized: false,
            change_error: None,
        }
    }

    /// 模块更新：只改自身 `State`；需要父层配合的事以 [`Event`] 经 `Task` 上行。
    ///
    /// `ctx` 为父层传入的只读上下文（当前 store / vault / sessions / 记住开关），
    /// 模块据此计算但**绝不写父状态**；写回一律经对应的 [`Event`] 由父层落地。
    pub fn update(&mut self, msg: Message, ctx: &Ctx) -> Task<Event> {
        match msg {
            Message::Noop => Task::none(),
            Message::Input(v) => {
                self.input = v;
                self.error = None;
                Task::none()
            }
            Message::Confirm(v) => {
                self.confirm = v;
                self.error = None;
                Task::none()
            }
            Message::Memorized(b) => {
                self.memorized = b;
                Task::none()
            }
            Message::Submit => submit(self, ctx),
            Message::Cancel => {
                self.setup = false;
                clear_fields(self);
                Task::none()
            }
            Message::SetupOpen => {
                self.setup = true;
                self.input = String::new();
                self.confirm = String::new();
                self.memorized = false;
                self.error = None;
                Task::none()
            }
            Message::SetupDerive(v) => setup_derive(self, v, ctx),
            Message::SetupRekey(res) => setup_rekey(self, res, ctx),
            Message::ChangeOpen => {
                self.change_open = true;
                self.change_current = String::new();
                self.change_new = String::new();
                self.change_new_confirm = String::new();
                self.change_memorized = false;
                self.change_error = None;
                Task::none()
            }
            Message::ChangeCurrent(v) => {
                self.change_current = v;
                self.change_error = None;
                Task::none()
            }
            Message::ChangeNew(v) => {
                self.change_new = v;
                self.change_error = None;
                Task::none()
            }
            Message::ChangeConfirm(v) => {
                self.change_new_confirm = v;
                self.change_error = None;
                Task::none()
            }
            Message::ChangeMemorized(b) => {
                self.change_memorized = b;
                Task::none()
            }
            Message::ChangeCancel => {
                self.change_open = false;
                self.change_current = String::new();
                self.change_new = String::new();
                self.change_new_confirm = String::new();
                self.change_memorized = false;
                self.change_error = None;
                Task::none()
            }
            Message::ChangeSubmit => change_submit(self, ctx),
            Message::ChangeDerive(res) => change_derive(self, res, ctx),
            Message::ChangeRekey(res) => change_rekey(self, res, ctx),
            Message::Disable => disable(self, ctx),
            Message::RememberToggled(v) => Task::done(Event::SetRemember(v)),
        }
    }
}

/// 模块内部消息：UI 意图与模块自处理的异步结果。
///
/// 父层经 `Message::MasterPw` 路由进来，模块 `update` 自行消费，不外泄。
#[derive(Clone)]
pub enum Message {
    /// 无操作占位消息（用于禁用态按钮点击）。
    Noop,
    /// 主密码输入框内容变更（设置 / 解锁共用）。
    Input(String),
    /// 「再次输入」框内容变更。
    Confirm(String),
    /// 「我已牢记主密码」勾选状态变更。
    Memorized(bool),
    /// 提交主密码（设置：创建保险库；解锁：派生主密钥）。
    Submit,
    /// 取消「设置主密码」弹窗（仅设置模式下可用）。
    Cancel,
    /// 打开「设置主密码」流程（模式 0 → 1）：复用设置弹窗（输入新口令×2）。
    SetupOpen,
    /// 设置模式：Argon2id 密钥派生完成。
    SetupDerive(Arc<Vault>),
    /// 设置模式：凭据重加密 + 落盘完成。
    SetupRekey(Result<(Arc<Vault>, Vec<SessionConfig>), String>),
    /// 打开「更改主密码」弹窗。
    ChangeOpen,
    /// 「更改主密码」弹窗中「当前主密码」框内容变更。
    ChangeCurrent(String),
    /// 「更改主密码」弹窗中「新主密码」框内容变更。
    ChangeNew(String),
    /// 「更改主密码」弹窗中「确认新主密码」框内容变更。
    ChangeConfirm(String),
    /// 「更改主密码」弹窗中「我已牢记主密码」勾选状态变更。
    ChangeMemorized(bool),
    /// 取消「更改主密码」弹窗。
    ChangeCancel,
    /// 提交「更改主密码」：校验旧口令并用新口令重新加密全部凭据。
    ChangeSubmit,
    /// 更改主密码：校验旧口令并派生新密钥完成。
    ChangeDerive(Result<(Arc<Vault>, Arc<Vault>), String>),
    /// 更改主密码：凭据重加密 + 落盘完成。
    ChangeRekey(Result<(Arc<Vault>, Vec<SessionConfig>), String>),
    /// 关闭主密码（模式 1 → 模式 0）：重新用随机密钥加密全部凭据。
    Disable,
    /// 「本机记住主密码」开关切换（自动解锁用系统钥匙串）。
    RememberToggled(bool),
}

/// 上行事件：仅通知父层，由父层 `Message::MasterPwEvent` 分支修改父状态。
///
/// 模块绝不写父状态；`Emit` 用于两阶段异步自回路（见 [`State::update`]）。
#[derive(Clone)]
pub enum Event {
    /// 请求父层写入新保险库（`app.vault`）。
    SetVault(Arc<Vault>),
    /// 请求父层写入重加密后的会话列表（`app.sessions`）。
    SetSessions(Vec<SessionConfig>),
    /// 请求父层切换「本机记住主密码」开关并落盘配置。
    SetRemember(bool),
    /// 请求父层弹出 toast 通知（携带类型与文案）。
    Toast(ToastKind, String),
    /// 自回路：把一条模块内部消息经父层派发回 `State::update`。
    Emit(Box<Message>),
}

/// 父层只读上下文：模块计算所需，但不写回（写回经 [`Event`]）。
pub struct Ctx {
    /// 会话存储（落盘加密头 / 凭据用）。
    pub store: Option<SessionStore>,
    /// 当前保险库（rekey / 关闭时作为「旧 vault」解密凭据）。
    pub vault: Option<Arc<Vault>>,
    /// 当前会话列表（rekey 用）。
    pub sessions: Vec<SessionConfig>,
    /// 「本机记住主密码」开关（派生完成后是否把 DEK 存入钥匙串）。
    pub remember: bool,
}

/// 提交主密码：按当前模式创建或解锁保险库。
fn submit(state: &mut State, ctx: &Ctx) -> Task<Event> {
    if state.setup {
        submit_setup(state, ctx)
    } else {
        submit_unlock(state, ctx)
    }
}

/// 设置模式（模式 0 → 模式 1）：先即时校验，再把「Argon2id 派生密钥 → 重加密凭据 + 落盘」
/// 两段重活依次丢到阻塞线程，期间通过 `stage` 在按钮上展示进度，避免界面卡死观感。
///
/// 设计为两阶段：阶段一仅做密钥派生（最耗时），阶段二做全量重加密与写盘；每阶段结束以一个
/// `Event::Emit` 回到模块推进，从而让 iced 在等待期间继续刷新 UI（按钮显示「正在派生密钥…」等）。
fn submit_setup(state: &mut State, ctx: &Ctx) -> Task<Event> {
    // 异步流程进行中忽略重复提交，避免重入。
    if state.stage != MpwStage::Idle {
        return Task::none();
    }
    if state.input.is_empty() {
        state.error = Some(t!("masterpw.empty"));
        return Task::none();
    }
    if state.input != state.confirm {
        state.error = Some(t!("masterpw.mismatch"));
        return Task::none();
    }
    if !state.memorized {
        state.error = Some(t!("masterpw.not_memorized"));
        return Task::none();
    }
    let Some(_store) = ctx.store.clone() else {
        state.error = Some(t!("masterpw.no_store"));
        return Task::none();
    };
    let new_pass = state.input.clone();
    // 进入派生阶段：按钮显示「正在派生密钥…」。
    state.stage = MpwStage::Deriving;
    Task::perform(
        async move { spawn_blocking(move || Vault::new(&new_pass)).await.unwrap() },
        |v| Event::Emit(Box::new(Message::SetupDerive(Arc::new(v)))),
    )
}

/// 阶段一完成：密钥已派生，进入重加密 + 落盘阶段（按钮显示「正在重新加密凭证…」）。
fn setup_derive(state: &mut State, new_vault: Arc<Vault>, ctx: &Ctx) -> Task<Event> {
    state.stage = MpwStage::Reencrypting;
    let header = new_vault.header().clone();
    let old_vault = ctx.vault.clone();
    let sessions = ctx.sessions.clone();
    let Some(store) = ctx.store.clone() else {
        state.stage = MpwStage::Idle;
        state.error = Some(t!("masterpw.no_store"));
        return Task::none();
    };
    Task::perform(
        async move {
            spawn_blocking(move || {
                let new_sessions: Vec<SessionConfig> = sessions
                    .iter()
                    .map(|cfg| match old_vault.as_deref() {
                        Some(old) => rekey_session(cfg, old, &new_vault),
                        None => cfg.clone(),
                    })
                    .collect();
                store
                    .save(&new_sessions, &header)
                    .map(|_| (new_vault, new_sessions))
                    .map_err(|e| format!("failed to write encrypted file: {e}"))
            })
            .await
            .unwrap()
        },
        |r| Event::Emit(Box::new(Message::SetupRekey(r))),
    )
}

/// 阶段二完成：写入保险库与重加密后的会话，回到空闲态，并上行 `SetVault` / `SetSessions`
/// 由父层落地（模块本身不写 `app.vault` / `app.sessions`）。
fn setup_rekey(
    state: &mut State,
    res: Result<(Arc<Vault>, Vec<SessionConfig>), String>,
    ctx: &Ctx,
) -> Task<Event> {
    match res {
        Ok((new_vault, new_sessions)) => {
            // 设置已完成，离开设置模式（否则内部状态仍为「首次设置」，
            // 若保险库被清空会错误地重新进入设置流程并覆盖已有加密头）。
            state.setup = false;
            // 若开启「本机记住主密码」，把派生出的 DEK 存入系统钥匙串，供下次启动自动解锁。
            if ctx.remember {
                crate::vault_keyring::store_dek_quietly(&new_vault.dek_bytes());
            }
            state.stage = MpwStage::Idle;
            clear_fields(state);
            Task::batch(vec![
                Task::done(Event::SetVault(new_vault)),
                Task::done(Event::SetSessions(new_sessions)),
            ])
        }
        Err(e) => {
            state.stage = MpwStage::Idle;
            state.error = Some(e);
            Task::none()
        }
    }
}

/// 解锁模式：读取加密头，用主密码派生主密钥并校验哨兵。
fn submit_unlock(state: &mut State, ctx: &Ctx) -> Task<Event> {
    let Some(store) = ctx.store.clone() else {
        state.error = Some(t!("masterpw.no_store"));
        return Task::none();
    };
    let header = match store.load_crypto_header() {
        Ok(h) => h,
        Err(e) => {
            state.error = Some(t!("masterpw.read_failed", err => e));
            return Task::none();
        }
    };
    match Vault::unlock(&state.input, &header) {
        Ok(vault) => {
            let vault = Arc::new(vault);
            // 若开启「本机记住主密码」，缓存 DEK 供下次启动自动解锁。
            if ctx.remember {
                crate::vault_keyring::store_dek_quietly(&vault.dek_bytes());
            }
            clear_fields(state);
            Task::done(Event::SetVault(vault))
        }
        Err(_) => {
            state.error = Some(t!("masterpw.wrong"));
            Task::none()
        }
    }
}

/// 清空主密码弹窗的输入与提示（解锁 / 设置成功后调用，避免明文残留内存）。
fn clear_fields(state: &mut State) {
    state.input = String::new();
    state.confirm = String::new();
    state.memorized = false;
    state.error = None;
}

/// 提交「更改主密码」：先即时校验，再把「校验旧口令 + 派生新密钥 → 重加密凭据 + 落盘」
/// 两段重活依次丢到阻塞线程（见 [`change_derive`] / [`change_rekey`]），期间通过 `stage`
/// 在按钮上展示进度。
///
/// 设计为两阶段：阶段一仅做 Argon2id（校验旧口令 + 派生新密钥，最耗时），阶段二做全量重加密
/// 与写盘；每阶段结束以一个 `Event::Emit` 回到模块推进，从而让 iced 在等待期间继续刷新 UI
/// （按钮显示「正在派生密钥…」等），避免界面卡死观感。
fn change_submit(state: &mut State, ctx: &Ctx) -> Task<Event> {
    // 异步流程进行中忽略重复提交，避免重入。
    if state.stage != MpwStage::Idle {
        return Task::none();
    }
    if state.change_current.is_empty() || state.change_new.is_empty() {
        state.change_error = Some(t!("masterpw.empty"));
        return Task::none();
    }
    if state.change_new != state.change_new_confirm {
        state.change_error = Some(t!("masterpw.mismatch"));
        return Task::none();
    }
    if !state.change_memorized {
        state.change_error = Some(t!("masterpw.not_memorized"));
        return Task::none();
    }
    if ctx.vault.is_none() {
        state.change_error = Some(t!("masterpw.no_store"));
        return Task::none();
    }
    if ctx.store.is_none() {
        state.change_error = Some(t!("masterpw.no_store"));
        return Task::none();
    }

    let header = ctx.vault.as_ref().unwrap().header().clone();
    let current = state.change_current.clone();
    let new = state.change_new.clone();
    // 当前口令是否正确的判定发生在派生线程内，预先取好本地化文案带入（派生线程默认英文 locale）。
    let wrong_msg = t!("masterpw.wrong").to_string();
    // 进入派生阶段：按钮显示「正在派生密钥…」。
    state.stage = MpwStage::Deriving;
    Task::perform(
        async move {
            spawn_blocking(move || {
                // 校验当前口令（错误则不改任何凭据，直接回错）。
                let old_vault = match Vault::unlock(&current, &header) {
                    Ok(v) => v,
                    Err(_) => return Err(wrong_msg),
                };
                // 用新口令派生新保险库（另一次 Argon2id）。
                let new_vault = Vault::new(&new);
                Ok((old_vault, new_vault))
            })
            .await
            .unwrap()
        },
        |r| {
            Event::Emit(Box::new(Message::ChangeDerive(
                r.map(|(o, n)| (Arc::new(o), Arc::new(n))),
            )))
        },
    )
}

/// 阶段一完成：旧口令已校验、新旧密钥已派生，进入重加密 + 落盘阶段（按钮显示「正在重新加密凭证…」）。
fn change_derive(
    state: &mut State,
    res: Result<(Arc<Vault>, Arc<Vault>), String>,
    ctx: &Ctx,
) -> Task<Event> {
    let (old_vault, new_vault) = match res {
        Ok(pair) => pair,
        Err(e) => {
            // 旧口令错误：保留弹窗与输入，仅提示，不改动凭据 / 加密头。
            state.stage = MpwStage::Idle;
            state.change_error = Some(e);
            return Task::none();
        }
    };
    state.stage = MpwStage::Reencrypting;
    let sessions = ctx.sessions.clone();
    let Some(store) = ctx.store.clone() else {
        state.stage = MpwStage::Idle;
        state.change_error = Some(t!("masterpw.no_store"));
        return Task::none();
    };
    Task::perform(
        async move {
            spawn_blocking(move || {
                // 逐会话用新 DEK 重新加密凭据（无凭据的会话原样保留）。
                let new_sessions: Vec<SessionConfig> = sessions
                    .iter()
                    .map(|cfg| rekey_session(cfg, &old_vault, &new_vault))
                    .collect();
                store
                    .save(&new_sessions, new_vault.header())
                    .map(|_| (new_vault, new_sessions))
                    .map_err(|e| format!("failed to write encrypted file: {e}"))
            })
            .await
            .unwrap()
        },
        |r| Event::Emit(Box::new(Message::ChangeRekey(r))),
    )
}

/// 阶段二完成：写入保险库与重加密后的会话、关闭弹窗、给出成功提示，回到空闲态。
///
/// 经 `SetVault` / `SetSessions` / `Toast` 三个上行事件由父层落地（模块不写 `app`）。
fn change_rekey(
    state: &mut State,
    res: Result<(Arc<Vault>, Vec<SessionConfig>), String>,
    ctx: &Ctx,
) -> Task<Event> {
    match res {
        Ok((new_vault, new_sessions)) => {
            // 若开启「本机记住主密码」，用新 DEK 覆盖钥匙串条目（盐已变化，旧条目失效）。
            if ctx.remember {
                crate::vault_keyring::store_dek_quietly(&new_vault.dek_bytes());
            }
            // 落盘成功：关闭弹窗、清空输入，避免明文残留内存。
            state.change_open = false;
            state.change_current = String::new();
            state.change_new = String::new();
            state.change_new_confirm = String::new();
            state.change_memorized = false;
            state.change_error = None;
            state.stage = MpwStage::Idle;
            Task::batch(vec![
                Task::done(Event::SetVault(new_vault)),
                Task::done(Event::SetSessions(new_sessions)),
                Task::done(Event::Toast(ToastKind::Success, t!("masterpw.changed"))),
            ])
        }
        Err(e) => {
            state.stage = MpwStage::Idle;
            state.change_error = Some(e);
            Task::none()
        }
    }
}

/// 关闭主密码（模式 1 → 模式 0）：重新生成随机密钥，把全部凭据从口令派生保险库
/// 重加密到随机密钥保险库，写回 `master_password_set = false` 头；钥匙串中的 DEK 被新随机密钥覆盖。
///
/// 关闭后凭据仅本机可解密，同步将不可用（见 [`crate::app::App::can_enable_sync`]）。
fn disable(state: &mut State, ctx: &Ctx) -> Task<Event> {
    let Some(old_vault) = ctx.vault.clone() else {
        state.error = Some(t!("masterpw.no_store"));
        return Task::none();
    };
    let new_vault = Vault::create_random();
    let header = new_vault.header().clone();
    let new_sessions: Vec<SessionConfig> = ctx
        .sessions
        .iter()
        .map(|cfg| rekey_session(cfg, &old_vault, &new_vault))
        .collect();

    let Some(store) = ctx.store.clone() else {
        error!("session store unavailable, cannot write encrypted header");
        state.error = Some(t!("masterpw.no_store"));
        return Task::none();
    };
    if let Err(e) = store.save(&new_sessions, &header) {
        error!("failed to write file when disabling master password: {e}");
        state.error = Some(t!("masterpw.write_failed", err => e));
        return Task::none();
    }
    // 新随机密钥覆盖钥匙串原条目（原口令派生 DEK 随之丢弃）。
    crate::vault_keyring::store_dek_quietly(&new_vault.dek_bytes());
    Task::batch(vec![
        Task::done(Event::SetVault(Arc::new(new_vault))),
        Task::done(Event::SetSessions(new_sessions)),
        Task::done(Event::Toast(ToastKind::Success, t!("masterpw.disabled"))),
    ])
}

/// 用新保险库的主密钥重新加密单个会话的凭据，旧保险库用于先解密。
///
/// 仅 `Some` 信封参与重加密；`None`（未设置凭据，如仅导入连接配置）保持 `None`。
/// 无凭据的 `Agent` 认证原样返回。
fn rekey_session(cfg: &SessionConfig, old_vault: &Vault, new_vault: &Vault) -> SessionConfig {
    let auth = match &cfg.auth {
        AuthMethod::Password { password } => {
            let new_env = password
                .as_ref()
                .and_then(|env| match old_vault.decrypt(env) {
                    Ok(plain) => Some(new_vault.encrypt(plain.as_str())),
                    Err(e) => {
                        warn!(
                            "Failed to reencrypt password credential (session {}): {e}",
                            cfg.id
                        );
                        None
                    }
                });
            AuthMethod::Password { password: new_env }
        }
        AuthMethod::PublicKey {
            key_path,
            passphrase,
        } => {
            let new_env = passphrase
                .as_ref()
                .and_then(|env| match old_vault.decrypt(env) {
                    Ok(plain) => Some(new_vault.encrypt(plain.as_str())),
                    Err(e) => {
                        warn!(
                            "Failed to reencrypt public key passphrase (session {}): {e}",
                            cfg.id
                        );
                        None
                    }
                });
            AuthMethod::PublicKey {
                key_path: key_path.clone(),
                passphrase: new_env,
            }
        }
        AuthMethod::Agent => cfg.auth.clone(),
    };
    SessionConfig {
        auth,
        ..cfg.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::contexts;
    use futures::StreamExt;
    use std::sync::Mutex;

    /// 串行化涉及「真实文件系统（XDG 配置目录）」的测试。
    ///
    /// 这些测试都会改写进程级环境变量 `XDG_CONFIG_HOME` 并读写 `sessions.toml`，
    /// 若并行执行会相互覆盖会话文件，导致「重启」后读到的加密头与设置时的不一致。
    /// 用一把模块级互斥锁保证同一时刻只有一个此类测试在跑。
    static XDG_LOCK: Mutex<()> = Mutex::new(());

    /// 在临时 tokio 运行时里把 `Task<Event>` 跑完并收集产出的事件。
    ///
    /// `Task::perform(...)` 内部用 `spawn_blocking` 跑 Argon2id 等重活，必须在真正的
    /// tokio 运行时上下文里 `block_on`，否则 `spawn_blocking` 会因「无 runtime」而 panic。
    /// 异步主密码流程被拆成多阶段，每阶段产出一个含 `Event::Emit` 的事件驱动下一阶段，
    /// 故测试需手动收集并逐一投递这些事件以走完整个状态机。
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

    /// 驱动一次 `masterpw::Message`：跑 `update` 并把产出的 [`Event`] **像父层一样**应用到 `app`
    /// （`SetVault` / `SetSessions` / `SetRemember` / `Toast` 落地到 App），返回遇到的
    /// `Emit` 包裹的内层消息（两阶段异步自回路的下一步），无则 `None`。
    fn step(app: &mut crate::app::App, msg: Message) -> Option<Message> {
        let ctx = contexts::masterpw_ctx(app);
        let events = run_events(app.masterpw.update(msg, &ctx));
        let mut emit = None;
        for e in events {
            match e {
                Event::SetVault(v) => app.vault = Some(v),
                Event::SetSessions(s) => app.session.sessions = s,
                Event::SetRemember(v) => {
                    app.config.remember_master_key = v;
                    contexts::save_config(app);
                }
                Event::Toast(kind, msg) => contexts::set_toast(app, kind, msg),
                Event::Emit(m) => emit = Some(*m),
            }
        }
        emit
    }

    /// 走完「设置主密码」异步两阶段：派生 -> 重加密落盘。
    fn drive_setup(app: &mut crate::app::App) {
        let m = step(app, Message::Submit).expect("设置阶段一应 Emit SetupDerive");
        let m = step(app, m).expect("设置阶段二应 Emit SetupRekey");
        let _ = step(app, m);
    }

    /// 走完「更改主密码」异步两阶段：校验旧口令 + 派生 -> 重加密落盘。
    ///
    /// 旧口令错误时阶段一直接回错，派生处理器会写入错误并停滞（不再进入重加密阶段），
    /// 故此处需把派生结果交给处理器、再视是否产出阶段二消息决定是否继续。
    fn drive_change(app: &mut crate::app::App) {
        let m = step(app, Message::ChangeSubmit).expect("更改阶段一应 Emit ChangeDerive");
        let Some(m) = step(app, m) else {
            return;
        };
        let _ = step(app, m);
    }

    /// 在临时 XDG 配置目录下隔离测试，避免触碰真实 `~/.config/rterm`。
    ///
    /// 同时把钥匙串读写切到隔离账户（`RTERM_TEST_KEYRING_ACCOUNT`），避免测试删写的
    /// DEK 污染开发者本机真实钥匙串条目（`rterm`/`vault-dek`）——否则跑完测试后
    /// 真实环境的自动解锁会失效、甚至被卡在「解锁」弹窗。
    fn use_temp_config_home() {
        let tmp = std::env::temp_dir().join(format!("rterm_mpw_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        unsafe { std::env::set_var("RTERM_TEST_KEYRING_ACCOUNT", "rterm-test-vault-dek") };
        // 清空可能已存在的会话文件，确保「首次运行」设置模式。
        let _ = std::fs::remove_file(tmp.join("rterm").join("sessions.toml"));
    }

    /// 首次运行（无加密头）应进入**模式 0**：随机密钥保险库就绪、`setup=false`、
    /// 无设置 / 解锁弹窗，且随机密钥已写入系统钥匙串、加密头落盘 `master_password_set=false`。
    #[test]
    fn mode0_startup_creates_random_key_and_self_unlocks() {
        use rterm_config::SessionStore;

        let _guard = XDG_LOCK.lock().unwrap();
        use_temp_config_home();
        if crate::vault_keyring::available() {
            crate::vault_keyring::delete_dek_quietly();
        }

        let (app, _task) = crate::app::App::new();
        // 首次运行应为模式 0：保险库就绪、非设置流程、无弹窗条件。
        assert!(app.vault.is_some(), "首次运行应生成随机密钥保险库");
        assert!(!app.masterpw.setup, "首次运行不应进入设置模式");
        assert!(
            !app.vault.as_ref().unwrap().header().master_password_set,
            "首次运行应为模式 0（master_password_set=false）"
        );

        // 模式 0 随机密钥应写入系统钥匙串，供本机自动解锁。
        if crate::vault_keyring::available() {
            assert!(
                crate::vault_keyring::load_dek().ok().flatten().is_some(),
                "模式 0 随机密钥应写入系统钥匙串"
            );
        }

        // 加密头须落盘，且为模式 0。
        let header = SessionStore::new()
            .expect("store")
            .load_crypto_header()
            .expect("应存在加密头");
        assert!(!header.master_password_set, "落盘头应为模式 0");

        if crate::vault_keyring::available() {
            crate::vault_keyring::delete_dek_quietly();
        }
    }

    /// **模式 0 → 模式 1**：用户从设置面板「设置主密码」，全部凭据重加密到口令派生保险库，
    /// 落盘 `master_password_set=true`；新口令可解锁，旧随机密钥随之失效。
    #[test]
    fn setup_mode0_to_mode1_persists_and_round_trips() {
        use rterm_config::SessionStore;

        let _guard = XDG_LOCK.lock().unwrap();
        use_temp_config_home();
        if crate::vault_keyring::available() {
            crate::vault_keyring::delete_dek_quietly();
        }

        let (mut app, _task) = crate::app::App::new();
        // 首次运行是模式 0：保险库就绪、非设置模式。
        assert!(app.vault.is_some());
        assert!(!app.vault.as_ref().unwrap().header().master_password_set);
        // 记下模式 0 的随机 DEK，用于验证设置后失效。
        let old_random_dek = app.vault.as_ref().unwrap().dek_bytes();

        // 用户从设置面板发起「设置主密码」。
        app.masterpw.setup = true;
        assert!(app.masterpw.setup, "应进入设置流程");
        app.masterpw.input = "hunter2-master".to_string();
        app.masterpw.confirm = "hunter2-master".to_string();
        app.masterpw.memorized = true;
        drive_setup(&mut app);

        // 设置成功：保险库就绪、退出设置流程、切换为模式 1。
        assert!(app.vault.is_some(), "设置成功后保险库应就绪");
        assert!(!app.masterpw.setup, "设置成功后应退出设置流程");
        assert!(
            app.vault.as_ref().unwrap().header().master_password_set,
            "设置后应切换为模式 1（master_password_set=true）"
        );

        // 加密头必须落盘，否则「主密码无法保存」——下次启动会回到设置模式。
        let path = app
            .session
            .store
            .as_ref()
            .expect("store 应可用")
            .path()
            .clone();
        assert!(path.exists(), "加密文件头未落盘，主密码实际未被保存");

        if crate::vault_keyring::available() {
            crate::vault_keyring::delete_dek_quietly();
        }

        // 模拟重启：用同一主密码解锁，校验 salt 一致；错误口令不应解锁。
        let header = SessionStore::new()
            .expect("store")
            .load_crypto_header()
            .expect("应能读取加密头");
        let reloaded = Vault::unlock("hunter2-master", &header).expect("重启后主密码应能解锁");
        assert_eq!(
            reloaded.header().salt,
            app.vault.as_ref().unwrap().header().salt
        );
        assert!(
            Vault::unlock("wrong-pass", &header).is_err(),
            "错误口令不应解锁模式 1 头"
        );
        // 旧随机密钥无法解锁模式 1 头（DEK 已改由口令派生）。
        assert!(
            Vault::from_dek(&old_random_dek, &header).is_err(),
            "旧随机密钥不应再能解锁模式 1 头"
        );
    }

    /// 存储不可用（store 为 `None`）时，设置提交必须报错并保留设置弹窗，不得静默“成功”。
    #[test]
    fn setup_without_store_reports_error_instead_of_silent_skip() {
        // 隔离钥匙串，避免删写污染开发者真实条目。
        unsafe { std::env::set_var("RTERM_TEST_KEYRING_ACCOUNT", "rterm-test-vault-dek") };
        // 构造一个 store 为 None 的 App 状态，验证提交不会静默“成功”
        // （此前会在内存建立 vault、关闭弹窗，却什么都不落盘）。
        let mut app = crate::app::App {
            session: crate::app::session::State {
                store: None,
                ..crate::app::App::new().0.session.clone()
            },
            ..crate::app::App::new().0
        };
        app.masterpw.setup = true;
        app.masterpw.input = "pw".to_string();
        app.masterpw.confirm = "pw".to_string();
        app.masterpw.memorized = true;

        let _ = step(&mut app, Message::Submit);

        // 存储不可用时必须报错并停留在设置流程，不得假装成功。
        assert!(
            app.masterpw.error.is_some(),
            "存储不可用时应给出错误提示而非静默跳过"
        );
        assert!(app.masterpw.setup, "存储不可用时应停留在设置流程");

        if crate::vault_keyring::available() {
            crate::vault_keyring::delete_dek_quietly();
        }
    }

    /// **模式 1 更改主密码**：校验当前口令、用新口令派生新 DEK，并重新加密全部会话凭据；
    /// 新口令可解锁、旧口令失效。
    #[test]
    fn change_master_password_rekeys_all_credentials() {
        use rterm_config::{AuthMethod, SessionConfig};

        let _guard = XDG_LOCK.lock().unwrap();
        use_temp_config_home();
        if crate::vault_keyring::available() {
            crate::vault_keyring::delete_dek_quietly();
        }

        let (mut app, _task) = crate::app::App::new();
        // 先完成模式 0 → 模式 1 设置，建立口令保险库。
        app.masterpw.setup = true;
        app.masterpw.input = "old-master".to_string();
        app.masterpw.confirm = "old-master".to_string();
        app.masterpw.memorized = true;
        drive_setup(&mut app);
        let old_vault = app.vault.clone().expect("设置后保险库应就绪");
        let old_salt = old_vault.header().salt.clone();

        // 用旧保险库加密一个会话密码，写入该会话。
        let secret = "topsecret".to_string();
        let env = old_vault.encrypt(&secret);
        let session = SessionConfig {
            id: rterm_config::new_id(),
            name: "demo".into(),
            host: "example.com".into(),
            port: 22,
            username: "root".into(),
            auth: AuthMethod::Password {
                password: Some(env),
            },
            group: None,
        };
        app.session.sessions.push(session);

        // 提交「更改主密码」：当前=old，新=new。
        app.masterpw.change_current = "old-master".to_string();
        app.masterpw.change_new = "new-master".to_string();
        app.masterpw.change_new_confirm = "new-master".to_string();
        app.masterpw.change_memorized = true;
        drive_change(&mut app);

        // 应成功：无错误、弹窗关闭、出现成功提示、内存保险库已切换。
        assert!(app.masterpw.change_error.is_none(), "更改主密码不应报错");
        assert!(!app.masterpw.change_open, "更改成功后应关闭弹窗");
        assert!(!app.toaster.is_empty(), "更改成功应给出提示");
        let new_vault = app.vault.clone().expect("更改后保险库应就绪");
        assert_ne!(
            new_vault.header().salt,
            old_salt,
            "更改主密码后盐值必须变化"
        );

        // 新保险库能解密已重新加密的凭据。
        let new_env = match &app.session.sessions[0].auth {
            AuthMethod::Password { password } => password.clone().expect("凭据应仍在"),
            _ => panic!("认证方式不应改变"),
        };
        let decrypted = new_vault.decrypt(&new_env).expect("新保险库应能解密");
        assert_eq!(decrypted.as_str(), secret, "重新加密后明文应保持一致");

        // 旧保险库无法解密（DEK 已随主密码改变）——这是「旧口令失效」的核心回归点。
        assert!(
            old_vault.decrypt(&new_env).is_err(),
            "旧保险库不应再能解密重新加密后的凭据"
        );

        // 清理：本测试会向本机钥匙串写入（更新后的）DEK，移除避免污染开发者真实钥匙串。
        if crate::vault_keyring::available() {
            crate::vault_keyring::delete_dek_quietly();
        }
    }

    /// 更改主密码时若当前口令错误，必须拒绝且不应改动任何凭据 / 加密头。
    #[test]
    fn change_master_password_rejects_wrong_current() {
        let _guard = XDG_LOCK.lock().unwrap();
        use_temp_config_home();
        if crate::vault_keyring::available() {
            crate::vault_keyring::delete_dek_quietly();
        }

        let (mut app, _task) = crate::app::App::new();
        // 先建立模式 1 保险库。
        app.masterpw.setup = true;
        app.masterpw.input = "old-master".to_string();
        app.masterpw.confirm = "old-master".to_string();
        app.masterpw.memorized = true;
        drive_setup(&mut app);
        let before_salt = app.vault.as_ref().unwrap().header().salt.clone();

        // 打开「更改主密码」弹窗，再用错误当前口令尝试更改。
        app.masterpw.change_open = true;
        app.masterpw.change_current = "WRONG".to_string();
        app.masterpw.change_new = "new-master".to_string();
        app.masterpw.change_new_confirm = "new-master".to_string();
        app.masterpw.change_memorized = true;
        drive_change(&mut app);

        assert!(
            app.masterpw.change_error.is_some(),
            "错误当前口令应被拒绝并给出提示"
        );
        assert!(app.masterpw.change_open, "拒绝后应停留在更改弹窗");
        assert_eq!(
            app.vault.as_ref().unwrap().header().salt,
            before_salt,
            "拒绝后加密头盐值不应变化"
        );

        if crate::vault_keyring::available() {
            crate::vault_keyring::delete_dek_quietly();
        }
    }

    /// 本机自动解锁依赖系统钥匙串：模式 0 与模式 1（开启记住）均经钥匙串自动解锁；
    /// 关闭「本机记住」后回退到主密码弹窗（vault 为 `None`）。
    #[test]
    fn auto_unlock_uses_keyring_when_available() {
        // 仅当本机钥匙串后端可用时才有意义；无后端（如部分 CI）直接跳过，避免误报。
        // 钥匙串为系统级共享存储，测试前后清理条目，降低与其他测试的相互影响。
        if !crate::vault_keyring::available() {
            eprintln!("Skipping auto_unlock: no keyring backend available");
            return;
        }
        crate::vault_keyring::delete_dek_quietly();

        let _guard = XDG_LOCK.lock().unwrap();
        use_temp_config_home();

        // 首次运行模式 0：应自动解锁（vault 就绪），无需弹窗。
        let (app, _task) = crate::app::App::new();
        assert!(app.vault.is_some(), "模式 0 应自动解锁");
        assert!(!app.masterpw.setup, "模式 0 不应弹设置框");
        assert!(
            crate::vault_keyring::load_dek().ok().flatten().is_some(),
            "模式 0 钥匙串应有随机密钥"
        );

        // 设置主密码 → 模式 1，默认开启记住 → DEK 写入钥匙串。
        let (mut app, _task) = crate::app::App::new();
        app.masterpw.setup = true;
        app.masterpw.input = "launch-master".to_string();
        app.masterpw.confirm = "launch-master".to_string();
        app.masterpw.memorized = true;
        drive_setup(&mut app);
        assert!(app.vault.is_some(), "设置成功后保险库应就绪");
        assert!(
            crate::vault_keyring::load_dek().ok().flatten().is_some(),
            "模式 1 开启记住后钥匙串应有缓存 DEK"
        );

        // 模拟重启：经钥匙串自动解锁，不再弹窗（vault 已就绪）。
        let (app2, _task2) = crate::app::App::new();
        assert!(
            app2.vault.is_some(),
            "重启后应经钥匙串自动解锁，无需输入主密码"
        );
        assert!(!app2.masterpw.setup, "自动解锁成功后不应停留在设置模式");

        // 关闭开关→删除钥匙串条目；再次重启应回退到弹窗（vault 为 None）。
        app.config.remember_master_key = false;
        contexts::save_config(&mut app);
        crate::vault_keyring::delete_dek_quietly();
        let (app3, _task3) = crate::app::App::new();
        assert!(
            app3.vault.is_none(),
            "关闭记住主密码后重启应回退到主密码弹窗"
        );

        crate::vault_keyring::delete_dek_quietly();
    }
}
