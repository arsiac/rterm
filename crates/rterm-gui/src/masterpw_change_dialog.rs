//! 「更改主密码」弹窗（从设置面板进入，保险库已就绪时可用）。
//!
//! 与首次运行的「设置 / 解锁」弹窗不同，此弹窗在已解锁状态下工作：需先校验当前主密码，
//! 再用新主密码派生新密钥并重加密全部凭据（见 `crate::app::masterpw::Message::ChangeSubmit`）。
//!
//! 本视图生产 `masterpw::Message`，由 `layout` 经 `.map(Message::MasterPw)` 接入顶层路由。

use crate::app::App;
use crate::app::masterpw::{Message, MpwStage};
use crate::sftp_dialogs::overlay_wrap;
use crate::t;
use crate::ui::{
    DialogBtnStyle, DialogButton, dialog_footer, dialog_panel_style, dialog_title_bar, field_label,
    hairline, text_input_style,
};

use iced::widget::{checkbox, column, container, text_input};
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

/// 弹窗主体：骨架与会话编辑器一致（强调条标题栏 + 细分割线 + 左错误 / 右按钮行）。
fn panel(app: &App) -> Element<'_, Message> {
    // 异步流程进行中禁止关闭 / 提交，避免中途打断重加密。
    let busy = app.masterpw.stage != MpwStage::Idle;
    let cancel = || {
        if busy {
            Message::Noop
        } else {
            Message::ChangeCancel
        }
    };

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
    .spacing(12);

    // 实时校验：确认框已输入且与新口令不一致时立即提示（与处理器写入的提交错误不重复）。
    let error = if app.masterpw.change_error.is_none()
        && !app.masterpw.change_new_confirm.is_empty()
        && app.masterpw.change_new != app.masterpw.change_new_confirm
    {
        Some(t!("masterpw.mismatch"))
    } else {
        app.masterpw.change_error.clone()
    };

    // 更改按钮：空闲时可点，交由处理器做空值 / 未牢记等提交校验；异步流程进行中禁用并展示进度。
    let change_label = match app.masterpw.stage {
        MpwStage::Idle => t!("masterpw.change"),
        MpwStage::Deriving => t!("masterpw.deriving"),
        MpwStage::Reencrypting => t!("masterpw.reencrypting"),
    };

    container(column![
        dialog_title_bar(t!("masterpw.change_title"), Some(cancel())),
        hairline(),
        container(body).width(Length::Fill).padding([16.0, 20.0]),
        hairline(),
        dialog_footer(
            error,
            Some(DialogButton {
                label: t!("masterpw.cancel"),
                on_press: cancel(),
                style: DialogBtnStyle::Neutral,
            }),
            DialogButton {
                label: change_label,
                on_press: if busy {
                    Message::Noop
                } else {
                    Message::ChangeSubmit
                },
                style: DialogBtnStyle::Emphasis { danger: false },
            },
        ),
    ])
    .width(PANEL_WIDTH)
    .style(dialog_panel_style(None))
    .into()
}

/// 带标签的密文（掩码）输入框；标签复用共享的 [`field_label`]。
fn labeled_secure<'a>(
    label: impl Into<String>,
    value: &'a str,
    on_input: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    column![
        field_label(label),
        text_input("", value)
            .secure(true)
            .on_input(on_input)
            .style(text_input_style),
    ]
    .spacing(4)
    .into()
}
