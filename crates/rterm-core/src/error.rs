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

/// 错误的可重试性分类，供上层决定是否自动重试。
///
/// 只回答「重试一次是否可能成功」，不改变错误本身，也不参与用户文案生成
/// （用户可见文案仍由 GUI 的 `localize_error` 按当前语言产出）。分类逻辑刻意留在核心层：
/// 上层不该解析 `russh_sftp` 的内部错误形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// 瞬时故障：网络抖动、超时、通道断开等，带退避地重试有意义。
    Transient,
    /// 永久故障：权限不足、路径不存在、磁盘满等，重试只会重复失败。
    Permanent,
    /// 会话已终结：承载这次调用的 SFTP 通道**本地执行端已经退出**。
    ///
    /// 与 [`Transient`](Self::Transient) 的区别不是程度而是性质：抖动时那个客户端还是活的，
    /// 重试有可能成功；而这一类的客户端对象已经死了 —— 重试同一个客户端**永远**会立刻失败，
    /// 只有重新建立会话（拿到新的客户端）才有意义。故上层的自动重试必须在这里停下，
    /// 并把「连接已断开」这个可操作的事实告诉用户，而不是继续空烧重试预算。
    SessionGone,
    /// 无法判定：既非明确的瞬时也非明确的永久，交由调用方决定。
    Unknowable,
}

impl CoreError {
    /// 判定该错误的可重试性分类。
    ///
    /// 判定依据见各分支注释：本地 I/O 看 [`io::ErrorKind`]，SSH 层看是否为通道 / 子系统级故障，
    /// SFTP 层按操作类型下钻到底层来源
    /// （结构化类型优先，退化为消息文本）。
    pub fn class(&self) -> ErrorClass {
        match self {
            CoreError::Io(e) => classify_io_kind(e.kind()),
            // SSH 层的通道 / 子系统故障：重开通道即可重试；其余（认证、密钥、agent 等）
            // 与网络状态无必然关系，无法判定。
            CoreError::Ssh { kind, .. } => match kind {
                CoreErrorKind::Connect
                | CoreErrorKind::ChannelOpen
                | CoreErrorKind::SftpChannelOpen
                | CoreErrorKind::SftpSubsystem
                | CoreErrorKind::SftpInit => ErrorClass::Transient,
                _ => ErrorClass::Unknowable,
            },
            // 只有「传输过程中」的远端错误才谈得上重试：列目录 / 建目录 / 重命名等语义性失败
            // 即使瞬时也不该由传输层自动重放。
            CoreError::Sftp { kind, source } => match kind {
                CoreErrorKind::ReadRemote
                | CoreErrorKind::WriteRemote
                | CoreErrorKind::OpenRemoteFile
                | CoreErrorKind::CreateRemoteFile
                | CoreErrorKind::CloseRemoteFile => classify_remote(source.as_deref()),
                _ => ErrorClass::Unknowable,
            },
        }
    }
}

/// 按 [`io::ErrorKind`] 判定本地 I/O 错误分类。
fn classify_io_kind(kind: io::ErrorKind) -> ErrorClass {
    use io::ErrorKind as K;
    match kind {
        K::TimedOut
        | K::WouldBlock
        | K::Interrupted
        | K::ConnectionReset
        | K::ConnectionAborted
        | K::BrokenPipe
        | K::UnexpectedEof => ErrorClass::Transient,
        K::NotFound | K::PermissionDenied | K::AlreadyExists | K::StorageFull => {
            ErrorClass::Permanent
        }
        _ => ErrorClass::Unknowable,
    }
}

