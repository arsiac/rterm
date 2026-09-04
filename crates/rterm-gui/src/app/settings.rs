//! 应用设置弹窗模块

use iced::Task;
use iced::widget::combo_box;
use log::error;
use rterm_config::{AppConfig, Language, LogLevel};

/// 设置弹窗的分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsCategory {
    /// 通用：连接超时、日志级别、界面语言。
    General,
    /// 主密码：设置 / 更改 / 关闭主口令、本机记住开关。
    MasterPassword,
    /// 外观：程序主题、界面字体、终端配色与字号。
    Appearance,
    /// 更新：自动检查、版本对比、前往下载。
    Updates,
    /// 关于：版本与简要信息。
    About,
}

/// 模块状态：设置弹窗的 UI 私有字段（原散落在 `App` 上的 `show_settings` /
/// `settings_category` / `ui_font_combo` / `terminal_font_combo`）。
#[derive(Clone)]
pub struct State {
    /// 设置弹窗是否显示。
    pub show_settings: bool,
    /// 设置弹窗当前选中的分类。
    pub category: SettingsCategory,
    /// 界面字体下拉框（`combo_box`）的可搜索状态，选项为「系统默认 + 已安装字体」。
    pub ui_font_combo: combo_box::State<String>,
    /// 终端字体下拉框状态，选项为「系统默认 + 已安装的等宽字体」。
    pub terminal_font_combo: combo_box::State<String>,
}

impl State {
    /// 用初始配置（含界面 / 终端字体选择）构造模块状态。
    pub fn new(config: &AppConfig) -> Self {
        Self {
            show_settings: false,
            category: SettingsCategory::General,
            ui_font_combo: combo_box::State::new(crate::font::ui_font_options(&config.ui_font)),
            terminal_font_combo: combo_box::State::new(crate::font::terminal_font_options(
                &config.terminal_font,
            )),
        }
    }
}

/// 模块内部消息：设置弹窗的 UI 意图。
///
/// 父层经 `Message::Settings` 路由进来，模块 `update` 自行消费，不外泄。
#[derive(Clone)]
pub enum Message {
    /// 切换设置弹窗的显示 / 隐藏。
    Toggle,
    /// 切换设置弹窗中的当前分类（携带目标分类）。
    CategorySelected(SettingsCategory),
    /// 修改“连接超时”设置（携带输入框最新文本，解析失败则忽略）。
    ConnectTimeout(String),
    /// 修改“历史缓冲行数”设置（携带输入框最新文本，解析失败则忽略）。
    Scrollback(String),
    /// 修改“终端字号”设置（携带滑块最新值）。
    FontSize(f32),
    /// 修改“程序主题”设置（携带主题标识，如 `dark` / `light`）。
    Theme(String),
    /// 修改“程序界面字体”设置（携带字体族名称，重启后生效）。
    UiFont(String),
    /// 修改“终端字体”设置（携带等宽字体族名称，即时作用于所有已打开的终端标签）。
    TerminalFont(String),
    /// 修改“终端配色主题”设置（携带预设名，即时作用于所有已打开的终端标签）。
    TerminalTheme(String),
    /// 修改“日志级别”设置（携带所选级别，仅持久化，重启后生效）。
    LogLevel(LogLevel),
    /// 修改“界面语言”设置（携带所选语言，即时生效并持久化）。
    Language(Language),
    /// 在文件管理器中打开日志所在目录（与「日志级别」设置并列，便于查看 / 导出日志）。
    OpenLogFolder,
    /// 修改「自动检查更新」设置（携带开关状态，即时持久化）。
    AutoCheckUpdates(bool),
}

