//! 窗口级模态遮罩：全屏半透明背景 + 拦截穿透事件。
//!
//! 各模态弹窗（设置 / 主密钥 / 主机密钥 / 会话编辑器 / 粘贴确认 / SFTP）统一经
//! [`overlay_wrap`] 包装后，由 [`crate::layout`] 用 `stack` 叠加到窗口最顶层。

use iced::widget::container;
use iced::{Color, Element, Length, Theme};
use iced_core::layout::{Layout, Limits, Node};
use iced_core::mouse::{self, Cursor, Interaction};
use iced_core::widget::operation;
use iced_core::widget::tree::{self, Tree};
use iced_core::{Clipboard, Event, Rectangle, Shell, Size, Vector};
use iced_core::{overlay, renderer};

/// 将子元素包装为全屏模态遮罩：半透明黑底 + 居中 + 拦截鼠标 / 键盘事件穿透。
///
/// 与消息类型无关（遮罩层只做布局 / 事件拦截），故对消息类型 `M` 泛型，
/// 既可用于 SFTP 模块消息，也可用于主密钥 / 主机密钥等顶层消息对话框。
pub fn overlay_wrap<'a, M: Clone + 'a>(child: Element<'a, M>) -> Element<'a, M> {
    let overlay = container(child)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_: &Theme| container::Style {
            background: Some(iced::Background::Color(Color::from_rgba(
                0.0, 0.0, 0.0, 0.30,
            ))),
            ..Default::default()
        })
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    modal(overlay.into())
}

/// 包装模态遮罩内容：在子元素处理事件之后捕获鼠标按下与键盘事件，阻止穿透到下层。
///
/// iced 内建 `opaque` 只捕获鼠标按下，键盘事件仍会下探到下层组件（如终端持有键盘焦点时
/// 照常把按键写入 PTY），故此处对键盘一并捕获。捕获发生在内层处理之后，故弹窗自身控件
/// 仍能收到按键，仅阻断向窗口其余部分的传播。
fn modal<'a, M, T, R>(content: Element<'a, M, T, R>) -> Element<'a, M, T, R>
where
    M: 'a,
    T: 'a,
    R: iced_core::Renderer + 'a,
{
    Element::new(Modal { content })
}

/// [`modal`] 的包装组件；仅拦截穿透事件，布局与绘制全部转发给子元素。
struct Modal<'a, M, T, R> {
    content: Element<'a, M, T, R>,
}

impl<M, T, R> iced_core::Widget<M, T, R> for Modal<'_, M, T, R>
where
    R: iced_core::Renderer,
{
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &R, limits: &Limits) -> Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut R,
        theme: &T,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &R,
        operation: &mut dyn operation::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &R,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        // 鼠标按下仅在命中遮罩范围内拦截；键盘与位置无关，一律拦截。
        let block = matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(_)) | Event::Keyboard(_)
        );

        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );

        if block && (matches!(event, Event::Keyboard(_)) || cursor.is_over(layout.bounds())) {
            shell.capture_event();
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &R,
    ) -> Interaction {
        let interaction = self
            .content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer);

        if interaction == Interaction::None && cursor.is_over(layout.bounds()) {
            Interaction::Idle
        } else {
            interaction
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &R,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, T, R>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}
