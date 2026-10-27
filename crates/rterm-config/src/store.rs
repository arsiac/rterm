//! 会话配置的加载与保存。
//!
//! 配置目录经 [`crate::paths`] 解析（按 XDG 规范定位，开发沙箱下为 `.dev/config`），
//! 并以 TOML 格式读写 `sessions.toml`。

use crate::{ConfigError, SessionConfig};
use log::debug;
use rterm_crypto::{CryptoHeader, Vault};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// 会话导出文件的格式标记：只有导出件携带此值（本机 `sessions.toml` 永不写），两侧据此互斥辨识——
/// 加载本机配置见到它即拒载，导入时也只在它存在时才承认「带凭据通道」。
pub const EXPORT_FORMAT: &str = "rterm-export/1";

/// 会话配置的持久化管理器（基于 `sessions.toml`）。
///
/// 定位 XDG 配置目录下的会话文件，提供加载、保存与路径访问能力。
#[derive(Clone)]
pub struct SessionStore {
    /// 会话文件在磁盘上的绝对路径（`sessions.toml`）。
    path: PathBuf,
}

/// 本机 `sessions.toml` **存在但不可用**的原因。与「不存在」严格区分：不存在是首启动，可建库写盘；
/// 不可用若被当首启动处理，就会用空会话覆盖用户已有文件。文本为底层技术错误，供 GUI 拼提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionsFileProblem {
    /// 读盘失败（权限 / I/O）。
    Unreadable(String),
    /// TOML 解析失败（文件被截断 / 手改坏）。
    Unparsable(String),
    /// 该文件是导出件（含 [`EXPORT_FORMAT`] 标记），不是本机配置。
    ExportFile,
    /// 缺少 `[crypto]` 加密头，无从解锁。
    MissingCryptoHeader,
}

/// 启动时会话文件的三态。
#[derive(Debug, Clone)]
pub enum SessionsFileState {
    /// 文件不存在——首启动，可安全建库并落盘。
    Absent,
    /// 文件可用，携带其加密文件头。
    Ready(CryptoHeader),
    /// 文件存在但不可用——调用方**不得写盘**，应把原因报给用户。
    Unusable(SessionsFileProblem),
}

/// TOML 文档根结构。
///
/// TOML 不允许文档根直接为数组，故用此表包裹会话列表，序列化为数组表 `[[sessions]]`；
/// `[crypto]` 段保存加密文件头（salt / KDF 参数 / 校验哨兵），与凭据信封共用同一主密钥。
/// 凭据字段在导出「仅连接配置」时会被剥离（见 [`SessionConfig::without_secrets`]），
/// 此时整份文件不含敏感信息，`crypto` 为 `None`。
#[derive(Debug, Serialize, Deserialize)]
struct SessionsFile {
    /// 导出格式标记：两类导出件都写入 [`EXPORT_FORMAT`]；本机配置为 `None`（不落盘，字节不变）。
    /// 旧版无标记的导出件按「仅连接配置」兼容处理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    format: Option<String>,
    /// 加密文件头：保存 DEK 的保护方式。
    ///
    /// 模式 1（已设主口令）时头内带 KDF 参数，读取后由主口令派生 DEK；模式 0（默认，
    /// 未设主口令）时 DEK 是随机密钥、存在系统钥匙串里，头内 KDF 相关字段为空，读取后
    /// 直接用钥匙串里的 DEK。导出「仅连接配置」时不含凭证，无需加密头，故为 `Option`。
    crypto: Option<CryptoHeader>,
    /// 会话列表（凭据字段为密文信封，可为 `None`）。
    sessions: Vec<SessionConfig>,
}

impl SessionsFile {
    /// 是否为导出件（携带导出格式标记）。
    fn is_export_file(&self) -> bool {
        self.format.is_some()
    }
}

impl SessionStore {
    /// 创建存储实例，定位并准备好配置目录。
    ///
    /// 目录由 [`crate::paths::config_dir`] 解析（开发沙箱下为 `.dev/config`）。
    ///
    /// # 错误
    /// 当无法定位配置目录或创建目录失败时返回 [`ConfigError::ConfigDir`] / [`ConfigError::Store`]。
    pub fn new() -> Result<Self, ConfigError> {
        let dir = crate::paths::config_dir()
            .ok_or_else(|| ConfigError::ConfigDir("cannot locate XDG config directory".into()))?;
        fs::create_dir_all(&dir)
            .map_err(|e| ConfigError::Store(format!("failed to create config directory: {e}")))?;
        let path = dir.join("sessions.toml");
        debug!("Session config file path: {}", path.display());
        Ok(Self { path })
    }