/// 上行事件：仅通知父层，由父层 `Message::SettingsEvent` 分支修改父状态并落盘。
///
/// 模块绝不写父状态；配置值一律经对应事件由父层写入 `AppConfig` 并 `save_config()`。
#[derive(Clone)]
pub enum Event {
    /// 写回“连接超时”配置（携带解析后的秒数）。
    ConnectTimeout(u64),
    /// 写回“历史缓冲行数”配置（携带解析后的行数）。
    Scrollback(usize),
    /// 写回“终端字号”配置（携带滑块值），并热替换到所有已打开的终端标签。
    FontSize(f32),
    /// 写回“程序主题”配置（携带主题标识）。
    Theme(String),
    /// 写回“界面字体”配置（携带字体族名称）。
    UiFont(String),
    /// 写回“终端字体”配置（携带等宽字体族名称），并热替换到所有已打开的终端标签。
    TerminalFont(String),
    /// 写回“终端配色主题”配置（携带预设名），并热替换到所有已打开的终端标签。
    TerminalTheme(String),
    /// 写回“日志级别”配置（携带所选级别）。
    LogLevel(LogLevel),
    /// 写回“界面语言”配置（携带所选语言）。
    Language(Language),
    /// 打开日志目录（纯副作用，无需写状态，模块内已完成）。
    OpenLogFolder,
    /// 写回“自动检查更新”配置（携带开关状态）。
    AutoCheckUpdates(bool),
}

/// 父层只读上下文：当前 `AppConfig`，供模块构建下拉框选项等读取，不写回。
pub struct Ctx {
    /// 当前应用配置（读取用，写回经 [`Event`]）。
    pub config: AppConfig,
}

impl State {
    /// 模块更新：只改自身 `State`；需要父层配合的事以 [`Event`] 经 `Task` 上行。
    ///
    /// `ctx` 为父层传入的只读上下文（当前 `AppConfig`），模块据此构建下拉框选项等，
    /// 但**绝不写父状态**；写回一律经对应 [`Event`] 由父层落地。
    pub fn update(&mut self, msg: Message, ctx: &Ctx) -> Task<Event> {
        match msg {
            Message::Toggle => {
                self.show_settings = !self.show_settings;
                Task::none()
            }
            Message::CategorySelected(category) => {
                self.category = category;
                Task::none()
            }
            // 0 表示不限制超时；仅可解析为非负整数时才上行写回。
            Message::ConnectTimeout(text) => match text.parse::<u64>() {
                Ok(timeout) => Task::done(Event::ConnectTimeout(timeout)),
                Err(_) => Task::none(),
            },
            // 仅可解析为非负整数时才上行写回；0 表示不保留历史。
            Message::Scrollback(text) => match text.parse::<usize>() {
                Ok(scrollback) => Task::done(Event::Scrollback(scrollback)),
                Err(_) => Task::none(),
            },
            Message::FontSize(size) => Task::done(Event::FontSize(size)),
            Message::Theme(theme) => Task::done(Event::Theme(theme)),
            Message::UiFont(text) => Task::done(Event::UiFont(text)),
            Message::TerminalFont(name) => Task::done(Event::TerminalFont(name)),
            Message::TerminalTheme(name) => Task::done(Event::TerminalTheme(name)),
            Message::LogLevel(level) => Task::done(Event::LogLevel(level)),
            Message::Language(lang) => {
                // 重建两个字体下拉框：选项含翻译后的「系统默认」标签，须按新 locale 重建，
                // 否则列表仍为旧语言标签，用户选中后 `map_default_font` 无法识别。
                // 选项列表依赖当前已选字体（来自只读 ctx），故在模块内完成重建。
                self.ui_font_combo =
                    combo_box::State::new(crate::font::ui_font_options(&ctx.config.ui_font));
                self.terminal_font_combo = combo_box::State::new(
                    crate::font::terminal_font_options(&ctx.config.terminal_font),
                );
                Task::done(Event::Language(lang))
            }
            Message::OpenLogFolder => {
                // 确保目录存在，避免文件管理器打开空路径失败；失败仅记录，不影响程序。
                let dir = rterm_config::log_dir();
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    error!("failed to create log directory: {e}");
                }
                let mut cmd = match std::env::consts::OS {
                    "windows" => std::process::Command::new("explorer"),
                    "macos" => std::process::Command::new("open"),
                    _ => std::process::Command::new("xdg-open"),
                };
                if let Err(e) = cmd.arg(&dir).spawn() {
                    error!("failed to open log directory: {e}");
                }
                Task::none()
            }
            Message::AutoCheckUpdates(enabled) => Task::done(Event::AutoCheckUpdates(enabled)),
        }
    }
}
