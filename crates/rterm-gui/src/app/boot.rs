//! 应用启动装配：加载会话存储、配置与保险库，构建初始 `App` 与启动任务。

use crate::app::App;
use crate::app::contexts;
use crate::app::{
    hostkey, masterpw, panes, paste, remote_edit, search, session, session_io, settings, sftp,
    shortcuts, snippets, tabs, transfer, updates, window,
};
use crate::message::Message;
use crate::state::{CenterView, ToastKind};
use crate::t;
use crate::vault_keyring;
use crate::widget::toast::toaster;
use iced::Task;
use log::{debug, error};
use rterm_config::{AppConfig, SessionStore, SessionsFileProblem, SessionsFileState};
use rterm_crypto::Vault;
use std::sync::Arc;

/// 应用启动：加载会话存储与保险库，使用调用方传入的应用配置，返回初始状态与任务。
/// 会话文件按**三态**处理（见 [`SessionStore::startup_state`]）：不存在才建模式 0 随机密钥并落盘，
/// 可用则解锁；**存在但不可用**时绝不写盘、绝不重建密钥——当首启动处理会用空会话覆盖用户文件。
pub(crate) fn new(config: AppConfig) -> (App, Task<Message>) {
    let store = SessionStore::new()
        .map_err(|e| error!("failed to initialize session store: {e}"))
        .ok();
    // 存储本身不可用（配置目录建不出来）时按首启动走内存态，与既有行为一致。
    let state = store
        .as_ref()
        .map(|s| s.startup_state())
        .unwrap_or(SessionsFileState::Absent);

    // 应用级偏好配置由调用方加载并传入，此处不再重复读盘。

    // 解析三态，得到初始保险库、会话列表与「文件不可用」原因：
    // - `Absent`（首次运行）：生成随机密钥（模式 0）存入钥匙串并落盘，零弹窗、零配置；
    //   `Ready`：用钥匙串 DEK 静默解锁，失败时模式 1 回退解锁弹窗、模式 0 视为异常。
    // - `Unusable`：保险库缺席、会话列表为空、**不写盘**，原因交上层提示并抑制解锁弹窗。
    let mut keyring_warning = false;
    let (initial_vault, sessions, unusable) = match state {
        SessionsFileState::Unusable(problem) => {
            error!("session file is unusable, refusing to overwrite: {problem:?}");
            (None, Vec::new(), Some(problem))
        }
        SessionsFileState::Absent => {
            let vault = Vault::create_random();
            vault_keyring::store_dek_quietly(&vault.dek_bytes());
            if let Some(store) = store.as_ref()
                && let Err(e) = store.save(&[], vault.header())
            {
                error!("failed to write initial encrypted header: {e}");
            }
            debug!("first run: generated random key (mode 0), stored in system keyring");
            (Some(vault), Vec::new(), None)
        }
        SessionsFileState::Ready(h) => {
            let sessions = store
                .as_ref()
                .and_then(|s| s.load().ok())
                .unwrap_or_default();
            let try_keyring = if h.master_password_set {
                config.security.remember_master_key
            } else {
                true
            };
            let mut vault = if try_keyring {
                vault_keyring::load_dek()
                    .ok()
                    .flatten()
                    .and_then(|dek| Vault::from_dek(&dek, &h).ok())
            } else {
                None
            };
            if vault.is_some() {
                debug!("auto-unlocked via system keyring, skipping master password prompt");
            } else if h.master_password_set {
                debug!("no keyring cache or verification failed, will show unlock prompt");
            } else {
                // 模式 0 钥匙串取不到随机密钥（被清空 / 钥匙串异常）。模式 0 本无主密码，
                // 绝不该弹「解锁」框把用户卡死：就地重生本机随机密钥让应用可用，并提示
                // 既有凭据可能失效（旧密钥不可恢复）。
                error!(
                    "mode 0: no random key in keyring, regenerated local key (existing credentials may be invalid)"
                );
                let recovered = Vault::create_random();
                vault_keyring::store_dek_quietly(&recovered.dek_bytes());
                keyring_warning = true;
                vault = Some(recovered);
            }
            (vault, sessions, None)
        }
    };

    // 两栏 `pane_grid` 布局态交由 `app::panes` 模块构建（初始左栏固定像素宽度 320）。
    // 比例按 320 / (恢复后的窗口宽度 - 活动栏宽) 换算：窗口宽度取配置中记录的尺寸，
    // 未启用 / 未记录时回退默认宽度，保证启动首帧分隔比例即正确。
    let initial_window_width = config
        .remembered_size()
        .map(|(w, _)| w)
        .unwrap_or(crate::DEFAULT_WINDOW_WIDTH);
    let panes = panes::State::new(initial_window_width);

    // 设置弹窗模块状态须在 `config` move 进结构体之前构建，因其读取 `config.appearance.ui_font` /
    // `config.terminal.font` 构建下拉框选项。
    let settings = settings::State::new(&config);

    // 会话文件不可用时先出提示文案（`store` 即将被 move 进会话模块）。
    let sessions_path = store
        .as_ref()
        .map(|s| s.path().display().to_string())
        .unwrap_or_default();
    let unusable_hint = unusable
        .as_ref()
        .map(|problem| sessions_file_unusable_text(&sessions_path, problem));

    let mut app = App {
        session: session::State::new(store, sessions),
        session_io: session_io::State::new(),
        active_session: None,
        center: CenterView::Sessions,
        tabs: tabs::State::new(),
        terminal_focused: true,
        window_focus_saved: None,
        panes,
        config,
        sftp: sftp::State::default(),
        transfer: transfer::State::default(),
        settings,
        settings_menu_open: false,
        shortcuts: shortcuts::State::new(),
        paste: paste::State::new(),
        remote_edit: remote_edit::State::new(),
        search: search::State::new(),
        snippets: snippets::State::new(),
        status: None,
        toaster: toaster(),
        hostkey: hostkey::State::default(),
        updates: updates::State::new(),
        vault: initial_vault.map(Arc::new),
        masterpw: masterpw::State::new(),
        sessions_file_problem: unusable,
        window: window::State::new(),
    };

    // 会话文件不可用：必须**常驻**显示在状态栏，toast 只出现 4 秒会被错过，而此时的正确处置
    // （修文件、而不是设主密码）必须一直看得见。
    if let Some(hint) = unusable_hint {
        app.status = Some(hint.clone());
        contexts::set_toast(&mut app, ToastKind::Error, hint);
    }

    // 模式 0 钥匙串缺失后就地重生随机密钥：提示用户既有凭据可能失效。
    if keyring_warning {
        contexts::set_toast(
            &mut app,
            ToastKind::Warning,
            t!("masterpw.keyring_lost").to_string(),
        );
    }

    // 启动自动检查：是否发起（自动检查开关 + 24h 节流）由更新检查模块判定，
    // 时间戳经 `updates::Event::SetLastCheck` 上行，由父层写回配置并落盘。
    let updates_ctx = contexts::updates_ctx(&app);
    let check_task = app
        .updates
        .update(updates::Message::CheckOnStartup, &updates_ctx)
        .map(Message::UpdatesEvent);

    // 启动清理本地编辑缓存：过期日期目录逐会话删、删空后连目录收走（0 = 关闭）。
    // 彼时会话表为空，快照仅作防御性不变量传参（见 sweep 文档）。
    let sweep_task = if app.config.files.edit_cache_days > 0 {
        Task::perform(
            remote_edit::sweep_expired_edit_cache(
                app.config.files.edit_cache_days,
                app.remote_edit.live_session_ids(),
            ),
            |_| Message::Noop,
        )
    } else {
        Task::none()
    };

    (app, Task::batch([check_task, sweep_task]))
}

