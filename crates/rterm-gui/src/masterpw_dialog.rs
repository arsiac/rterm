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
use crate::session_panel::panel_style;
use crate::sftp_dialogs::overlay_wrap;
use crate::ui::DANGER;
use iced::widget::{Space, button, checkbox, column, container, row, rule, text, text_input};
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

/// 主密码弹窗主体：设置或解锁模式复用同一面板骨架。
fn panel(app: &App) -> Element<'_, Message> {
    let body = if app.masterpw.setup {
        setup_body(app)
    } else {
        unlock_body(app)
    };

    // 标题行：设置模式下附带右上角关闭按钮（可中途取消，回到已就绪的随机密钥保险库）；
    // 解锁模式不提供关闭，否则应用将处于无保险库状态。异步流程进行中禁止关闭，避免中途打断。
    let busy = app.masterpw.stage != MpwStage::Idle;
    let mut title_row = row![
        text(if app.masterpw.setup {
            t!("masterpw.set_title")
        } else {
            t!("masterpw.unlock_title")
        })
        .size(18),
    ]
    .spacing(8);
    if app.masterpw.setup {
        title_row = title_row
            .push(Space::new().width(Length::Fill))
            .push(close_button(busy));
    }

    container(
        column![title_row, rule::horizontal(1), body,]
            .spacing(14)
            .padding(20),
    )
    .width(PANEL_WIDTH)
    .style(panel_style)
    .into()
}

/// 设置弹窗右上角的关闭按钮（✕）：取消「设置主密码」流程；`busy` 为异步流程进行中时禁用。
fn close_button(busy: bool) -> Element<'static, Message> {
    crate::ui::dialog_close_button(if busy { Message::Noop } else { Message::Cancel })
}

/// 设置模式主体：两次输入 + 「我已牢记」勾选 + 保存按钮。
///
/// 表单校验分两层：
/// - 实时校验：确认框已输入且与口令不一致时，立刻在按钮上方给出红色提示（无需等到提交）；
/// - 提交校验：空口令 / 未勾选「牢记」/ 存储或写入失败等由处理器写入 `masterpw.error` 后展示。
fn setup_body(app: &App) -> Element<'_, Message> {
    let mut col = column![
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
    .spacing(10);

    // 实时校验：确认框已输入且与口令不一致时立即提示（与下方提交错误不重复展示）。
    if app.masterpw.error.is_none()
        && !app.masterpw.confirm.is_empty()
        && app.masterpw.input != app.masterpw.confirm
    {
        col = col.push(text(t!("masterpw.mismatch")).size(13).color(DANGER));
    }

    // 提交校验结果（空口令 / 未牢记 / 存储或写入失败等）由处理器写入 `masterpw.error`。
    if let Some(e) = &app.masterpw.error {
        col = col.push(text(e.clone()).size(13).color(DANGER));
    }

    // 保存按钮：异步流程进行中显示进度文案并禁用，告诉用户程序在忙而非卡死。
    let busy = app.masterpw.stage != MpwStage::Idle;
    let save_label = match app.masterpw.stage {
        MpwStage::Idle => t!("masterpw.save"),
        MpwStage::Deriving => t!("masterpw.deriving"),
        MpwStage::Reencrypting => t!("masterpw.reencrypting"),
    };

    col = col.push(
        container(
            button(text(save_label).size(16))
                .on_press(if busy { Message::Noop } else { Message::Submit })
                .style(save_btn_style)
                .padding([10, 24]),
        )
        .width(Length::Fill)
        .align_x(iced::alignment::Horizontal::Center),
    );

    col.into()
}

/// 解锁模式主体：单次输入 + 解锁按钮（非空即可点）。
fn unlock_body(app: &App) -> Element<'_, Message> {
    let can = !app.masterpw.input.is_empty();
    let mut col = column![
        text(t!("masterpw.unlock_body")).size(14),
        labeled_secure(t!("masterpw.password"), &app.masterpw.input, Message::Input),
    ]
    .spacing(10);

    if let Some(e) = &app.masterpw.error {
        col = col.push(text(e.clone()).size(13).color(DANGER));
    }

    col = col.push(
        container(
            button(text(t!("masterpw.unlock")).size(16))
                .on_press(if can { Message::Submit } else { Message::Noop })
                .style(save_btn_style)
                .padding([10, 24]),
        )
        .width(Length::Fill)
        .align_x(iced::alignment::Horizontal::Center),
    );

    col.into()
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

/// 保存按钮样式：复用会话编辑器主按钮样式。
fn save_btn_style(theme: &iced::Theme, status: button::Status) -> button::Style {
    crate::session_panel::save_btn_style(theme, status)
}
