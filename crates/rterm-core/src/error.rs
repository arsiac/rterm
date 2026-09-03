//! rterm 核心库统一错误类型。
//!
//! 所有 SSH / SFTP / I/O 错误在此收敛为 [`CoreError`]，便于上层 GUI 统一处理与记录日志。
//! 配置读写错误由 [`rterm_config::ConfigError`] 单独承载。
//!
//! 错误语义以 [`CoreErrorKind`] 枚举表达（而非写死的中文串），底层来源（如 russh / IO
//! 错误）保留在 [`std::error::Error::source`] 中。真正的用户可见文案由 GUI 层的
//! `localize_error` 按当前 locale 翻译，故切换界面语言时错误提示能同步国际化。

use std::error::Error as StdError;
use std::fmt;
use std::io;

/// rterm 核心库统一错误类型：聚合 SSH / SFTP / I/O 等各类底层错误。
#[derive(Debug)]
pub enum CoreError {
    /// 本地文件系统 I/O 失败（如读取 known_hosts、密钥文件）。
    Io(io::Error),

    /// SSH 连接或认证过程中的错误（含连接、认证、通道操作失败）。
    Ssh {
        /// 稳定的错误语义（用于 GUI 侧按当前语言翻译）。
        kind: CoreErrorKind,
        /// 底层错误（若有），保留错误链以便排障。
        source: Option<Box<dyn StdError + Send + Sync + 'static>>,
    },

    /// SFTP 子系统操作错误（如列目录、传输文件失败）。
    Sftp {
        /// 稳定的错误语义（用于 GUI 侧按当前语言翻译）。
        kind: CoreErrorKind,
        /// 底层错误（若有），保留错误链以便排障。
        source: Option<Box<dyn StdError + Send + Sync + 'static>>,
    },
}

/// 核心错误的稳定语义分类，与界面语言无关。
///
/// GUI 层据此在翻译表中找到对应文案；底层来源错误（英文）作为 `detail` 补充。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreErrorKind {
    /// 建立 TCP / SSH 连接失败。
    Connect,
    /// 凭据保险库未提供解密后的密码。
    MissingPassword,
    /// 密码认证请求发出失败。
    AuthPasswordRequest,
    /// 密码认证被服务器拒绝。
    AuthPasswordRejected,
    /// 读取私钥文件失败。
    ReadKey,
    /// 解密私钥失败。
    DecryptKey,
    /// 公钥认证请求发出失败。
    AuthPublicKeyRequest,
    /// 公钥认证被服务器拒绝。
    AuthPublicKeyRejected,
    /// 连接 SSH agent 失败（Unix）。
    AgentConnect,
    /// 连接 SSH agent（Pageant）失败（Windows）。
    AgentConnectPageant,
    /// 获取 agent 可用身份失败。
    AgentIdentities,
    /// agent 认证失败或无可用身份。
    AgentAuthFailed,
    /// 打开通道失败。
    ChannelOpen,
    /// 请求 PTY 失败。
    RequestPty,
    /// 启动 shell 失败。
    StartShell,
    /// 打开 sftp 通道失败。
    SftpChannelOpen,
    /// 请求 sftp 子系统失败。
    SftpSubsystem,
    /// 初始化 sftp 会话失败。
    SftpInit,
    /// 无法定位缓存目录。
    CacheDirUnknown,
    /// 创建缓存目录失败。
    CreateCacheDir,
    /// 写入 known_hosts 失败。
    WriteKnownHosts,
    /// 解析远端路径失败。
    ParsePath,
    /// 读取目录失败。
    ReadDir,
    /// 创建目录失败。
    CreateDir,
    /// 删除文件失败。
    DeleteFile,
    /// 删除目录失败。
    DeleteDir,
    /// 重命名失败。
    Rename,
    /// 创建远端文件失败。
    CreateRemoteFile,
    /// 写入远端失败。
    WriteRemote,
    /// 关闭远端文件失败。
    CloseRemoteFile,
    /// 打开远端文件失败。
    OpenRemoteFile,
    /// 读取远端失败。
    ReadRemote,
}

impl CoreError {
    /// 构造带底层来源的 SSH 错误。
    pub fn ssh(kind: CoreErrorKind, source: impl StdError + Send + Sync + 'static) -> Self {
        CoreError::Ssh {
            kind,
            source: Some(Box::new(source)),
        }
    }

    /// 构造无底层来源的 SSH 错误（语义性失败，如认证被服务器拒绝）。
    pub fn ssh_msg(kind: CoreErrorKind) -> Self {
        CoreError::Ssh { kind, source: None }
    }

    /// 构造带底层来源的 SFTP 错误。
    pub fn sftp(kind: CoreErrorKind, source: impl StdError + Send + Sync + 'static) -> Self {
        CoreError::Sftp {
            kind,
            source: Some(Box::new(source)),
        }
    }

    /// 构造无底层来源的 SFTP 错误（语义性失败）。
    pub fn sftp_msg(kind: CoreErrorKind) -> Self {
        CoreError::Sftp { kind, source: None }
    }
}

impl fmt::Display for CoreErrorKind {
    /// 以稳定标识符呈现（供日志与排障，非用户文案）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl fmt::Display for CoreError {
    /// 将错误渲染为可读描述（含底层来源，若有）。
    ///
    /// 此为开发侧日志用文本（非按界面语言翻译的用户文案）；用户可见文案由 GUI 的
    /// `localize_error` 生成。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::Io(e) => write!(f, "I/O error: {e}"),
            CoreError::Ssh { kind, source } | CoreError::Sftp { kind, source } => match source {
                Some(s) => write!(f, "{kind}: {s}"),
                None => write!(f, "{kind}"),
            },
        }
    }
}

impl StdError for CoreError {
    /// 返回底层错误来源，保留错误链以便回溯根因。
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            CoreError::Io(e) => Some(e),
            CoreError::Ssh { source, .. } | CoreError::Sftp { source, .. } => source
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
