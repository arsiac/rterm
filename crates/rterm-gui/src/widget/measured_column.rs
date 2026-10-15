//! 两趟布局的竖排列：第一趟在压缩态下量出每个子项的自报宽度取最大，第二趟以该宽度为定值重排，
//! `Fill` 子项即铺满该宽度。
//!
//! iced 的 `Column` 在收缩布局下无法兼顾「宽度自适应最长子项」与「`Fill` 子项铺满行宽」
//! （全 `Fill` 时行宽基准恒为 0，整列坍缩）。菜单（[`crate::ui::menu_container`]）用此布局。

use iced_core::layout::flex::{self, Axis};
use iced_core::layout::{Layout, Limits, Node};
use iced_core::mouse::{Cursor, Interaction};
use iced_core::widget::operation;
use iced_core::widget::tree::Tree;
use iced_core::{
    Alignment, Clipboard, Element, Event, Length, Padding, Pixels, Rectangle, Shell, Size, Vector,
    Widget, overlay, renderer,
};

/// 宽度自适应、子项铺满的竖排列。
#[allow(missing_debug_implementations)]
pub struct MeasuredColumn<'a, Message, Theme = iced::Theme, Renderer = iced::Renderer> {
    spacing: f32,
    max_width: f32,
    align: Alignment,
    children: Vec<Element<'a, Message, Theme, Renderer>>,
}

impl<'a, Message, Theme, Renderer> MeasuredColumn<'a, Message, Theme, Renderer> {
    /// 构造空列。
    pub fn new() -> Self {
        Self {
            spacing: 0.0,
            max_width: f32::INFINITY,
            align: Alignment::Start,
            children: Vec::new(),
        }
    }

    /// 以给定子项构造列。
    pub fn with_children(children: Vec<Element<'a, Message, Theme, Renderer>>) -> Self {
        Self {
            children,
            ..Self::new()
        }
    }

    /// 追加一个子项。
    pub fn push(mut self, child: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        self.children.push(child.into());
        self
    }

    /// 设置子项间距（像素）。
    pub fn spacing(mut self, amount: impl Into<Pixels>) -> Self {
        self.spacing = amount.into().0;
        self
    }

    /// 设置宽度上限（像素）：超过上限的子项按自身规则折行。
    pub fn max_width(mut self, limit: impl Into<Pixels>) -> Self {
        self.max_width = limit.into().0;
        self
    }
}

impl<Message, Theme, Renderer> Default for MeasuredColumn<'_, Message, Theme, Renderer> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for MeasuredColumn<'_, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        // 第一趟：压缩态下量自报宽度——`Fill` 子项此时退到内容宽，不贡献虚高的基准。
        let max = limits.max();
        let probe = Limits::new(
            Size::ZERO,
            Size::new(self.max_width.min(max.width), max.height),
        )
        .width(Length::Shrink)
        .height(Length::Shrink);
        let mut widest: f32 = 0.0;
        for (child, state) in self.children.iter_mut().zip(&mut tree.children) {
            let size = child.as_widget_mut().layout(state, renderer, &probe).size();
            widest = widest.max(size.width);
        }

        // 第二趟：以该宽度为定值重排，压缩标记随之清除，`Fill` 子项铺满整行。
        let width = Length::Fixed(widest);
        flex::resolve(
            Axis::Vertical,
            renderer,
            &limits.width(width).height(Length::Shrink),
            width,
            Length::Shrink,
            Padding::ZERO,
            self.spacing,
            self.align,
            &mut self.children,
            &mut tree.children,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn operation::Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, state), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(state, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let viewport = layout.bounds().intersection(viewport).unwrap_or(*viewport);
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .filter(|(_, layout)| layout.bounds().intersects(&viewport))
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, &viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<MeasuredColumn<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced_core::Renderer + 'a,
{
    fn from(column: MeasuredColumn<'a, Message, Theme, Renderer>) -> Self {
        Self::new(column)
    }
}
