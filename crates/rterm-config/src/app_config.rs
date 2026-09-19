//! 应用级偏好配置（GUI 设置）的加载与保存。

use crate::ConfigError;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

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
/// 实际解析委托 [`crate::paths::log_dir`]，使开发沙箱下整体改落 `.dev/cache/logs`。
/// 保留此函数作为对外入口，供 `main.rs` 与 GUI 共用，避免调用方感知路径来源变化。
pub fn log_dir() -> PathBuf {
    crate::paths::log_dir()
}

/// `[connection]` 段：连接相关设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    /// 连接超时（秒），0 表示不限制。
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

impl Default for ConnectionConfig {
    /// 超时默认 30 秒。
    fn default() -> Self {
        Self {
            timeout: default_timeout(),
        }
    }
}

/// `[terminal]` 段：终端显示与目录追踪设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalConfig {
    /// 终端字体族名；空字符串表示使用 iced 等宽回退 `Font::MONOSPACE`。
    ///
    /// 仅接受等宽字体以保证字符网格对齐，切换即时作用于所有终端标签。
    #[serde(default)]
    pub font: String,
    /// 终端字号（像素）。
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// 终端配色主题名；具体调色板由 GUI 层 `rterm_gui::terminal_theme` 解析，
    /// 切换会即时作用于所有终端标签。
    #[serde(default = "default_terminal_theme")]
    pub theme: String,
    /// 终端历史缓冲行数（滚动回看上限）；0 表示不保留历史。仅对新建终端标签生效。
    #[serde(default = "default_scrollback")]
    pub scrollback: usize,
    /// 是否在终端连接时向远端 shell 注入 CWD 上报钩子（OSC 7 序列）。
    ///
    /// 开启时每个新终端标签连接后会自动向 shell 注入一段 prompt 钩子，使 shell 在每个
    /// 提示符处输出当前工作目录（`ESC ]7;file://<pwd> BEL`），供 SFTP 面板「进入终端目录」
    /// 按钮使用。关闭后不注入钩子，SFTP 面板中该按钮也将隐藏。默认开启。
    #[serde(default = "default_true")]
    pub cwd_bootstrap: bool,
    /// 注入 CWD 钩子时是否通过 `request_pty` 关闭远端回显（ECHO=0）。
    ///
    /// 开启后 `request_pty` 会传 `ECHO=0`，避免远端 PTY 回显钩子脚本文本。
    /// 仅在 `cwd_bootstrap` 开启时生效。默认开启。
    #[serde(default = "default_true")]
    pub suppress_bootstrap_echo: bool,
    /// 是否在复制时将选中区域各行的尾部空格去除，默认开启。
    #[serde(default = "default_true")]
    pub trim_trailing_whitespace: bool,
}

impl Default for TerminalConfig {
    /// 终端段默认值：默认字号 / 配色 / 10000 行历史，各项开关默认开启。
    fn default() -> Self {
        Self {
            font: String::new(),
            font_size: default_font_size(),
            theme: default_terminal_theme(),
            scrollback: default_scrollback(),
            cwd_bootstrap: default_true(),
            suppress_bootstrap_echo: default_true(),
            trim_trailing_whitespace: default_true(),
        }
    }
}

/// 全局最大并发传输数的下限：1（任何时刻至少允许一个传输在跑）。
pub const MIN_CONCURRENT: usize = 1;

/// 全局最大并发传输数的上限：8。
///
/// 上限存在是为了给远端 `sftp-server` 与磁盘留下余量：并发数过高会把服务端压成排队，
/// 反而降低总吞吐；同时配置被手工改成 `999` 时也需要一个兜底，避免瞬间拉起大批通道。
pub const MAX_CONCURRENT: usize = 8;

/// `[transfer]` 段：文件传输设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferConfig {
    /// 全局最大并发传输数（跨标签、跨方向共享同一份额度）。
    ///
    /// 加载后由 [`TransferConfig::normalize`] 裁剪到 [`MIN_CONCURRENT`]`..=`[`MAX_CONCURRENT`]。
    /// 该值为应用级单值而非每标签一份：左侧传输面板本身就是全局聚合展示，用户的心智是
    /// 「同时最多 N 个文件在动」；按标签分额度会让「3 并发」在开两个标签时变成 6。
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            max_concurrent: default_max_concurrent(),
        }
    }
}

