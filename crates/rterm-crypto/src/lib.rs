//! 凭据加密层：以 AES-256-GCM 对敏感字段（密码 / 私钥口令）做信封加密，DEK 有**两种来源**。
//!
//! - **模式 0（默认，未设主口令）**：DEK 是随机 32 字节（[`Vault::create_random`]），
//!   存入系统钥匙串由登录口令保护。开箱即用、零弹窗，但密文**不可跨设备同步**——
//!   换机没有钥匙串里的随机密钥就解不开。
//! - **模式 1（已设主口令）**：DEK 由主口令经 Argon2id 派生，口令本身不上盘；同一口令
//!   在任意设备派生出相同的 DEK，因此**密文 blob 可安全同步**而无需同步密钥。
//!
//! 两种模式由 [`CryptoHeader::master_password_set`] 区分，模式可互相转换（设置 / 关闭 /
//! 更改主口令，由 GUI 层的 `rekey_session` 完成）。每个敏感字段独立随机 nonce 加密，密文以
//! base64 形式随 `serde` 持久化。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Argon2, Params, Version};
use base64::Engine as _;
use secrecy::{ExposeSecret, SecretBox};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use zeroize::Zeroizing;

/// 派生主密钥所用的 KDF 参数。
///
/// `mem_mib` 为内存开销（MiB），`ops` 为迭代次数，`parallel` 为并行度（lane 数）。
/// 这些值随 [`CryptoHeader`] 落盘，解锁时读取创建时所用的值，保证派生结果一致。
const KDF_MEMORY_MIB: u32 = 64;
/// Argon2 迭代次数（轮数）。
const KDF_OPS: u32 = 3;
/// 并行度上限：派生时在「可用核心数」与上限之间取较小值，多核机器上可缩短派生耗时，
/// 但 Argon2 派生结果依赖该值，故必须随头持久化（见 [`CryptoHeader::parallel`]）。
const KDF_PARALLELISM_MAX: u32 = 4;
/// Argon2 内存开销以 KiB 计：64 MiB = 65536 KiB。
const KDF_MEMORY_KIB: u32 = KDF_MEMORY_MIB * 1024;

/// 创建保险库时选取的 Argon2 并行度：取「可用核心数」与 [`KDF_PARALLELISM_MAX`] 的较小值，
/// 在合法范围内尽量利用多核加速派生（并行度不降低总内存硬度，只把工作分到多核）。
fn derive_parallelism() -> u32 {
    std::thread::available_parallelism()
        .map(|n| (n.get() as u32).clamp(1, KDF_PARALLELISM_MAX))
        .unwrap_or(2)
}
/// 主密钥长度（字节），AES-256 所需。
const DEK_LEN: usize = 32;
/// AES-GCM 随机 nonce 长度（字节）。
const NONCE_LEN: usize = 12;
/// 用于校验主密码是否正确的哨兵明文（加密后存入 [`CryptoHeader::verifier`]）。
const VERIFIER_PLAINTEXT: &str = "rterm-vault-v1";

/// 加密层错误。
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// Argon2 参数非法或派生失败。
    ///
    /// 注意：当前**没有构造点**——`derive_dek` 在参数非法 / 派生失败时直接 `expect` panic
    /// （参数与口令长度都是编译期或调用前约束好的，视为不可能发生）。保留该变体以免
    /// 将来把派生改为可恢复错误时改动公开 API。
    #[error("密钥派生失败")]
    Kdf,
    /// AEAD 加解密失败（密文被篡改或密钥不匹配）。
    #[error("加解密失败")]
    Cipher,
    /// 主密码校验失败（verifier 解密后与哨兵不符）。
    #[error("主密码不正确")]
    Verify,
    /// 密文信封格式损坏（base64 或长度非法）。
    #[error("密文格式损坏")]
    BadEnvelope,
}

/// 密文信封：单字段加密结果，随 `serde` 以 base64 字符串持久化。
///
/// 内部字节布局为 `nonce(12) ‖ ciphertext ‖ tag(16)`，不暴露明文。
#[derive(Clone, PartialEq, Eq)]
pub struct Envelope(Vec<u8>);

