//! 快速命令弹窗（Ctrl+Shift+P）。
//!
//! 与其余模态框共用遮罩（[`crate::modal::overlay_wrap_dismissable`]，单击遮罩空白关闭），顶部对齐、宽度随窗口自适应；
//! 只有过滤输入框与命中列表，无按钮行。键盘路由见 [`crate::app::routing`]：Enter / ↑ / ↓ /
//! toggle 由全局分支就地接管，输入框只收字符。

use crate::t;

use crate::App;
use crate::app::snippets::{Message, filtered_snippets};
use iced::widget::text::Wrapping;
use iced::widget::{button, column, container, scrollable, text, text_input};
use iced::{Element, Length, Theme};
use rterm_core::ConnectionStatus;

/// 弹窗宽度（像素）。
const PALETTE_WIDTH: f32 = 560.0;
/// 弹窗顶部与窗口顶边的间距（像素）：浮层贴近顶部，像命令面板而不是居中对话框。
const TOP_GAP: f32 = 96.0;
/// 列表最大高度（像素）；超出部分滚动。
pub(crate) const MAX_LIST_HEIGHT: f32 = 320.0;
/// 行高估为（像素）：两行文字 + 上下 padding，用于把列表高度收拢到实际行数。
pub(crate) const ROW_HEIGHT: f32 = 48.0;
/// 命中行间距（像素）。
pub(crate) const LIST_SPACING: f32 = 2.0;
/// 行步距（行高 + 间距）：键盘滚动按此把第 `pos` 行映射为内容 y。
pub(crate) const ROW_STRIDE: f32 = ROW_HEIGHT + LIST_SPACING;
/// 键盘滚动越界时的额外余量（像素）：让相邻行边缘入画，提示还有更多内容。
pub(crate) const REVEAL_MARGIN: f32 = 10.0;
/// 弹窗与窗口边缘的最小留白（像素），同其它弹窗的防贴边约定。
const DIALOG_MARGIN: f32 = 16.0;

/// 快速命令弹窗；关闭时返回 `None`（不占据叠加层）。
pub fn view(app: &App) -> Option<Element<'_, Message>> {
    if !app.snippets.palette.open {
        return None;
    }
    let panel = iced::widget::responsive(move |size| {
        let max_w = (size.width - DIALOG_MARGIN * 2.0).max(0.0);
        let width = PALETTE_WIDTH.min(max_w);
        let max_h = (size.height - TOP_GAP - DIALOG_MARGIN).max(0.0);
        let hits = filtered_hits(app);
        let list_h = (hits.len() as f32 * ROW_HEIGHT)
            .min(MAX_LIST_HEIGHT)
            .min(max_h);
        container(
            container(body(app, list_h))
                .width(width)
                .style(crate::ui::dialog_panel_style(None))
                .padding(10),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding {
            top: TOP_GAP,
            ..Default::default()
        })
        .center_x(Length::Fill)
        .into()
    });
    Some(crate::modal::overlay_wrap_dismissable(
        panel.into(),
        Message::ClosePalette,
    ))
}

/// 弹窗主体：过滤输入框 + 命中列表（单行：名称与文本预览，单击即发送），样式对齐会话列表。
fn body(app: &App, list_h: f32) -> Element<'_, Message> {
    let input = text_input(
        &t!("snippet.palette_placeholder"),
        &app.snippets.palette.query,
    )
    .id(app.snippets.palette.input_id.clone())
    .on_input(Message::PaletteQuery)
    .size(13)
    .padding([5u16, 8u16])
    .style(crate::ui::search_input_style);

    let hits = filtered_hits(app);
    let list: Element<'_, Message> = if hits.is_empty() {
        container(
            text(t!("snippet.palette_no_match"))
                .size(13)
                .style(secondary_text),
        )
        .width(Length::Fill)
        .padding([12u16, 14u16])
        .into()
    } else {
        let rows: Vec<Element<'_, Message>> = hits
            .iter()
            .enumerate()
            .map(|(pos, &idx)| palette_row(app, pos, idx))
            .collect();
        scrollable(column(rows).spacing(LIST_SPACING))
            .id(app.snippets.palette.list_id.clone())
            .on_scroll(|viewport| Message::PaletteScrolled(viewport.absolute_offset().y))
            .direction(crate::ui::thin_scrollbar_direction())
            .width(Length::Fill)
            .height(Length::Fixed(list_h))
            .into()
    };

    column![input, list].spacing(8).into()
}