impl TransferConfig {
    /// 把越界值裁剪到合法区间。
    ///
    /// 用户手工编辑 `config.toml` 写成 `0`（永不启动任何传输）或 `999`（瞬间开一堆通道），
    fn normalize(&mut self) {
        self.max_concurrent = self.max_concurrent.clamp(MIN_CONCURRENT, MAX_CONCURRENT);
    }
}

/// `[appearance]` 段：程序外观与界面语言设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceConfig {
    /// iced 主题显示名（如 `"Dark"` / `"Light"` / `"Dracula"`）；
    /// 旧值 `"dark"` / `"light"` 由 GUI 层 `rterm_gui::theme` 兼容映射。
    #[serde(default = "default_theme")]
    pub theme: String,
    /// 界面字体族名；空字符串表示使用 iced 默认字体，重启后生效。
    #[serde(default)]
    pub ui_font: String,
    /// 界面语言；默认跟随系统区域。
    #[serde(default)]
    pub language: Language,
}

impl Default for AppearanceConfig {
    /// 外观段默认值：深色主题、默认界面字体、跟随系统语言。
    fn default() -> Self {
        Self {
            theme: default_theme(),
            ui_font: String::new(),
            language: Language::default(),
        }
    }
}

/// `[logging]` 段：日志设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// 日志级别，重启生效；序列化为 flexi_logger 接受的小写名。
    #[serde(default)]
    pub level: LogLevel,
}

impl Default for LoggingConfig {
    /// 日志段默认值：`info` 级别。
    fn default() -> Self {
        Self {
            level: LogLevel::default(),
        }
    }
}

/// `[updates]` 段：应用更新检查设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatesConfig {
    /// 启动时是否自动检查更新。
    #[serde(default = "default_auto_check_updates")]
    pub auto_check: bool,
    /// 用于 24h 节流，避免频繁请求 GitHub API。
    #[serde(default)]
    pub last_check_unix: Option<i64>,
}

impl Default for UpdatesConfig {
    /// 更新段默认值：自动检查开启、无上次检查时间。
    fn default() -> Self {
        Self {
            auto_check: default_auto_check_updates(),
            last_check_unix: None,
        }
    }
}

/// `[security]` 段：安全相关设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// 是否在本机记住主密钥（系统钥匙串自动解锁）。
    ///
    /// 无论模式 0 还是模式 1，本开关开启时都会把**当前 DEK**（模式 0 的随机密钥 / 模式 1
    /// 的口令派生密钥）存入系统钥匙串，启动若读到钥匙串 DEK 且校验通过则静默解锁、不弹窗；
    /// 仅模式 1 且关闭此开关时，回到每次启动输入主密码。默认开启。无钥匙串后端时不适用
    /// （视为不支持，GUI 不展示该开关）。
    #[serde(default = "default_true")]
    pub remember_master_key: bool,
}

impl Default for SecurityConfig {
    /// 安全段默认值：本机记住主密钥开启。
    fn default() -> Self {
        Self {
            remember_master_key: default_true(),
        }
    }
}

/// `[window]` 段：主窗口几何记忆设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowConfig {
    /// 是否记住窗口大小并在下次启动时恢复。默认开启。
    ///
    /// 记录的是**非最大化**时的窗口尺寸：关闭时若窗口处于最大化状态，则保留上一次
    /// 非最大化的尺寸，不会把最大化后的尺寸写入配置。在设置中关闭该开关时会同时清除
    /// 已保存的尺寸。
    #[serde(default = "default_true")]
    pub remember_size: bool,
    /// 上次退出时窗口的宽度（逻辑像素）。`None` 表示尚未记录或已被用户清除。
    #[serde(default)]
    pub width: Option<f32>,
    /// 上次退出时窗口的高度（逻辑像素）。`None` 表示尚未记录或已被用户清除。
    #[serde(default)]
    pub height: Option<f32>,
}

