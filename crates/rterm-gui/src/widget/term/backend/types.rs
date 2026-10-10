//! 终端后端的类型定义：宿主命令、鼠标 / 超链接动作与终端几何尺寸。

use crate::widget::term::actions::Action;
use alacritty_terminal::event::Event;
use alacritty_terminal::event::WindowSize;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use iced::keyboard::Modifiers;
use iced_core::Size;

/// 匹配可点击超链接的正则（覆盖 ipfs、magnet、http(s)、ssh 等协议）。
pub(super) const URL_REGEX: &str = r#"(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file://|git://|ssh:|ftp://)[^\u{0000}-\u{001F}\u{007F}-\u{009F}<>"\s{-}\^⟨⟩`]+"#;

/// 宿主向终端后端下发的命令。
#[derive(Debug, Clone)]
pub enum Command {
    /// 向终端写入原始字节流（键盘输入等）。
    Write(Vec<u8>),
    /// 按行滚动视口：**正值向上**（回滚历史）、**负值向下**（回到实时输出）。
    ///
    /// 沿用 alacritty `Scroll::Delta` 语义（`display_offset + n`）；像素到行的折算由 `view` 完成。
    Scroll(i32),
    /// 重设布局尺寸与字体测量结果（用于重算行列数）。
    Resize(Option<Size<f32>>, Option<Size<f32>>),
    /// 在给定像素坐标起点开始一次选区（指定选区类型）。
    SelectStart(SelectionType, (f32, f32)),
    /// 以给定像素坐标更新选区终点。
    SelectUpdate((f32, f32)),
    /// 在指定网格点执行超链接动作（悬停 / 清除 / 打开）。
    ProcessLink(LinkAction, Point),
    /// 向终端回送鼠标报告（按键、修饰键、坐标、是否按下）。
    MouseReport(MouseButton, Modifiers, Point, bool),
    /// 处理底层 alacritty 事件（标题变更、退出、PTY 写入等）。
    ProcessAlacrittyEvent(Event),
}
/// 鼠标协议模式，由 alacritty `TermMode` 推导而来。
#[derive(Debug, Clone)]
pub enum MouseMode {
    /// SGR（1006）扩展鼠标模式。
    Sgr,
    /// 普通鼠标模式，`bool` 表示是否使用 UTF-8 坐标编码（1005 模式）。
    Normal(bool),
}

impl From<TermMode> for MouseMode {
    /// 依据 alacritty `TermMode` 判断应使用的鼠标协议模式。
    fn from(term_mode: TermMode) -> Self {
        if term_mode.contains(TermMode::SGR_MOUSE) {
            MouseMode::Sgr
        } else if term_mode.contains(TermMode::UTF8_MOUSE) {
            MouseMode::Normal(true)
        } else {
            MouseMode::Normal(false)
        }
    }
}
/// 鼠标报告使用的按键编码（对应 X10 / SGR 鼠标协议的 button 字段）。
#[derive(Debug, Clone)]
pub enum MouseButton {
    /// 左键（编码 0）。
    LeftButton = 0,
    /// 中键（编码 1）。
    MiddleButton = 1,
    /// 右键（编码 2）。
    RightButton = 2,
    /// 左键拖动移动（编码 32）。
    LeftMove = 32,
    /// 中键拖动移动（编码 33）。
    MiddleMove = 33,
    /// 右键拖动移动（编码 34）。
    RightMove = 34,
    /// 无按键移动（编码 35）。
    NoneMove = 35,
    /// 向上滚动（编码 64）。
    ScrollUp = 64,
    /// 向下滚动（编码 65）。
    ScrollDown = 65,
    /// 其它按键（编码 99）。
    Other = 99,
}
/// 超链接（OSC 8）相关动作。
#[derive(Debug, Clone)]
pub enum LinkAction {
    /// 清除当前悬停的超链接。
    Clear,
    /// 标记某点处于超链接悬停态（用于高亮与点击判定）。
    Hover,
    /// 打开悬停的超链接。
    Open,
}
/// 终端的几何尺寸与字体度量，作为 alacritty `Dimensions` 的实现。
#[derive(Clone, Copy, Debug)]
pub struct TerminalSize {
    /// 单个单元格的像素宽度。
    pub cell_width: u16,
    /// 单个单元格的像素高度。
    pub cell_height: u16,
    /// 当前可见列数（由布局尺寸除以单元格宽度折算）。
    pub(super) num_cols: u16,
    /// 当前可见行数（由布局尺寸除以单元格高度折算）。
    pub(super) num_lines: u16,
    /// 布局区域的总宽度（像素）。
    pub(super) layout_width: f32,
    /// 布局区域的总高度（像素）。
    pub(super) layout_height: f32,
}

impl Default for TerminalSize {
    /// 返回 TerminalSize 的默认值（80 列、50 行、单元格 1×1 像素）。
    fn default() -> Self {
        Self {
            cell_width: 1,
            cell_height: 1,
            num_cols: 80,
            num_lines: 50,
            layout_width: 80.0,
            layout_height: 50.0,
        }
    }
}

impl Dimensions for TerminalSize {
    /// 返回视口总行数（此处等于可见行数）。
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    /// 返回可见列数。
    fn columns(&self) -> usize {
        self.num_cols as usize
    }

    /// 返回最后一列的索引。
    fn last_column(&self) -> Column {
        Column(self.num_cols as usize - 1)
    }

    /// 返回最底行的索引。
    fn bottommost_line(&self) -> Line {
        Line(self.num_lines as i32 - 1)
    }

    /// 返回可见行数。
    fn screen_lines(&self) -> usize {
        self.num_lines as usize
    }
}

impl From<TerminalSize> for WindowSize {
    /// 将 TerminalSize 转换为 alacritty `WindowSize`。
    fn from(size: TerminalSize) -> Self {
        Self {
            num_lines: size.num_lines,
            num_cols: size.num_cols,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        }
    }
}

/// 把 alacritty 事件归约为宿主动作：退出 / 标题 / 响铃需要宿主响应，其余静默忽略。
pub(super) fn action_for_event(event: Event) -> Action {
    match event {
        Event::Exit => Action::Shutdown,
        Event::Title(title) => Action::ChangeTitle(title),
        Event::Bell => Action::Bell,
        _ => Action::default(),
    }
}
