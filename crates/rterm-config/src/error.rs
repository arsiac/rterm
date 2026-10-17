//! rterm 配置层统一错误类型。
//!
//! 所有配置读写错误（I/O、TOML 序列化、目录定位、存储）在此收敛为 [`ConfigError`]，
//! 便于上层统一处理与记录日志。

use thiserror::Error;

/// 配置层统一错误类型。
///
/// 所有配置读写错误（I/O、TOML 序列化、目录定位、存储）在此收敛，便于上层统一处理与记录日志。
#[derive(Debug, Error)]
pub enum ConfigError {
    /// 底层 I/O 错误（文件读写等）。
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// TOML 反序列化失败（读取配置文件时）。
    #[error("failed to parse config: {0}")]
    TomlDe(#[from] toml::de::Error),

    /// TOML 序列化失败（保存配置文件时）。
    #[error("failed to serialize config: {0}")]
    TomlSer(#[from] toml::ser::Error),

    /// 配置存储相关错误：目录创建失败、配置文件读取 / 解析 / 写入失败、加密头缺失等。
    #[error("config store error: {0}")]
    Store(String),

    /// 会话文件位置上的内容是**导出件**（携带 [`EXPORT_FORMAT`](crate::store::EXPORT_FORMAT) 标记），
    /// 不能作为本机 `sessions.toml` 加载——它由导出口令保护，用主密码永远解不开。
    #[error("the file is a session export file and cannot be loaded as the local session config")]
    ExportFileInPlace,

    /// 凭据加解密错误（导出口令 / 主密码不正确、密文损坏、KDF 参数越界等）。上层用
    /// `matches!(err, ConfigError::Crypto(rterm_crypto::CryptoError::Verify))` 区分
    /// 「口令错」与「文件坏」两类反馈。
    #[error("credential error: {0}")]
    Crypto(#[from] rterm_crypto::CryptoError),

    /// 找不到合适的配置 / 数据目录。
    #[error("config directory unavailable: {0}")]
    ConfigDir(String),
}

impl From<crate::store::SessionsFileProblem> for ConfigError {
    /// 把会话文件的「不可用原因」收敛为统一错误类型（启动三态判定用 [`crate::store::SessionStore::startup_state`]
    /// 拿原始原因，普通读写路径经此转换）。
    fn from(problem: crate::store::SessionsFileProblem) -> Self {
        use crate::store::SessionsFileProblem;
        match problem {
            SessionsFileProblem::Unreadable(e) => {
                ConfigError::Store(format!("failed to read session file: {e}"))
            }
            SessionsFileProblem::Unparsable(e) => {
                ConfigError::Store(format!("failed to parse session file: {e}"))
            }
            SessionsFileProblem::ExportFile => ConfigError::ExportFileInPlace,
            SessionsFileProblem::MissingCryptoHeader => ConfigError::Store(
                "session file has no crypto header (possibly a connection-only export file)".into(),
            ),
        }
    }
}