impl Default for WindowConfig {
    /// 窗口段默认值：记住大小开启、无已保存尺寸。
    fn default() -> Self {
        Self {
            remember_size: default_true(),
            width: None,
            height: None,
        }
    }
}

/// 应用级偏好配置（`config.toml` 根结构）。
///
/// 除运行时字段外均可在 GUI 设置弹窗中修改并即时持久化。各功能域拆分为独立子段
/// （见 [`ConnectionConfig`] / [`TerminalConfig`] / [`TransferConfig`] / [`AppearanceConfig`] /
/// [`LoggingConfig`] / [`UpdatesConfig`] / [`SecurityConfig`] / [`WindowConfig`]）。
/// 文件路径在构造时确定并跳过序列化，因此不写入配置文件；`last_check_unix` 等由程序
/// 内部写回、不出现在设置界面。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 配置文件绝对路径（运行时持有，不参与序列化）。
    #[serde(skip)]
    path: PathBuf,
    /// `[connection]` 段：连接相关设置。
    #[serde(default)]
    pub connection: ConnectionConfig,
    /// `[terminal]` 段：终端显示与目录追踪设置。
    #[serde(default)]
    pub terminal: TerminalConfig,
    /// `[transfer]` 段：文件传输设置。
    #[serde(default)]
    pub transfer: TransferConfig,
    /// `[appearance]` 段：程序外观与界面语言设置。
    #[serde(default)]
    pub appearance: AppearanceConfig,
    /// `[logging]` 段：日志设置。
    #[serde(default)]
    pub logging: LoggingConfig,
    /// `[updates]` 段：应用更新检查设置。
    #[serde(default)]
    pub updates: UpdatesConfig,
    /// `[security]` 段：安全相关设置。
    #[serde(default)]
    pub security: SecurityConfig,
    /// `[window]` 段：主窗口几何记忆设置。
    #[serde(default)]
    pub window: WindowConfig,
}

/// 连接超时默认值（秒）：30 秒（0 表示不限制）。
fn default_timeout() -> u64 {
    30
}

/// 全局最大并发传输数默认值：3。
fn default_max_concurrent() -> usize {
    3
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

/// 按系统外观给出首次启动的默认主题组合 `(程序主题, 终端主题)`。
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
            connection: ConnectionConfig::default(),
            terminal: TerminalConfig::default(),
            transfer: TransferConfig::default(),
            appearance: AppearanceConfig::default(),
            logging: LoggingConfig::default(),
            updates: UpdatesConfig::default(),
            security: SecurityConfig::default(),
            window: WindowConfig::default(),
        }
    }
}

/// 旧版扁平配置（迁移快照）。
///
/// 冻结迁移前的顶层键 schema，仅用于把旧 `config.toml` 读入并转换。**刻意**保持独立，
/// 不随新结构演进而漂移，以免旧文件的字段映射被无意改变。
#[derive(Debug, Deserialize)]
struct LegacyAppConfig {
    /// 旧字段：连接超时（秒）。
    #[serde(default = "default_timeout")]
    connect_timeout: u64,
    /// 旧字段：终端与界面字号（像素）。
    #[serde(default = "default_font_size")]
    font_size: f32,
    /// 旧字段：程序主题显示名。
    #[serde(default = "default_theme")]
    theme: String,
    /// 旧字段：界面字体族名。
    #[serde(default)]
    ui_font: String,
    /// 旧字段：终端字体族名。
    #[serde(default)]
    terminal_font: String,
    /// 旧字段：终端配色主题名。
    #[serde(default = "default_terminal_theme")]
    terminal_theme: String,
    /// 旧字段：日志级别。
    #[serde(default)]
    log_level: LogLevel,
    /// 旧字段：界面语言。
    #[serde(default)]
    language: Language,
    /// 旧字段：终端历史缓冲行数。
    #[serde(default = "default_scrollback")]
    scrollback: usize,
    /// 旧字段：自动检查更新开关。
    #[serde(default = "default_auto_check_updates")]
    auto_check_updates: bool,
    /// 旧字段：上次更新检查时间戳。
    #[serde(default)]
    last_update_check_unix: Option<i64>,
    /// 旧字段：本机记住主密钥开关。
    #[serde(default = "default_true")]
    remember_master_key: bool,
    /// 旧字段：复制去除行尾空格开关。
    #[serde(default = "default_true")]
    trim_trailing_whitespace: bool,
    /// 旧字段：CWD 钩子注入开关。
    #[serde(default = "default_true")]
    cwd_bootstrap: bool,
    /// 旧字段：抑制 CWD 钩子回显开关。
    #[serde(default = "default_true")]
    suppress_bootstrap_echo: bool,
    /// 旧字段：记住窗口大小开关。
    #[serde(default = "default_true")]
    remember_window_size: bool,
    /// 旧字段：上次窗口宽度（逻辑像素）。
    #[serde(default)]
    window_width: Option<f32>,
    /// 旧字段：上次窗口高度（逻辑像素）。
    #[serde(default)]
    window_height: Option<f32>,
}

