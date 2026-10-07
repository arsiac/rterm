//! 已知主机信任（TOFU）存储。
//!
//! 是否信任某主机的决策由用户在 GUI 弹窗中做出，本模块提供只读校验、用户确认后的
//! 落盘，以及设置界面的列出 / 遗忘能力：指纹以纯文本 `host:port SHA256:xxxx` 逐行
//! 存于 `<缓存目录>/rterm/known_hosts`（开发沙箱下为 `<工作区>/.dev/cache/known_hosts`），
//! 不依赖额外序列化库。

use crate::{CoreError, CoreErrorKind};
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

/// known_hosts 中的一条已信任主机记录（供管理界面展示与遗忘）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownHostEntry {
    /// 主机名或地址（按文件内容原样解析，IPv6 地址不加方括号）。
    pub host: String,
    /// 端口。
    pub port: u16,
    /// 记录时的 SHA256 指纹（含 `SHA256:` 前缀）。
    pub fingerprint: String,
}

/// 计算主机密钥的 SHA256 指纹。
pub fn fingerprint(key: &PublicKeyOrCertificate) -> String {
    key.public_key().fingerprint(HashAlg::Sha256).to_string()
}

/// 主机密钥的算法名称（如 `ssh-ed25519`），供弹窗展示。
pub fn key_type(key: &PublicKeyOrCertificate) -> String {
    key.public_key().algorithm().as_str().to_string()
}

/// 解析 known_hosts 文件的完整路径：取缓存目录下的 `known_hosts`，必要时创建目录。
///
/// 目录由 [`rterm_config::paths::cache_dir`] 统一解析（开发沙箱下为 `.dev/cache`），
/// 使开发期不会写入真实的 known_hosts。
fn path() -> Result<PathBuf, CoreError> {
    let dir = rterm_config::paths::cache_dir()
        .ok_or_else(|| CoreError::ssh_msg(CoreErrorKind::CacheDirUnknown))?;
    fs::create_dir_all(&dir).map_err(|e| CoreError::ssh(CoreErrorKind::CreateCacheDir, e))?;
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

/// 列出 known_hosts 中的全部条目（供管理界面展示）。
///
/// 注释、空行，以及无法解析出 `host:port`（缺冒号、端口非数字）或无指纹的行一律跳过
/// 文件不存在按空列表处理。
pub fn list_known_hosts() -> Result<Vec<KnownHostEntry>, CoreError> {
    let path = path()?;
    let content = fs::read_to_string(&path).unwrap_or_default();
    let mut entries = Vec::new();
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let (Some(entry), Some(fingerprint)) = (parts.next(), parts.next()) else {
            continue;
        };
        if entry.starts_with('#') {
            continue;
        }
        // 从右拆分端口：IPv6 地址本身含冒号（如 `fe80::1:22` 应拆出 `fe80::1` + `22`）。
        let Some((host, port)) = entry.rsplit_once(':') else {
            continue;
        };
        let Ok(port) = port.parse::<u16>() else {
            continue;
        };
        entries.push(KnownHostEntry {
            host: host.to_string(),
            port,
            fingerprint: fingerprint.to_string(),
        });
    }
    Ok(entries)
}

/// 追加新条目（未知主机被用户接受后调用）。
pub fn trust_host_key(host: &str, port: u16, fp: &str) -> Result<(), CoreError> {
    let path = path()?;
    let mut next = fs::read_to_string(&path).unwrap_or_default();
    if !next.ends_with('\n') && !next.is_empty() {
        next.push('\n');
    }
    next.push_str(&format!("{host}:{port} {fp}\n"));
    fs::write(&path, next).map_err(|e| CoreError::ssh(CoreErrorKind::WriteKnownHosts, e))?;
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
    fs::write(&path, next).map_err(|e| CoreError::ssh(CoreErrorKind::WriteKnownHosts, e))?;
    info!("用户确认后覆盖主机 {entry} 的旧指纹，新指纹 {fp}");
    Ok(())
}

