//! 端口转发管理面板。
//!
//! 只负责规则清单展示与启停 / 删除入口（新增 / 编辑走 `crate::forwards_dialog` 弹窗）；
//! 作用域跟随活动标签（无标签时可由会话行右键菜单指定），运行时行状态由引擎事件驱动。

use crate::t;

use crate::App;
use crate::app::contexts::forwards_ctx;
use crate::app::forwards::{Ctx, Message, RowStatus};
use crate::icons::{ICON_BUTTON_DEFAULT_PADDING, ICON_SIZE, Icon};
use crate::ui::{
    DialogBtnStyle, DialogButton, dialog_panel, menu_container, menu_entry, menu_separator,
};
use crate::widget::measured_column::MeasuredColumn;
use iced::widget::text::Wrapping;
use iced::widget::tooltip::Position;
use iced::widget::{Space, button, column, container, row, text};
use iced::{Element, Length, Theme};
use iced_aw::widget::context_menu::ContextMenu;
use rterm_config::{ForwardKind, ForwardRule};

/// 操作按钮图标尺寸（像素），与其它面板的列表操作一致。
const ACTION_ICON_SIZE: f32 = ICON_SIZE;

/// 删除转发规则确认弹窗层：存在待确认删除下标时返回遮罩层，否则 `None`。
/// 复用 `dialog_panel` + `overlay_wrap_dismissable`，与会话删除确认弹窗同源。
pub fn confirm_delete_overlay(app: &App) -> Option<Element<'_, Message>> {
    let idx = app.forwards.pending_delete?;
    let ctx = forwards_ctx(app);
    let desc = ctx.rules.get(idx).map(rule_description).unwrap_or_default();
    let body: Element<'_, Message> =
        column![text(t!("forward.delete_confirm", name => desc)).size(14)]
            .spacing(8)
            .into();
    let panel = dialog_panel(
        t!("forward.delete_title"),
        body,
        None,
        360.0,
        Some(DialogButton {
            label: t!("common.cancel").to_string(),
            on_press: Message::CancelDeleteRule,
            style: DialogBtnStyle::Neutral,
        }),
        DialogButton {
            label: t!("forward.delete").to_string(),
            on_press: Message::ConfirmDeleteRule,
            style: DialogBtnStyle::Emphasis { danger: true },
        },
    );
    Some(crate::modal::overlay_wrap_dismissable(
        panel,
        Message::CancelDeleteRule,
    ))
}

/// 规则的主行描述（类型 + 绑定 → 目的），用于确认弹窗的单行标识。
fn rule_description(rule: &ForwardRule) -> String {
    match rule.kind {
        ForwardKind::Dynamic => format!(
            "{}:{} — {}",
            rule.bind_addr(),
            rule.bind_port,
            t!("forward.dynamic_hint")
        ),
        ForwardKind::Local | ForwardKind::Remote => format!(
            "{}:{} → {}:{}",
            rule.bind_addr(),
            rule.bind_port,
            rule.target_addr(),
            rule.target_port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".into())
        ),
    }
}

/// 端口转发面板（中心视图之一，与「会话 / 文件 / 传输」并列切换）。
pub fn view(app: &App) -> Element<'_, Message> {
    let ctx = forwards_ctx(app);

    let body: Element<'_, Message> = match &ctx.session_id {
        None => empty_state(),
        Some(_) if ctx.rules.is_empty() => no_rules_state(),
        Some(_) => crate::ui::thin_vertical_scrollbar(
            column(
                ctx.rules
                    .iter()
                    .enumerate()
                    .map(|(i, rule)| forward_row(app, &ctx, i, rule))
                    .collect::<Vec<_>>(),
            )
            .spacing(4),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
    };

    column![toolbar(&ctx), body]
        .spacing(8)
        .padding(10)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 顶部工具栏：右对齐的「添加规则」图标按钮（与常用命令面板的工具栏同构，无标题）。
fn toolbar<'a>(ctx: &Ctx) -> Element<'a, Message> {
    let add = button(Icon::Add.svg(ACTION_ICON_SIZE))
        .on_press_maybe(ctx.writable.then_some(Message::StartEdit(None)))
        .padding(ICON_BUTTON_DEFAULT_PADDING)
        .style(move |theme, st| crate::theme::icon_button_style(theme, st, false));
    let add: Element<'static, Message> = if ctx.writable {
        crate::ui::hover_tooltip(add, t!("forward.add"), Position::Bottom)
    } else {
        add.into()
    };
    container(row![add].align_y(iced::alignment::Vertical::Center))
        .width(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right)
        .into()
}

