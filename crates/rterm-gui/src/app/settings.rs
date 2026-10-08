//! 应用设置弹窗模块

use iced::Task;
use iced::widget::combo_box;
use log::error;
use rterm_config::{
    AppConfig, Language, LogLevel, MAX_CONCURRENT, MAX_KEEPALIVE_MAX, MAX_RETRY_ATTEMPTS,
    MIN_CONCURRENT,
};
use rterm_core::host_key::KnownHostEntry;

/// 设置弹窗的分类（按用户心智分组，而非按底层 TOML 段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsCategory {
    /// 通用：界面语言、日志、窗口记忆等应用级偏好。
    General,
    /// 连接与传输：连接超时、最大并发传输、失败自动重试。
    Connection,
    /// 终端：配色、字体、字号、滚动缓冲与终端行为。
    Terminal,
    /// 外观：程序主题与界面字体。
    Appearance,
    /// 安全：主密码与本地自动解锁。
    Security,
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
    /// 已信任主机列表缓存：打开设置（停在「安全」分类）或切到该分类时从 known_hosts 重读。
    pub known_hosts: Vec<KnownHostEntry>,
    /// 鼠标悬浮的已知主机条目键（`host:port`），用于渲染行悬浮高亮。
    pub known_hosts_hovered: Option<String>,
    /// known_hosts 读取 / 遗忘失败时的内联错误文案（`None` = 无错误）。
    pub known_hosts_error: Option<String>,
    /// 已知主机过滤词：对 `host:port` 显示串做大小写不敏感子串匹配，空串 = 全量。
    pub known_hosts_filter: String,
}

impl State {
    /// 用初始配置（含界面 / 终端字体选择）构造模块状态。
    pub fn new(config: &AppConfig) -> Self {
        Self {
            show_settings: false,
            category: SettingsCategory::General,
            ui_font_combo: combo_box::State::new(crate::font::ui_font_options(
                &config.appearance.ui_font,
            )),
            terminal_font_combo: combo_box::State::new(crate::font::terminal_font_options(
                &config.terminal.font,
            )),
            known_hosts: Vec::new(),
            known_hosts_hovered: None,
            known_hosts_error: None,
            known_hosts_filter: String::new(),
        }
    }

    /// 从 known_hosts 重读已信任主机列表；失败时保留旧缓存并置内联错误。
    fn reload_known_hosts(&mut self) {
        match rterm_core::host_key::list_known_hosts() {
            Ok(list) => {
                self.known_hosts = list;
                self.known_hosts_error = None;
            }
            Err(e) => self.known_hosts_error = Some(crate::i18n::localize_error(&e)),
        }
    }
}

/// 按过滤词筛选已知主机：对显示串 `host:port` 做大小写不敏感的子串匹配。
///
/// 过滤词先 `trim`，空串返回全量；返回引用切片避免逐帧克隆条目。纯展示过滤，
/// 不触碰磁盘——列表内容以模块重读时的磁盘状态为准。
pub(crate) fn filtered_known_hosts<'a>(
    entries: &'a [KnownHostEntry],
    query: &str,
) -> Vec<&'a KnownHostEntry> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return entries.iter().collect();
    }
    entries
        .iter()
        .filter(|e| {
            format!("{}:{}", e.host, e.port)
                .to_lowercase()
                .contains(&query)
        })
        .collect()
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
    /// 修改“SSH 保活间隔”设置（携带输入框最新文本，解析失败则忽略；0 表示关闭）。
    Keepalive(String),
    /// 修改“SSH 保活判死次数”设置（携带滑块最新值，即时生效但不落盘）。
    KeepaliveMax(f32),
    /// 提交“SSH 保活判死次数”设置（滑块释放时触发，仅落盘）。
    KeepaliveMaxPersist,
    /// 修改“历史缓冲行数”设置（携带输入框最新文本，解析失败则忽略）。
    Scrollback(String),
    /// 修改“最大并发传输数”设置（携带滑块最新值，即时生效但不落盘）。
    MaxConcurrent(f32),
    /// 提交“最大并发传输数”设置（滑块释放时触发，仅落盘，避免拖动过程中反复写文件）。
    MaxConcurrentPersist,
    /// 修改“失败自动重试次数”设置（携带滑块最新值，即时生效但不落盘）。
    RetryAttempts(f32),
    /// 提交“失败自动重试次数”设置（滑块释放时触发，仅落盘）。
    RetryAttemptsPersist,
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
    /// 修改「自动追踪终端目录」设置（携带开关状态，即时持久化）。
    CwdBootstrap(bool),
    /// 修改「注入脚本时抑制终端回显」设置（携带开关状态，即时持久化）。
    SuppressBootstrapEcho(bool),
    /// 修改「复制时去除行尾空格」设置（携带开关状态，即时持久化）。
    TrimTrailingWhitespace(bool),
    /// 修改「响铃视觉提示」设置（携带开关状态，即时持久化；关闭后新响铃不再闪烁标签）。
    Bell(bool),
    /// 修改「记住窗口大小」设置（携带开关状态，即时持久化；关闭时清除已存尺寸）。
    RememberWindowSize(bool),
    /// 遗忘某台已信任主机（携带 host 与 port，删除对应 known_hosts 记录并刷新列表）。
    ForgetKnownHost(String, u16),
    /// 鼠标进入已知主机某条目（携带 `host:port` 键），用于渲染悬浮高亮。
    KnownHostEnter(String),
    /// 鼠标离开已知主机条目，清除悬浮高亮（与文件列表一致，不校验具体是哪一条）。
    KnownHostExit,
    /// 修改已知主机过滤词（携带输入框最新文本；空白等价于不过滤）。
    KnownHostFilterChanged(String),
}

