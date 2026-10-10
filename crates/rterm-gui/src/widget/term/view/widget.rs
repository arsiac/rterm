//! [`TerminalView`] 的 iced `Widget` 实现：布局、事件转发与鼠标样式的入口。

use super::scrollbar::scrollbar_geometry;
use super::{TerminalView, TerminalViewState, input, render};
use crate::widget::term::terminal::Event;
use alacritty_terminal::term::TermMode;
use iced::mouse::Cursor;
use iced::{Element, Length, Rectangle, Size, Theme};
use iced_core::widget::operation;
use iced_graphics::core::Widget;
use iced_graphics::core::widget::{Tree, tree};

impl Widget<Event, Theme, iced::Renderer> for TerminalView<'_> {
    /// 返回部件建议尺寸（宽高均填满父容器）。
    fn size(&self) -> Size<Length> {
        Size {
            width: Length::Fill,
            height: Length::Fill,
        }
    }

    /// 返回部件状态类型标签，指向 `TerminalViewState`。
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<TerminalViewState>()
    }

    /// 构造并返回部件的初始内部状态 `TerminalViewState`。
    fn state(&self) -> tree::State {
        tree::State::new(TerminalViewState::new())
    }

    /// 计算部件布局节点，使其填满可用的宽高限制。
    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &iced_core::layout::Limits,
    ) -> iced_core::layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fill, Size::ZERO);
        iced::advanced::layout::Node::new(size)
    }

    /// 应用部件操作（此处无需额外处理，留空）。
    fn operate(
        &mut self,
        _tree: &mut Tree,
        _layout: iced_core::Layout<'_>,
        _renderer: &iced::Renderer,
        _operation: &mut dyn operation::Operation,
    ) {
    }

    /// 将后端渲染内容绘制为几何图元（背景、文本、光标、下划线）。
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout,
        _cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<TerminalViewState>();
        render::draw(self, state, renderer, layout, viewport);
    }

    /// 转发鼠标与键盘事件，收集命令并发布给后端。
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced_core::Event,
        layout: iced_graphics::core::Layout<'_>,
        cursor: Cursor,
        _renderer: &iced::Renderer,
        clipboard: &mut dyn iced_graphics::core::Clipboard,
        shell: &mut iced_graphics::core::Shell<'_, Event>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<TerminalViewState>();
        input::update(self, state, event, layout, cursor, clipboard, shell);
    }
    /// 返回鼠标交互样式：滚动条上竖直调整，超链接上显示手型，内容区为文本光标，其余默认。
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> iced_core::mouse::Interaction {
        let state = tree.state.downcast_ref::<TerminalViewState>();
        let mut cursor_mode = iced_core::mouse::Interaction::Idle;
        let terminal_mode = self.term.backend.renderable_content().terminal_mode;
        if self.is_cursor_in_layout(cursor, layout) && !terminal_mode.contains(TermMode::SGR_MOUSE)
        {
            cursor_mode = iced_core::mouse::Interaction::Text;
        }

        if self.is_cursor_hovered_hyperlink(state) {
            cursor_mode = iced_core::mouse::Interaction::Pointer;
        }

        // 滚动条条带优先于文本 / 手型：拖动中或悬停在轨道上都用竖直调整光标。
        if let Some(position) = cursor.position() {
            let over_track = self.scrollbar
                && scrollbar_geometry(
                    layout.bounds(),
                    self.padding,
                    self.term.backend.renderable_content(),
                )
                .is_some_and(|(track, _)| track.contains(position));
            if state.scrollbar_drag.is_some() || over_track {
                cursor_mode = iced_core::mouse::Interaction::ResizingVertically;
            }
        }

        cursor_mode
    }
}

impl<'a> From<TerminalView<'a>> for Element<'a, Event, Theme, iced::Renderer> {
    /// 将终端视图包装为可加入 iced 元素树的 `Element`。
    fn from(widget: TerminalView<'a>) -> Self {
        Self::new(widget)
    }
}
