//! 应用级偏好配置（GUI 设置）的加载与保存。
//!
//! 与 [`store`](crate::store) 的会话配置不同，本模块仅持久化 UI 偏好（连接超时、
//! 终端字号、程序主题与界面字体、主密码「本机记住」开关），不涉及任何敏感凭据。
//! 配置以 TOML 格式存放于 `~/.config/rterm/config.toml`。

use crate::ConfigError;
use dirs::config_dir;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::PathBuf;

/// 日志级别（设置中切换，重启生效）。
///
/// 以小写名序列化进 `config.toml`，同时直接作为 flexi_logger 指令串的级别 token
/// （`off`/`error`/`warn`/`info`/`debug`/`trace`），无需额外映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// 关闭日志输出，不记录任何内容。
    Off,
    /// 仅记录错误级别日志。
    Error,
    /// 记录警告及以上级别日志。
    Warn,
    /// 默认级别：仅保留可预期异常与重大事件，降低日常噪声。
    #[default]
    Info,
    /// 记录调试及以上级别日志（含调试细节）。
    Debug,
    /// 记录最详细级别日志（含追踪信息）。
    Trace,
}

impl fmt::Display for LogLevel {
    /// 将日志级别格式化为小写名（与序列化一致）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            LogLevel::Off => "off",
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        };
        f.write_str(s)
    }
}

impl LogLevel {
    /// 设置面板下拉框选项（从最少到最多输出）。
    pub const ALL: [LogLevel; 6] = [
        LogLevel::Off,
        LogLevel::Error,
        LogLevel::Warn,
        LogLevel::Info,
        LogLevel::Debug,
        LogLevel::Trace,
    ];
}

/// 界面语言（设置中切换并持久化，默认跟随系统）。
///
/// 序列化为 rust-i18n 使用的 locale 码（`system`/`zh-CN`/`en`）；
/// `System` 在每次启动解析为具体语言，便于系统区域变化时自动跟随。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    /// 跟随系统区域（跨平台读取系统 locale，含 `zh` 取中文，否则英文）。
    #[default]
    System,
    /// 简体中文（locale 码 `zh-CN`）。
    #[serde(rename = "zh-CN")]
    ZhCn,
    /// 英文（locale 码 `en`）。
    En,
}

impl Language {
    /// 设置面板下拉框选项（跟随系统在最前）。
    pub const ALL: [Language; 3] = [Language::System, Language::ZhCn, Language::En];

    /// 检测系统语言：跨平台读取系统 locale（Windows 上 `LANG` 等环境变量通常为空，
    /// 必须走系统 API），locale 串含 `zh` 取中文，否则英文。
    ///
    /// 依次尝试系统 API → `LC_ALL` → `LANG` → `LANGUAGE`，最后回退英文。
    pub fn detect_system() -> Language {
        let lang = sys_locale::get_locale()
            .or_else(|| std::env::var("LC_ALL").ok())
            .or_else(|| std::env::var("LANG").ok())
            .or_else(|| std::env::var("LANGUAGE").ok())
            .unwrap_or_default();
        if lang.to_lowercase().contains("zh") {
            Language::ZhCn
        } else {
            Language::En
        }
    }

    /// 将 `System` 落地为具体语言（其余原样返回）。
    pub fn resolve(self) -> Language {
        match self {
            Language::System => Self::detect_system(),
            other => other,
        }
    }

    /// 映射为 rust-i18n 的 locale 码：先 [`Self::resolve`] 把 `System` 落地为具体语言。
    pub fn as_locale(self) -> &'static str {
        match self.resolve() {
            Language::ZhCn => "zh-CN",
            Language::En => "en",
            // `resolve()` 已把 `System` 解析掉，此分支不可达，仅为穷尽匹配而保留。
            Language::System => "en",
        }
    }
}

impl fmt::Display for Language {
    /// 将语言格式化为设置面板展示名（如「跟随系统」「简体中文」）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Language::System => "跟随系统",
            Language::ZhCn => "简体中文",
            Language::En => "English",
        };
        f.write_str(s)
    }
}

/// 日志文件所在目录：平台缓存目录下的 `rterm/logs` 子目录（Linux 为 `~/.cache/rterm/logs`）。
///
/// 缓存目录不可定位时回退当前目录，与日志初始化（`main.rs`）保持一致，避免打开失败；
/// 该计算逻辑集中于此，供 `main.rs` 与 GUI 共用，避免散落重复。
pub fn log_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rterm")
        .join("logs")
}

