//! 终端部件向宿主（App）回流的语义动作。
//!
//! 由 view 事件循环（`view`）在收到后端事件后产出，App 据此更新 UI 状态
//! （如修改窗口标题、关闭标签等）。

#[derive(Debug, Clone, PartialEq, Default)]
/// 终端回流动作：把底层终端事件归纳为 App 需要响应的高层意图。
pub enum Action {
    /// 请求关闭当前终端标签。
    Shutdown,
    /// 请求把窗口 / 标签标题改为携带的字符串。
    ChangeTitle(String),
    /// 无需处理的事件（默认分支）。
    #[default]
    Ignore,
}