/// 会话文件不可用（存在但读不出）的提示文案：外层给出文件路径，内层按原因取对应文案。
///
/// 每种原因都要求用户**先修文件**，绝不能提示设主密码——重建保险库会用空会话覆盖掉现有文件。
fn sessions_file_unusable_text(path: &str, problem: &SessionsFileProblem) -> String {
    let reason = match problem {
        SessionsFileProblem::Unreadable(e) => t!("app.sessions_file_unreadable", err => e.clone()),
        SessionsFileProblem::Unparsable(e) => t!("app.sessions_file_unparsable", err => e.clone()),
        SessionsFileProblem::ExportFile => t!("app.sessions_file_is_export"),
        SessionsFileProblem::MissingCryptoHeader => t!("app.sessions_file_no_crypto"),
    };
    t!("app.sessions_file_unusable", path => path.to_string(), reason => reason).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::STATE_ROOT_LOCK as STATE_LOCK;
    use std::fs;
    use std::path::PathBuf;

    /// 把状态根重定向到独占临时目录，并把钥匙串切到隔离账户（避免污染开发者真实钥匙串）。
    fn use_temp_state_root() -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("rterm_boot_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).expect("创建临时状态根");
        rterm_config::paths::set_test_root(Some(tmp.clone()));
        unsafe { std::env::set_var("RTERM_TEST_KEYRING_ACCOUNT", "rterm-test-vault-dek") };
        tmp
    }

    /// 被误放到本机配置位置的**导出件**必须判为不可用：不建保险库、不写盘、不覆盖用户文件。
    ///
    /// 回归锁：把它当首启动处理会先用空会话覆写那份文件。
    #[test]
    fn boot_refuses_to_overwrite_an_export_file_in_place() {
        let _guard = STATE_LOCK.lock().unwrap();
        let tmp = use_temp_state_root();
        let sessions = tmp.join("config").join("sessions.toml");
        fs::create_dir_all(sessions.parent().unwrap()).expect("创建配置目录");
        rterm_config::export_sessions(&sessions, &[]).expect("写入导出件");
        let before = fs::read(&sessions).expect("读导出件");

        let (app, _task) = App::new();

        assert!(app.vault.is_none(), "导出件不可用，不应据它建立任何保险库");
        assert!(
            matches!(
                app.sessions_file_problem,
                Some(SessionsFileProblem::ExportFile)
            ),
            "应把该文件判为「是导出件」"
        );
        assert!(app.session.sessions.is_empty(), "不可用态会话列表应为空");
        assert!(
            app.status.is_some(),
            "不可用原因须常驻状态栏（toast 只有数秒，会被错过）"
        );
        assert_eq!(
            before,
            fs::read(&sessions).expect("重读导出件"),
            "启动绝不改动那份文件"
        );

        let _ = fs::remove_dir_all(&tmp);
        rterm_config::paths::set_test_root(None);
    }

    /// 损坏的会话文件同样不得被当成首启动覆写。
    #[test]
    fn boot_refuses_to_overwrite_an_unparsable_sessions_file() {
        let _guard = STATE_LOCK.lock().unwrap();
        let tmp = use_temp_state_root();
        let sessions = tmp.join("config").join("sessions.toml");
        fs::create_dir_all(sessions.parent().unwrap()).expect("创建配置目录");
        fs::write(&sessions, "sessions = [ this is not valid toml\n").expect("写入坏文件");
        let before = fs::read(&sessions).expect("读坏文件");

        let (app, _task) = App::new();

        assert!(app.vault.is_none(), "坏文件不应建立保险库");
        assert!(
            matches!(
                app.sessions_file_problem,
                Some(SessionsFileProblem::Unparsable(_))
            ),
            "应把该文件判为「无法解析」"
        );
        assert_eq!(
            before,
            fs::read(&sessions).expect("重读坏文件"),
            "启动绝不覆写损坏文件"
        );

        let _ = fs::remove_dir_all(&tmp);
        rterm_config::paths::set_test_root(None);
    }
}