impl From<LegacyAppConfig> for AppConfig {
    /// 把旧扁平配置逐字段映射到新分组结构。
    fn from(l: LegacyAppConfig) -> Self {
        AppConfig {
            path: PathBuf::new(),
            connection: ConnectionConfig {
                timeout: l.connect_timeout,
            },
            terminal: TerminalConfig {
                font: l.terminal_font,
                font_size: l.font_size,
                theme: l.terminal_theme,
                scrollback: l.scrollback,
                cwd_bootstrap: l.cwd_bootstrap,
                suppress_bootstrap_echo: l.suppress_bootstrap_echo,
                trim_trailing_whitespace: l.trim_trailing_whitespace,
            },
            // 旧版没有传输段：一律取默认值（并发 3）。
            transfer: TransferConfig::default(),
            appearance: AppearanceConfig {
                theme: l.theme,
                ui_font: l.ui_font,
                language: l.language,
            },
            logging: LoggingConfig { level: l.log_level },
            updates: UpdatesConfig {
                auto_check: l.auto_check_updates,
                last_check_unix: l.last_update_check_unix,
            },
            security: SecurityConfig {
                remember_master_key: l.remember_master_key,
            },
            window: WindowConfig {
                remember_size: l.remember_window_size,
                width: l.window_width,
                height: l.window_height,
            },
        }
    }
}

/// 旧扁平格式的顶层键名（任一存在即判定为旧格式）。
const LEGACY_KEYS: [&str; 18] = [
    "connect_timeout",
    "font_size",
    "theme",
    "ui_font",
    "terminal_font",
    "terminal_theme",
    "log_level",
    "language",
    "scrollback",
    "auto_check_updates",
    "last_update_check_unix",
    "remember_master_key",
    "trim_trailing_whitespace",
    "cwd_bootstrap",
    "suppress_bootstrap_echo",
    "remember_window_size",
    "window_width",
    "window_height",
];

/// 判断 TOML 根表是否为旧扁平格式：根层出现任一 [`LEGACY_KEYS`] 键即视为旧格式。
///
/// 新格式的根键只有各分段名（`connection` 等），与旧键不重叠，故可无歧义区分。
fn is_legacy(table: &toml::Table) -> bool {
    LEGACY_KEYS.iter().any(|k| table.contains_key(*k))
}

/// 解析配置文本，返回配置及「是否由旧扁平格式迁移而来」。
///
/// 旧格式走 [`LegacyAppConfig`] 转换；新格式直接反序列化（缺失段 / 字段补默认）。
///
/// # 错误
/// TOML 解析失败时返回 [`ConfigError::Store`]。
fn parse_config(content: &str) -> Result<(AppConfig, bool), ConfigError> {
    let table: toml::Table = toml::from_str(content)
        .map_err(|e| ConfigError::Store(format!("解析配置文件失败: {e}")))?;
    if is_legacy(&table) {
        let legacy: LegacyAppConfig = toml::from_str(content)
            .map_err(|e| ConfigError::Store(format!("解析旧版配置文件失败: {e}")))?;
        Ok((AppConfig::from(legacy), true))
    } else {
        let config: AppConfig = toml::from_str(content)
            .map_err(|e| ConfigError::Store(format!("解析配置文件失败: {e}")))?;
        Ok((config, false))
    }
}

