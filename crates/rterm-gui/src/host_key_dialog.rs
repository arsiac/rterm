//! 主机密钥确认弹窗（窗口级安全模态）。
//!
//! 决策发生在 SSH 握手中途（连接尚未建立），因此该弹窗不属于任何标签的 SFTP
//! 视图，而是叠加在整个窗口最顶层——安全决策必须压过设置 / 编辑器等其他弹窗。
//! 未知主机走普通确认框；指纹变更走红色警告框，且“拒绝”是默认强调（推荐）按钮，
//! 与删除等操作的危险方向相反（此处推荐拒绝，风险动作“仍然信任”走中性样式）。

use crate::t;

use crate::app::hostkey::Message;
use crate::sftp_dialogs::{overlay_wrap, prop_row};
use crate::ui::DANGER;
use iced::widget::{column, text};
use iced::{Color, Element};
use rterm_core::HostKeyPrompt;

/// 根据队列首项返回主机密钥弹窗遮罩层；无待确认项时返回 `None`。
pub fn view(app: &crate::App) -> Option<Element<'_, Message>> {
    let state = app.hostkey.head()?;
    Some(overlay_wrap(match &state.prompt.mismatch {
        Some(stored) => mismatch_dialog(&state.prompt, stored),
        None => unknown_dialog(&state.prompt),
    }))
}

/// 弹窗面板宽度：需容纳 50 字符左右的 SHA256 指纹单行放下（超长仍可字符级断行兜底）。
const PANEL_WIDTH: f32 = 580.0;
/// 属性行标签列宽：容纳“已记录指纹”等五字标签不折行。
const LABEL_WIDTH: f32 = 96.0;

/// 未知主机：普通确认框，展示主机 / 端口 / 密钥类型 / 指纹。
fn unknown_dialog(p: &HostKeyPrompt) -> Element<'_, Message> {
    panel(
        t!("hostkey.unknown_title"),
        None,
        column![
            text(t!("hostkey.unknown_body")).size(14),
            prop_row(t!("hostkey.host"), p.host.clone(), LABEL_WIDTH),
            prop_row(t!("hostkey.port"), p.port.to_string(), LABEL_WIDTH),
            prop_row(t!("hostkey.key_type"), p.key_type.clone(), LABEL_WIDTH),
            prop_row(
                t!("hostkey.fingerprint"),
                p.fingerprint.clone(),
                LABEL_WIDTH
            ),
        ]
        .spacing(10),
        // 信任是常规路径，用强调色；取消走中性样式。
        (t!("hostkey.trust"), Message::Decision(true), false),
        (t!("common.cancel"), Message::Decision(false)),
    )
}

/// 指纹变更：红色警告框，展示新旧指纹对照；“仍然信任”才是危险按钮。
fn mismatch_dialog<'a>(p: &'a HostKeyPrompt, stored: &'a str) -> Element<'a, Message> {
    panel(
        t!("hostkey.changed_title"),
        Some(DANGER),
        column![
            text(t!("hostkey.changed_body")).size(14).color(DANGER),
            prop_row(
                t!("hostkey.host"),
                format!("{}:{}", p.host, p.port),
                LABEL_WIDTH
            ),
            prop_row(t!("hostkey.stored_fp"), stored.to_string(), LABEL_WIDTH),
            prop_row(t!("hostkey.current_fp"), p.fingerprint.clone(), LABEL_WIDTH),
        ]
        .spacing(10),
        // 与普通确认相反：拒绝是推荐动作（强调色），仍然信任走中性样式。
        (t!("hostkey.reject"), Message::Decision(false), false),
        (t!("hostkey.still_trust"), Message::Decision(true)),
    )
}

/// 主机密钥弹窗面板：复用 [`crate::ui::dialog_panel`]。
///
/// 与 SFTP 弹窗的差异仅为按钮消息与面板宽度（需容纳长指纹），故仅外置这些参数；
/// 指纹变更时以 [`DANGER`] 加粗描边。`primary` 为右侧主决定按钮（信任 / 仍信任），
/// `secondary` 为左侧中性按钮（拒绝 / 取消）。
fn panel<'a>(
    title: impl Into<String>,
    border: Option<Color>,
    body: impl Into<Element<'a, Message>>,
    primary: (impl Into<String>, Message, bool),
    secondary: (impl Into<String>, Message),
) -> Element<'a, Message> {
    let (primary_label, primary_msg, primary_danger) = primary;
    let (secondary_label, secondary_msg) = secondary;
    crate::ui::dialog_panel(
        title,
        body,
        border,
        PANEL_WIDTH,
        Some(crate::ui::DialogButton {
            label: secondary_label.into(),
            on_press: secondary_msg,
            style: crate::ui::DialogBtnStyle::Neutral,
        }),
        crate::ui::DialogButton {
            label: primary_label.into(),
            on_press: primary_msg,
            style: crate::ui::DialogBtnStyle::Emphasis {
                danger: primary_danger,
            },
        },
    )
}
