//! GitHub Releases 更新检查（app 级，与 SSH 业务无关）。
//!
//! 仅读取最新发布版本号并与当前 `CARGO_PKG_VERSION` 比较，best-effort：网络 / 解析失败
//! 一律返回 `None` 或错误以让调用方静默跳过，绝不阻塞主流程。

use log::{debug, warn};
use serde::Deserialize;

/// 仓库 owner/repo 回退常量：运行时优先从 `git remote get-url origin` 探测，失败才用此值。
/// 发布的二进制不含 `.git`，只能靠此回退，故必须填真实仓库而非占位名。
const DEFAULT_REPO: &str = "arsiac/rterm";

/// GitHub Releases API 返回的精简结构（仅取比较与展示所需字段）。
#[derive(Debug, Clone, Deserialize)]
struct GithubRelease {
    /// 发布标签（如 `v0.2.0`）。
    tag_name: String,
    /// 发布页地址。
    html_url: String,
    /// 是否为预发布版本。
    prerelease: bool,
    /// 是否为草稿（草稿通常不对外）。
    draft: bool,
}

/// 一次成功检查得到的可用更新信息。
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    /// 去前缀 `v` 后的纯版本号（如 `0.2.0`）。
    pub version: String,
    /// 发布页地址，供「前往下载」打开。
    pub html_url: String,
}

/// 解析 `git@github.com:owner/repo.git` 或 `https://github.com/owner/repo(.git)` 为 `owner/repo`。
fn parse_github_remote(url: &str) -> Option<String> {
    let url = url.trim();
    // 依次尝试 SSH 与 HTTPS 两种 origin 写法；均不匹配则非 GitHub 仓库。
    let repo = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    // 去掉末尾 `.git` 与斜杠；`owner/repo` 已是最终形态，不要再按 `/` 切分。
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let repo = repo.trim_end_matches('/');
    (repo.matches('/').count() == 1).then(|| repo.to_string())
}

/// 运行时确定被检查的仓库：探测 `origin` 远程，失败回退 `DEFAULT_REPO`。
pub fn resolve_repo() -> String {
    std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| parse_github_remote(&s))
        .unwrap_or_else(|| DEFAULT_REPO.to_string())
}

/// 向 GitHub Releases API 查询最新稳定版，返回比当前版本更新的发布（无更新或不可用时为 `None`）。
///
/// 跳过 `prerelease` / `draft`；仅当远端版本号严格大于当前版本才视为有更新。
pub async fn check_latest(repo: &str) -> Result<Option<ReleaseInfo>, String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    debug!("检查更新：{url}");

    let client = reqwest::Client::builder()
        .user_agent("rterm-update-check")
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))?;

    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("请求更新信息失败: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("GitHub API 返回状态 {}", resp.status()));
    }

    let release: GithubRelease = resp
        .json()
        .await
        .map_err(|e| format!("解析更新信息失败: {e}"))?;

    if release.prerelease || release.draft {
        debug!("忽略预发布 / 草稿版本 {}", release.tag_name);
        return Ok(None);
    }

    let remote = release.tag_name.trim_start_matches('v');
    let current = env!("CARGO_PKG_VERSION");
    let is_newer = match (
        semver::Version::parse(remote),
        semver::Version::parse(current),
    ) {
        (Ok(r), Ok(c)) => r > c,
        // 版本号无法解析时（如非标准 tag）不提示更新，避免误报。
        _ => {
            warn!("版本号无法解析（远端 {remote} / 当前 {current}），跳过更新提示");
            false
        }
    };

    if is_newer {
        Ok(Some(ReleaseInfo {
            version: remote.to_string(),
            html_url: release.html_url,
        }))
    } else {
        debug!("已是最新（远端 {remote} / 当前 {current}）");
        Ok(None)
    }
}
