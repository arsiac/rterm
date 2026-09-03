//! rterm GUI 层（基于 iced）。
//!
//! 本 crate 实现三栏式 SSH 客户端界面：左侧活动栏切换中心面板（会话管理 / 文件管理），
//! 中心为列表或文件浏览器，右侧为终端区域，中心与右栏之间可拖拽分隔。

pub mod activity_bar;
pub mod app;
pub mod font;
pub mod host_key_dialog;
pub mod i18n;
pub mod icons;
pub mod layout;
pub mod masterpw_change_dialog;
pub mod masterpw_dialog;
pub mod message;
pub mod session_panel;
pub mod settings_dialog;
pub mod sftp_dialogs;
pub mod sftp_panel;
pub mod state;
pub mod terminal_pane;
pub mod terminal_theme;
pub mod theme;
pub mod transfer_panel;
pub mod ui;
pub mod update_check;
pub mod vault_keyring;
pub mod widget;

pub use app::App;

use iced::Result;
use iced::font::Font;
use rterm_config::AppConfig;

// 编译期嵌入翻译资源；缺失键回退到 en。
rust_i18n::i18n!("locales", fallback = "en");

/// 国际化文本宏：调用 `rust_i18n::t!` 并立即转为 `String`。
///
/// 因本 crate 的状态字段与 iced 文本接口需要所有权字符串，统一在此取自有字符串，
/// 避免每个调用点都 `.to_string()`。
#[macro_export]
macro_rules! t {
    ($($all:tt)*) => {{
        rust_i18n::t!($($all)*).to_string()
    }};
}

/// 应用入口：初始化配置、字体与窗口图标，启动 iced 应用。
pub fn run() -> Result {
    let config = AppConfig::new().ok();
    // 按持久化的语言偏好（默认跟随系统）设定全局 locale，供 `t!` 取用。
    if let Some(c) = &config {
        rust_i18n::set_locale(c.language.as_locale());
    }

    let font = config
        .and_then(|c| {
            if c.ui_font.is_empty() {
                None
            } else {
                Some(crate::font::resolve_font(&c.ui_font))
            }
        })
        .unwrap_or(Font::DEFAULT);

    let window_icon = iced::window::icon::from_file_data(crate::icons::WINDOW_ICON, None).ok();

    iced::application(app::App::new, app::App::update, app::App::view)
        .subscription(app::App::subscription)
        .theme(app_theme)
        .default_font(font)
        .title(app_title)
        .window(iced::window::Settings {
            size: iced::Size::new(1144.0, 768.0),
            icon: window_icon,
            platform_specific: platform_specific_settings(),
            ..Default::default()
        })
        .run()
}

/// 平台相关窗口设置：Linux 下设定 `application_id`，使窗口管理器按应用归类并关联 `.desktop`。
#[cfg(target_os = "linux")]
fn platform_specific_settings() -> iced::window::settings::PlatformSpecific {
    iced::window::settings::PlatformSpecific {
        application_id: "rterm".into(),
        ..Default::default()
    }
}

/// 构造非 Linux 平台的窗口平台特定设置（Linux 走 XDG Portal，无需此项）。
#[cfg(not(target_os = "linux"))]
fn platform_specific_settings() -> iced::window::settings::PlatformSpecific {
    iced::window::settings::PlatformSpecific::default()
}

/// 应用主题回调。
///
/// 依据应用配置中的主题显示名解析出 iced 内置主题；切换会触发整窗重绘。
/// 兼容旧版配置值 `"dark"` / `"light"`，其余按显示名在可用主题表中查找。
fn app_theme(state: &app::App) -> iced::Theme {
    crate::theme::resolve_theme(&state.config.theme)
}

/// 应用窗口标题回调，固定返回 `rterm`。
fn app_title(_state: &app::App) -> String {
    "rterm".to_string()
}
