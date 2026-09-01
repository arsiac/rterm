//! 「更改主密码」弹窗（从设置面板进入，保险库已就绪时可用）。
//!
//! 与首次运行的「设置 / 解锁」弹窗不同，此弹窗在已解锁状态下工作：需先校验当前主密码，
//! 再用新主密码派生新密钥并重加密全部凭据（见 `crate::app::masterpw::Message::ChangeSubmit`）。
//!
//! 本视图生产 `masterpw::Message`，由 `layout` 经 `.map(Message::MasterPw)` 接入顶层路由。

use crate::app::App;
use crate::app::masterpw::{Message, MpwStage};
use crate::session_panel::{panel_style, save_btn_style};
use crate::sftp_dialogs::overlay_wrap;
use crate::t;
use crate::ui::DANGER;

use iced::widget::{Space, button, checkbox, column, container, row, rule, text, text_input};
use iced::{Element, Length};

/// 弹窗面板宽度（容纳说明与三个输入框）。
const PANEL_WIDTH: f32 = 440.0;

/// 返回「更改主密码」弹窗遮罩层；未打开时返回 `None`。
pub fn view(app: &App) -> Option<Element<'_, Message>> {
    if !app.masterpw.change_open {
        return None;
    }
    Some(overlay_wrap(panel(app)))
}

/// 弹窗主体。
fn panel(app: &App) -> Element<'_, Message> {
    // 异步流程进行中禁止关闭，避免中途打断重加密。
    let busy = app.masterpw.stage != MpwStage::Idle;
    let body = column![
        labeled_secure(
            t!("masterpw.change_current"),
            &app.masterpw.change_current,
            Message::ChangeCurrent
        ),
        labeled_secure(
            t!("masterpw.change_new"),
            &app.masterpw.change_new,
            Message::ChangeNew
        ),
        labeled_secure(
            t!("masterpw.change_confirm"),
            &app.masterpw.change_new_confirm,
            Message::ChangeConfirm
        ),
        checkbox(app.masterpw.change_memorized)
            .label(t!("masterpw.memorized"))
            .on_toggle(Message::ChangeMemorized)
            .spacing(8),
    ]
    .spacing(10);

    let mut col = column![
        row![
            text(t!("masterpw.change_title")).size(18),
            Space::new().width(Length::Fill),
            crate::ui::dialog_close_button(if busy {
                Message::Noop
            } else {
                Message::ChangeCancel
            }),
        ]
        .align_y(iced::alignment::Vertical::Center)
        .spacing(8),
        rule::horizontal(1),
        body,
    ]
    .spacing(14);

    // 实时校验：确认框已输入且与新口令不一致时立即提示（与下方提交错误不重复展示）。
    if app.masterpw.change_error.is_none()
        && !app.masterpw.change_new_confirm.is_empty()
        && app.masterpw.change_new != app.masterpw.change_new_confirm
    {
        col = col.push(text(t!("masterpw.mismatch")).size(13).color(DANGER));
    }

    if let Some(e) = &app.masterpw.change_error {
        col = col.push(text(e.clone()).size(13).color(DANGER));
    }

    // 更改按钮：空闲时可点，交由处理器做空值 / 未牢记等提交校验；异步流程进行中禁用并展示进度。
    let change_label = match app.masterpw.stage {
        MpwStage::Idle => t!("masterpw.change"),
        MpwStage::Deriving => t!("masterpw.deriving"),
        MpwStage::Reencrypting => t!("masterpw.reencrypting"),
    };

    col = col.push(
        container(
            row![
                button(text(change_label).size(16))
                    .on_press(if busy {
                        Message::Noop
                    } else {
                        Message::ChangeSubmit
                    })
                    .style(save_btn_style)
                    .padding([10, 24]),
                button(text(t!("masterpw.cancel")).size(16))
                    .on_press(if busy {
                        Message::Noop
                    } else {
                        Message::ChangeCancel
                    })
                    .padding([10, 24]),
            ]
            .spacing(10),
        )
        .width(Length::Fill),
    );

    container(col.padding(20))
        .width(PANEL_WIDTH)
        .style(panel_style)
        .into()
}

/// 带标签的密文（掩码）输入框。
fn labeled_secure<'a>(
    label: impl Into<String>,
    value: &'a str,
    on_input: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    column![
        text(label.into()).size(14),
        text_input("", value)
            .secure(true)
            .on_input(on_input)
            .style(crate::ui::text_input_style),
    ]
    .spacing(2)
    .into()
}