impl Envelope {
    /// 由随机 nonce 与密文（含 tag）构造信封。
    fn new(nonce: [u8; NONCE_LEN], ciphertext: Vec<u8>) -> Self {
        let mut buf = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        buf.extend_from_slice(&nonce);
        buf.extend_from_slice(&ciphertext);
        Envelope(buf)
    }

    /// 取 12 字节 nonce。
    fn nonce(&self) -> [u8; NONCE_LEN] {
        let mut n = [0u8; NONCE_LEN];
        n.copy_from_slice(&self.0[..NONCE_LEN]);
        n
    }

    /// 取密文 + tag 部分（nonce 之后全部字节）。
    fn ciphertext(&self) -> &[u8] {
        &self.0[NONCE_LEN..]
    }
}

impl Serialize for Envelope {
    /// 将信封整体（含随机 nonce 与密文）编码为 base64 字符串持久化。
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Envelope {
    /// 从 base64 字符串解码信封，并校验长度不少于 nonce 加 tag。
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&s)
            .map_err(serde::de::Error::custom)?;
        if bytes.len() < NONCE_LEN + 16 {
            return Err(serde::de::Error::custom("envelope too short"));
        }
        Ok(Envelope(bytes))
    }
}

impl fmt::Debug for Envelope {
    /// 仅输出密文长度，避免调试日志泄露密文内容。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Envelope")
            .field("len", &self.0.len())
            .finish()
    }
}

/// 加密文件头：随会话文件持久化，保存 KDF 参数与校验哨兵。
///
/// 两种模式由 [`Self::master_password_set`] 区分：
/// - `false`（模式 0，默认）：DEK 为随机密钥，仅存于本机钥匙串；无口令、无 `verifier`，
///   `salt`/`kdf` 等字段为空（跨设备无法解密，故同步被禁止，由调用方的 `App::can_enable_sync` 控制）。
/// - `true`（模式 1）：DEK 由主密码派生；`verifier` 为口令校验哨兵，`salt`/`kdf` 有效，
///   文件自包含，可跨设备靠口令解密（同步可用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoHeader {
    /// 格式版本，预留演进空间。
    pub version: u32,
    /// 用户是否已设置主密码（模式 1）。`false` 表示随机密钥模式。
    pub master_password_set: bool,
    /// KDF 算法标识（`argon2id`）；模式 0 为空。
    pub kdf: String,
    /// 派生盐（base64），公开；模式 0 为空。
    pub salt: String,
    /// Argon2 迭代次数；模式 0 为 0。
    pub ops: u32,
    /// Argon2 内存开销（MiB）；模式 0 为 0。
    pub mem_mib: u32,
    /// Argon2 并行度（lane 数）；模式 0 为 0。
    ///
    /// 必须随头持久化：Argon2 派生结果依赖该参数，若派生时用的值与创建时不一致，
    /// 算出的 DEK 会不同，导致旧保险库无法解锁。旧格式文件缺省回退为 1（见 `default_parallel_one`）。
    #[serde(default = "default_parallel_one")]
    pub parallel: u32,
    /// 主密码校验哨兵的密文信封；仅模式 1 为 `Some`。解锁时解密成功即证明口令正确。
    pub verifier: Option<Envelope>,
}

/// 旧版加密头未持久化 `parallel` 时的缺省值（当时硬编码为 1）。
fn default_parallel_one() -> u32 {
    1
}

/// 凭据保险库：持有主密钥（DEK），提供加解密能力。
///
/// DEK 以 [`SecretBox`] 持有，drop 时自动擦除；本结构不实现 `Clone` 以避免密钥被随意复制，
/// 需要共享时由调用方用 `Arc` 包裹（见 GUI 层）。DEK 来源可能是主口令派生（模式 1）
/// 或随机生成（模式 0），见 [`Vault::new`] / [`Vault::create_random`]。
pub struct Vault {
    /// 数据加密密钥（DEK），存放于 [`SecretBox`] 中以在 drop 时擦除。
    dek: SecretBox<[u8; DEK_LEN]>,
    /// 保险库头部：盐、派生参数与校验哨兵等元数据。
    header: CryptoHeader,
}

