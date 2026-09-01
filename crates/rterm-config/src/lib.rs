//! rterm 配置层：应用偏好设置与会话配置的持久化（磁盘 TOML）。
//!
//! 本 crate 不依赖 GUI 与 SSH，仅依赖序列化与目录定位库，便于独立单元测试。
//! 持久化文件统一存放在系统配置目录下（经 `dirs::config_dir()`：Linux 为
//! `~/.config/rterm/`、macOS 为 `~/Library/Application Support/rterm/`、Windows 为
//! `%APPDATA%\rterm\`），分别为：
//! - `config.toml`：应用级偏好（[`AppConfig`]）。
//! - `sessions.toml`：会话连接配置（[`SessionStore`] / [`SessionConfig`]）。

pub mod app_config;
pub mod error;
pub mod session;
pub mod store;

/// SSH 默认端口（用于 [`SessionConfig::port`] 的 serde 默认值，以及新建会话编辑器端口框的预填）。
pub const DEFAULT_SSH_PORT: u16 = 22;

pub use app_config::{AppConfig, Language, LogLevel, log_dir};
pub use error::ConfigError;
pub use session::{AuthMethod, SessionConfig, new_id};
pub use store::{SessionStore, export_sessions, import_sessions};