/// 遗忘某主机：删除 known_hosts 中 `host:port` 匹配的行并重写文件。
///
/// 返回是否确有删除；未命中时**不重写文件**（避免为空操作重排注释等无关内容）。
/// 注释、空行及无法解析的行一律原样保留；文件不存在按「无此条目」处理。
pub fn forget_host_key(host: &str, port: u16) -> Result<bool, CoreError> {
    let entry = format!("{host}:{port}");
    let path = path()?;
    let content = fs::read_to_string(&path).unwrap_or_default();
    let mut kept = Vec::new();
    let mut removed = false;
    for line in content.lines() {
        // 与写入路径同样的条目语义：首个空白前的 `host:port` 必须逐字相等
        // （`a.com:22` 不得误伤 `a.com:2200`）。
        let matched = line
            .trim()
            .split_once(char::is_whitespace)
            .is_some_and(|(stored_entry, _)| stored_entry == entry);
        if matched {
            removed = true;
        } else {
            kept.push(line);
        }
    }
    if !removed {
        return Ok(false);
    }
    let mut next = kept.join("\n");
    if !kept.is_empty() {
        next.push('\n');
    }
    fs::write(&path, next).map_err(|e| CoreError::ssh(CoreErrorKind::WriteKnownHosts, e))?;
    info!("forgot stored host key for {entry}");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Mutex;

    /// 串行化会改写全局状态根（`paths::set_test_root`）的测试。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// 把状态根重定向到临时目录并返回它，覆盖 debug 构建默认的工作区 `.dev/cache`，
    /// 避免测试读写开发者真实的 known_hosts。
    fn use_temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rterm_hostkey_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("应能创建临时目录");
        rterm_config::paths::set_test_root(Some(dir.clone()));
        dir
    }

    /// 恢复默认状态根并清理临时目录。
    fn cleanup(root: &Path) {
        rterm_config::paths::set_test_root(None);
        let _ = fs::remove_dir_all(root);
    }

    /// 预写 known_hosts 文件（父目录一并创建）。
    fn write_known_hosts(root: &Path, content: &str) {
        let path = root.join("cache").join("known_hosts");
        fs::create_dir_all(path.parent().expect("应有父目录")).expect("应能创建缓存目录");
        fs::write(&path, content).expect("应能写入 known_hosts");
    }

    fn read_known_hosts(root: &Path) -> String {
        fs::read_to_string(root.join("cache").join("known_hosts")).expect("应能读取 known_hosts")
    }

    #[test]
    fn list_returns_valid_entries_and_skips_unparseable_lines() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_root("list");
        write_known_hosts(
            &dir,
            &[
                "# comment",
                "",
                "  example.com:22 SHA256:aaa",
                "example.com:22   SHA256:spaced",
                "fe80::1:2222 SHA256:bbb",
                "no-port SHA256:ccc",
                "bad:port SHA256:ddd",
                "entry-without-fingerprint",
            ]
            .join("\n"),
        );

        let list = list_known_hosts().expect("应能列出条目");
        cleanup(&dir);

        assert_eq!(
            list,
            vec![
                KnownHostEntry {
                    host: "example.com".into(),
                    port: 22,
                    fingerprint: "SHA256:aaa".into(),
                },
                // 多空格分隔的行按空白切分，指纹不挟带前导空格。
                KnownHostEntry {
                    host: "example.com".into(),
                    port: 22,
                    fingerprint: "SHA256:spaced".into(),
                },
                // IPv6 地址含冒号：端口须从右侧拆分。
                KnownHostEntry {
                    host: "fe80::1".into(),
                    port: 2222,
                    fingerprint: "SHA256:bbb".into(),
                },
            ]
        );
    }

    #[test]
    fn forget_removes_only_exact_matching_entry_and_keeps_rest() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_root("forget");
        write_known_hosts(
            &dir,
            "# keep me\na.com:22 SHA256:aaa\na.com:2200 SHA256:bbb\nb.com:22 SHA256:ccc\n",
        );

        let removed = forget_host_key("a.com", 22).expect("应能遗忘");
        let content = read_known_hosts(&dir);
        cleanup(&dir);

        assert!(removed, "命中的条目应被删除");
        // `a.com:2200` 与 `a.com:22` 前缀相同，但必须逐字匹配、不得误伤。
        assert_eq!(
            content,
            "# keep me\na.com:2200 SHA256:bbb\nb.com:22 SHA256:ccc\n"
        );
    }

    #[test]
    fn forget_missing_entry_reports_false_and_leaves_file_untouched() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_root("missing");
        let original = "# note\nx.com:22 SHA256:xxx\n";
        write_known_hosts(&dir, original);

        let removed = forget_host_key("y.com", 22).expect("未命中应正常返回");
        let content = read_known_hosts(&dir);
        cleanup(&dir);

        assert!(!removed);
        assert_eq!(content, original, "未命中时不得重写文件");
    }

    #[test]
    fn forget_after_trust_makes_host_unknown_again() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_root("roundtrip");

        trust_host_key("example.com", 22, "SHA256:fp1").expect("应能信任");
        assert!(matches!(
            check_host_key("example.com", 22, "SHA256:fp1").expect("应能校验"),
            HostKeyStatus::Known
        ));

        let removed = forget_host_key("example.com", 22).expect("应能遗忘");
        let status = check_host_key("example.com", 22, "SHA256:fp1").expect("应能校验");
        cleanup(&dir);

        assert!(removed);
        // 遗忘后回到未知状态：下次连接会重新弹出 TOFU 确认框。
        assert!(matches!(status, HostKeyStatus::Unknown));
    }
}
