//! 核心错误的国际化映射。
//!
//! [`CoreError`] 携带稳定语义的 [`CoreErrorKind`] 与底层来源错误（英文）。本模块按当前
//! locale 把错误翻译成用户可见文案：带底层来源的种类会附带 `detail`，纯语义失败则只给文案。

use crate::t;
use rterm_core::{CoreError, CoreErrorKind};
use std::error::Error as StdError;

/// 把 [`CoreError`] 翻译为当前语言下的用户可见文案。
///
/// 在 GUI 边界（错误即将展示前）调用，确保跟随界面当前语言。底层来源错误（russh / IO 等）
/// 一般为英文，作为 `detail` 补充在译文的占位符中。
pub fn localize_error(err: &CoreError) -> String {
    let detail_of = |source: &Option<Box<dyn StdError + Send + Sync + 'static>>| -> String {
        source.as_ref().map(|e| e.to_string()).unwrap_or_default()
    };

    match err {
        CoreError::Io(e) => t!("errors.io", detail => e.to_string()),
        CoreError::Ssh { kind, source } | CoreError::Sftp { kind, source } => {
            let detail = detail_of(source);
            match kind {
                CoreErrorKind::Connect => t!("errors.ssh_connect", detail => detail),
                CoreErrorKind::MissingPassword => t!("errors.ssh_missing_password"),
                CoreErrorKind::AuthPasswordRequest => {
                    t!("errors.ssh_auth_password_request", detail => detail)
                }
                CoreErrorKind::AuthPasswordRejected => t!("errors.ssh_auth_password_rejected"),
                CoreErrorKind::ReadKey => t!("errors.ssh_read_key", detail => detail),
                CoreErrorKind::DecryptKey => t!("errors.ssh_decrypt_key", detail => detail),
                CoreErrorKind::AuthPublicKeyRequest => {
                    t!("errors.ssh_auth_pubkey_request", detail => detail)
                }
                CoreErrorKind::AuthPublicKeyRejected => t!("errors.ssh_auth_pubkey_rejected"),
                CoreErrorKind::AgentConnect => t!("errors.ssh_agent_connect", detail => detail),
                CoreErrorKind::AgentConnectPageant => {
                    t!("errors.ssh_agent_connect_pageant", detail => detail)
                }
                CoreErrorKind::AgentIdentities => {
                    t!("errors.ssh_agent_identities", detail => detail)
                }
                CoreErrorKind::AgentAuthFailed => t!("errors.ssh_agent_auth_failed"),
                CoreErrorKind::ChannelOpen => t!("errors.ssh_channel_open", detail => detail),
                CoreErrorKind::RequestPty => t!("errors.ssh_request_pty", detail => detail),
                CoreErrorKind::StartShell => t!("errors.ssh_start_shell", detail => detail),
                CoreErrorKind::SftpChannelOpen => {
                    t!("errors.ssh_sftp_channel_open", detail => detail)
                }
                CoreErrorKind::SftpSubsystem => t!("errors.ssh_sftp_subsystem", detail => detail),
                CoreErrorKind::SftpInit => t!("errors.ssh_sftp_init", detail => detail),
                CoreErrorKind::CacheDirUnknown => t!("errors.ssh_cache_dir_unknown"),
                CoreErrorKind::CreateCacheDir => {
                    t!("errors.ssh_create_cache_dir", detail => detail)
                }
                CoreErrorKind::WriteKnownHosts => {
                    t!("errors.ssh_write_known_hosts", detail => detail)
                }
                CoreErrorKind::ParsePath => t!("errors.sftp_parse_path", detail => detail),
                CoreErrorKind::ReadDir => t!("errors.sftp_read_dir", detail => detail),
                CoreErrorKind::CreateDir => t!("errors.sftp_create_dir", detail => detail),
                CoreErrorKind::DeleteFile => t!("errors.sftp_delete_file", detail => detail),
                CoreErrorKind::DeleteDir => t!("errors.sftp_delete_dir", detail => detail),
                CoreErrorKind::Rename => t!("errors.sftp_rename", detail => detail),
                CoreErrorKind::CreateRemoteFile => {
                    t!("errors.sftp_create_remote_file", detail => detail)
                }
                CoreErrorKind::WriteRemote => t!("errors.sftp_write_remote", detail => detail),
                CoreErrorKind::CloseRemoteFile => {
                    t!("errors.sftp_close_remote_file", detail => detail)
                }
                CoreErrorKind::OpenRemoteFile => {
                    t!("errors.sftp_open_remote_file", detail => detail)
                }
                CoreErrorKind::ReadRemote => t!("errors.sftp_read_remote", detail => detail),
            }
        }
    }
}
