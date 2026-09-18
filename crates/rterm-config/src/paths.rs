//! 运行时状态目录的统一解析入口。
//!
//! 应用偏好、会话配置、known_hosts、日志的落盘位置全部经由此模块定位，
//! 以免各 crate 各自调用 `dirs::` 而无法整体重定向。
//!
//! # 开发沙箱
//!
//! `debug` 构建（`cargo run` / `cargo test`）一律落在工作区内的 `.dev/` 下，
//! 使开发期读写不触碰真实配置；`release` 构建不受影响，仍走系统目录：
//!
//! ```text
//! 生产: <系统配置目录>/rterm/     <系统缓存目录>/rterm/
//! 开发: <工作区根>/.dev/config/   <工作区根>/.dev/cache/
//! ```
//!
//! 沙箱根基于编译期常量推导，与进程当前工作目录无关；`release` 构建下相关代码被
//! 条件编译移除，源码路径不会进入二进制。单元测试可用 [`set_test_root`] 注入临时根。

use std::path::PathBuf;
use std::sync::Mutex;

/// 沙箱状态根目录名（位于工作区根下）。
///
/// 仅 `debug` 构建使用；`release` 下连同 [`dev_root`] 一起被条件编译移除，
/// 否则会成为未使用的常量。
#[cfg(debug_assertions)]
const DEV_DIR_NAME: &str = ".dev";

/// 应用在系统配置 / 缓存目录下的子目录名。
const APP_DIR_NAME: &str = "rterm";

/// 状态根下的配置子目录名。
const CONFIG_SUBDIR: &str = "config";

/// 状态根下的缓存子目录名。
const CACHE_SUBDIR: &str = "cache";

/// 测试注入的状态根；`None` 表示按构建模式默认解析。
static TEST_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 是否已就「沙箱根不可达」告警过，避免每次路径解析都刷同一行日志。
#[cfg(debug_assertions)]
static FALLBACK_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 校验沙箱根是否仍可用：源码树被移走或二进制被拷到其它机器时回退系统目录。
#[cfg(debug_assertions)]
fn usable_dev_root(root: PathBuf) -> Option<PathBuf> {
    // `.dev` 的父目录即工作区根，其存在性等价于「编译期记录的源码树仍在本机」。
    if root.parent().is_some_and(|p| p.exists()) {
        return Some(root);
    }
    if !FALLBACK_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::warn!(
            "dev sandbox root {} is unreachable (source tree moved?); falling back to system directories",
            root.display()
        );
    }
    None
}

/// 开发沙箱的状态根；非 `debug` 构建恒为 `None`。
///
/// 由编译期常量 [`env!`]`("CARGO_MANIFEST_DIR")` 上溯两级得到工作区根：本 crate 位于
/// `crates/rterm-config`。用 `parent()` 而非 `join("..")`，避免产出带 `..` 的未规范化
/// 路径——该路径会进入启动日志，`..` 形式既不美观也不便排查。
/// 条件编译保证 `release` 构建不含此路径字面量。
///
/// 注意：本函数及 `DEV_DIR_NAME` 引用的**一切**（含 `path::Path`）都必须写成全限定形式，
/// 否则 `release` 构建会因「未使用的 import / 常量」被 `-D warnings` 拒绝——而只跑
/// `debug` 的 CI 不会发现这类问题。
#[cfg(debug_assertions)]
fn dev_root() -> Option<PathBuf> {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?;
    usable_dev_root(workspace.join(DEV_DIR_NAME))
}

/// 非 `debug` 构建没有沙箱根，直接走系统目录。
#[cfg(not(debug_assertions))]
fn dev_root() -> Option<PathBuf> {
    None
}

/// 当前生效的状态根：测试注入优先，其次按构建模式默认。
fn state_root() -> Option<PathBuf> {
    if let Ok(guard) = TEST_ROOT.lock()
        && let Some(root) = guard.as_ref()
    {
        return Some(root.clone());
    }
    dev_root()
}

/// 是否运行在开发沙箱中（`debug` 构建，或测试注入了状态根）。
///
/// 供钥匙串等无法用目录重定向的资源判断是否需要换用隔离标识。
pub fn is_sandboxed() -> bool {
    state_root().is_some()
}

/// 应用配置目录：沙箱下为 `<状态根>/config`，否则为 `<系统配置目录>/rterm`。
///
/// 仅计算路径，不创建目录；调用方按需 `create_dir_all`。
pub fn config_dir() -> Option<PathBuf> {
    match state_root() {
        Some(root) => Some(root.join(CONFIG_SUBDIR)),
        None => Some(dirs::config_dir()?.join(APP_DIR_NAME)),
    }
}

/// 应用缓存目录：沙箱下为 `<状态根>/cache`，否则为 `<系统缓存目录>/rterm`。
///
/// 仅计算路径，不创建目录；调用方按需 `create_dir_all`。
pub fn cache_dir() -> Option<PathBuf> {
    match state_root() {
        Some(root) => Some(root.join(CACHE_SUBDIR)),
        None => Some(dirs::cache_dir()?.join(APP_DIR_NAME)),
    }
}

/// 日志文件所在目录：缓存目录下的 `logs` 子目录。
///
/// 缓存目录不可定位时回退当前目录，保证日志初始化不会因缺目录而失败。
pub fn log_dir() -> PathBuf {
    cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("logs")
}

/// 将状态根重定向到指定目录，仅供测试使用。
///
/// 设置后 [`config_dir`] / [`cache_dir`] 改为在 `<root>/config`、`<root>/cache` 下解析，
/// 避免测试读写开发者真实配置；传 `None` 恢复按构建模式默认解析。
///
/// 覆盖是全局的且不会自动复原，因此测试应传入自己的临时目录，并在使用前调用。
pub fn set_test_root(root: Option<PathBuf>) {
    if let Ok(mut guard) = TEST_ROOT.lock() {
        *guard = root;
    }
}
