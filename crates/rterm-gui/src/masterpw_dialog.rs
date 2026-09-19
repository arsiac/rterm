//! 主密码设置 / 解锁弹窗（窗口级模态）。
//!
//! 与主机密钥弹窗同级，叠加于窗口最顶层。注意：**该弹窗通常只在模式 1 且钥匙串里
//! 取不到 DEK 时出现**（启动解锁失败，或用户在设置面板主动「设置主密码」）。
//! 默认的模式 0 用钥匙串随机 DEK 自动静默解锁，不会触发此弹窗——只有设置流程或
//! 钥匙串失效（如被手动清除）才会要求用户输入主密码。
//!
//! 本视图生产 `masterpw::Message`，由 `layout` 经 `.map(Message::MasterPw)` 接入顶层路由。

use crate::t;

use crate::app::App;
use crate::app::masterpw::{Message, MpwStage};
use crate::sftp_dialogs::overlay_wrap;
use crate::ui::{
    DialogBtnStyle, DialogButton, dialog_footer, dialog_panel, dialog_panel_style,
    dialog_title_bar, field_label, hairline, hint_text, text_input_style,
};
use iced::widget::{checkbox, column, container, text, text_input};
use iced::{Element, Length};

/// 弹窗面板宽度（容纳说明与两个输入框）。
const PANEL_WIDTH: f32 = 440.0;

/// 返回主密码弹窗遮罩层。
///
/// 两种情形下显示：
/// 1. 用户主动发起设置 / 更改流程（`masterpw.setup` 为 `true`）；
/// 2. 保险库尚未就绪（`vault` 为 `None`）——即模式 1 且关闭「本机记住」时，
///    启动后需输入主密码解锁。
///
/// 模式 0（随机密钥、已存钥匙串）与模式 1 且开启「本机记住」时，`vault` 已就绪、
/// `masterpw.setup` 为 `false`，故不弹窗、自动解锁。
pub fn view(app: &App) -> Option<Element<'_, Message>> {
    if !app.masterpw.setup && app.vault.is_some() {
        return None;
    }
    Some(overlay_wrap(panel(app)))
}

/// 「关闭主密码」二次确认弹窗遮罩层；未处于确认态时返回 `None`。
///
/// 与 [`view`] 相互独立：确认弹窗由**设置面板**发起（彼时保险库已就绪，[`view`] 返回 `None`），
/// 二者不会同时出现。骨架与主机密钥弹窗一致——这是不可逆的降权操作，故**不加标题栏 ✕**，
/// 只能通过显式的「取消 / 关闭主密码」二选一离开。
pub fn confirm_disable_overlay(app: &App) -> Option<Element<'_, Message>> {
    if !app.masterpw.confirm_disable {
        return None;
    }
    let mut body = column![text(t!("masterpw.disable_confirm_body")).size(14)].spacing(10);
    body = body.push(hint_text(t!("masterpw.disable_confirm_note")));
    // 失败原因就地显示：`disable` 出错时确认框会保留（见 `masterpw::State::update`）。
    if let Some(e) = &app.masterpw.error {
        body = body.push(text(e.clone()).size(13).color(crate::ui::DANGER));
    }

    Some(overlay_wrap(dialog_panel(
        t!("masterpw.disable_confirm_title"),
        body,
        Some(crate::ui::DANGER),
        PANEL_WIDTH,
        Some(DialogButton {
            label: t!("common.cancel"),
            on_press: Message::DisableCancel,
            style: DialogBtnStyle::Neutral,
        }),
        DialogButton {
            label: t!("masterpw.disable"),
            on_press: Message::Disable,
            style: DialogBtnStyle::Emphasis { danger: true },
        },
    )))
}

