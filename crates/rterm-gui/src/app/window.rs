//! 主窗口生命周期与几何记忆模块。
//!
//! 负责捕获主窗口 id，并在窗口关闭时查询「是否最大化」，据此决定是否把**非最大化**的
//! 窗口尺寸上行给父层写回配置。最大化状态下关闭会保留上一次非最大化尺寸，避免把最大化
//! 后的尺寸记入配置。模块自身只持有窗口 id，配置写回与窗口关闭一律经 [`Event`] 由父层落地。

use iced::window;
use iced::{Size, Task};

/// 模块状态：主窗口 id。
#[derive(Default)]
pub struct State {
    /// 主窗口 id（由 `Opened` / `CloseRequested` 事件捕获），供关闭流程与后续扩展使用。
    id: Option<window::Id>,
}

impl State {
    /// 构造空状态。
    pub fn new() -> Self {
        Self::default()
    }

    /// 主窗口 id；尚未收到窗口事件时为 `None`。
    pub fn id(&self) -> Option<window::Id> {
        self.id
    }

    /// 模块更新：记录窗口 id 并驱动关闭流程；写回配置 / 关闭窗口经 [`Event`] 上行，
    /// 模块绝不直接改父状态。
    pub fn update(&mut self, msg: Message, ctx: &Ctx) -> Task<Event> {
        match msg {
            // 窗口创建完成：记录 id（关闭时需据此查询几何）。
            Message::Opened(id) => {
                self.id = Some(id);
                Task::none()
            }
            // 用户请求关闭：未开启窗口记忆时直接关闭，否则先查询是否最大化。
            Message::CloseRequested(id) => {
                self.id = Some(id);
                if ctx.remember_window_size {
                    Task::done(Event::QueryMaximized(id))
                } else {
                    Task::done(Event::Close(id))
                }
            }
            // 关闭前查询到最大化状态：最大化时保留旧尺寸直接关闭；否则继续查询当前尺寸。
            Message::MaximizedForClose(id, maximized) => {
                if maximized {
                    Task::done(Event::Close(id))
                } else {
                    Task::done(Event::QuerySize(id))
                }
            }
            // 关闭前查询到非最大化尺寸：上行父层写回后关闭窗口。
            Message::SizeForClose(id, size) => Task::done(Event::PersistAndClose(id, size)),
        }
    }
}

/// 模块内部消息。
///
/// 由父层经 `Message::Window` 路由进来；模块 `update` 自行消费，不外泄。
#[derive(Clone, Copy)]
pub enum Message {
    /// 主窗口创建完成（记录 id）。
    Opened(window::Id),
    /// 用户请求关闭主窗口。
    CloseRequested(window::Id),
    /// 关闭前查询「窗口是否最大化」的结果。
    MaximizedForClose(window::Id, bool),
    /// 关闭前查询到的窗口尺寸。
    SizeForClose(window::Id, Size),
}

/// 模块上行事件：请求父层执行窗口查询、配置写回与关闭。
#[derive(Clone, Copy)]
pub enum Event {
    /// 请求父层查询窗口是否最大化。
    QueryMaximized(window::Id),
    /// 请求父层查询窗口尺寸。
    QuerySize(window::Id),
    /// 直接关闭窗口（最大化或未开启窗口记忆，无需保存尺寸）。
    Close(window::Id),
    /// 把非最大化尺寸写回配置并关闭窗口。
    PersistAndClose(window::Id, Size),
}

/// 父层只读上下文：是否开启窗口大小记忆。
pub struct Ctx {
    /// 对应 `AppConfig::remember_window_size`。
    pub remember_window_size: bool,
}