impl Vault {
    /// 用新主密码创建保险库（模式 1）：生成随机盐，派生 DEK，并写入校验哨兵。
    ///
    /// 用于「设置主密码」（由模式 0 升级，见 GUI 层 `rekey_session`）。返回值中的
    /// [`CryptoHeader`] 需由调用方持久化（含 `master_password_set = true`）。
    pub fn new(passphrase: &str) -> Vault {
        let salt: [u8; 16] = rand::random();
        let mut dek = [0u8; DEK_LEN];
        let parallel = derive_parallelism();
        derive_dek(
            passphrase,
            &salt,
            KDF_OPS,
            KDF_MEMORY_KIB,
            parallel,
            &mut dek,
        );

        let dek_box = SecretBox::new(Box::new(dek));
        let verifier = seal(&dek_box, VERIFIER_PLAINTEXT.as_bytes());

        Vault {
            dek: dek_box,
            header: CryptoHeader {
                version: 1,
                master_password_set: true,
                kdf: "argon2id".to_string(),
                salt: base64::engine::general_purpose::STANDARD.encode(salt),
                ops: KDF_OPS,
                mem_mib: KDF_MEMORY_MIB,
                parallel,
                verifier: Some(verifier),
            },
        }
    }

    /// 生成随机密钥保险库（模式 0，默认）：随机 32 字节 DEK，无口令、无校验哨兵。
    ///
    /// 调用方须把 [`Vault::dek_bytes`] 存入本机钥匙串，并把返回的 [`CryptoHeader`]
    /// （`master_password_set = false`）持久化。凭据由该随机 DEK 加密，故仅本机可解密。
    pub fn create_random() -> Vault {
        let key: [u8; DEK_LEN] = rand::random();
        let dek_box = SecretBox::new(Box::new(key));
        Vault {
            dek: dek_box,
            header: CryptoHeader {
                version: 1,
                master_password_set: false,
                kdf: String::new(),
                salt: String::new(),
                ops: 0,
                mem_mib: 0,
                parallel: 0,
                verifier: None,
            },
        }
    }

    /// 用主密码解锁已有保险库（仅模式 1）：读取文件头中的 salt / 参数派生 DEK，
    /// 解密 `verifier` 校验口令正确性。口令错误返回 [`CryptoError::Verify`]。
    ///
    /// 模式 0（随机密钥）无主密码，调用本函数返回 [`CryptoError::Verify`]。
    pub fn unlock(passphrase: &str, header: &CryptoHeader) -> Result<Vault, CryptoError> {
        if !header.master_password_set {
            // 模式 0 不存在主密码，不应走口令解锁流程。
            return Err(CryptoError::Verify);
        }
        let salt = base64::engine::general_purpose::STANDARD
            .decode(&header.salt)
            .map_err(|_| CryptoError::BadEnvelope)?;
        let mut dek = [0u8; DEK_LEN];
        derive_dek(
            passphrase,
            &salt,
            header.ops,
            header.mem_mib * 1024,
            header.parallel,
            &mut dek,
        );
        let dek_box = SecretBox::new(Box::new(dek));

        // 校验口令：解密哨兵必须与已知明文一致。
        // 解锁场景下，任何解密失败都等价于「口令不正确」（哨兵无法还原）。
        let verifier = header.verifier.as_ref().ok_or(CryptoError::Verify)?;
        let opened = open(&dek_box, verifier).map_err(|_| CryptoError::Verify)?;
        if &opened[..] != VERIFIER_PLAINTEXT.as_bytes() {
            return Err(CryptoError::Verify);
        }
        Ok(Vault {
            dek: dek_box,
            header: header.clone(),
        })
    }

    /// 取加密文件头（含 salt / 参数 / 哨兵），供持久化。
    pub fn header(&self) -> &CryptoHeader {
        &self.header
    }

    /// 取出主密钥原始字节（用于在本机钥匙串中缓存以实现自动解锁）。
    ///
    /// 返回 `DEK_LEN` 字节的副本；调用方须以安全方式保管（钥匙串本身由系统登录口令保护）。
    pub fn dek_bytes(&self) -> [u8; DEK_LEN] {
        *self.dek.expose_secret()
    }

