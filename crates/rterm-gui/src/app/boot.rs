//! 应用启动装配：加载会话存储、配置与保险库，构建初始 `App` 与启动任务。

use crate::app::App;
use crate::app::contexts;
use crate::app::{hostkey, masterpw, panes, session, settings, sftp, tabs, transfer, updates};
use crate::message::Message;
use crate::state::{CenterView, ToastKind};
use crate::t;
use crate::vault_keyring;
use crate::widget::toast::toaster;
use iced::Task;
use log::{debug, error};
use rterm_config::{AppConfig, SessionStore};
use rterm_crypto::Vault;
use std::sync::Arc;

/// 应用启动：加载会话存储与配置，返回初始状态与任务。
pub(crate) fn new() -> (App, Task<Message>) {
    let store = SessionStore::new()
        .map_err(|e| error!("failed to initialize session store: {e}"))
        .ok();
    // 读取加密文件头（含模式标志），判断首启动 / 解锁 / 自动解锁。
    let header = store.as_ref().and_then(|s| s.load_crypto_header().ok());

    // 加载应用级偏好配置；失败则回退到默认值（path 为空，后续保存会被跳过并记录日志）。
    let config = AppConfig::new()
        .map_err(|e| error!("failed to initialize app config: {e}"))
        .unwrap_or_default();

    // 解析首启动 / 自动解锁，得到初始保险库与会话列表：
    // - 无文件头（首次运行）：生成随机密钥（模式 0）存入钥匙串并落盘，零弹窗、零配置。
    // - 有文件头：尝试用钥匙串 DEK 静默解锁（模式 0 跳过哨兵校验；模式 1 校验且受
    //   `remember_master_key` 控制）。失败时模式 1 回退到解锁弹窗，模式 0 视为异常。
    let mut keyring_warning = false;
    let (initial_vault, sessions) = match &header {
        None => {
            let vault = Vault::create_random();
            vault_keyring::store_dek_quietly(&vault.dek_bytes());
            if let Some(store) = store.as_ref()
                && let Err(e) = store.save(&[], vault.header())
            {
                error!("failed to write initial encrypted header: {e}");
            }
            debug!("first run: generated random key (mode 0), stored in system keyring");
            (Some(vault), Vec::new())
        }
        Some(h) => {
            let sessions = store
                .as_ref()
                .and_then(|s| s.load().ok())
                .unwrap_or_default();
            let try_keyring = if h.master_password_set {
                config.remember_master_key
            } else {
                true
            };
            let mut vault = if try_keyring {
                vault_keyring::load_dek()
                    .ok()
                    .flatten()
                    .and_then(|dek| Vault::from_dek(&dek, h).ok())
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
            (vault, sessions)
        }
    };

    // 两栏 `pane_grid` 布局态交由 `app::panes` 模块构建（初始左栏固定像素宽度 320，
    // 比例按 320 / (1144 - 活动栏宽) 设置，与 iced 窗口默认宽度一致）。
    let panes = panes::State::new();

    // 设置弹窗模块状态须在 `config` move 进结构体之前构建，因其读取 `config.ui_font` /
    // `config.terminal_font` 构建下拉框选项。
    let settings = settings::State::new(&config);

    let mut app = App {
        session: session::State::new(store, sessions),
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
        status: None,
        toaster: toaster(),
        hostkey: hostkey::State::default(),
        updates: updates::State::new(),
        vault: initial_vault.map(Arc::new),
        masterpw: masterpw::State::new(),
    };

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

    (app, check_task)
}
