//! rterm 核心库统一错误类型。
//!
//! 所有 SSH / SFTP / I/O 错误在此收敛为 [`CoreError`]，便于上层 GUI 统一处理与记录日志。
//! 配置读写错误由 [`rterm_config::ConfigError`] 单独承载。
//!
//! `Ssh` / `Sftp` 变体在保留中文上下文（用于 UI 展示）的同时，通过 [`std::error::Error::source`]
//! 携带底层错误（如 russh / IO 错误），避免一进核心就把原始失败原因压成纯字符串、丢失错误链。

use std::error::Error as StdError;
use std::io;

/// rterm 核心库统一错误类型：聚合 SSH / SFTP / I/O 等各类底层错误。
#[derive(Debug)]
pub enum CoreError {
    /// 本地文件系统 I/O 失败（如读取 known_hosts、密钥文件）。
    Io(io::Error),

    /// SSH 连接或认证过程中的错误（含连接、认证、通道操作失败）。
    Ssh {
        /// 中文上下文描述（展示用）。
        context: String,
        /// 底层错误（若有），保留错误链以便排障。
        source: Option<Box<dyn StdError + Send + Sync + 'static>>,
    },

    /// SFTP 子系统操作错误（如列目录、传输文件失败）。
    Sftp {
        /// 中文上下文描述（展示用）。
        context: String,
        /// 底层错误（若有），保留错误链以便排障。
        source: Option<Box<dyn StdError + Send + Sync + 'static>>,
    },
}

impl CoreError {
    /// 构造带底层来源的 SSH 错误。
    pub fn ssh(context: impl Into<String>, source: impl StdError + Send + Sync + 'static) -> Self {
        CoreError::Ssh {
            context: context.into(),
            source: Some(Box::new(source)),
        }
    }

    /// 构造无底层来源的 SSH 错误（语义性失败，如认证被服务器拒绝）。
    pub fn ssh_msg(context: impl Into<String>) -> Self {
        CoreError::Ssh {
            context: context.into(),
            source: None,
        }
    }

    /// 构造带底层来源的 SFTP 错误。
    pub fn sftp(context: impl Into<String>, source: impl StdError + Send + Sync + 'static) -> Self {
        CoreError::Sftp {
            context: context.into(),
            source: Some(Box::new(source)),
        }
    }

    /// 构造无底层来源的 SFTP 错误（语义性失败）。
    pub fn sftp_msg(context: impl Into<String>) -> Self {
        CoreError::Sftp {
            context: context.into(),
            source: None,
        }
    }
}

impl std::fmt::Display for CoreError {
    /// 将错误渲染为中文可读描述（含底层来源，若有）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::Io(e) => write!(f, "I/O 错误: {e}"),
            CoreError::Ssh { context, source } => match source {
                Some(s) => write!(f, "SSH 错误: {context}: {s}"),
                None => write!(f, "SSH 错误: {context}"),
            },
            CoreError::Sftp { context, source } => match source {
                Some(s) => write!(f, "SFTP 错误: {context}: {s}"),
                None => write!(f, "SFTP 错误: {context}"),
            },
        }
    }
}

impl StdError for CoreError {
    /// 返回底层错误来源，保留错误链以便回溯根因。
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            CoreError::Io(e) => Some(e),
            CoreError::Ssh { source, .. } => source
                .as_ref()
                .map(|b| b.as_ref() as &(dyn StdError + 'static)),
            CoreError::Sftp { source, .. } => source
                .as_ref()
                .map(|b| b.as_ref() as &(dyn StdError + 'static)),
        }
    }
}

impl From<io::Error> for CoreError {
    /// 由标准 I/O 错误转换为 [`CoreError::Io`]。
    fn from(e: io::Error) -> Self {
        CoreError::Io(e)
    }
}
