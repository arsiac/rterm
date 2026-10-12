//! 大量粘贴确认弹窗（窗口级模态）。
//!
//! 三条粘贴入口收敛到 `app::paste` 模块后，达到量级阈值的粘贴在此弹窗询问：展示量级摘要
//! 与内容预览，用户确认后才写入。含控制字符时面板转红、且「取消」为强调（推荐）按钮——
//! 与主机密钥指纹变更的强调反转同理。面板随窗口等比例缩放（见 [`view`]）。

use crate::app::paste::{self, PendingPaste};
use crate::sftp_dialogs::overlay_wrap;
use crate::t;
use crate::ui::{DANGER, WARNING};
use iced::widget::text::Wrapping;
use iced::widget::{checkbox, column, container, text, text_editor};
use iced::{Background, Border, Color, Element, Font, Length};

/// 面板基准宽度（像素）：默认窗口宽度下的目标宽度。
const PANEL_WIDTH_BASE: f32 = 580.0;
/// 预览框基准高度（像素）：面板为基准宽度时的预览高度。
const PREVIEW_HEIGHT_BASE: f32 = 160.0;
/// 面板宽度占窗口宽度的比例：等比缩放的基准。
const PANEL_WIDTH_RATIO: f32 = 0.5;
/// 面板最小宽度（像素）：窗口过窄时不再继续收缩，保证可读。
const MIN_PANEL_WIDTH: f32 = 360.0;
/// 预览框最小高度（像素）。
const MIN_PREVIEW_HEIGHT: f32 = 72.0;
/// 非预览部分的大致高度（像素）：标题 / 摘要 / 警告 / 勾选 / 按钮等，用于预览高度上限。
const CHROME_HEIGHT: f32 = 220.0;
/// 面板与窗口边缘的留白（像素）。
const DIALOG_MARGIN: f32 = 16.0;

/// 返回粘贴确认弹窗遮罩层；无待确认项时返回 `None`。
///
/// 面板尺寸随窗口等比例缩放：宽度取窗口宽度的一半（夹在最小宽度与可用宽度之间），预览框
/// 高度按同一比例同步缩放，从而保持面板长宽比例不变；预览高度再受窗口高度约束以免溢出。
pub fn view(app: &crate::App) -> Option<Element<'_, paste::Message>> {
    let pending = app.paste.pending()?;
    let suppress = app.paste.suppress_checked();
    let preview = app.paste.preview();
    let panel = iced::widget::responsive(move |size| {
        container(panel(pending, suppress, preview, size))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    });
    Some(overlay_wrap(panel.into()))
}

/// 组装确认弹窗面板；`size` 为窗口尺寸，用于等比缩放面板宽度与预览高度。
fn panel<'a>(
    pending: &'a PendingPaste,
    suppress: bool,
    preview: &'a text_editor::Content,
    size: iced::Size,
) -> Element<'a, paste::Message> {
    let width = (size.width * PANEL_WIDTH_RATIO).clamp(
        MIN_PANEL_WIDTH,
        (size.width - DIALOG_MARGIN * 2.0).max(MIN_PANEL_WIDTH),
    );
    let scale = width / PANEL_WIDTH_BASE;
    let preview_height = (PREVIEW_HEIGHT_BASE * scale).clamp(
        MIN_PREVIEW_HEIGHT,
        (size.height - CHROME_HEIGHT).max(MIN_PREVIEW_HEIGHT),
    );

    let stats = pending.stats;
    let has_control = stats.control_chars > 0;

    let mut body = column![
        text(t!(
            "terminal.paste_warn_summary",
            lines => stats.lines,
            chars => stats.chars,
            bytes => stats.bytes
        ))
        .size(14)
    ]
    .spacing(8);

    if stats.lines > 1 {
        body = body.push(warn_row(
            t!("terminal.paste_warn_multiline", lines => stats.lines),
            WARNING,
        ));
    }
    if has_control {
        body = body.push(warn_row(
            t!("terminal.paste_warn_control", count => stats.control_chars),
            DANGER,
        ));
    }

    body = body.push(preview_box(preview, preview_height));
    body = body.push(
        checkbox(suppress)
            .label(t!("terminal.paste_suppress"))
            .on_toggle(paste::Message::SuppressToggled)
            .spacing(8),
    );

    // 含控制字符时强调反转：面板转红、「取消」为推荐动作，粘贴降为中性样式。
    let border = has_control.then_some(DANGER);
    let (primary_label, primary_msg, primary_style) = if has_control {
        (
            t!("common.cancel"),
            paste::Message::Cancel,
            crate::ui::DialogBtnStyle::Emphasis { danger: false },
        )
    } else {
        (
            t!("terminal.paste_confirm"),
            paste::Message::Confirm,
            crate::ui::DialogBtnStyle::Emphasis { danger: false },
        )
    };
    let (secondary_label, secondary_msg) = if has_control {
        (t!("terminal.paste_confirm"), paste::Message::Confirm)
    } else {
        (t!("common.cancel"), paste::Message::Cancel)
    };

    crate::ui::dialog_panel(
        t!("terminal.paste_warn_title"),
        body,
        border,
        width,
        Some(crate::ui::DialogButton {
            label: secondary_label,
            on_press: secondary_msg,
            style: crate::ui::DialogBtnStyle::Neutral,
        }),
        crate::ui::DialogButton {
            label: primary_label,
            on_press: primary_msg,
            style: primary_style,
        },
    )
}

/// 单条警告行：文字取警告色并按词换行。
fn warn_row<'a>(label: String, color: Color) -> Element<'a, paste::Message> {
    text(label)
        .size(13)
        .color(color)
        .wrapping(Wrapping::Word)
        .into()
}

/// 预览框：等宽只读 `text_editor`，内建纵向滚动；底为表面色 + 细边框。
///
/// 只读靠 `paste::State::update` 屏蔽编辑动作实现，此处照常挂 `on_action` 以放行滚动与选区。
fn preview_box<'a>(content: &'a text_editor::Content, height: f32) -> Element<'a, paste::Message> {
    text_editor(content)
        .font(Font::MONOSPACE)
        .size(12)
        .padding(8)
        .height(Length::Fixed(height))
        .wrapping(Wrapping::WordOrGlyph)
        .on_action(paste::Message::PreviewAction)
        .style(|theme: &iced::Theme, status| {
            let mut style = text_editor::default(theme, status);
            let palette = crate::theme::custom_palette(theme);
            style.background = Background::Color(palette.surface);
            style.border = Border {
                color: palette.border,
                width: 1.0,
                radius: 6.0.into(),
            };
            style
        })
        .into()
}
