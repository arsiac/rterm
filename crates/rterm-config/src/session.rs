//! 会话连接相关的数据模型定义。
//!
//! 这些类型既用于内存中的状态管理，也通过 [`serde`] 持久化到 `sessions.toml`。

use log::warn;
use rterm_crypto::{Envelope, Vault};
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

/// 端口转发类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardKind {
    /// 本地转发（`ssh -L`）：客户端监听 `bind`，每条连接由**服务端**连向 `target`。
    Local,
    /// 远程转发（`ssh -R`）：**服务端**监听 `bind`，连接送回客户端后由客户端连向 `target`。
    Remote,
    /// 动态转发（`ssh -D`，SOCKS5）：客户端监听 `bind`，目的地址来自每条连接的 SOCKS 请求。
    Dynamic,
}

/// 一条端口转发规则，随会话配置持久化。
///
/// `forwards` 列表中的规则在连接建立时按 `enabled` 自动启动（见 rterm-core
/// 的转发引擎）；已连接会话上也可通过引擎的增量 API 临时增减 / 启停。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardRule {
    /// 转发类型。
    pub kind: ForwardKind,
    /// 监听（Local / Dynamic）或声明（Remote）的绑定地址；留空表示 `127.0.0.1`。
    #[serde(default)]
    pub bind_address: String,
    /// 监听 / 声明的绑定端口（1..=65535，不允许 0）。
    pub bind_port: u16,
    /// 目的主机：Local / Remote 留空（或 `None`）按 `127.0.0.1` 处理，Dynamic 恒为 `None`。
    #[serde(default)]
    pub target_host: Option<String>,
    /// 目的端口：Local / Remote 必填，Dynamic 恒为 `None`。
    #[serde(default)]
    pub target_port: Option<u16>,
    /// 启停状态；停用的规则不随连接自动建立，状态随会话持久化。
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl ForwardRule {
    /// 生效的绑定地址：留空（或纯空白）按 `127.0.0.1` 处理。
    pub fn bind_addr(&self) -> &str {
        let addr = self.bind_address.trim();
        if addr.is_empty() { "127.0.0.1" } else { addr }
    }

    /// 目的主机（去除首尾空白；`None` 或空白视为未填）。
    pub fn target_host_trimmed(&self) -> Option<&str> {
        self.target_host
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
    }

    /// 生效的目的主机：Local / Remote 留空（或纯空白）按 `127.0.0.1` 处理。
    pub fn target_addr(&self) -> &str {
        self.target_host_trimmed().unwrap_or("127.0.0.1")
    }

    /// 结构完整性校验（端口范围与 Local / Remote 的目的字段约束）。
    ///
    /// 在编辑器保存与转发引擎启动前各兜一道底；返回的静态文案面向日志（英文），
    /// 用户可见的逐字段提示由 GUI 编辑器自行产出。
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.bind_port == 0 {
            return Err("bind port must be between 1 and 65535");
        }
        match self.kind {
            ForwardKind::Dynamic => {
                if self.target_host_trimmed().is_some() || self.target_port.is_some() {
                    Err("dynamic forwarding must not carry a fixed target")
                } else {
                    Ok(())
                }
            }
            ForwardKind::Local | ForwardKind::Remote => {
                if self.target_port.is_none() {
                    Err("local/remote forwarding requires a target port")
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// ForwardRule::enabled 的 serde 缺省值（规则默认启用）。
fn default_true() -> bool {
    true
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
    /// 连接成功后自动下发到远端 shell 的启动作业（明文，非凭据）；`None` 或纯空白表示不执行，
    /// 是否真正下发还取决于 [`TerminalConfig::run_startup_command`](crate::TerminalConfig::run_startup_command)。
    #[serde(default)]
    pub startup_command: Option<String>,
    /// 端口转发规则（随会话持久化）；连接建立时仅自动启动 enabled 的规则。
    #[serde(default)]
    pub forwards: Vec<ForwardRule>,
    /// 是否在会话列表顶部收藏区展示；属用户意图，随会话文件导出 / 导入。
    #[serde(default)]
    pub favorite: bool,
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
    /// 用新保险库重新加密本会话的全部凭据（含每一跳），旧保险库用于先解密。
    /// `None` 信封与 `Agent` 认证原样保留；返回 `(重加密后的会话, 被略过的凭据条数)`——
    /// 解不开的信封置 `None` 并记 `warn`，调用方**须把条数报给用户**，静默丢弃会产出看似完整的备份。
    pub fn rekeyed(&self, old: &Vault, new: &Vault) -> (SessionConfig, usize) {
        let mut dropped = 0usize;
        let jumps = self
            .jumps
            .iter()
            .map(|j| {
                let (auth, d) = rekey_auth(&j.auth, &self.id, old, new);
                dropped += d;
                JumpHost { auth, ..j.clone() }
            })
            .collect();
        let (auth, d) = rekey_auth(&self.auth, &self.id, old, new);
        dropped += d;
        (
            SessionConfig {
                auth,
                jumps,
                ..self.clone()
            },
            dropped,
        )
    }
}

/// 用新保险库重新加密单个 [`AuthMethod`] 的凭据信封；返回替换后的值与被略过的条数（`0` 或 `1`）。
///
/// 解密失败（密钥已换 / 信封损坏）时该凭据置 `None` 并记 `warn`，认证方式与私钥路径保留。
fn rekey_auth(
    auth: &AuthMethod,
    session_id: &str,
    old: &Vault,
    new: &Vault,
) -> (AuthMethod, usize) {
    let reencrypt = |env: &Envelope| -> (Option<Envelope>, usize) {
        match old.decrypt(env) {
            Ok(plain) => (Some(new.encrypt(plain.as_str())), 0),
            Err(e) => {
                warn!("Failed to reencrypt credential (session {session_id}): {e}");
                (None, 1)
            }
        }
    };
    match auth {
        AuthMethod::Password { password } => match password.as_ref().map(reencrypt) {
            Some((password, d)) => (AuthMethod::Password { password }, d),
            None => (AuthMethod::Password { password: None }, 0),
        },
        AuthMethod::PublicKey {
            key_path,
            passphrase,
        } => match passphrase.as_ref().map(reencrypt) {
            Some((passphrase, d)) => (
                AuthMethod::PublicKey {
                    key_path: key_path.clone(),
                    passphrase,
                },
                d,
            ),
            None => (
                AuthMethod::PublicKey {
                    key_path: key_path.clone(),
                    passphrase: None,
                },
                0,
            ),
        },
        AuthMethod::Agent => (AuthMethod::Agent, 0),
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
            startup_command: None,
            forwards: Vec::new(),
            favorite: false,
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

    /// 启动作业随 serde 往返；旧文件（删掉该键）读出为 `None`；剥凭据与重加密后仍在。
    #[test]
    fn startup_command_round_trips_and_survives_transforms() {
        let vault = Vault::new("test-master");
        let cfg = SessionConfig {
            startup_command: Some("cd /srv && tmux attach".into()),
            ..session_with_jumps(&vault)
        };

        let toml = toml::to_string(&cfg).expect("serialize");
        let back: SessionConfig = toml::from_str(&toml).expect("deserialize");
        assert_eq!(
            back.startup_command.as_deref(),
            Some("cd /srv && tmux attach")
        );

        // 旧文件不含该键：读出为 None（无需迁移）。
        let mut value: toml::Value = toml::from_str(&toml).expect("parse back");
        value
            .as_table_mut()
            .expect("session is a table")
            .remove("startup_command");
        let legacy = toml::to_string(&value).expect("re-serialize legacy file");
        let legacy_cfg: SessionConfig =
            toml::from_str(&legacy).expect("legacy config must still load");
        assert!(legacy_cfg.startup_command.is_none());

        // 明文配置：剥凭据与重加密都原样携带。
        assert_eq!(
            cfg.without_secrets().startup_command.as_deref(),
            Some("cd /srv && tmux attach")
        );
        let (rekeyed, _) = cfg.rekeyed(&vault, &Vault::new("new"));
        assert_eq!(
            rekeyed.startup_command.as_deref(),
            Some("cd /srv && tmux attach")
        );
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

    /// `rekeyed`：目标 + 每一跳的凭据都换到新 DEK，明文不变。
    #[test]
    fn rekeyed_moves_every_credential_to_the_new_vault() {
        let old = Vault::new("old-master");
        let new = Vault::new("new-master");
        let cfg = session_with_jumps(&old);

        let (rekeyed, dropped) = cfg.rekeyed(&old, &new);
        assert_eq!(dropped, 0, "同一 DEK 加密的信封都应可解");
        assert_eq!(rekeyed.id, cfg.id, "重加密不应改动任何连接信息");
        assert_eq!(rekeyed.jumps.len(), 2);

        // 新 DEK 解得开，旧 DEK 解不开——证明确实换了密钥而非原样复制。
        let env = match &rekeyed.auth {
            AuthMethod::Password { password } => password.as_ref().expect("目标凭据应在"),
            other => panic!("认证方式不应改变，实际 {other:?}"),
        };
        assert_eq!(new.decrypt(env).unwrap().as_str(), "target-secret");
        assert!(old.decrypt(env).is_err(), "旧 DEK 不应再解得开");
        for jump in &rekeyed.jumps {
            match &jump.auth {
                AuthMethod::Password { password } => {
                    let env = password.as_ref().expect("跳板凭据应在");
                    assert_eq!(new.decrypt(env).unwrap().as_str(), "jump-secret");
                }
                other => panic!("跳板认证方式不应改变，实际 {other:?}"),
            }
        }
    }

    /// `rekeyed`：解不开的旧信封**逐条计入 `dropped`**（含跳板机），会话本身仍带出。
    /// 这是导出 / 导入回报「略过了几条口令」的依据——静默丢弃比直接失败更危险。
    #[test]
    fn rekeyed_counts_every_credential_it_cannot_decrypt() {
        let foreign = Vault::new("someone-elses-dek");
        let local = Vault::new("local");
        // 三条凭据（目标 + 两跳）全部由外来 DEK 加密，本机一条都解不开。
        let cfg = session_with_jumps(&foreign);

        let (rekeyed, dropped) = cfg.rekeyed(&local, &local);
        assert_eq!(dropped, 3, "目标与每一跳各计一条");
        match &rekeyed.auth {
            AuthMethod::Password { password } => {
                assert!(password.is_none(), "解不开的凭据须置空，不得留下死密文")
            }
            other => panic!("认证方式不应改变，实际 {other:?}"),
        }
        assert!(
            rekeyed.jumps.iter().all(
                |j| matches!(&j.auth, AuthMethod::Password { password } if password.is_none())
            ),
            "跳板机解不开的凭据同样置空"
        );
        assert_eq!(rekeyed.jumps.len(), 2, "会话与跳板链本身仍须完整带出");
    }

    /// `rekeyed`：`None` 凭据与 `Agent` 认证不计入略过数（它们本就没有凭据）。
    #[test]
    fn rekeyed_ignores_credentialless_auth_methods() {
        let vault = Vault::new("m");
        let cfg = SessionConfig {
            id: "sess-none".into(),
            name: "n".into(),
            host: "h".into(),
            port: 22,
            username: "u".into(),
            auth: AuthMethod::Password { password: None },
            group: None,
            jumps: vec![JumpHost {
                host: "j".into(),
                port: 22,
                username: "ju".into(),
                auth: AuthMethod::Agent,
            }],
            startup_command: None,
            forwards: Vec::new(),
            favorite: false,
        };

        let (rekeyed, dropped) = cfg.rekeyed(&vault, &vault);
        assert_eq!(dropped, 0, "无凭据不该被算成丢弃");
        match &rekeyed.auth {
            AuthMethod::Password { password } => assert!(password.is_none()),
            other => panic!("认证方式不应改变，实际 {other:?}"),
        }
        assert!(matches!(rekeyed.jumps[0].auth, AuthMethod::Agent));
    }

    /// 构造覆盖三种转发类型的规则清单。
    fn sample_forwards() -> Vec<ForwardRule> {
        vec![
            ForwardRule {
                kind: ForwardKind::Local,
                bind_address: String::new(),
                bind_port: 5432,
                target_host: Some("db.internal".into()),
                target_port: Some(5432),
                enabled: true,
            },
            ForwardRule {
                kind: ForwardKind::Remote,
                bind_address: "0.0.0.0".into(),
                bind_port: 8080,
                target_host: Some("localhost".into()),
                target_port: Some(80),
                enabled: false,
            },
            ForwardRule {
                kind: ForwardKind::Dynamic,
                bind_address: String::new(),
                bind_port: 1080,
                target_host: None,
                target_port: None,
                enabled: true,
            },
        ]
    }

    /// 转发规则随 serde 往返；enabled 缺省为 true；旧文件（删掉该键）读出为空表。
    #[test]
    fn forwards_round_trip_and_default_to_empty_on_legacy_files() {
        let vault = Vault::new("test-master");
        let cfg = SessionConfig {
            forwards: sample_forwards(),
            ..session_with_jumps(&vault)
        };

        let toml = toml::to_string(&cfg).expect("serialize");
        let back: SessionConfig = toml::from_str(&toml).expect("deserialize");
        assert_eq!(back.forwards, cfg.forwards);
        assert!(!back.forwards[1].enabled, "显式停用须原样往返");

        // 单条规则缺省 enabled：删掉该键后读出 true。
        let rule_toml = toml::to_string(&sample_forwards()[0]).expect("serialize rule");
        let mut value: toml::Value = toml::from_str(&rule_toml).expect("parse rule");
        value
            .as_table_mut()
            .expect("rule is a table")
            .remove("enabled");
        let rule: ForwardRule =
            toml::from_str(&toml::to_string(&value).expect("re-serialize")).expect("load rule");
        assert!(rule.enabled, "enabled 缺省必须为 true");

        // 旧文件不含 forwards 键：读出为空表（无需迁移）。
        let mut value: toml::Value = toml::from_str(&toml).expect("parse back");
        value
            .as_table_mut()
            .expect("session is a table")
            .remove("forwards");
        let legacy = toml::to_string(&value).expect("re-serialize legacy file");
        let legacy_cfg: SessionConfig =
            toml::from_str(&legacy).expect("legacy config must still load");
        assert!(legacy_cfg.forwards.is_empty(), "缺省 forwards 必须为空表");
    }

    /// bind_address 留空 / 空白按 127.0.0.1 处理，非空原样保留。
    #[test]
    fn bind_addr_defaults_to_loopback_when_blank() {
        let mut rule = sample_forwards()[0].clone();
        assert_eq!(rule.bind_addr(), "127.0.0.1", "留空 = 回环");
        rule.bind_address = "   ".into();
        assert_eq!(rule.bind_addr(), "127.0.0.1", "纯空白同样视为回环");
        rule.bind_address = "0.0.0.0".into();
        assert_eq!(rule.bind_addr(), "0.0.0.0");
    }

    /// validate：端口 0 拒绝；Local/Remote 必须带目的；Dynamic 必须不带。
    #[test]
    fn validate_rejects_incomplete_rules() {
        let local = sample_forwards()[0].clone();
        assert_eq!(local.validate(), Ok(()));

        // bind_port 0。
        let zero_port = ForwardRule {
            bind_port: 0,
            ..local.clone()
        };
        assert!(zero_port.validate().is_err());

        // Local 目的主机留空 / 纯空白 = 127.0.0.1，仍然合法。
        let mut no_host = local.clone();
        no_host.target_host = None;
        assert_eq!(no_host.validate(), Ok(()));
        assert_eq!(no_host.target_addr(), "127.0.0.1");
        let mut blank_host = local.clone();
        blank_host.target_host = Some("   ".into());
        assert_eq!(blank_host.validate(), Ok(()));
        assert_eq!(blank_host.target_addr(), "127.0.0.1", "纯空白同样视为回环");
        let mut explicit = local.clone();
        explicit.target_host = Some("  db.internal  ".into());
        assert_eq!(
            explicit.target_addr(),
            "db.internal",
            "非空目的主机去除首尾空白"
        );
        // Local 缺目的端口仍然非法。
        let mut no_port = local;
        no_port.target_port = None;
        assert!(no_port.validate().is_err());

        // Dynamic 带目的即非法。
        let mut dyn_rule = sample_forwards()[2].clone();
        assert_eq!(dyn_rule.validate(), Ok(()));
        dyn_rule.target_host = Some("localhost".into());
        assert!(dyn_rule.validate().is_err());
    }
}
