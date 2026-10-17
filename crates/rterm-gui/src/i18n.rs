//! 核心错误的国际化映射。
//!
//! [`CoreError`] 携带稳定语义的 [`CoreErrorKind`] 与底层来源错误（英文）。本模块按当前
//! locale 把错误翻译成用户可见文案：带底层来源的种类会附带 `detail`，纯语义失败则只给文案。

use crate::t;
use rterm_config::ConfigError;
use rterm_core::{CoreError, CoreErrorKind};
use rterm_crypto::CryptoError;
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
                // 跳板机失败：底层来源是那一步的 CoreError，递归本地化后嵌进文案，
                // 使「认证被拒 / 连不上」这类具体原因跟着跳序与 host 一起显示。
                CoreErrorKind::JumpConnect { index, host } => {
                    let inner = source
                        .as_deref()
                        .and_then(|s| s.downcast_ref::<CoreError>())
                        .map(localize_error)
                        .unwrap_or(detail);
                    t!(
                        "errors.ssh_jump_connect",
                        index => *index + 1,
                        host => host.as_str(),
                        detail => inner
                    )
                }
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
                CoreErrorKind::DirNotEmpty => t!("errors.sftp_dir_not_empty"),
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
                CoreErrorKind::RemoteMetadata => {
                    t!("errors.sftp_remote_metadata", detail => detail)
                }
            }
        }
    }
}

/// 把 [`ConfigError`] 翻译为当前语言下的用户可见文案，在 GUI 边界（展示前）调用。
///
/// 底层来源（TOML / I/O 错误、`Store` 自由文本）作为 `detail` 补充，与 [`localize_error`]
/// 同一纪律；`Verify` 复用导入口令错文案 `cred.wrong_pass`。
pub fn localize_config_error(err: &ConfigError) -> String {
    match err {
        ConfigError::Io(e) => t!("errors.config.io", detail => e.to_string()),
        ConfigError::TomlDe(e) => t!("errors.config.parse", detail => e.to_string()),
        ConfigError::TomlSer(e) => t!("errors.config.serialize", detail => e.to_string()),
        ConfigError::Store(s) => t!("errors.config.store", detail => s.as_str()),
        ConfigError::ExportFileInPlace => t!("errors.config.export_file_in_place"),
        ConfigError::Crypto(ce) => match ce {
            CryptoError::Kdf => t!("errors.crypto.kdf"),
            CryptoError::Cipher => t!("errors.crypto.cipher"),
            CryptoError::Verify => t!("cred.wrong_pass"),
            CryptoError::BadEnvelope => t!("errors.crypto.envelope"),
        },
        ConfigError::ConfigDir(s) => t!("errors.config.dir", detail => s.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ConfigError` 边界翻译：带 detail 的变体必须把底层文本嵌进译文，
    /// `Verify` 复用导入口令错文案，与导入通道特判口径一致。
    #[test]
    fn config_error_kinds_translate_with_detail() {
        let store = ConfigError::Store("failed to read session file: boom".into());
        assert_eq!(
            localize_config_error(&store),
            t!("errors.config.store", detail => "failed to read session file: boom")
        );

        let io = ConfigError::Io(std::io::Error::other("disk full"));
        assert_eq!(
            localize_config_error(&io),
            t!("errors.config.io", detail => "disk full")
        );

        let verify = ConfigError::Crypto(CryptoError::Verify);
        assert_eq!(localize_config_error(&verify), t!("cred.wrong_pass"));

        assert!(!localize_config_error(&ConfigError::ExportFileInPlace).is_empty());
        assert!(!localize_config_error(&ConfigError::Crypto(CryptoError::Kdf)).is_empty());
    }

    /// 跳板机失败要同时给出跳序（1 起）、host 与内层原因，且内层也要本地化——
    /// 否则用户只知道「某一跳挂了」，看不到究竟卡在认证还是网络。
    #[test]
    fn jump_failure_names_the_hop_and_the_inner_reason() {
        let inner = CoreError::ssh_msg(CoreErrorKind::AuthPasswordRejected);
        let err = CoreError::ssh(
            CoreErrorKind::JumpConnect {
                index: 1,
                host: "bastion".into(),
            },
            inner,
        );

        let expected = t!(
            "errors.ssh_jump_connect",
            index => 2,
            host => "bastion",
            detail => t!("errors.ssh_auth_password_rejected")
        );
        assert_eq!(localize_error(&err), expected);
    }
}
