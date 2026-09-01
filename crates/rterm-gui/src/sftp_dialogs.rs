//! SFTP 文件管理面板的模态对话框与统一遮罩层。
//!
//! 借鉴 s3dm 的弹窗规范：半透明黑遮罩 + 居中面板 + 圆角 + `opaque` 拦截穿透，
//! 面板内统一为“标题 / 分隔线 / 正文 / 确认·取消按钮”结构。所有对话框经
//! [`view`] 分发，由 [`App`] 的 `sftp.dialog` 状态驱动。

use crate::t;

use crate::App;
use crate::app::sftp::Message;
use crate::sftp_panel;
use crate::state::SftpDialog;
use iced::widget::{button, column, container, row, text};
use iced::{Border, Color, Element, Length, Theme};
use rterm_core::FileEntry;

/// 将子元素包装为全屏模态遮罩：半透明黑底 + 居中 + `opaque` 拦截事件穿透。
///
/// 与消息类型无关（遮罩层只做布局 / 事件拦截），故对消息类型 `M` 泛型，
/// 既可用于 SFTP 模块消息，也可用于主密码 / 主机密钥等顶层消息对话框。
pub fn overlay_wrap<'a, M: Clone + 'a>(child: Element<'a, M>) -> Element<'a, M> {
    let overlay = container(child)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_: &Theme| container::Style {
            background: Some(iced::Background::Color(Color::from_rgba(
                0.0, 0.0, 0.0, 0.6,
            ))),
            ..Default::default()
        })
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    iced::widget::opaque(overlay)
}

/// 根据当前打开的对话框返回遮罩层元素；无对话框时返回 `None`。
///
/// 该函数仅在当前活动标签的 SFTP 视图存在对话框时返回内容（`App::active_sftp`）；
/// 实际由 `crate::layout` 用 `iced::widget::stack` 叠加，覆盖**整个窗口**而非仅文件列表。
pub fn view(app: &App) -> Option<Element<'_, Message>> {
    let dialog = app.active_sftp().and_then(|s| s.dialog.as_ref())?;
    let content = match dialog {
        SftpDialog::Delete { name, is_dir } => delete_dialog(name, *is_dir),
        SftpDialog::OverwriteDownload { name, .. } => overwrite_dialog(name),
        SftpDialog::Properties { entry, path } => properties_dialog(entry, path),
    };
    Some(overlay_wrap(content))
}

/// 删除确认弹窗：按条目类型展示「删除文件 / 目录」警告与确认按钮。
fn delete_dialog(name: &str, is_dir: bool) -> Element<'_, Message> {
    let kind = if is_dir {
        t!("sftp.dir")
    } else {
        t!("sftp.file")
    };
    dialog_panel(
        t!("sftp.delete_title"),
        column![text(t!("sftp.delete_confirm", kind => kind, name => name)).size(14),],
        t!("sftp.delete"),
        true,
    )
}

/// 下载覆盖确认弹窗：提示本地已存在同名文件，确认后将覆盖。
fn overwrite_dialog(name: &str) -> Element<'_, Message> {
    dialog_panel(
        t!("sftp.overwrite_title"),
        column![text(t!("sftp.overwrite_confirm", name => name)).size(14),],
        t!("sftp.overwrite"),
        true,
    )
}

/// 只读信息框，展示名称 / 类型 / 大小 / 修改时间 / 完整路径。
fn properties_dialog<'a>(entry: &'a FileEntry, path: &'a str) -> Element<'a, Message> {
    let kind = if entry.is_dir {
        t!("sftp.dir")
    } else {
        t!("sftp.file")
    };
    let size = if entry.is_dir {
        "—".to_string()
    } else {
        sftp_panel::format_size(entry.size)
    };
    let modified = entry.modified.clone().unwrap_or_else(|| t!("sftp.unknown"));
    dialog_panel(
        t!("sftp.properties"),
        column![
            prop_row(t!("sftp.prop_name"), entry.name.clone(), 72.0),
            prop_row(t!("sftp.prop_type"), kind.to_string(), 72.0),
            prop_row(t!("sftp.prop_size"), size, 72.0),
            prop_row(t!("sftp.prop_modified"), modified, 72.0),
            prop_row(t!("sftp.prop_path"), path.to_string(), 72.0),
        ]
        .spacing(10),
        t!("common.close"),
        false,
    )
}

/// 属性框中的单行“标签 : 值”；`label_width` 为标签列固定宽度（按最长标签调整）。
///
/// 与消息类型无关，故对消息类型 `M` 泛型（SFTP 属性框用 `sftp::Message`、主机密钥框用顶层
/// `Message`，均可复用）。
pub(crate) fn prop_row<'a, M: Clone + 'a>(
    label: impl Into<String>,
    value: String,
    label_width: f32,
) -> Element<'a, M> {
    row![
        text(label.into())
            .size(14)
            .width(Length::Fixed(label_width)),
        // WordOrGlyph：指纹这类无空格长 token 按词换行放不下时退化为字符级断行，
        // 否则整个 token 溢出面板。
        text(value)
            .size(14)
            .width(Length::Fill)
            .wrapping(text::Wrapping::WordOrGlyph),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center)
    .into()
}

/// SFTP 文件操作确认 / 取消弹窗：复用 [`crate::ui::dialog_panel`]。
///
/// 确认按钮固定为 [`crate::app::sftp::Message::SftpDialogConfirm`]、取消为 [`crate::app::sftp::Message::SftpCancelDialog`]，
/// 面板宽度 360、无加粗描边；`danger` 控制确认按钮是否为危险红。
fn dialog_panel<'a>(
    title: impl Into<String>,
    body: impl Into<Element<'a, Message>>,
    confirm_label: impl Into<String>,
    danger: bool,
) -> Element<'a, Message> {
    crate::ui::dialog_panel(
        title,
        body,
        None,
        360.0,
        crate::ui::DialogButton {
            label: confirm_label.into(),
            on_press: crate::app::sftp::Message::SftpDialogConfirm,
            style: crate::ui::DialogBtnStyle::Emphasis { danger },
        },
        crate::ui::DialogButton {
            label: t!("common.cancel"),
            on_press: crate::app::sftp::Message::SftpCancelDialog,
            style: crate::ui::DialogBtnStyle::Neutral,
        },
    )
}

/// 对话框危险（确认）按钮样式：常态为纯色，悬停 / 按下降到 85% 不透明度，文字恒为白色。
pub(crate) fn dialog_btn_style(status: button::Status, danger: bool) -> button::Style {
    let base = if danger {
        crate::ui::DANGER
    } else {
        crate::theme::ACCENT
    };
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => {
            Color::from_rgba(base.r, base.g, base.b, 0.85)
        }
        _ => base,
    };
    button::Style {
        background: Some(iced::Background::Color(bg)),
        text_color: Color::WHITE,
        border: Border::default().rounded(6.0),
        ..Default::default()
    }
}

/// 对话框中性（取消）按钮样式。
pub(crate) fn dialog_btn_style_neutral(theme: &Theme, _status: button::Status) -> button::Style {
    let p = crate::theme::custom_palette(theme);
    button::Style {
        background: Some(iced::Background::Color(p.surface)),
        text_color: p.text_secondary,
        border: Border {
            color: p.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}