/// 计算迁移备份路径：首选 `config.toml.bak`，已存在时追加 Unix 秒时间戳避免覆盖旧备份。
fn backup_path(path: &Path) -> PathBuf {
    let file = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config.toml");
    let backup = path.with_file_name(format!("{file}.bak"));
    if backup.exists() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        backup.with_file_name(format!("{file}.bak.{ts}"))
    } else {
        backup
    }
}

impl AppConfig {
    /// 创建配置实例：定位并准备好配置目录，加载已有文件或回退到默认配置。
    ///
    /// 目录由 [`crate::paths::config_dir`] 解析（开发沙箱下为 `.dev/config`）。
    /// 首次启动（文件缺失）按系统外观写入默认主题并落盘新格式；检测到旧扁平格式时
    /// 转换、备份旧文件并落盘新格式，实现透明迁移。
    ///
    /// # 错误
    /// 无法定位配置目录时返回 [`ConfigError::ConfigDir`]；创建目录失败、读取或解析
    /// 配置文件失败时返回 [`ConfigError::Store`]。
    pub fn new() -> Result<Self, ConfigError> {
        let dir = crate::paths::config_dir()
            .ok_or_else(|| ConfigError::ConfigDir("无法定位系统配置目录".into()))?;
        fs::create_dir_all(&dir)
            .map_err(|e| ConfigError::Store(format!("创建配置目录失败: {e}")))?;
        let path = dir.join("config.toml");
        debug!("App config file path: {}", path.display());
        if !path.exists() {
            // 首次启动：按系统外观决定默认主题组合，并立即落盘，
            // 使得「配置文件缺失即回退默认」的语义同时完成一次性初始化。
            let (theme, terminal_theme) = first_launch_theme();
            let mut config = AppConfig {
                path,
                ..Default::default()
            };
            config.appearance.theme = theme;
            config.terminal.theme = terminal_theme;
            // `security.remember_master_key` 由默认值补为 `true`，与首次启动语义一致。
            if let Err(e) = config.save() {
                warn!("Failed to write back default config on first launch: {e}");
            } else {
                info!(
                    "First launch: app theme {} / terminal theme {} based on system theme",
                    config.appearance.theme, config.terminal.theme
                );
            }
            return Ok(config);
        }
        let content = fs::read_to_string(&path)
            .map_err(|e| ConfigError::Store(format!("读取配置文件失败: {e}")))?;
        let (mut config, migrated) = parse_config(&content)?;
        // `path` 字段标记了 `#[serde(skip)]`，反序列化不会填充它，必须在此回填，
        // 否则 `save()` 将向空路径写入而失败，导致配置（含主题）无法持久化。
        config.path = path.clone();
        // 用户手改配置可能写出越界并发数（0 / 999）：解析后立刻裁剪，
        // 使内存值与磁盘值在本次落盘后一致（迁移分支紧随其后，会一并写入裁剪结果）。
        config.transfer.normalize();
        if migrated {
            // 旧扁平格式：先备份旧文件，再落盘新分组格式。备份 / 写入失败均不致命——
            // 旧文件仍在磁盘上，下次启动会再次尝试迁移；本次以内存中的转换结果继续运行。
            let backup = backup_path(&path);
            match fs::copy(&path, &backup) {
                Ok(_) => info!(
                    "Migrated legacy flat config to sectioned format, backup at {}",
                    backup.display()
                ),
                Err(e) => warn!(
                    "Failed to back up legacy config to {}: {e}",
                    backup.display()
                ),
            }
            if let Err(e) = config.save() {
                warn!("Failed to write migrated config: {e}");
            }
        }
        debug!(
            "App config loaded (timeout {}s, terminal font size {})",
            config.connection.timeout, config.terminal.font_size
        );
        Ok(config)
    }

    /// 返回下次启动应恢复的窗口尺寸 `(宽, 高)`。
    ///
    /// 当「记住窗口大小」关闭、字段缺失或数值非法（非正 / 非有限）时返回 `None`，
    /// 由调用方回退到内置默认尺寸。
    pub fn remembered_size(&self) -> Option<(f32, f32)> {
        if !self.window.remember_size {
            return None;
        }
        match (self.window.width, self.window.height) {
            (Some(w), Some(h)) if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 => {
                Some((w, h))
            }
            _ => None,
        }
    }

