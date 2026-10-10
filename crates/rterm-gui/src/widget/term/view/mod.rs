//! 终端视图部件：把后端渲染内容绘成像素，并转发鼠标 / 键盘 / 输入法事件。
//!
//! 本模块声明 [`TerminalView`] 与跨帧状态 [`TerminalViewState`]；事件处理见 `input`、绘制见 `render`、滚动条见 `scrollbar`、`Widget` 实现见 `widget`。

mod input;
mod render;
mod scrollbar;
mod widget;

use crate::widget::term::terminal::Terminal;
use alacritty_terminal::index::Point as TerminalGridPoint;
use iced::Size;
use iced_core::keyboard::Modifiers;
use std::cell::Cell;

/// 终端画布部件：实现 iced `Widget`，把后端渲染内容绘成像素并转发鼠标 / 键盘事件。
pub struct TerminalView<'a> {
    /// 被渲染的终端实例，提供后端渲染内容、主题与字体等。
    term: &'a Terminal,
    /// 来自 app 的「终端是否持有键盘焦点」，用于绘制实心/空心光标与门控输入。
    focused: bool,
    /// 终端内容与外层容器边框之间的内边距（像素）。
    padding: f32,
    /// 是否显示并接管右侧滚动条。
    scrollbar: bool,
}

impl<'a> TerminalView<'a> {
    /// 以给定终端与焦点态构造可装箱的终端视图元素。
    pub fn show(term: &'a Terminal, focused: bool) -> Self {
        Self {
            term,
            focused,
            padding: 4.0,
            scrollbar: true,
        }
    }

    /// 设置终端内容的内边距（像素）。
    pub fn padding(mut self, value: f32) -> Self {
        self.padding = value;
        self
    }

    /// 设置是否显示右侧滚动条。
    pub fn scrollbar(mut self, enabled: bool) -> Self {
        self.scrollbar = enabled;
        self
    }
}

/// 终端视图的部件内部状态，跨帧保留交互与输入上下文。
#[derive(Debug, Clone)]
struct TerminalViewState {
    /// 当前是否处于鼠标拖拽（选区）进行中。
    is_dragged: bool,
    /// 上一次鼠标点击信息，用于判定单击、双击、三击。
    last_click: Option<iced_core::mouse::Click>,
    /// 滚轮像素滚动的累积余量，满一行才折算为行滚动。
    scroll_pixels: f32,
    /// 当前键盘修饰键状态（如 Ctrl、Cmd）。
    keyboard_modifiers: Modifiers,
    /// 部件最近一次布局得到的尺寸。
    size: Size<f32>,
    /// 鼠标当前对应的网格坐标点。
    mouse_position_on_grid: TerminalGridPoint,
    /// 上次绘制时记录的焦点状态：焦点变化但布局尺寸不变时，几何缓存不会重跑，故在 `draw` 中据此判断是否需清缓存以重绘光标（实心/空心）。
    last_focus: Cell<bool>,
    /// 上次绘制时记录的滚动条开关：同理，开关切换需清缓存才能让滚动条消失 / 出现。
    last_scrollbar: Cell<bool>,
    /// 输入法预编辑串（组合中、尚未上屏的文本），由本部件绘制在光标格处。
    preedit: Option<String>,
    /// 滚动条拖拽态：`Some` 为抓取点相对滑块顶部的偏移，`None` 表示未拖拽。
    scrollbar_drag: Option<f32>,
    /// 拖拽中上一次已下达的回滚偏移，作为相对增量的基准。
    ///
    /// 后端 `display_offset` 晚一轮才更新，同批事件读到同一旧值；拿它当基准会让增量叠加过冲。
    scrollbar_target: usize,
    /// 指针是否悬停在滚动条上（仅用于加深透明度）。
    scrollbar_hovered: bool,
}

impl TerminalViewState {
    /// 构造全部字段取默认初值的终端视图状态。
    fn new() -> Self {
        Self {
            is_dragged: false,
            last_click: None,
            scroll_pixels: 0.0,
            keyboard_modifiers: Modifiers::empty(),
            size: Size::from([0.0, 0.0]),
            mouse_position_on_grid: TerminalGridPoint::default(),
            last_focus: Cell::new(false),
            last_scrollbar: Cell::new(true),
            preedit: None,
            scrollbar_drag: None,
            scrollbar_target: 0,
            scrollbar_hovered: false,
        }
    }

    /// 收起指针离窗 / 窗口失焦后不再有释放事件送达的交互锁存；返回是否需要重绘。
    fn end_pointer_interactions(&mut self) -> bool {
        // 两个 take 都必须求值，不能并入 `||` 链——否则前项为真时短路，后续锁存清不掉。
        let hovered = std::mem::take(&mut self.scrollbar_hovered);
        let dragging = self.scrollbar_drag.take().is_some();
        let selecting = std::mem::take(&mut self.is_dragged);
        hovered || dragging || selecting
    }
}

impl Default for TerminalViewState {
    /// 经由 `new` 生成终端视图状态的默认值。
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