    /// 用已派生的主密钥（如从系统钥匙串取出的 DEK）直接构造保险库，跳过 Argon2 派生。
    ///
    /// - 模式 1：解密 [`CryptoHeader::verifier`] 哨兵校验 DEK 正确性；不匹配（陈旧 / 被篡改 /
    ///   来自其他保险库）返回 [`CryptoError::Verify`]，调用方应回退到主密码解锁。
    /// - 模式 0（随机密钥）：无口令、无哨兵，直接信任钥匙串中的 DEK。
    pub fn from_dek(dek: &[u8; DEK_LEN], header: &CryptoHeader) -> Result<Vault, CryptoError> {
        let dek_box = SecretBox::new(Box::new(*dek));
        if header.master_password_set {
            // 模式 1：哨兵必须能还原为已知明文，否则说明密钥不对。
            let verifier = header.verifier.as_ref().ok_or(CryptoError::Verify)?;
            let opened = open(&dek_box, verifier).map_err(|_| CryptoError::Verify)?;
            if &opened[..] != VERIFIER_PLAINTEXT.as_bytes() {
                return Err(CryptoError::Verify);
            }
        }
        Ok(Vault {
            dek: dek_box,
            header: header.clone(),
        })
    }

    /// 加密一段明文，返回密文信封（每次随机 nonce）。
    pub fn encrypt(&self, plaintext: &str) -> Envelope {
        seal(&self.dek, plaintext.as_bytes())
    }

    /// 解密信封，返回自动擦除的明文字符串。失败（篡改 / 密钥不符）返回 [`CryptoError::Cipher`]。
    pub fn decrypt(&self, env: &Envelope) -> Result<Zeroizing<String>, CryptoError> {
        let bytes = open(&self.dek, env)?;
        let s = String::from_utf8(bytes.to_vec()).map_err(|_| CryptoError::Cipher)?;
        Ok(Zeroizing::new(s))
    }
}

/// 以 Argon2id 从口令 + 盐派生定长主密钥。
fn derive_dek(
    passphrase: &str,
    salt: &[u8],
    ops: u32,
    mem_kib: u32,
    parallel: u32,
    out: &mut [u8],
) {
    let params = Params::new(mem_kib, ops, parallel, None).expect("Argon2 参数非法");
    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, Version::V0x13, params);
    // `hash_password_into` 直接把原始密钥写入 `out`，不走 PHC 字符串编码。
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, out)
        .expect("Argon2 派生失败");
}

/// AES-256-GCM 密封：随机 nonce + 明文 → 密文（含 tag）。
fn seal(dek: &SecretBox<[u8; DEK_LEN]>, plaintext: &[u8]) -> Envelope {
    let cipher = Aes256Gcm::new_from_slice(dek.expose_secret()).expect("密钥长度固定为 32 字节");
    let nonce: [u8; NONCE_LEN] = rand::random();
    let ct = cipher
        .encrypt(&Nonce::from(nonce), plaintext)
        .expect("AES-GCM 加密不应失败（除非 nonce 复用）");
    Envelope::new(nonce, ct)
}

