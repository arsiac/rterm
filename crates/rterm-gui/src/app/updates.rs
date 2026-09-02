//! 更新检查模块

use iced::Task;
use log::warn;
use std::time::{SystemTime, UNIX_EPOCH};

/// 更新检查节流间隔（24 小时），避免每次启动都请求网络。
const THROTTLE_SECS: i64 = 24 * 3600;

/// 模块状态：顶部更新提示横幅。
#[derive(Default)]
pub struct State {
    /// 顶部更新提示横幅：发现新版本时持有（版本号 + 发布页 URL），关闭后置 `None`。
    pub banner: Option<(String, String)>,
}

impl State {
    /// 构造空状态（无横幅）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 模块更新：只改自身 `State`；需要父层配合的事以 [`Event`] 经 `Task` 上行。
    ///
    /// `ctx` 为父层传入的只读上下文（自动检查开关 + 上次检查时间戳），模块据此判定节流，
    /// 但**绝不写父状态**；时间戳写回经 [`Event::SetLastCheck`] 由父层落地。
    pub fn update(&mut self, msg: Message, ctx: &Ctx) -> Task<Event> {
        match msg {
            // 启动自动检查：仅在开启且距上次检查超过 24h 时才发起。
            Message::CheckOnStartup => {
                if !ctx.auto_check || !should_check(ctx.last_check_unix) {
                    return Task::none();
                }
                // 立即写回时间戳以确立节流窗口（无论本次成败，24h 内不再重复）。
                Task::batch([
                    Task::done(Event::SetLastCheck(Some(now_unix()))),
                    check_task(),
                ])
            }
            // 手动检查：忽略节流（用户显式点了「检查更新」）。
            Message::CheckNow => check_task(),
            Message::CheckResult(result) => match result {
                Ok(Some(info)) => {
                    self.banner = Some((info.version, info.html_url));
                    // 写回时间戳以维持节流窗口（无更新 / 出错则不写，故失败不会阻断后续自动检查）。
                    Task::done(Event::SetLastCheck(Some(now_unix())))
                }
                // 无更新或版本无法判定：清除可能残留的横幅。
                Ok(None) => {
                    self.banner = None;
                    Task::none()
                }
                Err(e) => {
                    // 尽力而为：网络 / 状态 / 解析失败仅记录，不向用户弹错以免打扰。
                    warn!("更新检查失败: {e}");
                    Task::none()
                }
            },
            Message::OpenReleasePage(url) => {
                if let Err(e) = open::that(&url) {
                    warn!("打开发布页失败: {e}");
                }
                Task::none()
            }
            // 关闭横幅仅隐藏，下次检查仍可能重新出现。
            Message::DismissBanner => {
                self.banner = None;
                Task::none()
            }
        }
    }
}

/// 模块内部消息：检查意图与模块自处理的异步结果。
///
/// 由父层经 `Message::Updates` 路由进来；模块 `update` 自行消费，不外泄。
#[derive(Clone)]
pub enum Message {
    /// 启动自动检查：仅当开启且距上次检查超过 24h 时才真正发起，否则空转。
    CheckOnStartup,
    /// 立即检查一次（设置页「检查更新」按钮，忽略节流）。
    CheckNow,
    /// 检查完成（有更新为 `ReleaseInfo`，无更新为 `Ok(None)`，出错为 `Err`）。
    CheckResult(Result<Option<crate::update_check::ReleaseInfo>, String>),
    /// 在浏览器 / 系统默认处理器中打开发布页（携带 URL）。
    OpenReleasePage(String),
    /// 关闭顶部更新提示横幅（仅隐藏，下次检查仍可能重新出现）。
    DismissBanner,
}

/// 上行事件：仅通知父层，由父层 `Message::UpdatesEvent` 分支写回配置并落盘。
///
/// 模块绝不写父状态；时间戳一律经本事件由父层写入 `AppConfig` 并 `save()`。
#[derive(Clone)]
pub enum Event {
    /// 写回「上次检查时间戳」并落盘（`None` 表示清除）。
    SetLastCheck(Option<i64>),
    /// 自回路：把一条模块内部消息经父层派发回 `State::update`。
    ///
    /// 检查结果需重新进入模块自身（更新横幅 / 记录日志），但模块 `update` 只能经 `Event` 上行、
    /// 不能写父态；故把内部消息装进 `Emit` 上行，父层在 `Message::UpdatesEvent` 分支收到后再
    /// `self.updates.update` 一次，形成自回路。
    Emit(Box<Message>),
}

/// 父层只读上下文：自动检查开关与上次检查时间戳，供模块判定节流，不写回。
pub struct Ctx {
    /// 是否开启启动自动检查（`AppConfig::auto_check_updates`）。
    pub auto_check: bool,
    /// 上次检查的 Unix 时间戳（`None` 表示从未检查）。
    pub last_check_unix: Option<i64>,
}

