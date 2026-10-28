//! 会话快速选择弹窗（Ctrl+Shift+K）
//!
//! 与快速命令弹窗同构：共用遮罩，顶部对齐、宽度随窗口自适应，只有过滤输入框与命中列表。
//! 键盘路由见 [`crate::app::routing`]：Enter / ↑ / ↓ / toggle 由全局分支就地接管。

use crate::t;

use crate::App;
use crate::app::session::{Message, palette_sessions};
use iced::widget::text::Wrapping;
use iced::widget::{button, column, container, text, text_input};
use iced::{Element, Length, Theme};
use rterm_config::SessionConfig;
use rterm_core::ConnectionStatus;

/// 弹窗宽度（像素）
const PALETTE_WIDTH: f32 = 560.0;
/// 弹窗顶部与窗口顶边的间距（像素）：浮层贴近顶部，像命令面板而不是居中对话框
const TOP_GAP: f32 = 96.0;
/// 列表最大高度（像素）；超出部分滚动
pub(crate) const MAX_LIST_HEIGHT: f32 = 320.0;
/// 行高估为（像素）：两行文字 + 上下 padding，用于把列表高度收拢到实际行数
pub(crate) const ROW_HEIGHT: f32 = 48.0;
/// 命中行间距（像素）
pub(crate) const LIST_SPACING: f32 = 2.0;
/// 行步距（行高 + 间距）：键盘滚动按此把第 `pos` 行映射为内容 y
pub(crate) const ROW_STRIDE: f32 = ROW_HEIGHT + LIST_SPACING;
/// 键盘滚动越界时的额外余量（像素）：让相邻行边缘入画，提示还有更多内容
pub(crate) const REVEAL_MARGIN: f32 = 10.0;
/// 弹窗与窗口边缘的最小留白（像素），同其它弹窗的防贴边约定
const DIALOG_MARGIN: f32 = 16.0;

/// 快速选择弹窗；关闭时返回 `None`（不占据叠加层）
pub fn view(app: &App) -> Option<Element<'_, Message>> {
    if !app.session.palette.open {
        return None;
    }
    let panel = iced::widget::responsive(move |size| {
        let max_w = (size.width - DIALOG_MARGIN * 2.0).max(0.0);
        let width = PALETTE_WIDTH.min(max_w);
        let max_h = (size.height - TOP_GAP - DIALOG_MARGIN).max(0.0);
        let hits = hits(app);
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

/// 弹窗主体：过滤输入框 + 命中列表（单行：名称与 `user@host`，单击即连接）
fn body(app: &App, list_h: f32) -> Element<'_, Message> {
    let input = text_input(
        &t!("session.palette_placeholder"),
        &app.session.palette.query,
    )
    .id(app.session.palette.input_id.clone())
    .on_input(Message::PaletteQuery)
    .size(13)
    .padding([5u16, 8u16])
    .style(crate::ui::search_input_style);

    let hits = hits(app);
    let list: Element<'_, Message> = if hits.is_empty() {
        container(
            text(t!("session.palette_no_match"))
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
            .map(|(pos, s)| palette_row(app, pos, s))
            .collect();
        crate::ui::thin_vertical_scrollbar(column(rows).spacing(LIST_SPACING))
            .id(app.session.palette.list_id.clone())
            .on_scroll(|viewport| Message::PaletteScrolled(viewport.absolute_offset().y))
            .width(Length::Fill)
            .height(Length::Fixed(list_h))
            .into()
    };

    column![input, list].spacing(8).into()
}

/// 当前过滤词下的命中会话（收藏优先，其后按名称排序）
fn hits(app: &App) -> Vec<&SessionConfig> {
    palette_sessions(&app.session.sessions, &app.session.palette.query)
}

/// 命中行：高亮位 = 当前键盘高亮；单击对齐高亮并连接
fn palette_row<'a>(app: &'a App, pos: usize, session: &'a SessionConfig) -> Element<'a, Message> {
    let selected = app.session.palette.highlight == Some(pos);
    let conn = app.session_status(&session.id);
    button(
        container(
            column![
                text(&session.name).size(14).wrapping(Wrapping::None),
                text(format!("{}@{}", session.username, session.host))
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
    .style(move |theme, status| row_style(theme, status, selected, conn))
    .into()
}

/// 命中行样式：复用列表行底色（高亮位同选中态、悬浮同悬浮态），连接状态参与背景着色
fn row_style(
    theme: &Theme,
    status: button::Status,
    selected: bool,
    conn: ConnectionStatus,
) -> button::Style {
    let cs = crate::theme::list_row_bg(
        theme,
        selected,
        matches!(status, button::Status::Hovered),
        conn,
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

/// 次要文本样式（行内地址、无命中提示）
fn secondary_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(crate::theme::custom_palette(theme).text_secondary),
    }
}

/// 让第 `pos` 行完整可见的滚动目标 y；已在视口内返回 `None`（无需滚动）
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
    fn scroll_target_reveals_edge_rows() {
        assert_eq!(scroll_target(0.0, 6, 15), Some(28.0 + REVEAL_MARGIN));
        assert_eq!(scroll_target(100.0, 0, 15), Some(0.0));
        let y = scroll_target(0.0, 14, 15).expect("末行必须触发滚动");
        assert_eq!(y, 428.0 + REVEAL_MARGIN);
        assert_eq!(scroll_target(y, 14, 15), None, "落点后应已完整可见");
    }
}