/// 主密码弹窗主体：设置 / 解锁模式复用同一面板骨架（骨架与会话编辑器一致）。
fn panel(app: &App) -> Element<'_, Message> {
    // 异步流程进行中禁用关闭与提交，避免中途打断密钥派生 / 重加密。
    let busy = app.masterpw.stage != MpwStage::Idle;
    // 标题栏关闭按钮仅在设置模式下提供：解锁模式不提供，否则应用会停在「无保险库」状态。
    let close = app
        .masterpw
        .setup
        .then(|| if busy { Message::Noop } else { Message::Cancel });
    let title = if app.masterpw.setup {
        t!("masterpw.set_title")
    } else {
        t!("masterpw.unlock_title")
    };

    // 实时校验：设置模式下确认框已输入且与口令不一致时立即提示（与处理器写入的提交错误不重复）。
    let error = if app.masterpw.setup
        && app.masterpw.error.is_none()
        && !app.masterpw.confirm.is_empty()
        && app.masterpw.input != app.masterpw.confirm
    {
        Some(t!("masterpw.mismatch"))
    } else {
        app.masterpw.error.clone()
    };

    let (body, primary) = if app.masterpw.setup {
        // 保存按钮：异步流程进行中显示进度文案，告诉用户程序在忙而非卡死。
        let label = match app.masterpw.stage {
            MpwStage::Idle => t!("masterpw.save"),
            MpwStage::Deriving => t!("masterpw.deriving"),
            MpwStage::Reencrypting => t!("masterpw.reencrypting"),
        };
        (
            setup_body(app),
            DialogButton {
                label,
                on_press: press_if(busy, Message::Submit),
                style: DialogBtnStyle::Emphasis { danger: false },
            },
        )
    } else {
        let empty = app.masterpw.input.is_empty();
        (
            unlock_body(app),
            DialogButton {
                label: t!("masterpw.unlock"),
                on_press: press_if(empty, Message::Submit),
                style: DialogBtnStyle::Emphasis { danger: false },
            },
        )
    };

    // 设置模式补一个底部「取消」，与标题栏关闭按钮同语义 —— 与会话编辑器一致：
    // 顶部 ✕ 与底部取消互为发现路径，两条路径必须指向同一个消息。
    let secondary = app.masterpw.setup.then(|| DialogButton {
        label: t!("masterpw.cancel"),
        on_press: press_if(busy, Message::Cancel),
        style: DialogBtnStyle::Neutral,
    });

    container(column![
        dialog_title_bar(title, close),
        hairline(),
        container(body).width(Length::Fill).padding([16.0, 20.0]),
        hairline(),
        dialog_footer(error, secondary, primary),
    ])
    .width(PANEL_WIDTH)
    .style(dialog_panel_style(None))
    .into()
}

/// 禁用态取 `Noop` 占位（按钮仍可点但无副作用），避免额外的 disabled 状态传递。
fn press_if(disabled: bool, msg: Message) -> Message {
    if disabled { Message::Noop } else { msg }
}

/// 设置模式主体：两次输入 + 「我已牢记」勾选（错误与主操作按钮由 [`panel`] 统一置于底部）。
fn setup_body(app: &App) -> Element<'_, Message> {
    column![
        text(t!("masterpw.set_body")).size(14),
        labeled_secure(t!("masterpw.password"), &app.masterpw.input, Message::Input),
        labeled_secure(
            t!("masterpw.confirm"),
            &app.masterpw.confirm,
            Message::Confirm
        ),
        checkbox(app.masterpw.memorized)
            .label(t!("masterpw.memorized"))
            .on_toggle(Message::Memorized)
            .spacing(8),
    ]
    .spacing(12)
    .into()
}

/// 解锁模式主体：单次输入（非空即可提交，空值由 [`panel`] 转为 `Noop`）。
fn unlock_body(app: &App) -> Element<'_, Message> {
    column![
        text(t!("masterpw.unlock_body")).size(14),
        labeled_secure(t!("masterpw.password"), &app.masterpw.input, Message::Input),
    ]
    .spacing(12)
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
