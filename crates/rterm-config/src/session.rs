//! 会话连接相关的数据模型定义。
//!
//! 这些类型既用于内存中的状态管理，也通过 [`serde`] 持久化到 `sessions.toml`。

use rterm_crypto::Envelope;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 认证方式枚举。
///
/// 凭据（密码 / 私钥口令）以[`Envelope`]（AES-256-GCM 密文）随会话配置保存：
/// 模式 0 由钥匙串里的随机 DEK 加密、模式 1 由主口令派生的 DEK 加密。
/// 明文仅在解密后短暂存在于内存（见 `rterm-gui` 的解锁流程）。
/// 凭据字段为 [`Option`]，允许「缺省」状态——例如从仅含连接配置的导入文件
/// 得到的会话尚未携带凭据，需用户在编辑器中补填后再连接。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    /// 使用密码认证。
    Password {
        /// 加密信封（由当前保险库 DEK 加密：模式 0 为钥匙串随机密钥、模式 1 为
        /// 主口令派生）。`None` 表示尚未设置密码。
        password: Option<Envelope>,
    },
    /// 使用公钥认证。
    PublicKey {
        /// 私钥文件路径。
        key_path: PathBuf,
        /// 私钥已加密时提供；为 `None` 表示私钥无口令，或尚未设置过口令。
        passphrase: Option<Envelope>,
    },
    /// 使用 SSH Agent 转发认证。
    Agent,
}

/// 单个 SSH 会话连接配置。
///
/// 该结构可序列化，持久化于 `~/.config/rterm/sessions.toml`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// 用于 UI 与连接映射。
    pub id: String,
    /// 会话显示名称（用户可读）。
    pub name: String,
    /// 远程主机地址（域名或 IP）。
    pub host: String,
    /// 连接使用的 TCP 端口（默认 22）。
    #[serde(default = "default_port")]
    pub port: u16,
    /// 登录用户名。
    pub username: String,
    /// 采用的认证方式。
    pub auth: AuthMethod,
    /// 所属分组名（用于侧边栏分组，可选）。
    #[serde(default)]
    pub group: Option<String>,
}

impl SessionConfig {
    /// 返回剥离凭据的副本：密码与私钥口令置为 `None`，其余字段（`id` / `name` / `group`
    /// 与连接参数）原样保留。用于导出「仅连接配置」——导出文件不携带任何敏感信息，
    /// 接收方需在编辑器中补填密码后才能连接。
    pub fn without_secrets(&self) -> SessionConfig {
        let auth = match &self.auth {
            AuthMethod::Password { .. } => AuthMethod::Password { password: None },
            AuthMethod::PublicKey { key_path, .. } => AuthMethod::PublicKey {
                key_path: key_path.clone(),
                passphrase: None,
            },
            AuthMethod::Agent => AuthMethod::Agent,
        };
        SessionConfig {
            auth,
            ..self.clone()
        }
    }
}

/// 生成新的会话唯一标识。
///
/// 基于当前时间戳生成可读的唯一 ID，避免引入额外依赖。
pub fn new_id() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("sess-{ts}")
}

/// SSH 连接的默认 TCP 端口（22），供 `SessionConfig` 缺省值使用。
fn default_port() -> u16 {
    crate::DEFAULT_SSH_PORT
}
