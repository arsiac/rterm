//! 会话凭据导入 / 导出的弹窗（窗口级模态），由 `session_io` 模块驱动，经 `.map(Message::SessionIo)`
//! 接入顶层路由。两个弹窗都把「解密靠导出口令、本机主密码不参与」写在正文里——这句不能省，
//! 漏了会让人反复试主密码，最后以为程序坏了。

use crate::app::App;
use crate::app::session_io::{self, Message, Stage};
use crate::modal::overlay_wrap;
use crate::t;
use crate::ui::{
    DialogBtnStyle, DialogButton, dialog_footer, dialog_panel_style, dialog_title_bar, field_label,
    hairline, text_input_style,
};
use iced::widget::text::Wrapping;
use iced::widget::{Column, column, container, text, text_input, toggler};
use iced::{Element, Length};

/// 弹窗面板宽度（容纳两条前提说明与两个输入框）。
const PANEL_WIDTH: f32 = 460.0;

/// 带凭据导出弹窗遮罩层；未处于导出态时返回 `None`。
pub fn export_overlay(app: &App) -> Option<Element<'_, Message>> {
    if !app.session_io.export_open {
        return None;
    }
    Some(overlay_wrap(export_panel(app)))
}

/// 带凭据导入弹窗遮罩层；未处于导入态时返回 `None`。
pub fn import_overlay(app: &App) -> Option<Element<'_, Message>> {
    if !app.session_io.import_open {
        return None;
    }
    Some(overlay_wrap(import_panel(app)))
}

/// 导出弹窗主体：凭据开关 + 开关开着时的两次口令输入与解密前提说明。
///
/// 开关关着时不含凭据、无需保险库也无需口令，故「导出」直接去选保存路径。
fn export_panel(app: &App) -> Element<'_, Message> {
    let io = &app.session_io;
    let deriving = io.stage != Stage::Idle;
    // 选路径与派生期间弹窗不可打断，但主按钮文案只在真忙时改口。
    let blocked = deriving || io.export_picking;
    let with_credentials = io.export_with_credentials;
    // 实时校验：确认框已输入且与口令不一致时立即提示，不等提交（与主密码弹窗同规）。
    let error =
        if with_credentials && !io.export_confirm.is_empty() && io.export_pass != io.export_confirm
        {
            Some(t!("cred.mismatch"))
        } else {
            io.export_error.clone()
        };
    let body = if with_credentials {
        column![
            text(t!("cred.export_body"))
                .size(14)
                .wrapping(Wrapping::Word),
            credentials_toggle(io),
            notices(false),
            labeled_secure(
                t!("cred.password"),
                &io.export_pass,
                Message::ExportPassInput,
                None,
            ),
            labeled_secure(
                t!("cred.confirm"),
                &io.export_confirm,
                Message::ExportConfirmInput,
                Some(press_if(blocked, Message::ExportSubmit)),
            ),
        ]
    } else {
        column![
            text(t!("cred.export_body_plain"))
                .size(14)
                .wrapping(Wrapping::Word),
            credentials_toggle(io),
        ]
    };

    panel(
        t!("cred.export_title"),
        body.spacing(12),
        error,
        blocked,
        Message::ExportCancel,
        if deriving {
            t!("cred.working")
        } else {
            t!("cred.export_submit")
        },
        press_if(blocked, Message::ExportSubmit),
    )
}

/// 凭据开关；保险库未解锁时不挂 `on_toggle`（点击无响应）并说明本次只能导出连接配置。
fn credentials_toggle(io: &session_io::State) -> Element<'_, Message> {
    let toggle = toggler(io.export_with_credentials).label(t!("cred.include_credentials"));
    let toggle = if io.credentials_available {
        toggle.on_toggle(Message::ToggleCredentials)
    } else {
        toggle
    };
    if io.credentials_available {
        toggle.spacing(8).into()
    } else {
        column![
            toggle.spacing(8),
            text(t!("cred.toggle_locked"))
                .size(13)
                .wrapping(Wrapping::Word),
        ]
        .spacing(4)
        .into()
    }
}

/// 导入弹窗主体：单次口令输入 + 解密前提说明。
fn import_panel(app: &App) -> Element<'_, Message> {
    let io = &app.session_io;
    let busy = io.stage != Stage::Idle;
    let body = column![
        text(t!("cred.import_body"))
            .size(14)
            .wrapping(Wrapping::Word),
        notices(true),
        labeled_secure(
            t!("cred.password"),
            &io.import_pass,
            Message::ImportPassInput,
            Some(press_if(
                busy || io.import_pass.is_empty(),
                Message::ImportSubmit
            )),
        ),
    ]
    .spacing(12);

    panel(
        t!("cred.import_title"),
        body,
        io.import_error.clone(),
        busy,
        Message::ImportCancel,
        if busy {
            t!("cred.working")
        } else {
            t!("cred.import_submit")
        },
        press_if(busy || io.import_pass.is_empty(), Message::ImportSubmit),
    )
}

/// 两个弹窗共用的骨架：标题栏（带 ✕）+ 主体 + 页脚（错误 + 取消 / 主操作）。
/// 忙碌期间标题栏 ✕ 与「取消」都转为 [`Message::Noop`]：派生与落盘不可中途打断，否则磁盘上会留下半份文件。
fn panel<'a>(
    title: impl Into<String>,
    body: Column<'a, Message>,
    error: Option<String>,
    busy: bool,
    cancel: Message,
    submit_label: impl Into<String>,
    submit: Message,
) -> Element<'a, Message> {
    container(column![
        dialog_title_bar(title, Some(press_if(busy, cancel.clone()))),
        hairline(),
        container(body).width(Length::Fill).padding([16.0, 20.0]),
        hairline(),
        dialog_footer(
            error,
            Some(DialogButton {
                label: t!("common.cancel"),
                on_press: press_if(busy, cancel),
                style: DialogBtnStyle::Neutral,
            }),
            DialogButton {
                label: submit_label.into(),
                on_press: submit,
                style: DialogBtnStyle::Emphasis { danger: false },
            },
        ),
    ])
    .width(PANEL_WIDTH)
    .style(dialog_panel_style(None))
    .into()
}

/// 两条「解密前提」正文提示（见模块文档）。
///
/// 刻意用正文字号而非 `hint_text` 的弱色小字：这是用户唯一会漏读的信息，漏读的代价是换机失败。
fn notices<'a>(for_import: bool) -> Column<'a, Message> {
    let same = if for_import {
        t!("cred.notice_import_password")
    } else {
        t!("cred.notice_same_password")
    };
    column![
        text(same).size(13).wrapping(Wrapping::Word),
        text(t!("cred.notice_master_password"))
            .size(13)
            .wrapping(Wrapping::Word),
    ]
    .spacing(4)
}

/// 禁用态取 `Noop` 占位（按钮仍可点但无副作用），避免额外的 disabled 状态传递。
fn press_if(disabled: bool, msg: Message) -> Message {
    if disabled { Message::Noop } else { msg }
}

/// 带标签的密文（掩码）输入框；`on_submit` 为回车派发（只挂在末位输入框上）。
fn labeled_secure<'a>(
    label: impl Into<String>,
    value: &'a str,
    on_input: impl Fn(String) -> Message + 'a,
    on_submit: Option<Message>,
) -> Element<'a, Message> {
    let input = text_input("", value)
        .secure(true)
        .on_input(on_input)
        .style(text_input_style);
    column![
        field_label(label),
        match on_submit {
            Some(msg) => input.on_submit(msg),
            None => input,
        },
    ]
    .spacing(4)
    .into()
}
