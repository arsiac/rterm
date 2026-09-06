#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use rterm_config::{AppConfig, LogLevel};

/// 程序入口：读取日志配置、初始化日志系统后启动 GUI 主循环。
fn main() {
    // 读取日志级别（配置缺失或损坏时回退默认），并构造 flexi_logger 指令串：
    // 白名单方式：全局默认 warn 抑制所有第三方库噪声，仅项目自身的 crate 获得用户选择的日志级别。
    // Off 时全局为 off，确保真正静默。
    let log_level = AppConfig::new().map(|c| c.log_level).unwrap_or_default();
    let directive = if log_level == LogLevel::Off {
        "off".to_string()
    } else {
        format!(
            "warn, rterm={lv}, rterm_core={lv}, rterm_gui={lv}, rterm_config={lv}, rterm_crypto={lv}",
            lv = log_level,
        )
    };

    // 日志目录：平台缓存目录下的 rterm/logs 子目录（Linux 为 ~/.cache/rterm/logs），
    // 与 GUI 共用同一来源，避免计算逻辑散落不一致。
    let cache_dir = rterm_config::log_dir();

    flexi_logger::Logger::try_with_str(&directive)
        .expect("failed to initialize logger")
        .log_to_file(
            flexi_logger::FileSpec::default()
                .directory(&cache_dir)
                .discriminant("rterm"),
        )
        .duplicate_to_stderr(flexi_logger::Duplicate::All)
        .rotate(
            flexi_logger::Criterion::Age(flexi_logger::Age::Day),
            flexi_logger::Naming::Timestamps,
            flexi_logger::Cleanup::KeepLogFiles(7),
        )
        .start()
        .expect("failed to start logger");

    if let Err(e) = rterm_gui::run() {
        log::error!("GUI failed: {e}");
        std::process::exit(1);
    }
}