/// 当前过滤词下的命中下标（清单原始下标）。
fn filtered_hits(app: &App) -> Vec<usize> {
    filtered_snippets(&app.snippets.items, &app.snippets.palette.query)
}

/// 命中行：高亮位 = 当前键盘高亮；单击对齐高亮并发送。
fn palette_row<'a>(app: &'a App, pos: usize, idx: usize) -> Element<'a, Message> {
    let snippet = &app.snippets.items[idx];
    let selected = app.snippets.palette.highlight == Some(pos);
    button(
        container(
            column![
                text(snippet.display_name())
                    .size(14)
                    .wrapping(Wrapping::None),
                text(preview_line(&snippet.text))
                    .size(12)
                    .wrapping(Wrapping::None)
                    .style(secondary_text),
            ]
            .spacing(2),
        )
        .width(Length::Fill)
        .clip(true),
    )
    .on_press(Message::PalettePick(pos))
    .padding([6u16, 10u16])
    .width(Length::Fill)
    .style(move |theme, status| row_style(theme, status, selected))
    .into()
}

/// 命中行样式：复用列表行底色（高亮位同选中态、悬浮同悬浮态），圆角对齐会话列表行，
/// 片段无连接态恒传 `Disconnected`。
fn row_style(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let cs = crate::theme::list_row_bg(
        theme,
        selected,
        matches!(status, button::Status::Hovered),
        ConnectionStatus::Disconnected,
    );
    button::Style {
        background: cs.background,
        text_color: cs.text_color.unwrap_or(theme.palette().text),
        border: iced::Border {
            radius: 6.0.into(),
            ..cs.border
        },
        shadow: cs.shadow,
        snap: false,
    }
}

/// 行内文本预览：取首个非空行并 trim（同管理面板，不撑高行高）。
fn preview_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 次要文本样式（行内预览、无命中提示）。
fn secondary_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(crate::theme::custom_palette(theme).text_secondary),
    }
}

/// 让第 `pos` 行完整可见的滚动目标 y；已在视口内返回 `None`（无需滚动）。
///
/// 视口高取 `count * ROW_HEIGHT` 与 [`MAX_LIST_HEIGHT`] 的较小值，同视图侧 `list_h` 的
/// 收拢公式（不含窗口过小时的贴边收窄，属可接受的边缘误差）。越界滚动额外带
/// [`REVEAL_MARGIN`] 余量；贴顶时夹到 0。
pub(crate) fn scroll_target(scroll_y: f32, pos: usize, count: usize) -> Option<f32> {
    let top = pos as f32 * ROW_STRIDE;
    let bottom = top + ROW_HEIGHT;
    let viewport = (count as f32 * ROW_HEIGHT).min(MAX_LIST_HEIGHT);
    if top < scroll_y {
        Some((top - REVEAL_MARGIN).max(0.0))
    } else if bottom > scroll_y + viewport {
        Some(bottom - viewport + REVEAL_MARGIN)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_target_within_viewport_is_none() {
        assert_eq!(scroll_target(0.0, 0, 3), None);
        assert_eq!(scroll_target(28.0 + REVEAL_MARGIN, 6, 15), None);
    }

    #[test]
    fn scroll_target_bottom_edge_scrolls_to_reveal() {
        assert_eq!(scroll_target(0.0, 6, 15), Some(28.0 + REVEAL_MARGIN));
    }

    #[test]
    fn scroll_target_top_edge_scrolls_back_up() {
        assert_eq!(scroll_target(100.0, 0, 15), Some(0.0));
    }

    #[test]
    fn scroll_target_last_row_lands_at_max_scroll() {
        let y = scroll_target(0.0, 14, 15).expect("末行必须触发滚动");
        assert_eq!(y, 428.0 + REVEAL_MARGIN);
        assert_eq!(scroll_target(y, 14, 15), None, "落点后应已完整可见");
    }
}
