//! 终端部件：基于 alacritty 渲染核心封装的 iced 自定义部件。
//!
//! 含 PTY 后端（`backend`）、按键 / 鼠标绑定（`bindings`）、设置（`settings`）、
//! 主题（`theme`）与渲染视图（`view`），并通过 `Terminal` / `Event` / `Command` 与宿主交互。
pub mod actions;
pub mod bindings;
pub mod settings;

mod backend;
mod font;
mod russh_pty;
mod terminal;
mod theme;
mod view;

pub use alacritty_terminal::event::Event as AlacrittyEvent;
pub use alacritty_terminal::index::Point as AlacrittyPoint;
pub use alacritty_terminal::selection::SelectionType;
pub use alacritty_terminal::term::TermMode;
pub use backend::Command as BackendCommand;
pub use backend::{LinkAction, MouseButton};
pub use russh_pty::RusshPty;
pub use terminal::{Command, Event, Terminal};
pub use theme::{ColorPalette, Theme};
pub use view::TerminalView;
