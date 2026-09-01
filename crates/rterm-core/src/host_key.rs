//! 已知主机信任（TOFU）存储。
//!
//! 是否信任某主机的决策由用户在 GUI 弹窗中做出，本模块只提供只读校验与
//! 用户确认后的落盘能力：指纹以纯文本 `host:port SHA256:xxxx` 逐行存于
//! `<缓存目录>/rterm/known_hosts`，不依赖额外序列化库。

use crate::CoreError;
use log::info;
use russh::keys::PublicKeyOrCertificate;
use russh::keys::ssh_key::HashAlg;
use std::fs;
use std::path::PathBuf;

/// 主机密钥比对结果（只读，不落盘）。
#[derive(Clone)]
pub enum HostKeyStatus {
    /// 已记录且本次指纹匹配。
    Known,
    /// 无记录（首次连接，待用户确认）。
    Unknown,
    /// 已记录但指纹不一致（疑似中间人攻击或服务器重装，待用户确认）。
    Mismatch {
        /// 已知主机文件中记录的历史指纹，用于提示用户“指纹已变更”。
        stored: String,
    },
}

/// 计算主机密钥的 SHA256 指纹。
pub fn fingerprint(key: &PublicKeyOrCertificate) -> String {
    key.public_key().fingerprint(HashAlg::Sha256).to_string()
}

/// 主机密钥的算法名称（如 `ssh-ed25519`），供弹窗展示。
pub fn key_type(key: &PublicKeyOrCertificate) -> String {
    key.public_key().algorithm().as_str().to_string()
}

/// 解析 known_hosts 文件的完整路径：取系统缓存目录下的 `rterm/known_hosts`，必要时创建目录。
fn path() -> Result<PathBuf, CoreError> {
    let base = dirs::cache_dir().ok_or_else(|| CoreError::ssh_msg("无法定位缓存目录"))?;
    let dir = base.join("rterm");
    fs::create_dir_all(&dir).map_err(|e| CoreError::ssh("创建缓存目录失败", e))?;
    Ok(dir.join("known_hosts"))
}

/// 校验主机密钥指纹：比对 known_hosts，返回 [`HostKeyStatus`]（命中 / 未知 / 变更）。
///
/// 文件不存在或读取失败时按未记录处理（返回 `Unknown`，交由 GUI 弹窗确认），
/// 不会因此中止握手；仅缓存目录无法定位、`create_dir_all` 失败时才返回 [`CoreError`]。
pub fn check_host_key(host: &str, port: u16, fp: &str) -> Result<HostKeyStatus, CoreError> {
    let entry = format!("{host}:{port}");
    let path = path()?;
    let content = fs::read_to_string(&path).unwrap_or_default();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((stored_entry, stored_fp)) = line.split_once(char::is_whitespace)
            && stored_entry == entry
        {
            return if stored_fp == fp {
                Ok(HostKeyStatus::Known)
            } else {
                Ok(HostKeyStatus::Mismatch {
                    stored: stored_fp.to_string(),
                })
            };
        }
    }
    Ok(HostKeyStatus::Unknown)
}

/// 追加新条目（未知主机被用户接受后调用）。
pub fn trust_host_key(host: &str, port: u16, fp: &str) -> Result<(), CoreError> {
    let path = path()?;
    let mut next = fs::read_to_string(&path).unwrap_or_default();
    if !next.ends_with('\n') && !next.is_empty() {
        next.push('\n');
    }
    next.push_str(&format!("{host}:{port} {fp}\n"));
    fs::write(&path, next).map_err(|e| CoreError::ssh("写入 known_hosts 失败", e))?;
    info!("首次信任主机 {host}:{port}，指纹 {fp}");
    Ok(())
}

/// 覆盖该主机的既有条目（指纹变更被用户“仍然信任”后调用）。
///
/// 必须整体重写而非追加：读取逻辑命中首条匹配即返回，追加旧条目会永远胜出。
pub fn replace_host_key(host: &str, port: u16, fp: &str) -> Result<(), CoreError> {
    let entry = format!("{host}:{port}");
    let path = path()?;
    let content = fs::read_to_string(&path).unwrap_or_default();
    let mut next = String::new();
    for line in content.lines() {
        let kept = match line.trim().split_once(char::is_whitespace) {
            Some((stored_entry, _)) => stored_entry != entry,
            // 无空白分隔的行一律保留：空行，以及 `#` 注释（注释行带空白时其实会走上面的
            // `Some` 分支，因 `stored_entry` 不等于任何 host 条目而保留）。
            None => true,
        };
        if kept {
            next.push_str(line);
            next.push('\n');
        }
    }
    next.push_str(&format!("{entry} {fp}\n"));
    fs::write(&path, next).map_err(|e| CoreError::ssh("写入 known_hosts 失败", e))?;
    info!("用户确认后覆盖主机 {entry} 的旧指纹，新指纹 {fp}");
    Ok(())
}
