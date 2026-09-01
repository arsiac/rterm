//! rterm 二进制入口：初始化日志系统并启动 GUI 主程序。

use rterm_config::{AppConfig, LogLevel};

/// 程序入口：读取日志配置、初始化日志系统后启动 GUI 主循环。
fn main() {
    // 读取日志级别（配置缺失或损坏时回退默认），并构造 flexi_logger 指令串：
    // 用户所选级别作为全局级别，第三方高噪声 crate 强制降到 warn 以抑制刷屏。
    // Off 时不附加任何覆盖项，确保真正静默。
    let log_level = AppConfig::new().map(|c| c.log_level).unwrap_or_default();
    let mut directive = log_level.to_string();
    if log_level != LogLevel::Off {
        directive.push_str(
            ", russh=warn, russh_sftp=warn, iced=warn, wgpu=warn, winit=warn, naga=warn, rfd=warn, iced_aw=warn",
        );
    }

    // 日志目录：平台缓存目录下的 rterm/logs 子目录（Linux 为 ~/.cache/rterm/logs），
    // 与 GUI 共用同一来源，避免计算逻辑散落不一致。
    let cache_dir = rterm_config::log_dir();

    flexi_logger::Logger::try_with_str(&directive)
        .expect("初始化日志器失败")
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
        .expect("启动日志器失败");

    if let Err(e) = rterm_gui::run() {
        log::error!("GUI 运行失败: {e}");
        std::process::exit(1);
    }
}