/// AES-256-GCM 开启：nonce + 密文 → 明文。
fn open(dek: &SecretBox<[u8; DEK_LEN]>, env: &Envelope) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let cipher = Aes256Gcm::new_from_slice(dek.expose_secret()).map_err(|_| CryptoError::Cipher)?;
    let pt = cipher
        .decrypt(&Nonce::from(env.nonce()), env.ciphertext())
        .map_err(|_| CryptoError::Cipher)?;
    Ok(Zeroizing::new(pt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let v = Vault::new("correct horse battery staple");
        let env = v.encrypt("s3cr3t-password");
        assert_ne!(env.0.len(), 0);
        let got = v.decrypt(&env).unwrap();
        assert_eq!(got.as_str(), "s3cr3t-password");
    }

    #[test]
    fn envelope_serializes_as_base64() {
        let v = Vault::new("pw");
        let env = v.encrypt("hello");
        let s = serde_json::to_string(&env).unwrap();
        assert!(s.contains('"'));
        let back: Envelope = serde_json::from_str(&s).unwrap();
        assert_eq!(v.decrypt(&back).unwrap().as_str(), "hello");
    }

    #[test]
    fn unlock_with_correct_passphrase_succeeds() {
        let v = Vault::new("master-pw");
        let header = v.header().clone();
        let v2 = Vault::unlock("master-pw", &header).unwrap();
        // 同一口令派生同一 DEK，可解密原始信封。
        let env = v.encrypt("shared-secret");
        assert_eq!(v2.decrypt(&env).unwrap().as_str(), "shared-secret");
    }

    #[test]
    fn unlock_with_wrong_passphrase_fails() {
        let v = Vault::new("master-pw");
        let header = v.header().clone();
        assert!(matches!(
            Vault::unlock("wrong-pw", &header),
            Err(CryptoError::Verify)
        ));
    }

    #[test]
    fn unlock_rejects_random_key_mode_header() {
        // 模式 0（随机密钥）不存在主密码，调用 unlock 应直接失败。
        let v = Vault::create_random();
        let header = v.header().clone();
        assert!(matches!(
            Vault::unlock("any-pass", &header),
            Err(CryptoError::Verify)
        ));
    }

    #[test]
    fn create_random_round_trips_and_skips_verifier() {
        // 模式 0：随机密钥可加解密，且无 verifier、master_password_set=false。
        let v = Vault::create_random();
        assert!(!v.header().master_password_set);
        assert!(v.header().verifier.is_none());
        let env = v.encrypt("local-secret");
        assert_eq!(v.decrypt(&env).unwrap().as_str(), "local-secret");

        // 从钥匙串取出的 DEK 经 from_dek 重建（模式 0 跳过哨兵校验）。
        let dek = v.dek_bytes();
        let v2 = Vault::from_dek(&dek, &v.header().clone()).expect("模式 0 应直接信任 DEK");
        assert_eq!(v2.decrypt(&env).unwrap().as_str(), "local-secret");
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let v = Vault::new("pw");
        let mut env = v.encrypt("data");
        // 翻转最后一个字节（tag 区）破坏完整性。
        let last = env.0.len() - 1;
        env.0[last] ^= 0xff;
        assert!(v.decrypt(&env).is_err());
    }

    #[test]
    fn dek_bytes_round_trips_via_from_dek() {
        let v = Vault::new("master-pw");
        let header = v.header().clone();
        let env = v.encrypt("shared-secret");
        // 取出 DEK 后用 from_dek 重建保险库，应能解密原信封（等价于钥匙串自动解锁）。
        let dek = v.dek_bytes();
        let v2 = Vault::from_dek(&dek, &header).expect("正确 DEK 应能重建保险库");
        assert_eq!(v2.decrypt(&env).unwrap().as_str(), "shared-secret");
    }

    #[test]
    fn from_dek_with_wrong_key_fails() {
        let v = Vault::new("master-pw");
        let header = v.header().clone();
        let mut wrong = [0u8; DEK_LEN];
        wrong[0] = 0xff;
        assert!(matches!(
            Vault::from_dek(&wrong, &header),
            Err(CryptoError::Verify)
        ));
    }

    #[test]
    fn new_vault_persists_parallelism_and_unlocks() {
        // 新建保险库必须把实际并行度写进头，且用该值解锁成功（派生参数与创建时一致）。
        let v = Vault::new("pw");
        let header = v.header().clone();
        assert!(
            header.parallel >= 1 && header.parallel <= 4,
            "并行度应在 1..=4，实际 {}",
            header.parallel
        );
        let v2 = Vault::unlock("pw", &header).expect("用存储的并行度应能解锁");
        let env = v.encrypt("secret");
        assert_eq!(v2.decrypt(&env).unwrap().as_str(), "secret");
    }

    #[test]
    fn legacy_header_without_parallel_defaults_to_one() {
        // 升级前的加密头未持久化 parallel 字段；反序列化必须回退为 1，
        // 与当时硬编码的 p=1 派生保持一致，否则存量保险库将无法解锁。
        let json = r#"{"version":1,"master_password_set":true,"kdf":"argon2id","salt":"AAAAAAAAAAAAAAAAAAAAAA==","ops":3,"mem_mib":64,"verifier":null}"#;
        let header: CryptoHeader = serde_json::from_str(json).expect("旧格式头应可反序列化");
        assert_eq!(header.parallel, 1);
    }
}
