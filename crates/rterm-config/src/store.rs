//! 会话配置的加载与保存。
//!
//! 使用 [`dirs`] 按 XDG 规范定位配置目录，并以 TOML 格式读写 `sessions.toml`。

use crate::{ConfigError, SessionConfig};
use dirs::config_dir;
use log::debug;
use rterm_crypto::CryptoHeader;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// 会话配置的持久化管理器（基于 `sessions.toml`）。
///
/// 定位 XDG 配置目录下的会话文件，提供加载、保存与路径访问能力。
#[derive(Clone)]
pub struct SessionStore {
    /// 会话文件在磁盘上的绝对路径（`sessions.toml`）。
    path: PathBuf,
}

/// TOML 文档根结构。
///
/// TOML 不允许文档根直接为数组，故用此表包裹会话列表，序列化为数组表 `[[sessions]]`；
/// `[crypto]` 段保存加密文件头（salt / KDF 参数 / 校验哨兵），与凭据信封共用同一主密钥。
/// 凭据字段在导出「仅连接配置」时会被剥离（见 [`SessionConfig::without_secrets`]），
/// 此时整份文件不含敏感信息，`crypto` 为 `None`。
#[derive(Debug, Serialize, Deserialize)]
struct SessionsFile {
    /// 加密文件头：保存 DEK 的保护方式。
    ///
    /// 模式 1（已设主口令）时头内带 KDF 参数，读取后由主口令派生 DEK；模式 0（默认，
    /// 未设主口令）时 DEK 是随机密钥、存在系统钥匙串里，头内 KDF 相关字段为空，读取后
    /// 直接用钥匙串里的 DEK。导出「仅连接配置」时不含凭证，无需加密头，故为 `Option`。
    crypto: Option<CryptoHeader>,
    /// 会话列表（凭据字段为密文信封，可为 `None`）。
    sessions: Vec<SessionConfig>,
}

impl SessionStore {
    /// 创建存储实例，定位并准备好配置目录。
    ///
    /// # 错误
    /// 当无法定位配置目录或创建目录失败时返回 [`ConfigError::ConfigDir`] / [`ConfigError::Store`]。
    pub fn new() -> Result<Self, ConfigError> {
        let base =
            config_dir().ok_or_else(|| ConfigError::ConfigDir("无法定位 XDG 配置目录".into()))?;
        let dir = base.join("rterm");
        fs::create_dir_all(&dir)
            .map_err(|e| ConfigError::Store(format!("创建配置目录失败: {e}")))?;
        let path = dir.join("sessions.toml");
        debug!("会话配置文件路径: {}", path.display());
        Ok(Self { path })
    }

    /// 返回会话配置文件的绝对路径。
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// 读取加密文件头（含 salt / KDF 参数 / 校验哨兵）。
    ///
    /// 文件不存在、解析失败或文件中不含加密头（如仅连接配置的导出文件）时返回错误。
    /// 调用方据此区分：文件不存在即为首次运行（由 GUI 生成随机密钥建模式 0），
    /// 无加密头则为导入的「仅连接配置」文件。
    pub fn load_crypto_header(&self) -> Result<CryptoHeader, ConfigError> {
        let content = fs::read_to_string(&self.path)
            .map_err(|e| ConfigError::Store(format!("读取会话文件失败: {e}")))?;
        let file: SessionsFile = toml::from_str(&content)?;
        file.crypto.ok_or_else(|| {
            ConfigError::Store("会话文件不含加密头（可能为仅连接配置的导出文件）".into())
        })
    }

    /// 加载全部会话配置。
    ///
    /// 若文件不存在，返回空列表（视为首次运行）。
    pub fn load(&self) -> Result<Vec<SessionConfig>, ConfigError> {
        if !self.path.exists() {
            debug!("会话文件不存在，返回空列表: {}", self.path.display());
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)
            .map_err(|e| ConfigError::Store(format!("读取会话文件失败: {e}")))?;
        let file: SessionsFile = toml::from_str(&content)?;
        debug!("已加载 {} 个会话配置", file.sessions.len());
        Ok(file.sessions)
    }

