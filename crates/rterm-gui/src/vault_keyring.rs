//! 系统钥匙串中 DEK 的存取封装，用于「本机记住主密钥」自动解锁。
//!
//! 两种模式下都把**当前 DEK**存入钥匙串：模式 0 是随机密钥（`Vault::create_random`），
//! 模式 1 是主口令派生的密钥。后续启动读回钥匙串中的 DEK 即可静默解锁，免去输入或设口令。
//! 钥匙串由系统登录口令保护（macOS Keychain / GNOME Keyring / Windows 凭据管理器）。
//! 无可用后端（如无图形界面的服务器）时调用方忽略错误：模式 0 会在启动阶段就地重生随机密钥
//! 让应用可用（既有凭据可能失效），而非弹主密码框——模式 0 本就无主密码可弹。

use log::error;

/// 钥匙串服务名。
const SERVICE: &str = "rterm";
/// 钥匙串账户名（单保险库，固定账户，避免多保险库留下孤儿条目）。
const ACCOUNT: &str = "vault-dek";

/// 实际使用的账户名。
///
/// 设置 `RTERM_TEST_KEYRING_ACCOUNT` 后读写改走该隔离账户，避免单元测试污染开发者
/// 真实钥匙串条目（默认 `vault-dek`）。测试在 `use_temp_config_home` 中开启此隔离。
fn account() -> &'static str {
    match std::env::var("RTERM_TEST_KEYRING_ACCOUNT") {
        Ok(a) if !a.is_empty() => Box::leak(a.into_boxed_str()),
        _ => ACCOUNT,
    }
}

/// DEK 长度（与 `rterm_crypto::DEK_LEN` 一致）。
const DEK_LEN: usize = 32;

/// 将派生的 DEK 写入系统钥匙串。
///
/// 失败（如后端不可用）仅记录日志并返回错误，调用方应忽略并回退到主密码解锁。
pub fn store_dek(dek: &[u8; DEK_LEN]) -> Result<(), keyring::Error> {
    let entry = keyring::Entry::new(SERVICE, account())?;
    entry.set_secret(dek)?;
    Ok(())
}

/// 从系统钥匙串读取已缓存的 DEK。
///
/// - 找到且长度合法 → `Ok(Some(dek))`；
/// - 无对应条目（`NoEntry`）→ `Ok(None)`，调用方走解锁流程；
/// - 条目长度不合法 → 视为无有效缓存，返回 `Ok(None)`；
/// - 后端不可用（`NoDefaultStore`）等 → 向上传 `Err`，由调用方决定重生随机密钥或提示。
pub fn load_dek() -> Result<Option<[u8; DEK_LEN]>, keyring::Error> {
    let entry = keyring::Entry::new(SERVICE, account())?;
    match entry.get_secret() {
        Ok(bytes) => {
            if bytes.len() == DEK_LEN {
                let mut arr = [0u8; DEK_LEN];
                arr.copy_from_slice(&bytes);
                Ok(Some(arr))
            } else {
                // 长度异常视为无有效缓存，不报错（避免卡在弹窗外）。
                Ok(None)
            }
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e),
    }
}

/// 删除钥匙串中缓存的 DEK（用户关闭「本机记住」开关时调用）。
///
/// 失败仅记录日志；条目本就不存在（`NoEntry`）视为成功。
pub fn delete_dek() -> Result<(), keyring::Error> {
    let entry = keyring::Entry::new(SERVICE, account())?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e),
    }
}

/// 是否可在本机使用钥匙串自动解锁：后端可初始化则返回 `true`。
///
/// 用于测试与诊断：无后端环境（如无 D-Bus 会话总线的服务器）返回 `false`，
/// 此时应跳过自动解锁相关逻辑并回退主密码弹窗。
#[allow(dead_code)]
pub fn available() -> bool {
    keyring::Entry::new(SERVICE, account()).is_ok()
}

/// 静默写入 DEK，失败仅记录（用于「设置/解锁/更改」成功后的缓存落盘）。
pub fn store_dek_quietly(dek: &[u8; DEK_LEN]) {
    if let Err(e) = store_dek(dek) {
        error!("写入钥匙串主密钥失败（将回退到每次输入主密码）: {e}");
    }
}

/// 静默删除 DEK，失败仅记录（用于关闭「本机记住」开关）。
pub fn delete_dek_quietly() {
    if let Err(e) = delete_dek() {
        error!("删除钥匙串主密钥失败: {e}");
    }
}
