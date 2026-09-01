//! rterm 核心库：SSH/SFTP 连接与会话模型（不依赖 GUI）。
//!
//! 会话配置类型复用配置层的 [`SessionConfig`](rterm_config::SessionConfig) 等模型，
//! 但持久化（读盘 / 写盘）不在此处。
//!
//! 依赖面刻意收窄：异步运行时（tokio）、SSH 栈（russh / russh-sftp）、日志、
//! `zeroize`（擦除明文凭据）、`dirs`（known_hosts 定位）、`chrono`（SFTP 时间戳格式化），
//! 以及配置层 [`rterm_config`]（只取会话模型，不含持久化），便于独立单元测试。
//! Windows 上额外用 `windows-sys` 建命名管道做终端桥接。
//! 后续的 GUI 层（`rterm-gui`）通过本 crate 提供的类型与连接管理接口与底层 SSH 交互。
//! 配置与持久化（应用偏好、会话 TOML）由 [`rterm_config`] 负责。

pub mod connection;
pub mod error;
pub mod host_key;
pub mod model;
pub mod sftp;
pub mod terminal_bridge;

pub use connection::{HostKeyPrompt, HostKeyReply, SessionSecrets, SshConnection};
pub use error::CoreError;
pub use model::{ConnectionStatus, FileEntry};
pub use sftp::SftpClient;
pub use terminal_bridge::spawn_terminal_bridge;