/// 应用级偏好配置。
///
/// 除运行时字段外均可在 GUI 设置弹窗中修改并即时持久化。文件路径在构造时确定并跳过序列化，
/// 因此不写入配置文件；另有 `last_update_check_unix` 等由程序内部写回、不出现在设置界面。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 配置文件绝对路径（运行时持有，不参与序列化）。
    #[serde(skip)]
    path: PathBuf,
    /// 0 表示不限制。
    #[serde(default = "default_timeout")]
    pub connect_timeout: u64,
    /// 终端与界面默认字号（像素）。
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// iced 主题显示名（如 `"Dark"` / `"Light"` / `"Dracula"`）；旧值 `"dark"` / `"light"` 由 GUI 层 rterm_gui::theme 兼容映射。
    #[serde(default = "default_theme")]
    pub theme: String,
    /// 空字符串表示使用 iced 默认字体，重启后生效。
    #[serde(default)]
    pub ui_font: String,
    /// 空字符串表示使用 iced 等宽回退 `Font::MONOSPACE`；仅接受等宽字体以保证字符网格对齐，切换即时作用于所有终端标签。
    #[serde(default)]
    pub terminal_font: String,
    /// 仅持久化名字，具体调色板由 GUI 层 rterm_gui::terminal_theme 解析；切换会即时作用于所有终端标签。
    #[serde(default = "default_terminal_theme")]
    pub terminal_theme: String,
    /// 重启生效；序列化为 flexi_logger 接受的小写名。
    #[serde(default)]
    pub log_level: LogLevel,
    /// 设置面板切换，实时生效；默认跟随系统区域。
    #[serde(default)]
    pub language: Language,
    /// 终端历史缓冲行数（滚动回看上限）；0 表示不保留历史。仅对新建终端标签生效。
    #[serde(default = "default_scrollback")]
    pub scrollback: usize,
    /// 启动时是否自动检查更新。
    #[serde(default = "default_auto_check_updates")]
    pub auto_check_updates: bool,
    /// 用于 24h 节流，避免频繁请求 GitHub API。
    #[serde(default)]
    pub last_update_check_unix: Option<i64>,
    /// 是否在本机记住主密钥（系统钥匙串自动解锁）。
    ///
    /// 无论模式 0 还是模式 1，本开关开启时都会把**当前 DEK**（模式 0 的随机密钥 / 模式 1
    /// 的口令派生密钥）存入系统钥匙串，启动若读到钥匙串 DEK 且校验通过则静默解锁、不弹窗；
    /// 仅模式 1 且关闭此开关时，回到每次启动输入主密码。默认开启。无钥匙串后端时不适用
    /// （视为不支持，GUI 不展示该开关）。
    #[serde(default = "default_true")]
    pub remember_master_key: bool,
    /// 是否在终端连接时向远端 shell 注入 CWD 上报钩子（OSC 7 序列）。
    ///
    /// 开启时每个新终端标签连接后会自动向 shell 注入一段 prompt 钩子，使 shell 在每个
    /// 提示符处输出当前工作目录（`ESC ]7;file://<pwd> BEL`），供 SFTP 面板「进入终端目录」
    /// 按钮使用。关闭后不注入钩子，SFTP 面板中该按钮也将隐藏。默认开启。
    #[serde(default = "default_true")]
    pub cwd_bootstrap: bool,
}

/// 连接超时默认值（秒）：30 秒（0 表示不限制）。
fn default_timeout() -> u64 {
    30
}

/// 终端与界面默认字号（像素）：14.0。
fn default_font_size() -> f32 {
    14.0
}

/// 程序主题默认值：`"Dark"`。
fn default_theme() -> String {
    "Dark".to_string()
}

/// 终端主题默认值：`"Default"`。
fn default_terminal_theme() -> String {
    "Default".to_string()
}

/// 启动时自动检查更新的默认开关：`true`（开启）。
fn default_auto_check_updates() -> bool {
    true
}

/// 布尔字段通用默认值：`true`（用于 `remember_master_key` 等）。
fn default_true() -> bool {
    true
}

/// 终端历史缓冲行数默认值：10000（与 alacritty 默认 `scrolling_history` 一致）。
fn default_scrollback() -> usize {
    10000
}

