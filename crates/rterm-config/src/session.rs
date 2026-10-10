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

/// 一跳跳板机（ProxyJump）配置。
///
/// 连接目标主机前，先逐跳建立隧道再在最后一跳上转发到目标。字段与目标会话同构，
/// 凭据沿用同一套 [`AuthMethod`] 信封加密。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JumpHost {
    /// 跳板机主机地址（域名或 IP）。
    pub host: String,
    /// 跳板机 SSH 端口（默认 22）。
    #[serde(default = "default_port")]
    pub port: u16,
    /// 登录跳板机的用户名。
    pub username: String,
    /// 连接跳板机采用的认证方式（凭据信封由当前保险库 DEK 加密）。
    pub auth: AuthMethod,
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
    /// 跳板机链（由外到内，`jumps[0]` 最先连接）；空表示直连目标主机。
    #[serde(default)]
    pub jumps: Vec<JumpHost>,
}

impl SessionConfig {
    /// 返回剥离凭据的副本：密码与私钥口令置为 `None`，其余字段（`id` / `name` / `group`
    /// 与连接参数，含跳板机链的连接信息）原样保留。用于导出「仅连接配置」——导出文件不携带
    /// 任何敏感信息，接收方需在编辑器中补填密码后才能连接。
    pub fn without_secrets(&self) -> SessionConfig {
        SessionConfig {
            auth: strip_credentials(&self.auth),
            jumps: self
                .jumps
                .iter()
                .map(|j| JumpHost {
                    auth: strip_credentials(&j.auth),
                    ..j.clone()
                })
                .collect(),
            ..self.clone()
        }
    }
}

/// 剥离 [`AuthMethod`] 中的凭据信封：密码与私钥口令置为 `None`，认证方式与路径保留。
fn strip_credentials(auth: &AuthMethod) -> AuthMethod {
    match auth {
        AuthMethod::Password { .. } => AuthMethod::Password { password: None },
        AuthMethod::PublicKey { key_path, .. } => AuthMethod::PublicKey {
            key_path: key_path.clone(),
            passphrase: None,
        },
        AuthMethod::Agent => AuthMethod::Agent,
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

#[cfg(test)]
mod tests {
    use super::*;
    use rterm_crypto::Vault;

    /// 构造一个带跳板链、且目标与每跳都带密文凭据的会话。
    fn session_with_jumps(vault: &Vault) -> SessionConfig {
        let jump = |host: &str| JumpHost {
            host: host.to_string(),
            port: 2222,
            username: "jumpuser".to_string(),
            auth: AuthMethod::Password {
                password: Some(vault.encrypt("jump-secret")),
            },
        };
        SessionConfig {
            id: "sess-jumps".into(),
            name: "behind-bastion".into(),
            host: "10.0.0.9".into(),
            port: 22,
            username: "root".into(),
            auth: AuthMethod::Password {
                password: Some(vault.encrypt("target-secret")),
            },
            group: None,
            jumps: vec![jump("bastion-a"), jump("bastion-b")],
        }
    }

    #[test]
    fn jumps_survive_a_serde_round_trip() {
        let vault = Vault::new("test-master");
        let cfg = session_with_jumps(&vault);

        let toml = toml::to_string(&cfg).expect("serialize session with jumps");
        let back: SessionConfig = toml::from_str(&toml).expect("deserialize session with jumps");

        assert_eq!(back.jumps.len(), 2);
        assert_eq!(back.jumps[0].host, "bastion-a");
        assert_eq!(back.jumps[0].port, 2222);
        assert_eq!(back.jumps[1].host, "bastion-b");
        assert_eq!(back.jumps[0], cfg.jumps[0]);
    }

    /// 旧版 `sessions.toml` 不含 `jumps`：读出为空链（直连语义），无需迁移代码。
    #[test]
    fn legacy_config_without_jumps_reads_as_direct() {
        let vault = Vault::new("test-master");
        let full = toml::to_string(&session_with_jumps(&vault)).expect("serialize");
        // 直接删掉 `jumps` 键，模拟旧版文件（比手写 TOML 更贴近真实序列化形态）。
        let mut value: toml::Value = toml::from_str(&full).expect("parse back");
        value
            .as_table_mut()
            .expect("session is a table")
            .remove("jumps");
        let legacy = toml::to_string(&value).expect("re-serialize legacy file");

        let cfg: SessionConfig = toml::from_str(&legacy).expect("legacy config must still load");
        assert!(cfg.jumps.is_empty(), "缺省 jumps 必须为空链（直连）");
        assert_eq!(cfg.host, "10.0.0.9");
    }

    /// 跳板机的 `port` 缺省为 22。
    #[test]
    fn jump_port_defaults_to_ssh_port() {
        let jump = JumpHost {
            host: "bastion".into(),
            port: 2200,
            username: "ops".into(),
            auth: AuthMethod::Agent,
        };
        let full = toml::to_string(&jump).expect("serialize jump");
        let mut value: toml::Value = toml::from_str(&full).expect("parse back");
        value
            .as_table_mut()
            .expect("jump is a table")
            .remove("port");

        let back: JumpHost = toml::from_str(&toml::to_string(&value).expect("re-serialize"))
            .expect("jump config must load");
        assert_eq!(back.port, 22);
    }

    /// 导出「仅连接配置」时，目标与**每一跳**跳板机的凭据信封都必须被剥离。
    #[test]
    fn without_secrets_strips_target_and_every_jump() {
        let vault = Vault::new("test-master");
        let stripped = session_with_jumps(&vault).without_secrets();

        match &stripped.auth {
            AuthMethod::Password { password } => assert!(password.is_none()),
            _ => panic!("目标认证方式不应改变"),
        }
        assert_eq!(stripped.jumps.len(), 2);
        for jump in &stripped.jumps {
            match &jump.auth {
                AuthMethod::Password { password } => {
                    assert!(password.is_none(), "跳板机凭据必须被剥离")
                }
                _ => panic!("跳板机认证方式不应改变"),
            }
        }
        // 连接信息原样保留。
        assert_eq!(stripped.jumps[0].host, "bastion-a");
        assert_eq!(stripped.jumps[1].username, "jumpuser");
    }
}