/// 单条规则行：类型徽标、主行（绑定 → 目的）、状态次行、启停开关与右键菜单。
fn forward_row<'a>(
    app: &'a App,
    ctx: &Ctx,
    idx: usize,
    rule: &ForwardRule,
) -> Element<'a, Message> {
    let kind_badge = match rule.kind {
        ForwardKind::Local => t!("forward.kind_short_local"),
        ForwardKind::Remote => t!("forward.kind_short_remote"),
        ForwardKind::Dynamic => t!("forward.kind_short_dynamic"),
    };
    let main_line = match rule.kind {
        ForwardKind::Dynamic => format!(
            "{}:{} — {}",
            rule.bind_addr(),
            rule.bind_port,
            t!("forward.dynamic_hint")
        ),
        ForwardKind::Local | ForwardKind::Remote => format!(
            "{}:{} → {}:{}",
            rule.bind_addr(),
            rule.bind_port,
            rule.target_addr(),
            rule.target_port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".into())
        ),
    };
    let status_line = row_status_text(app, ctx, idx);

    // 滑动开关位于条目内部右缘：文本列吃满剩余宽度并截断，开关恒可见。
    let content = row![
        container(
            column![
                row![
                    text(kind_badge).size(11).style(secondary_text),
                    text(main_line).size(14).wrapping(Wrapping::None),
                ]
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center),
                text(status_line)
                    .size(12)
                    .wrapping(Wrapping::None)
                    .style(secondary_text),
            ]
            .spacing(2),
        )
        .width(Length::Fill)
        .clip(true),
        toggle_switch(rule.enabled, Message::ToggleRule(idx)),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);
    let item: Element<'a, Message> = container(content)
        .padding([6u16, 8u16])
        .width(Length::Fill)
        .style(move |theme| {
            let mut style = crate::theme::list_row_bg(
                theme,
                false,
                false,
                rterm_core::ConnectionStatus::Disconnected,
            );
            style.border.radius = 6.0.into();
            style
        })
        .into();

    // 右侧尾部净空：垂直滚动条（6px）画在内容之上，尾垫保证开关永不
    // 被滚动条盖住（对应 SFTP 列表「大小列恒可见」的保证）。
    let with_clearance: Element<'a, Message> = row![item, Space::new().width(10)]
        .align_y(iced::alignment::Vertical::Center)
        .into();
    ContextMenu::new(with_clearance, move || forward_menu(idx)).into()
}

/// 滑动开关：直接用 iced 标准的 Toggler（无标签形态），随主题配色。
fn toggle_switch(enabled: bool, on_toggle: Message) -> Element<'static, Message> {
    iced::widget::toggler(enabled)
        .on_toggle_maybe(Some(move |_| on_toggle.clone()))
        .size(16.0)
        .spacing(0)
        .into()
}

/// 规则行的右键菜单（编辑 / 删除）。
fn forward_menu(idx: usize) -> Element<'static, Message> {
    let actions = vec![
        menu_entry(t!("forward.edit"), Message::StartEdit(Some(idx))),
        menu_separator(),
        menu_entry(t!("forward.delete"), Message::RequestDeleteRule(idx)),
    ];
    menu_container(MeasuredColumn::with_children(actions))
}

/// 规则行的状态次行文案：live 时查运行时状态，否则显示「未连接（仅编辑配置）」。
fn row_status_text(app: &App, ctx: &Ctx, idx: usize) -> String {
    if !ctx.live {
        return t!("forward.status_offline");
    }
    let Some(tab_id) = ctx.tab_id else {
        return t!("forward.status_offline");
    };
    match app
        .forwards
        .runtime
        .get(&tab_id)
        .and_then(|rows| rows.get(idx))
    {
        Some(RowStatus::Listening { conns }) => t!("forward.status_active", count => conns),
        Some(RowStatus::Failed) => t!("forward.status_failed"),
        Some(RowStatus::Stopped) => t!("forward.status_stopped"),
        // 尚无行条目或仍处启动中：一律按「启动中」展示。
        _ => t!("forward.status_starting"),
    }
}

/// 无作用域会话（未选择会话）时的占位提示。
fn empty_state<'a>() -> Element<'a, Message> {
    let content = column![
        text(t!("forward.empty")).size(15).wrapping(Wrapping::Word),
        text(t!("forward.empty_hint"))
            .size(13)
            .wrapping(Wrapping::Word)
            .style(secondary_text),
    ]
    .spacing(4)
    .align_x(iced::alignment::Horizontal::Center);
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

/// 作用域会话无规则时的占位提示。
fn no_rules_state<'a>() -> Element<'a, Message> {
    let content = column![
        text(t!("forward.no_rules"))
            .size(15)
            .wrapping(Wrapping::Word),
        text(t!("forward.no_rules_hint"))
            .size(13)
            .wrapping(Wrapping::Word)
            .style(secondary_text),
    ]
    .spacing(4)
    .align_x(iced::alignment::Horizontal::Center);
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

/// 次要文本样式（行内状态、空态说明）。
fn secondary_text(theme: &Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(crate::theme::custom_palette(theme).text_secondary),
    }
}