    /// 清除已保存的窗口尺寸（不影响当前窗口，仅使下次启动回到默认尺寸）。
    pub fn clear_window_size(&mut self) {
        self.window.width = None;
        self.window.height = None;
    }

    /// 把当前配置写回 `self.path`（序列化为分组 TOML）。
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 串行化会改写全局状态根（`paths::set_test_root`）的测试。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn remembered_size_requires_valid_positive_values() {
        // 默认无记录 -> 回退默认尺寸。
        assert_eq!(AppConfig::default().remembered_size(), None);
        // 记录有效尺寸后返回该值。
        let mut config = AppConfig::default();
        config.window.width = Some(1280.0);
        config.window.height = Some(720.0);
        assert_eq!(config.remembered_size(), Some((1280.0, 720.0)));
        // 非法值（非正 / 非有限）视为无记录。
        config.window.width = Some(0.0);
        assert_eq!(config.remembered_size(), None);
        config.window.width = Some(f32::NAN);
        config.window.height = Some(720.0);
        assert_eq!(config.remembered_size(), None);
    }

    #[test]
    fn remembered_size_respects_toggle() {
        let mut config = AppConfig::default();
        config.window.width = Some(1280.0);
        config.window.height = Some(720.0);
        config.window.remember_size = false;
        // 关闭开关时不恢复尺寸，但已记录值仍在，重新开启后可用。
        assert_eq!(config.remembered_size(), None);
        config.window.remember_size = true;
        assert_eq!(config.remembered_size(), Some((1280.0, 720.0)));
    }

    #[test]
    fn clear_window_size_erases_record() {
        let mut config = AppConfig::default();
        config.window.width = Some(1280.0);
        config.window.height = Some(720.0);
        config.clear_window_size();
        assert_eq!(config.window.width, None);
        assert_eq!(config.window.height, None);
        assert_eq!(config.remembered_size(), None);
    }

    #[test]
    fn legacy_flat_config_is_migrated_field_by_field() {
        let legacy = r#"
connect_timeout = 45
font_size = 18.0
theme = "Dracula"
ui_font = "Noto Sans"
terminal_font = "JetBrains Mono"
terminal_theme = "One Dark"
log_level = "debug"
language = "zh-CN"
scrollback = 5000
auto_check_updates = false
last_update_check_unix = 1700000000
remember_master_key = false
trim_trailing_whitespace = false
cwd_bootstrap = false
suppress_bootstrap_echo = false
remember_window_size = false
window_width = 1280.0
window_height = 720.0
"#;
        let (config, migrated) = parse_config(legacy).expect("解析旧配置应成功");
        assert!(migrated, "旧扁平格式应被识别为迁移");
        assert_eq!(config.connection.timeout, 45);
        assert_eq!(config.terminal.font, "JetBrains Mono");
        assert_eq!(config.terminal.font_size, 18.0);
        assert_eq!(config.terminal.theme, "One Dark");
        assert_eq!(config.terminal.scrollback, 5000);
        assert!(!config.terminal.cwd_bootstrap);
        assert!(!config.terminal.suppress_bootstrap_echo);
        assert!(!config.terminal.trim_trailing_whitespace);
        assert_eq!(config.appearance.theme, "Dracula");
        assert_eq!(config.appearance.ui_font, "Noto Sans");
        assert_eq!(config.appearance.language, Language::ZhCn);
        assert_eq!(config.logging.level, LogLevel::Debug);
        assert!(!config.updates.auto_check);
        assert_eq!(config.updates.last_check_unix, Some(1_700_000_000));
        assert!(!config.security.remember_master_key);
        assert!(!config.window.remember_size);
        assert_eq!(config.window.width, Some(1280.0));
        assert_eq!(config.window.height, Some(720.0));
        // 旧格式无传输段：迁移后取默认并发数。
        assert_eq!(config.transfer.max_concurrent, 3);
    }