/// 发起一次 GitHub Releases 查询，结果经 [`Message::CheckResult`] 回流（自回路）。
fn check_task() -> Task<Event> {
    let repo = crate::update_check::resolve_repo();
    Task::perform(
        async move { crate::update_check::check_latest(&repo).await },
        |res| Event::Emit(Box::new(Message::CheckResult(res))),
    )
}

/// 距上次检查已超过 24h（或从未检查）则返回 `true`。
fn should_check(last: Option<i64>) -> bool {
    match last {
        None => true,
        Some(ts) => now_unix() - ts >= THROTTLE_SECS,
    }
}

/// 当前 Unix 时间戳（秒）；系统时钟异常时回退 0。
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    /// 在临时 tokio 运行时里把 `Task<Event>` 跑完并收集产出的事件（与 `transfer` 的
    /// `run_events` 同源，只是 `Event` 落在此模块）。
    fn run_events(task: iced::Task<Event>) -> Vec<Event> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let mut stream = match iced_runtime::task::into_stream(task) {
                Some(s) => s,
                None => return Vec::new(),
            };
            let mut out = Vec::new();
            while let Some(action) = stream.next().await {
                if let iced_runtime::Action::Output(msg) = action {
                    out.push(msg);
                }
            }
            out
        })
    }

    /// 只读上下文：默认关闭自动检查，避免测试真的去请求 GitHub。
    fn ctx() -> Ctx {
        Ctx {
            auto_check: false,
            last_check_unix: None,
        }
    }

    /// 造一条「发现更新」的结果，避免依赖网络。
    fn release() -> crate::update_check::ReleaseInfo {
        crate::update_check::ReleaseInfo {
            version: "9.9.9".to_string(),
            html_url: "https://example.com/release".to_string(),
        }
    }

    /// 关闭自动检查 / 尚在 24h 节流窗口内时，启动检查应完全空转（不发请求、不写时间戳）。
    #[test]
    fn startup_check_is_throttled_or_disabled() {
        let mut s = State::new();
        // 关闭自动检查：空转。
        let events = run_events(s.update(Message::CheckOnStartup, &ctx()));
        assert!(events.is_empty(), "关闭自动检查后启动检查应空转");
        // 刚检查过（不足 24h）：空转。
        let events = run_events(s.update(
            Message::CheckOnStartup,
            &Ctx {
                auto_check: true,
                last_check_unix: Some(now_unix()),
            },
        ));
        assert!(events.is_empty(), "24h 内的启动检查应被节流掉");
    }

    /// 节流判定：从未检查过 / 超过 24h 才允许发起。
    #[test]
    fn throttle_window_is_24h() {
        assert!(should_check(None), "从未检查过应允许发起");
        assert!(!should_check(Some(now_unix())), "刚检查过不应再次发起");
        assert!(
            should_check(Some(now_unix() - THROTTLE_SECS - 1)),
            "超过 24h 应允许再次发起"
        );
    }

    /// 发现更新：持有横幅并上行写回时间戳（节流窗口由成功检查确立）。
    #[test]
    fn found_release_sets_banner_and_writes_timestamp() {
        let mut s = State::new();
        let events = run_events(s.update(Message::CheckResult(Ok(Some(release()))), &ctx()));
        assert_eq!(
            s.banner.as_ref().map(|(v, _)| v.as_str()),
            Some("9.9.9"),
            "发现更新应持有横幅"
        );
        assert!(
            matches!(events.as_slice(), [Event::SetLastCheck(Some(_))]),
            "发现更新应上行时间戳"
        );
    }

    /// 无更新 / 出错：不写时间戳（失败不会吃掉后续自动检查），无更新时清除残留横幅。
    #[test]
    fn no_release_or_error_keeps_throttle_window_open() {
        let mut s = State::new();
        s.banner = Some(("9.9.9".to_string(), "https://example.com".to_string()));

        let events = run_events(s.update(Message::CheckResult(Ok(None)), &ctx()));
        assert!(events.is_empty(), "无更新不应写时间戳");
        assert!(s.banner.is_none(), "无更新应清除残留横幅");

        s.banner = Some(("9.9.9".to_string(), "https://example.com".to_string()));
        let events = run_events(s.update(Message::CheckResult(Err("网络不可用".into())), &ctx()));
        assert!(events.is_empty(), "出错不应写时间戳");
        assert!(s.banner.is_some(), "出错不应改动已有横幅");
    }

    /// 关闭横幅：仅隐藏，不影响后续检查。
    #[test]
    fn dismiss_clears_banner_only() {
        let mut s = State::new();
        let _ = run_events(s.update(Message::CheckResult(Ok(Some(release()))), &ctx()));
        let _ = run_events(s.update(Message::DismissBanner, &ctx()));
        assert!(s.banner.is_none(), "关闭后应清除横幅");
    }
}