    /// 返回会话配置文件的绝对路径。
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// 读取加密文件头（含 salt / KDF 参数 / 校验哨兵）。文件不存在、解析失败、不含加密头，
    /// 或该位置上是**导出件**（[`ConfigError::ExportFileInPlace`]，绝不能当首启动重建）时返回错误。
    pub fn load_crypto_header(&self) -> Result<CryptoHeader, ConfigError> {
        let file = self
            .read_parsed()?
            .ok_or_else(|| ConfigError::Store("session file does not exist".into()))?;
        if file.is_export_file() {
            return Err(ConfigError::ExportFileInPlace);
        }
        file.crypto
            .ok_or_else(|| ConfigError::from(SessionsFileProblem::MissingCryptoHeader))
    }

    /// 加载全部会话配置。
    ///
    /// 若文件不存在，返回空列表（视为首次运行）。携带导出标记的文件一律拒绝：它的凭据信封
    /// 由导出口令（而非本机 DEK）加密，收下即等于写入解不了的密文。
    pub fn load(&self) -> Result<Vec<SessionConfig>, ConfigError> {
        let Some(file) = self.read_parsed()? else {
            debug!(
                "Session file not found, returning empty list: {}",
                self.path.display()
            );
            return Ok(Vec::new());
        };
        if file.is_export_file() {
            return Err(ConfigError::ExportFileInPlace);
        }
        debug!("Loaded {} session(s)", file.sessions.len());
        Ok(file.sessions)
    }

    /// 启动三态判定：文件不存在 / 可用（给出加密头）/ 不可用（给出原因）。调用方据此决定能否写盘：
    /// 只有 [`SessionsFileState::Absent`] 才是首启动可建库落盘；`Unusable` 若被当首启动处理，
    /// 会用空会话覆盖掉用户已有的文件。
    pub fn startup_state(&self) -> SessionsFileState {
        match self.read_parsed() {
            Ok(None) => SessionsFileState::Absent,
            Ok(Some(file)) if file.is_export_file() => {
                SessionsFileState::Unusable(SessionsFileProblem::ExportFile)
            }
            Ok(Some(file)) => match file.crypto {
                Some(header) => SessionsFileState::Ready(header),
                None => SessionsFileState::Unusable(SessionsFileProblem::MissingCryptoHeader),
            },
            Err(problem) => SessionsFileState::Unusable(problem),
        }
    }

    /// 读取并解析会话文件；文件不存在返回 `Ok(None)`。
    fn read_parsed(&self) -> Result<Option<SessionsFile>, SessionsFileProblem> {
        if !self.path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&self.path)
            .map_err(|e| SessionsFileProblem::Unreadable(e.to_string()))?;
        toml::from_str::<SessionsFile>(&content)
            .map(Some)
            .map_err(|e| SessionsFileProblem::Unparsable(e.to_string()))
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
            format: None,
            crypto: Some(header.clone()),
            sessions: sessions.to_vec(),
        })?;
        fs::write(&self.path, content)
            .map_err(|e| ConfigError::Store(format!("failed to write session file: {e}")))?;
        // 会话含密文凭据，Unix 下限制为仅属主可读写，降低泄露风险。
        #[cfg(unix)]
        fs::set_permissions(&self.path, PermissionsExt::from_mode(0o600)).map_err(|e| {
            ConfigError::Store(format!("failed to set session file permissions: {e}"))
        })?;
        debug!(
            "Saved {} session(s) to {}",
            sessions.len(),
            self.path.display()
        );
        Ok(())
    }
}

#[cfg(test)]
impl SessionStore {
    /// 以显式文件路径构造存储（仅测试用）：绕开 [`crate::paths::set_test_root`] 的全局状态根
    /// 改写，与其它改写状态根的测试可并行跑而不相互覆盖临时目录。
    fn at(path: PathBuf) -> Self {
        Self { path }
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
        format: Some(EXPORT_FORMAT.to_string()),
        crypto: None,
        sessions: stripped,
    })?;
    fs::write(path, content)
        .map_err(|e| ConfigError::Store(format!("failed to write export file: {e}")))?;
    debug!(
        "Exported {} session(s) (without credentials) to {}",
        sessions.len(),
        path.display()
    );
    Ok(())
}

