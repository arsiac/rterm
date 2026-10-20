//! 端口转发规则的新建 / 编辑弹窗。
//!
//! 与其余模态框共用遮罩（`crate::modal::overlay_wrap_dismissable`，单击遮罩空白关闭），
//! 由 `crate::layout` 叠到最上层；校验原因落在页脚、保存按钮保持可用（点过保存才提示）。

use crate::t;

use crate::App;
use crate::app::forwards::{KindOption, Message};
use iced::widget::{column, container, pick_list, row, text_input};
use iced::{Element, Length};
use rterm_config::ForwardKind;

/// 弹窗宽度（像素）；表单为三行（类型 / 地址+端口 / 地址+端口），取中等宽度即可。
const DIALOG_W: f32 = 520.0;

/// 地址+端口合并行中，端口列的固定宽度（容纳 5 位数字与内边距）。
const PORT_COL_W: f32 = 120.0;

/// 规则类型下拉的全部选项。
const KIND_OPTIONS: [KindOption; 3] = [
    KindOption(ForwardKind::Local),
    KindOption(ForwardKind::Remote),
    KindOption(ForwardKind::Dynamic),
];

/// 端口转发规则编辑弹窗；无草稿时返回 `None`（不占据叠加层）。
pub fn view(app: &App) -> Option<Element<'_, Message>> {
    let draft = app.forwards.editor.as_ref()?;
    let panel = container(body(draft))
        .width(DIALOG_W)
        .style(crate::ui::dialog_panel_style(None))
        .padding(0);
    Some(crate::modal::overlay_wrap_dismissable(
        panel.into(),
        Message::CancelEdit,
    ))
}

/// 「地址 + 端口」合并行：左列地址输入吃满剩余宽度，右列端口输入窄固定宽。
///
/// 各列自带标签（上下结构），行内两列间距与表单行距一致。
#[allow(clippy::too_many_arguments)]
fn addr_port_row<'a>(
    addr_label: String,
    addr_placeholder: &str,
    addr_value: &str,
    addr_on_input: fn(String) -> Message,
    port_label: String,
    port_value: &str,
    port_on_input: fn(String) -> Message,
) -> Element<'a, Message> {
    let addr = column![
        crate::ui::field_label(addr_label),
        text_input(addr_placeholder, addr_value)
            .on_input(addr_on_input)
            .style(crate::ui::text_input_style),
    ]
    .spacing(6);
    let port = column![
        crate::ui::field_label(port_label),
        text_input("", port_value)
            .on_input(port_on_input)
            .style(crate::ui::text_input_style),
    ]
    .spacing(6);
    row![
        container(addr).width(Length::Fill),
        container(port).width(Length::Fixed(PORT_COL_W)),
    ]
    .spacing(12)
    .align_y(iced::alignment::Vertical::Bottom)
    .into()
}

/// 弹窗主体：标题栏 + 表单 + 固定页脚（校验提示与按钮）。
fn body(draft: &crate::app::forwards::RuleDraft) -> Element<'_, Message> {
    let header = crate::ui::dialog_title_bar(
        if draft.is_new() {
            t!("forward.new")
        } else {
            t!("forward.edit")
        },
        Some(Message::CancelEdit),
    );

    let mut fields: Vec<Element<'_, Message>> = Vec::new();
    fields.push(crate::ui::field_label(t!("forward.kind")));
    fields.push(
        pick_list(&KIND_OPTIONS[..], Some(KindOption(draft.kind)), |o| {
            Message::EditorKind(format!("{:?}", o.0))
        })
        .width(Length::Fill)
        .style(crate::theme::pick_list_style)
        .into(),
    );
    // 地址 + 端口同行：地址列吃满剩余宽度（主机名/地址较长），端口列窄固定宽。
    fields.push(addr_port_row(
        t!("forward.bind_address"),
        &t!("forward.bind_placeholder"),
        &draft.bind_address,
        Message::EditorBindAddress,
        t!("forward.bind_port"),
        &draft.bind_port,
        Message::EditorBindPort,
    ));
    // 动态转发（SOCKS5）无固定目的：目标行整体隐藏。
    if draft.kind != ForwardKind::Dynamic {
        fields.push(addr_port_row(
            t!("forward.target_host"),
            &t!("forward.target_placeholder"),
            &draft.target_host,
            Message::EditorTargetHost,
            t!("forward.target_port"),
            &draft.target_port,
            Message::EditorTargetPort,
        ));
    }
    let form = column(fields).spacing(10);

    let footer = crate::ui::dialog_footer(
        draft.error.clone(),
        Some(crate::ui::DialogButton {
            label: t!("common.cancel"),
            on_press: Message::CancelEdit,
            style: crate::ui::DialogBtnStyle::Neutral,
        }),
        crate::ui::DialogButton {
            label: t!("forward.save"),
            on_press: Message::SaveRule,
            style: crate::ui::DialogBtnStyle::Emphasis { danger: false },
        },
    );

    column![
        header,
        crate::ui::hairline(),
        container(form).padding(crate::ui::DIALOG_BODY_PADDING),
        crate::ui::hairline(),
        footer,
    ]
    .into()
}