/// 上行事件：仅通知父层，由父层 `Message::SettingsEvent` 分支修改父状态并落盘。
///
/// 模块绝不写父状态；配置值一律经对应事件由父层写入 `AppConfig` 并 `save_config()`。
#[derive(Clone)]
pub enum Event {
    /// 写回“连接超时”配置（携带解析后的秒数）。
    ConnectTimeout(u64),
    /// 写回“SSH 保活间隔”配置（携带解析后的秒数，0 = 关闭；仅对新建连接生效）。
    Keepalive(u64),
    /// 写回“SSH 保活判死次数”配置（携带裁剪后的值；0 = 发保活但不因无应答判死；仅对新建连接生效）。
    KeepaliveMax(u64),
    /// 把当前“SSH 保活判死次数”落盘（滑块释放后调用）。
    KeepaliveMaxPersist,
    /// 写回“历史缓冲行数”配置（携带解析后的行数）。
    Scrollback(usize),
    /// 写回“最大并发传输数”配置（携带裁剪后的值），并立即重新调度传输队列。
    MaxConcurrent(usize),
    /// 把当前“最大并发传输数”落盘（滑块释放后调用，无重复调度）。
    MaxConcurrentPersist,
    /// 写回“失败自动重试次数”配置（携带裁剪后的值）。
    ///
    /// 与并发数不同，此处**不**触发重调度：重试次数是在「失败那一刻」从上下文读取的，
    /// 改动后下一次失败即用新值，队列无需重排。
    RetryAttempts(u32),
    /// 把当前“失败自动重试次数”落盘（滑块释放后调用）。
    RetryAttemptsPersist,
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
    /// 写回“自动追踪终端目录”配置（携带开关状态）。
    CwdBootstrap(bool),
    /// 写回"注入脚本时抑制终端回显"配置（携带开关状态）。
    SuppressBootstrapEcho(bool),
    /// 写回"复制时去除行尾空格"配置（携带开关状态）。
    TrimTrailingWhitespace(bool),
    /// 写回"响铃视觉提示"配置（携带开关状态）。
    Bell(bool),
    /// 写回"记住窗口大小"配置（携带开关状态）。
    RememberWindowSize(bool),
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
                if self.show_settings {
                    // 每次打开都从干净状态开始：清空过滤词；若停在「安全」分类，
                    // 重读 known_hosts，避免展示上次会话的陈旧列表。
                    self.known_hosts_filter.clear();
                    if self.category == SettingsCategory::Security {
                        self.reload_known_hosts();
                    }
                }
                Task::none()
            }
            Message::CategorySelected(category) => {
                self.category = category;
                // 进入「安全」分类时刷新已信任主机列表（列表只在设置界面内可见）。
                if category == SettingsCategory::Security {
                    self.reload_known_hosts();
                }
                Task::none()
            }
            // 0 表示不限制超时；仅可解析为非负整数时才上行写回。
            Message::ConnectTimeout(text) => match text.parse::<u64>() {
                Ok(timeout) => Task::done(Event::ConnectTimeout(timeout)),
                Err(_) => Task::none(),
            },
            // 0 表示关闭保活；仅可解析为非负整数时才上行写回。
            Message::Keepalive(text) => match text.parse::<u64>() {
                Ok(keepalive) => Task::done(Event::Keepalive(keepalive)),
                Err(_) => Task::none(),
            },
            // 保活判死次数是 0..=3 的小整数枚举：滑块已消除非法输入，此处只做范围兜底。
            // 刻意不重映射 0：russh 把 0 定义为「发保活但永不判死」，原样透传才是用户看到的语义。
            Message::KeepaliveMax(value) => {
                let n = (value.round() as i64).clamp(0, MAX_KEEPALIVE_MAX as i64);
                Task::done(Event::KeepaliveMax(n as u64))
            }
            Message::KeepaliveMaxPersist => Task::done(Event::KeepaliveMaxPersist),
            // 仅可解析为非负整数时才上行写回；0 表示不保留历史。
            Message::Scrollback(text) => match text.parse::<usize>() {
                Ok(scrollback) => Task::done(Event::Scrollback(scrollback)),
                Err(_) => Task::none(),
            },
            // 并发数是 1..=8 的小整数枚举：滑块已消除非法输入，此处只做范围兜底。
            Message::MaxConcurrent(value) => {
                let n = (value.round() as i64).clamp(MIN_CONCURRENT as i64, MAX_CONCURRENT as i64);
                Task::done(Event::MaxConcurrent(n as usize))
            }
            Message::MaxConcurrentPersist => Task::done(Event::MaxConcurrentPersist),
            // 重试次数是 0..=5 的小整数枚举（0 = 关闭自动重试）：滑块已消除非法输入，
            // 此处只做范围兜底。
            Message::RetryAttempts(value) => {
                let n = (value.round() as i64).clamp(0, MAX_RETRY_ATTEMPTS as i64);
                Task::done(Event::RetryAttempts(n as u32))
            }
            Message::RetryAttemptsPersist => Task::done(Event::RetryAttemptsPersist),
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
                self.ui_font_combo = combo_box::State::new(crate::font::ui_font_options(
                    &ctx.config.appearance.ui_font,
                ));
                self.terminal_font_combo = combo_box::State::new(
                    crate::font::terminal_font_options(&ctx.config.terminal.font),
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
            Message::CwdBootstrap(v) => Task::done(Event::CwdBootstrap(v)),
            Message::SuppressBootstrapEcho(v) => Task::done(Event::SuppressBootstrapEcho(v)),
            Message::TrimTrailingWhitespace(v) => Task::done(Event::TrimTrailingWhitespace(v)),
            Message::Bell(v) => Task::done(Event::Bell(v)),
            Message::RememberWindowSize(v) => Task::done(Event::RememberWindowSize(v)),
            Message::ForgetKnownHost(host, port) => {
                match rterm_core::host_key::forget_host_key(&host, port) {
                    // 未命中（`Ok(false)`）同样重读：说明列表与文件已不同步，以磁盘为准。
                    Ok(_) => self.reload_known_hosts(),
                    Err(e) => self.known_hosts_error = Some(crate::i18n::localize_error(&e)),
                }
                Task::none()
            }
            Message::KnownHostEnter(key) => {
                self.known_hosts_hovered = Some(key);
                Task::none()
            }
            Message::KnownHostExit => {
                self.known_hosts_hovered = None;
                Task::none()
            }
            Message::KnownHostFilterChanged(query) => {
                // 过滤后原悬浮项可能被隐藏，残留高亮会指向不存在的行，顺手清掉。
                self.known_hosts_hovered = None;
                self.known_hosts_filter = query;
                Task::none()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::STATE_ROOT_LOCK;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// 把状态根重定向到临时目录并返回它，覆盖 debug 构建默认的工作区 `.dev/cache`，
    /// 避免测试读写开发者真实的 known_hosts。
    fn use_temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rterm_settings_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("应能创建临时目录");
        rterm_config::paths::set_test_root(Some(dir.clone()));
        dir
    }

    /// 恢复默认状态根并清理临时目录。
    fn cleanup(root: &Path) {
        rterm_config::paths::set_test_root(None);
        let _ = fs::remove_dir_all(root);
    }

    fn write_known_hosts(root: &Path, content: &str) {
        let path = root.join("cache").join("known_hosts");
        fs::create_dir_all(path.parent().expect("应有父目录")).expect("应能创建缓存目录");
        fs::write(&path, content).expect("应能写入 known_hosts");
    }

    fn ctx() -> Ctx {
        Ctx {
            config: AppConfig::default(),
        }
    }

    #[test]
    fn security_category_loads_known_hosts_and_forget_updates_list_and_file() {
        let _guard = STATE_ROOT_LOCK.lock().unwrap();
        let dir = use_temp_root("forget");
        write_known_hosts(&dir, "a.com:22 SHA256:aaa\nb.com:22 SHA256:bbb\n");

        let mut state = State::new(&AppConfig::default());
        let ctx = ctx();
        let _ = state.update(Message::CategorySelected(SettingsCategory::Security), &ctx);
        assert_eq!(state.known_hosts.len(), 2, "切到「安全」分类应加载列表");

        let _ = state.update(Message::ForgetKnownHost("a.com".into(), 22), &ctx);
        let content = fs::read_to_string(dir.join("cache").join("known_hosts")).expect("应能读取");
        cleanup(&dir);

        assert_eq!(state.known_hosts.len(), 1, "遗忘后缓存应只剩另一条");
        assert_eq!(state.known_hosts[0].host, "b.com");
        assert_eq!(content, "b.com:22 SHA256:bbb\n", "遗忘应同时落到磁盘");
        assert!(state.known_hosts_error.is_none());
    }

    #[test]
    fn reopening_settings_on_security_rereads_known_hosts() {
        let _guard = STATE_ROOT_LOCK.lock().unwrap();
        let dir = use_temp_root("reopen");
        write_known_hosts(&dir, "a.com:22 SHA256:aaa\n");

        let mut state = State::new(&AppConfig::default());
        let ctx = ctx();
        let _ = state.update(Message::CategorySelected(SettingsCategory::Security), &ctx);
        assert_eq!(state.known_hosts.len(), 1);
        let _ = state.update(Message::Toggle, &ctx);
        let _ = state.update(Message::Toggle, &ctx);

        // 外部新增条目（如另一实例连接了新主机）：重新打开设置必须重读而非显示陈旧缓存。
        write_known_hosts(&dir, "a.com:22 SHA256:aaa\nc.com:22 SHA256:ccc\n");
        let _ = state.update(Message::Toggle, &ctx);
        let loaded = state.known_hosts.len();
        cleanup(&dir);

        assert_eq!(loaded, 2, "重新打开设置应重读 known_hosts");
    }

    /// 过滤词对 `host:port` 显示串做大小写不敏感子串匹配，空白等价于不过滤。
    #[test]
    fn filter_matches_host_and_port_case_insensitively() {
        let entries = vec![
            KnownHostEntry {
                host: "a.com".into(),
                port: 22,
                fingerprint: "SHA256:x".into(),
            },
            KnownHostEntry {
                host: "B.example.org".into(),
                port: 2200,
                fingerprint: "SHA256:y".into(),
            },
            KnownHostEntry {
                host: "2001:db8::1".into(),
                port: 22,
                fingerprint: "SHA256:z".into(),
            },
        ];

        let hit = filtered_known_hosts(&entries, "b.example");
        assert_eq!(hit.len(), 1, "主机名匹配应大小写不敏感");
        assert_eq!(hit[0].port, 2200);

        assert_eq!(
            filtered_known_hosts(&entries, ":2200").len(),
            1,
            "显示串含端口，应可按端口过滤"
        );
        assert_eq!(
            filtered_known_hosts(&entries, "db8").len(),
            1,
            "IPv6 子串应可命中"
        );
        assert_eq!(
            filtered_known_hosts(&entries, "  A.COM ").len(),
            1,
            "首尾空白应忽略"
        );
        assert!(
            filtered_known_hosts(&entries, "nope").is_empty(),
            "无匹配应为空"
        );
        assert_eq!(
            filtered_known_hosts(&entries, "   ").len(),
            3,
            "空过滤词应返回全量"
        );
    }

    /// 过滤词经消息写入状态并清除残留悬浮高亮；重新打开设置时清空过滤词。
    #[test]
    fn filter_message_updates_state_and_reopen_resets_it() {
        let mut state = State::new(&AppConfig::default());
        let ctx = ctx();
        let _ = state.update(Message::KnownHostEnter("a.com:22".into()), &ctx);
        let _ = state.update(Message::KnownHostFilterChanged("a".into()), &ctx);
        assert_eq!(state.known_hosts_filter, "a");
        assert!(
            state.known_hosts_hovered.is_none(),
            "过滤后应清除残留悬浮高亮"
        );

        let _ = state.update(Message::Toggle, &ctx);
        assert!(state.show_settings);
        assert!(
            state.known_hosts_filter.is_empty(),
            "重新打开设置应清空过滤词"
        );
    }
}