    #[test]
    fn sectioned_config_parses_without_migration_and_fills_defaults() {
        let sectioned = r#"
[connection]
timeout = 12

[terminal]
font = "Fira Code"
font_size = 20.0

[appearance]
theme = "Light"
"#;
        let (config, migrated) = parse_config(sectioned).expect("解析新配置应成功");
        assert!(!migrated, "新分组格式不应触发迁移");
        assert_eq!(config.connection.timeout, 12);
        assert_eq!(config.terminal.font, "Fira Code");
        assert_eq!(config.terminal.font_size, 20.0);
        assert_eq!(config.appearance.theme, "Light");
        // 未给出的段 / 字段回退默认。
        assert!(config.window.remember_size);
        assert_eq!(config.terminal.scrollback, 10_000);
        assert_eq!(config.appearance.language, Language::System);
    }

    #[test]
    fn empty_config_falls_back_to_defaults() {
        let (config, migrated) = parse_config("").expect("空配置应成功");
        assert!(!migrated);
        assert_eq!(config.connection.timeout, 30);
        assert_eq!(config.terminal.font_size, 14.0);
        assert_eq!(config.terminal.theme, "Default");
        assert_eq!(config.appearance.theme, "Dark");
        assert_eq!(config.transfer.max_concurrent, 3);
    }

    #[test]
    fn out_of_range_transfer_values_are_clamped_on_load() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("rterm_cfg_clamp_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("config")).expect("应能创建临时配置目录");
        let cfg_path = dir.join("config").join("config.toml");

        // 手改为 0（永不启动）与 999（瞬间开一堆通道）都必须在加载时被裁剪。
        crate::paths::set_test_root(Some(dir.clone()));
        for (written, expected) in [(0usize, MIN_CONCURRENT), (999, MAX_CONCURRENT)] {
            fs::write(
                &cfg_path,
                format!("[transfer]\nmax_concurrent = {written}\n"),
            )
            .expect("应能写入越界配置");
            let config = AppConfig::new().expect("加载越界配置应成功");
            assert_eq!(
                config.transfer.max_concurrent, expected,
                "写入 {written} 应被裁剪为 {expected}"
            );
        }
        crate::paths::set_test_root(None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_path_appends_bak_and_timestamps_on_collision() {
        let dir = std::env::temp_dir().join(format!("rterm_cfg_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("应能创建临时目录");
        let path = dir.join("config.toml");
        assert_eq!(backup_path(&path), dir.join("config.toml.bak"));
        // 已有备份时追加时间戳，避免覆盖。
        fs::write(dir.join("config.toml.bak"), "old").expect("应能写旧备份");
        let bp = backup_path(&path);
        let name = bp.file_name().expect("应有文件名").to_string_lossy();
        assert!(
            name.starts_with("config.toml.bak."),
            "已存在备份时应追加时间戳，实际为 {name}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_migrates_legacy_file_and_creates_backup() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("rterm_cfg_migrate_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("config")).expect("应能创建临时配置目录");
        let cfg_path = dir.join("config").join("config.toml");
        let legacy = "connect_timeout = 7\nfont_size = 22.0\nterminal_font = \"Hack\"\n";
        fs::write(&cfg_path, legacy).expect("应能写入旧配置");

        // 状态根重定向到临时目录，覆盖 debug 构建默认的工作区 `.dev`，
        // 避免测试读写开发者真实配置。
        crate::paths::set_test_root(Some(dir.clone()));
        let config = AppConfig::new().expect("加载并迁移旧配置应成功");
        crate::paths::set_test_root(None);

        assert_eq!(config.connection.timeout, 7);
        assert_eq!(config.terminal.font_size, 22.0);
        assert_eq!(config.terminal.font, "Hack");

        // 新文件应已写为分组格式，且旧文件被备份。
        let new_content = fs::read_to_string(&cfg_path).expect("应能读取迁移后的配置");
        assert!(
            new_content.contains("[connection]") && new_content.contains("[terminal]"),
            "应写为分组格式: {new_content}"
        );
        assert!(
            !new_content.contains("connect_timeout"),
            "不应再保留旧扁平键: {new_content}"
        );
        assert!(
            dir.join("config").join("config.toml.bak").exists(),
            "迁移应生成 config.toml.bak 备份"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