/// 从任意 TOML 路径解析会话列表（导入用），结构须与 [`export_sessions`] 输出一致。
///
/// 只读**连接配置**通道：解析出的每条会话一律施加 [`SessionConfig::without_secrets`]，
/// 使「外来密文信封绝不入库」成为函数级不变量——外来信封由别的机器的 DEK 加密，
/// 收下即等于写进本机文件、且本机永远解不开。带凭据的导出件须走
/// [`import_credential_export`]（先用品令解出明文，再用本机 DEK 重新包裹）。
pub fn import_sessions(path: &Path) -> Result<Vec<SessionConfig>, ConfigError> {
    let content = fs::read_to_string(path)
        .map_err(|e| ConfigError::Store(format!("failed to read import file: {e}")))?;
    let file: SessionsFile = toml::from_str(&content)?;
    debug!(
        "Parsed {} session(s) from {}",
        file.sessions.len(),
        path.display()
    );
    Ok(file
        .sessions
        .iter()
        .map(SessionConfig::without_secrets)
        .collect())
}

/// 文件元信息（不触碰凭据），供上层决定分流。
#[derive(Debug, Clone)]
pub struct SessionsFileMeta {
    /// 是否为带凭据导出件：含导出标记**且**带 `[crypto]` 头。
    pub with_credentials: bool,
    /// 是否携带导出标记（区分「裸拷贝的 sessions.toml」与旧版导出件）。
    pub is_export_file: bool,
    /// 是否含 `[crypto]` 头：带它却没有导出标记的文件是别机器的本机配置，须拒绝。
    pub has_crypto_header: bool,
}

/// 读取导出文件的元信息（导入前判定用），不解密任何凭据。调用方据 [`SessionsFileMeta`] 分流：
/// 带凭据 → [`import_credential_export`]，带标记无凭据 → [`import_sessions`]；**无导出标记却带
/// `[crypto]` 头**（裸拷贝的他机配置）→ 拒绝——绝不拿用户口令去解非导出文件，那是口令试探 oracle。
pub fn inspect_sessions_file(path: &Path) -> Result<SessionsFileMeta, ConfigError> {
    let content = fs::read_to_string(path)
        .map_err(|e| ConfigError::Store(format!("failed to read import file: {e}")))?;
    let file: SessionsFile = toml::from_str(&content)?;
    Ok(SessionsFileMeta {
        with_credentials: file.is_export_file() && file.crypto.is_some(),
        is_export_file: file.is_export_file(),
        has_crypto_header: file.crypto.is_some(),
    })
}

/// 带凭据导出：把每条会话的凭据从本机 DEK 重包裹为 `export`（[`Vault::for_export`] 现造的
/// 一次性保险库，头随文件落盘、导入端按头内参数还原）的 DEK 后写盘，全程不落明文。
/// 返回被略过的凭据条数（本机解不开的信封）——调用方须报给用户，否则会得到看似完整、实际缺口令的备份。
pub fn export_sessions_with_credentials(
    path: &Path,
    sessions: &[SessionConfig],
    local: &Vault,
    export: &Vault,
) -> Result<usize, ConfigError> {
    let mut dropped = 0usize;
    let rewrapped: Vec<SessionConfig> = sessions
        .iter()
        .map(|cfg| {
            let (cfg, d) = cfg.rekeyed(local, export);
            dropped += d;
            cfg
        })
        .collect();
    let content = toml::to_string_pretty(&SessionsFile {
        format: Some(EXPORT_FORMAT.to_string()),
        crypto: Some(export.header().clone()),
        sessions: rewrapped,
    })?;
    fs::write(path, content)
        .map_err(|e| ConfigError::Store(format!("failed to write export file: {e}")))?;
    // 凭据密文与导出口令的 KDF 参数都在文件里，Unix 下收紧为仅属主可读写。
    #[cfg(unix)]
    fs::set_permissions(path, PermissionsExt::from_mode(0o600))
        .map_err(|e| ConfigError::Store(format!("failed to set export file permissions: {e}")))?;
    debug!(
        "Exported {} session(s) (with credentials, {} credential(s) dropped) to {}",
        sessions.len(),
        dropped,
        path.display()
    );
    Ok(dropped)
}

