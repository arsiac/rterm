//! 终端部件向宿主（App）回流的语义动作：部件如实上报，不决定响应与否。
//! `Bell` 走标签闪烁、`SearchStats` 回搜索计数；`Shutdown` 与 `ChangeTitle` 有意忽略
//! （见 `app::terminal_bridge` 的分流）。

use crate::widget::term::backend::SearchOutcome;

#[derive(Debug, Clone, PartialEq, Default)]
/// 终端回流动作：底层事件归纳出的高层意图，宿主不一定都响应。
pub enum Action {
    /// alacritty 报告 PTY 已结束、终端自身收尾，不构成关标签请求。
    Shutdown,
    /// 远端经 OSC 0/2 上报的标题；标签标题承担会话身份，不随之变化。
    ChangeTitle(String),
    /// 终端收到响铃（BEL）。
    Bell,
    /// 一次搜索查询的执行结果（由 `Command::SearchSetQuery` 回带）。
    SearchStats(SearchOutcome),
    /// 无需处理的事件（默认分支）。
    #[default]
    Ignore,
}