/// 判定远端 SFTP 传输错误的分类。
///
/// 三条路径依次尝试，因为远端错误在链路上的保留程度并不一致：
/// 1. `open` / `create` / `close` / `metadata` 等直接会话调用把
///    `russh_sftp::client::error::Error` 原样存进 [`StdError::source`]，可直接下钻；
/// 2. 本地或套接字 I/O 错误按 [`io::ErrorKind`] 判定；
/// 3. 兜底按消息文本判定——分块读写必须经由 `AsyncRead` / `AsyncWrite`，而 russh-sftp 在
///    `File::poll_write` / `poll_read` 里用 `io::Error::other(e.to_string())` 转换错误，
///    结构化类型在这条链路上已经丢失，只能按状态码枚举的展示文本反推。
fn classify_remote(source: Option<&(dyn StdError + Send + Sync + 'static)>) -> ErrorClass {
    let Some(source) = source else {
        return ErrorClass::Unknowable;
    };
    if let Some(e) = source.downcast_ref::<russh_sftp::client::error::Error>() {
        return classify_sftp_error(e);
    }
    if let Some(e) = source.downcast_ref::<io::Error>() {
        let by_kind = classify_io_kind(e.kind());
        // `ErrorKind::Other` 正是 russh-sftp 包装远端状态码后留下的空壳
        // （见下条注释），此时 kind 无信息量，继续往下按文本判定。
        if by_kind != ErrorClass::Unknowable {
            return by_kind;
        }
    }
    classify_message(&source.to_string())
}

/// 按 russh-sftp 客户端错误枚举判定分类。
fn classify_sftp_error(e: &russh_sftp::client::error::Error) -> ErrorClass {
    use russh_sftp::client::error::Error as SftpError;
    match e {
        SftpError::Status(s) => classify_status_code(s.status_code),
        // 超时、超出 limits@openssh.com 限制、协议层 I/O 断链：重试有意义。
        SftpError::Timeout
        | SftpError::IO(_)
        | SftpError::Limited(_)
        | SftpError::UnexpectedPacket => ErrorClass::Transient,
        SftpError::UnexpectedBehavior(msg) => classify_message(msg),
    }
}

/// 按 SSH_FXP_STATUS 状态码判定分类（枚举取值见 SFTP 协议草案第 7 节）。
fn classify_status_code(code: russh_sftp::protocol::StatusCode) -> ErrorClass {
    use russh_sftp::protocol::StatusCode as S;
    match code {
        // 权限、路径、不支持的操作：重试无用。
        S::PermissionDenied | S::NoSuchFile | S::OpUnsupported => ErrorClass::Permanent,
        // 服务端通用失败 / 连接丢失 / 未连接：多为链路问题，重试有意义。
        S::Failure | S::NoConnection | S::ConnectionLost => ErrorClass::Transient,
        // `Ok` / `Eof` 本不该作为错误出现；`BadMessage` 说明协议解释不一致，无法判定。
        S::Ok | S::Eof | S::BadMessage => ErrorClass::Unknowable,
    }
}

/// 按消息文本判定分类：结构化类型不可得时的兜底。
///
/// 匹配对象主要是 [`russh_sftp::protocol::StatusCode`] 的展示文本（`"Permission denied"` 等）。
fn classify_message(msg: &str) -> ErrorClass {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("permission denied")
        || lower.contains("no such file")
        || lower.contains("operation unsupported")
        || lower.contains("not a directory")
        || lower.contains("is a directory")
    {
        return ErrorClass::Permanent;
    }
    if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection")
        || lower.contains("broken pipe")
        || lower.contains("limit exceeded")
        || lower.contains("unexpected eof")
    {
        return ErrorClass::Transient;
    }
    ErrorClass::Unknowable
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh_sftp::client::error::Error as SftpError;
    use russh_sftp::protocol::{Status, StatusCode};

    /// 构造一个 SFTP `Status` 错误包（id / 文案 / 语言标签与判定无关，取占位值）。
    fn status_error(code: StatusCode) -> SftpError {
        SftpError::Status(Status {
            id: 1,
            status_code: code,
            error_message: code.to_string(),
            language_tag: "en-US".to_string(),
        })
    }

    #[test]
    fn local_io_errors_split_by_kind() {
        assert_eq!(
            CoreError::Io(io::Error::from(io::ErrorKind::TimedOut)).class(),
            ErrorClass::Transient,
            "超时应视为瞬时故障"
        );
        assert_eq!(
            CoreError::Io(io::Error::from(io::ErrorKind::ConnectionReset)).class(),
            ErrorClass::Transient
        );
        assert_eq!(
            CoreError::Io(io::Error::from(io::ErrorKind::NotFound)).class(),
            ErrorClass::Permanent,
            "本地文件不存在应视为永久故障"
        );
        assert_eq!(
            CoreError::Io(io::Error::from(io::ErrorKind::PermissionDenied)).class(),
            ErrorClass::Permanent
        );
    }

    #[test]
    fn ssh_channel_failures_are_transient() {
        assert_eq!(
            CoreError::ssh_msg(CoreErrorKind::Connect).class(),
            ErrorClass::Transient
        );
        // 认证 / 密钥类失败与网络状态无关，不该被自动重放。
        assert_eq!(
            CoreError::ssh_msg(CoreErrorKind::AuthPasswordRejected).class(),
            ErrorClass::Unknowable
        );
    }

    #[test]
    fn structured_sftp_status_is_downcast_and_classified() {
        assert_eq!(
            CoreError::sftp(
                CoreErrorKind::OpenRemoteFile,
                status_error(StatusCode::PermissionDenied)
            )
            .class(),
            ErrorClass::Permanent,
            "远端权限不足应视为永久故障（可由结构化枚举直接判定）"
        );
        assert_eq!(
            CoreError::sftp(
                CoreErrorKind::ReadRemote,
                status_error(StatusCode::ConnectionLost)
            )
            .class(),
            ErrorClass::Transient
        );
        assert_eq!(
            CoreError::sftp(
                CoreErrorKind::WriteRemote,
                status_error(StatusCode::Failure)
            )
            .class(),
            ErrorClass::Transient
        );
    }

    #[test]
    fn lossy_streaming_wrapped_errors_fall_back_to_text() {
        // 分块读写链路上 russh-sftp 用 `io::Error::other(e.to_string())` 转换错误，
        // 结构化类型已丢失，只能按展示文本判定。
        assert_eq!(
            CoreError::sftp(
                CoreErrorKind::WriteRemote,
                io::Error::other("Permission denied: /root/secret")
            )
            .class(),
            ErrorClass::Permanent
        );
        assert_eq!(
            CoreError::sftp(
                CoreErrorKind::ReadRemote,
                io::Error::other("Connection lost")
            )
            .class(),
            ErrorClass::Transient
        );
    }

    #[test]
    fn non_transfer_sftp_kinds_are_unknowable() {
        assert_eq!(
            CoreError::sftp(CoreErrorKind::ParsePath, io::Error::other("bad path")).class(),
            ErrorClass::Unknowable,
            "路径解析失败与网络状态无关，不该由传输层自动重放"
        );
        // 传输类错误但没有底层来源时同样无法判定。
        assert_eq!(
            CoreError::sftp_msg(CoreErrorKind::WriteRemote).class(),
            ErrorClass::Unknowable
        );
        assert_eq!(
            CoreError::sftp(
                CoreErrorKind::CreateDir,
                io::Error::other("Permission denied")
            )
            .class(),
            ErrorClass::Unknowable,
            "建目录的语义性失败仍不该被自动重放"
        );
    }
}
