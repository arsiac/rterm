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
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),

    /// TOML 反序列化失败（读取配置文件时）。
    #[error("配置解析失败: {0}")]
    TomlDe(#[from] toml::de::Error),

    /// TOML 序列化失败（保存配置文件时）。
    #[error("配置序列化失败: {0}")]
    TomlSer(#[from] toml::ser::Error),

    /// 配置存储相关错误：目录创建失败、配置文件读取 / 解析 / 写入失败、加密头缺失等。
    #[error("配置存储错误: {0}")]
    Store(String),

    /// 找不到合适的配置 / 数据目录。
    #[error("配置目录不可用: {0}")]
    ConfigDir(String),
}