    /// 将全部会话配置与加密文件头写入 `sessions.toml` 并收紧文件权限（Unix 下仅属主可读写）。
    ///
    /// `header` 来自已解锁的 [`rterm_crypto::Vault`]，保证密文信封可被同一主密钥还原。
    ///
    /// # 错误
    /// 当序列化或写入失败时返回 [`ConfigError::Store`]。
    pub fn save(
        &self,
        sessions: &[SessionConfig],
        header: &CryptoHeader,
    ) -> Result<(), ConfigError> {
        let content = toml::to_string_pretty(&SessionsFile {
            crypto: Some(header.clone()),
            sessions: sessions.to_vec(),
        })?;
        fs::write(&self.path, content)
            .map_err(|e| ConfigError::Store(format!("写入会话文件失败: {e}")))?;
        // 会话含密文凭据，Unix 下限制为仅属主可读写，降低泄露风险。
        #[cfg(unix)]
        fs::set_permissions(&self.path, PermissionsExt::from_mode(0o600))
            .map_err(|e| ConfigError::Store(format!("设置会话文件权限失败: {e}")))?;
        debug!(
            "已保存 {} 个会话配置到 {}",
            sessions.len(),
            self.path.display()
        );
        Ok(())
    }
}

/// 将会话列表序列化为 TOML 写入任意路径（导出用）。
///
/// 仅写入**连接配置**：凭据字段（密码 / 私钥口令）会被 [`SessionConfig::without_secrets`]
/// 剥离，且不含 `[crypto]` 加密头，因此导出文件不携带任何敏感信息，可安全分享；
/// 导入方需在编辑器中补填密码后才能连接。导出文件不收紧权限（明文连接信息无保密必要）。
pub fn export_sessions(path: &Path, sessions: &[SessionConfig]) -> Result<(), ConfigError> {
    let stripped: Vec<SessionConfig> = sessions
        .iter()
        .map(SessionConfig::without_secrets)
        .collect();
    let content = toml::to_string_pretty(&SessionsFile {
        crypto: None,
        sessions: stripped,
    })?;
    fs::write(path, content).map_err(|e| ConfigError::Store(format!("写入导出文件失败: {e}")))?;
    debug!(
        "已导出 {} 个会话配置（不含凭证）到 {}",
        sessions.len(),
        path.display()
    );
    Ok(())
}

/// 从任意 TOML 路径解析会话列表（导入用），结构须与 [`export_sessions`] 输出一致。
///
/// 导入的会话凭据通常为 `None`（仅连接配置）；若文件含 `[crypto]` 头与密文信封
/// （旧版完整导出），则其信封由导入方当前保险库的主密钥解密——主密码与导出方一致才可还原。
pub fn import_sessions(path: &Path) -> Result<Vec<SessionConfig>, ConfigError> {
    let content = fs::read_to_string(path)
        .map_err(|e| ConfigError::Store(format!("读取导入文件失败: {e}")))?;
    let file: SessionsFile = toml::from_str(&content)?;
    debug!(
        "已从 {} 解析出 {} 个会话配置",
        path.display(),
        file.sessions.len()
    );
    Ok(file.sessions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuthMethod, SessionConfig};
    use rterm_crypto::Vault;

    /// 构造一个带密文密码信封的会话（用于验证导出会剥离凭证）。
    fn secret_session() -> SessionConfig {
        let vault = Vault::new("any-passphrase");
        let env = vault.encrypt("topsecret");
        SessionConfig {
            id: "sess-test".into(),
            name: "demo".into(),
            host: "example.com".into(),
            port: 22,
            username: "root".into(),
            auth: AuthMethod::Password {
                password: Some(env),
            },
            group: None,
        }
    }

    #[test]
    fn export_sessions_strips_credentials_and_crypto_header() {
        let tmp =
            std::env::temp_dir().join(format!("rterm_export_test_{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&tmp);

        let sessions = vec![secret_session()];
        export_sessions(&tmp, &sessions).expect("导出应成功");

        // 导出文件不应包含任何加密头段，避免泄露 salt / KDF 参数。
        let raw = std::fs::read_to_string(&tmp).expect("应能读取导出文件");
        assert!(
            !raw.contains("[crypto]"),
            "导出文件不得包含 [crypto] 段（不应携带加密头）"
        );

        // 读回后凭据必须为 None：导出仅含连接配置。
        let imported = import_sessions(&tmp).expect("导入应成功");
        assert_eq!(imported.len(), 1);
        match &imported[0].auth {
            AuthMethod::Password { password } => {
                assert!(password.is_none(), "导出后密码字段必须为 None（不含凭证）")
            }
            _ => panic!("认证方式不应改变"),
        }

        // without_secrets 自身也应将密码置空且保留其余连接信息。
        let stripped = sessions[0].without_secrets();
        match &stripped.auth {
            AuthMethod::Password { password } => assert!(password.is_none()),
            _ => panic!("without_secrets 不应改变认证方式"),
        }
        assert_eq!(stripped.host, "example.com");
        assert_eq!(stripped.username, "root");

        let _ = std::fs::remove_file(&tmp);
    }
}