/// 探测系统当前是否处于深色外观。
///
/// 3.x 在 Linux 上统一走 XDG Desktop Portal 的 `color-scheme`，即 GNOME/KDE 各自的权威来源，
/// 全平台可直接采用而无需按桌面环境特殊处理。无 portal / session bus 或结果不明时回退深色。
fn detect_system_is_dark() -> bool {
    match dark_light::detect() {
        Ok(dark_light::Mode::Dark) => true,
        Ok(dark_light::Mode::Light) => false,
        Ok(dark_light::Mode::Unspecified) => {
            warn!("System theme unspecified, falling back to dark");
            true
        }
        Err(e) => {
            warn!("System theme detection failed ({e}), falling back to dark");
            true
        }
    }
}

/// 按系统外观给出首次启动的默认主题组合。
///
/// 深色系统对应程序主题 `Dark` + 终端 `One Dark`，浅色对应 `Light` + `One Light`；
/// 该组合仅在配置文件缺失（即首次启动）时一次性写入，之后由用户手动设置覆盖。
fn first_launch_theme() -> (String, String) {
    if detect_system_is_dark() {
        ("Dark".to_string(), "One Dark".to_string())
    } else {
        ("Light".to_string(), "One Light".to_string())
    }
}

impl Default for AppConfig {
    /// 构造全部字段取默认值的配置；路径为空，运行时由 `new()` 回填。
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            connect_timeout: default_timeout(),
            font_size: default_font_size(),
            theme: default_theme(),
            ui_font: String::new(),
            terminal_font: String::new(),
            terminal_theme: default_terminal_theme(),
            log_level: LogLevel::default(),
            language: Language::default(),
            scrollback: default_scrollback(),
            auto_check_updates: default_auto_check_updates(),
            last_update_check_unix: None,
            // `remember_master_key` 默认开启（提供免输入体验）：这里走 `default_true()` 而非
            // 直接写字面量，是为了让 serde 的字段缺省值也保持 `true`（bool 默认是 `false`）。
            remember_master_key: default_true(),
            cwd_bootstrap: default_true(),
        }
    }
}

impl AppConfig {
    /// 创建配置实例：定位并准备好配置目录，加载已有文件或回退到默认配置。
    ///
    /// # 错误
    /// 无法定位配置目录时返回 [`ConfigError::ConfigDir`]；创建目录失败、读取或解析
    /// 配置文件失败时返回 [`ConfigError::Store`]。
    pub fn new() -> Result<Self, ConfigError> {
        let base =
            config_dir().ok_or_else(|| ConfigError::ConfigDir("无法定位系统配置目录".into()))?;
        let dir = base.join("rterm");
        fs::create_dir_all(&dir)
            .map_err(|e| ConfigError::Store(format!("创建配置目录失败: {e}")))?;
        let path = dir.join("config.toml");
        debug!("App config file path: {}", path.display());
        if !path.exists() {
            // 首次启动：按系统外观决定默认主题组合，并立即落盘，
            // 使得「配置文件缺失即回退默认」的语义同时完成一次性初始化。
            let (theme, terminal_theme) = first_launch_theme();
            let config = AppConfig {
                path,
                theme,
                terminal_theme,
                ..Default::default()
            };
            // `remember_master_key` 由 `Default::default()` 补为 `true`，与首次启动语义一致。
            if let Err(e) = config.save() {
                warn!("Failed to write back default config on first launch: {e}");
            } else {
                info!(
                    "First launch: app theme {} / terminal theme {} based on system theme",
                    config.theme, config.terminal_theme
                );
            }
            return Ok(config);
        }
        let content = fs::read_to_string(&path)
            .map_err(|e| ConfigError::Store(format!("读取配置文件失败: {e}")))?;
        let mut config: AppConfig = toml::from_str(&content)
            .map_err(|e| ConfigError::Store(format!("解析配置文件失败: {e}")))?;
        // `path` 字段标记了 `#[serde(skip)]`，反序列化不会填充它，必须在此回填，
        // 否则 `save()` 将向空路径写入而失败，导致配置（含主题）无法持久化。
        config.path = path;
        debug!(
            "App config loaded (timeout {}s, font size {})",
            config.connect_timeout, config.font_size
        );
        Ok(config)
    }

    /// 把当前配置写回 `self.path`（序列化为 TOML）。
    ///
    /// # 错误
    /// 当序列化或写入失败时返回 [`ConfigError::Store`]。
    pub fn save(&self) -> Result<(), ConfigError> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::Store(format!("序列化配置失败: {e}")))?;
        fs::write(&self.path, content)
            .map_err(|e| ConfigError::Store(format!("写入配置文件失败: {e}")))?;
        debug!("App config saved to {}", self.path.display());
        Ok(())
    }
}