/// 用导出口令解开带凭据导出件，并把每条凭据重包裹为 `local` 的本机 DEK。一步完成
/// 「读盘 + 按头内参数派生 + 哨兵校验 + 重包裹」使头与信封同源；口令错误（`CryptoError::Verify`）
/// 时**不改动任何会话**。返回重包裹后的会话列表与被略过的凭据条数；非导出件返回
/// [`ConfigError::ExportFileInPlace`]。
pub fn import_credential_export(
    path: &Path,
    passphrase: &str,
    local: &Vault,
) -> Result<(Vec<SessionConfig>, usize), ConfigError> {
    let content = fs::read_to_string(path)
        .map_err(|e| ConfigError::Store(format!("failed to read import file: {e}")))?;
    let file: SessionsFile = toml::from_str(&content)?;
    if !file.is_export_file() {
        return Err(ConfigError::ExportFileInPlace);
    }
    let header = file.crypto.ok_or_else(|| {
        ConfigError::Store("credential-bearing export file is missing its crypto header".into())
    })?;
    let export = Vault::unlock(passphrase, &header)?;

    let mut dropped = 0usize;
    let rewrapped: Vec<SessionConfig> = file
        .sessions
        .iter()
        .map(|cfg| {
            let (cfg, d) = cfg.rekeyed(&export, local);
            dropped += d;
            cfg
        })
        .collect();
    debug!(
        "Imported {} session(s) with credentials ({} credential(s) dropped) from {}",
        rewrapped.len(),
        dropped,
        path.display()
    );
    Ok((rewrapped, dropped))
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
            jumps: Vec::new(),
            startup_command: None,
            forwards: Vec::new(),
            favorite: false,
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

    /// 在唯一临时目录里构造一个会话文件路径（父目录已建）。
    fn tmp_sessions_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rterm_store_state_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("创建临时目录");
        dir.join("sessions.toml")
    }

    #[test]
    fn startup_state_absent_when_file_missing() {
        let path = tmp_sessions_path("absent");
        let store = SessionStore::at(path);
        assert!(matches!(store.startup_state(), SessionsFileState::Absent));
    }

    #[test]
    fn startup_state_ready_after_save() {
        let path = tmp_sessions_path("ready");
        let store = SessionStore::at(path.clone());
        let vault = Vault::new("m1");
        store.save(&[], vault.header()).expect("落盘");
        match store.startup_state() {
            SessionsFileState::Ready(header) => assert_eq!(header.salt, vault.header().salt),
            other => panic!("保存后应判为 Ready，实际 {other:?}"),
        }
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn startup_state_unusable_for_corrupt_and_headerless() {
        // 解析失败：坏 TOML 判为 Unusable(Unparsable)。
        let bad = tmp_sessions_path("corrupt");
        fs::write(&bad, "this is = = not valid toml").expect("写入坏文件");
        let store = SessionStore::at(bad.clone());
        assert!(
            matches!(
                store.startup_state(),
                SessionsFileState::Unusable(SessionsFileProblem::Unparsable(_))
            ),
            "坏 TOML 应判为 Unusable(Unparsable)"
        );

        // 缺加密头：合法 TOML 但无 [crypto] 段。
        let nohdr = tmp_sessions_path("nohdr");
        fs::write(&nohdr, "sessions = []\n").expect("写入无头文件");
        let store = SessionStore::at(nohdr.clone());
        assert!(
            matches!(
                store.startup_state(),
                SessionsFileState::Unusable(SessionsFileProblem::MissingCryptoHeader)
            ),
            "缺加密头应判为 Unusable(MissingCryptoHeader)"
        );

        let _ = fs::remove_file(bad);
        let _ = fs::remove_file(nohdr);
    }

    #[test]
    fn startup_state_and_load_reject_export_file_in_place() {
        // 把导出件放到 sessions.toml 位置：启动三态、load、load_crypto_header 三处都必须拒绝，
        // 且绝不能被当成首启动重建——否则会用空会话覆盖用户已有文件。
        let path = tmp_sessions_path("exportinplace");
        export_sessions(&path, &[secret_session()]).expect("导出");
        let store = SessionStore::at(path.clone());

        assert!(
            matches!(
                store.startup_state(),
                SessionsFileState::Unusable(SessionsFileProblem::ExportFile)
            ),
            "导出件应判为 Unusable(ExportFile)"
        );
        assert!(
            matches!(store.load(), Err(ConfigError::ExportFileInPlace)),
            "load 必须拒绝导出件"
        );
        assert!(
            matches!(
                store.load_crypto_header(),
                Err(ConfigError::ExportFileInPlace)
            ),
            "load_crypto_header 必须拒绝导出件"
        );
        // 拒绝路径不得改动文件字节。
        let before = fs::read(&path).expect("读导出件");
        let _ = store.startup_state();
        let _ = store.load();
        let _ = store.load_crypto_header();
        assert_eq!(before, fs::read(&path).expect("重读"), "拒载不得改动文件");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn plain_export_carries_format_marker_but_no_crypto_header() {
        let path = tmp_sessions_path("marker");
        export_sessions(&path, &[secret_session()]).expect("导出");
        let raw = fs::read_to_string(&path).expect("读导出件");
        assert!(
            raw.contains(&format!("format = \"{EXPORT_FORMAT}\"")),
            "导出件须带格式标记"
        );
        assert!(!raw.contains("[crypto]"), "仅连接配置导出不得含加密头段");
        // 标记存在即被 startup 三态识别为导出件（即便该文件被放到本机位置）。
        let store = SessionStore::at(path.clone());
        assert!(matches!(
            store.startup_state(),
            SessionsFileState::Unusable(SessionsFileProblem::ExportFile)
        ));
        let _ = fs::remove_file(&path);
    }

    /// 构造「目标密码 + 一跳跳板密码」都带凭据的会话。
    fn session_with_creds(vault: &Vault) -> SessionConfig {
        use crate::{AuthMethod, JumpHost};
        let jump = JumpHost {
            host: "jump.example.com".into(),
            port: 22,
            username: "jumper".into(),
            auth: AuthMethod::Password {
                password: Some(vault.encrypt("jump-secret")),
            },
        };
        SessionConfig {
            id: "sess-cred".into(),
            name: "demo".into(),
            host: "target.example.com".into(),
            port: 22,
            username: "root".into(),
            auth: AuthMethod::Password {
                password: Some(vault.encrypt("target-secret")),
            },
            group: None,
            jumps: vec![jump],
            startup_command: None,
            forwards: Vec::new(),
            favorite: false,
        }
    }

    /// 取会话明文凭据：`[目标, 跳板[0]]`。
    fn plaintext_creds(cfg: &SessionConfig, vault: &Vault) -> Vec<String> {
        use crate::AuthMethod;
        let target = match &cfg.auth {
            AuthMethod::Password { password } => vault
                .decrypt(password.as_ref().expect("目标密码应在"))
                .unwrap()
                .to_string(),
            _ => panic!("认证方式不应改变"),
        };
        let jump = match &cfg.jumps[0].auth {
            AuthMethod::Password { password } => vault
                .decrypt(password.as_ref().expect("跳板密码应在"))
                .unwrap()
                .to_string(),
            _ => panic!("跳板认证方式不应改变"),
        };
        vec![target, jump]
    }

    /// 带凭据导出后用正确口令导入，本机 vault 能解出目标与每一跳的原明文。
    #[test]
    fn credential_export_import_round_trips_through_the_export_passphrase() {
        let path = tmp_sessions_path("cred_roundtrip");
        let local = Vault::new("local-master");
        let sessions = vec![session_with_creds(&local)];

        let dropped = export_sessions_with_credentials(
            &path,
            &sessions,
            &local,
            &Vault::for_export("exp-pw"),
        )
        .expect("带凭据导出应成功");
        assert_eq!(dropped, 0, "本机凭据都应可解，无略过");

        let (imported, dropped) =
            import_credential_export(&path, "exp-pw", &local).expect("正确口令应能导入");
        assert_eq!(dropped, 0, "导出密钥可解，无略过");
        assert_eq!(imported.len(), 1);
        assert_eq!(
            plaintext_creds(&imported[0], &local),
            vec!["target-secret".to_string(), "jump-secret".to_string()],
            "导入后凭据须被重包裹回本机 DEK 并还原原明文"
        );
        let _ = fs::remove_file(&path);
    }

    /// 错误口令报 `Verify`，且不产出任何会话（零副作用）。
    #[test]
    fn credential_import_with_wrong_passphrase_fails_before_touching_sessions() {
        let path = tmp_sessions_path("cred_wrong_pw");
        let local = Vault::new("local-master");
        export_sessions_with_credentials(
            &path,
            &[session_with_creds(&local)],
            &local,
            &Vault::for_export("right-pw"),
        )
        .expect("导出");

        let err = import_credential_export(&path, "wrong-pw", &local)
            .expect_err("错误口令必须拒绝，不得产出会话");
        assert!(
            matches!(err, ConfigError::Crypto(rterm_crypto::CryptoError::Verify)),
            "错误口令应映射为 CryptoError::Verify，实际 {err:?}"
        );
        let _ = fs::remove_file(&path);
    }

    /// 模式 0（钥匙串随机 DEK）的保险库同样可作为带凭据导出的源与导入的目标。
    #[test]
    fn credential_export_works_from_a_mode0_random_key_vault() {
        let path = tmp_sessions_path("cred_mode0");
        let local = Vault::create_random();
        assert!(!local.header().master_password_set);

        let dropped = export_sessions_with_credentials(
            &path,
            &[session_with_creds(&local)],
            &local,
            &Vault::for_export("pw"),
        )
        .expect("模式 0 也应能带凭据导出");
        assert_eq!(dropped, 0);

        let (imported, _) = import_credential_export(&path, "pw", &local).expect("导入");
        assert_eq!(plaintext_creds(&imported[0], &local).len(), 2);
        let _ = fs::remove_file(&path);
    }

    /// 带凭据导出件不含任何明文，也不含本机 vault 的 salt（只有导出口令的 salt）。
    #[test]
    fn credential_export_leaks_neither_plaintext_nor_local_salt() {
        let path = tmp_sessions_path("cred_leak");
        let local = Vault::new("local-master");
        export_sessions_with_credentials(
            &path,
            &[session_with_creds(&local)],
            &local,
            &Vault::for_export("exp-pw"),
        )
        .expect("导出");
        let raw = fs::read_to_string(&path).expect("读导出件");

        for leak in ["target-secret", "jump-secret", "exp-pw", "local-master"] {
            assert!(!raw.contains(leak), "导出件不得含明文 {leak}");
        }
        assert!(
            !raw.contains(&local.header().salt),
            "导出件不得含本机 vault 的 salt"
        );
        assert!(
            raw.contains("[crypto]") && raw.contains(&format!("format = \"{EXPORT_FORMAT}\"")),
            "带凭据导出件应同时含加密头与导出标记"
        );
        let _ = fs::remove_file(&path);
    }

    /// 旧导出件（无 `format`）按「仅连接配置」兼容读入，不作废用户已有文件。
    #[test]
    fn legacy_export_file_without_marker_still_imports() {
        let path = tmp_sessions_path("legacy_plain");
        fs::write(&path, "[[sessions]]\nid = \"sess-1\"\nname = \"n\"\nhost = \"h\"\nport = 22\nusername = \"u\"\n[sessions.auth.Password]\n").expect("写旧导出件");
        let meta = inspect_sessions_file(&path).expect("旧导出件应可判定");
        assert!(!meta.is_export_file && !meta.has_crypto_header);

        let imported = import_sessions(&path).expect("旧导出件应可导入");
        assert_eq!(imported.len(), 1);
        let _ = fs::remove_file(&path);
    }

    /// `inspect_sessions_file` 对四种文件形态的判定。
    #[test]
    fn inspect_sessions_file_classifies_all_four_shapes() {
        let local = Vault::new("m");

        // (a) 仅连接配置导出（新）：有标记、无加密头。
        let plain = tmp_sessions_path("shape_plain");
        export_sessions(&plain, &[session_with_creds(&local)]).expect("导出");
        let meta = inspect_sessions_file(&plain).expect("判定");
        assert!(meta.is_export_file && !meta.has_crypto_header && !meta.with_credentials);

        // (b) 带凭据导出：有标记、有加密头。
        let cred = tmp_sessions_path("shape_cred");
        export_sessions_with_credentials(
            &cred,
            &[session_with_creds(&local)],
            &local,
            &Vault::for_export("e"),
        )
        .expect("导出");
        let meta = inspect_sessions_file(&cred).expect("判定");
        assert!(meta.with_credentials && meta.is_export_file && meta.has_crypto_header);

        // (c) 裸拷贝的 sessions.toml：无标记、有加密头 → 须被判为不可走口令通道。
        let live = tmp_sessions_path("shape_live");
        let store = SessionStore::at(live.clone());
        store
            .save(&[session_with_creds(&local)], local.header())
            .expect("保存");
        let meta = inspect_sessions_file(&live).expect("判定");
        assert!(
            !meta.is_export_file && meta.has_crypto_header && !meta.with_credentials,
            "裸拷贝不得被误判为带凭据导出件"
        );

        // (d) 旧导出件：无标记、无加密头。
        let legacy = tmp_sessions_path("shape_legacy");
        fs::write(&legacy, "sessions = []\n").expect("写文件");
        let meta = inspect_sessions_file(&legacy).expect("判定");
        assert!(!meta.is_export_file && !meta.has_crypto_header);
        assert!(import_sessions(&legacy).expect("导入").is_empty());

        for p in [plain, cred, live, legacy] {
            let _ = fs::remove_file(p);
        }
    }

    /// 非口令通道导入对外来信封一律剥离——「外来密文绝不入库」的函数级不变量。
    #[test]
    fn import_sessions_never_adopts_foreign_envelopes() {
        // 别机器（不同 DEK）的 sessions.toml 直接被当导入文件选中：今天会原样收下并挂本机头落盘，
        // 导入显示成功、连接必挂。现在必须剥离全部凭据。
        let foreign = Vault::new("other-machine-master");
        let path = tmp_sessions_path("foreign_live");
        let raw_live = {
            let store = SessionStore::at(path.clone());
            store
                .save(&[session_with_creds(&foreign)], foreign.header())
                .expect("构造外来 live 文件");
            fs::read_to_string(&path).expect("读")
        };
        assert!(
            raw_live.contains("[crypto]"),
            "前置条件：外来文件应带加密头"
        );

        let imported = import_sessions(&path).expect("导入");
        assert_eq!(imported.len(), 1);
        for cfg in &imported {
            match &cfg.auth {
                crate::AuthMethod::Password { password } => {
                    assert!(password.is_none(), "外来信封必须被剥离，绝不入库")
                }
                _ => panic!("认证方式不应改变"),
            }
            match &cfg.jumps[0].auth {
                crate::AuthMethod::Password { password } => {
                    assert!(password.is_none(), "跳板外来信封也必须被剥离")
                }
                _ => panic!("跳板认证方式不应改变"),
            }
        }
        // 连接信息仍完整保留（仅剥离凭据）。
        assert_eq!(imported[0].host, "target.example.com");
        assert_eq!(imported[0].jumps[0].host, "jump.example.com");
        let _ = fs::remove_file(&path);
    }

    /// Unix 下带凭据导出文件权限收紧为 `0600`；无凭据会话原样往返、`dropped == 0`。
    #[cfg(unix)]
    #[test]
    fn credential_export_tightens_permissions_and_keeps_credentialless_sessions() {
        use std::os::unix::fs::PermissionsExt;
        let path = tmp_sessions_path("cred_perm");
        let local = Vault::new("m");
        let agent_only = SessionConfig {
            id: "sess-agent".into(),
            name: "agent".into(),
            host: "h".into(),
            port: 22,
            username: "u".into(),
            auth: crate::AuthMethod::Agent,
            group: None,
            jumps: Vec::new(),
            startup_command: None,
            forwards: Vec::new(),
            favorite: false,
        };
        let dropped =
            export_sessions_with_credentials(&path, &[agent_only], &local, &Vault::for_export("e"))
                .expect("导出");
        assert_eq!(dropped, 0, "无凭据会话不应计入略过");

        let mode = fs::metadata(&path).expect("元数据").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "带凭据导出件须仅属主可读写");

        let (imported, _) = import_credential_export(&path, "e", &local).expect("导入");
        assert!(matches!(imported[0].auth, crate::AuthMethod::Agent));
        let _ = fs::remove_file(&path);
    }
}
