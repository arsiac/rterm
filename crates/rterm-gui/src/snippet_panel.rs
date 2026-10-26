//! 常用命令列表面板。
//!
//! 只负责列表与过滤（新建 / 编辑走 [`crate::snippet_dialog`] 弹窗）；右键菜单四组：新建 · 发送两项 · 编辑与排序 · 删除，排序 / 删除只在菜单里。

use crate::t;

use crate::App;
use crate::app::snippets::Message;
use crate::icons::{ICON_BUTTON_DEFAULT_PADDING, ICON_SIZE, Icon, icon_button};
use crate::ui::{
    DialogBtnStyle, DialogButton, dialog_panel, menu_entry, menu_entry_disabled, menu_separator,
};
use crate::widget::measured_column::MeasuredColumn;
use iced::widget::text::Wrapping;
use iced::widget::tooltip::Position;
use iced::widget::{button, column, container, mouse_area, row, text, text_input};
use iced::{Background, Border, Element, Length, Theme};
use iced_aw::widget::context_menu::ContextMenu;
use rterm_core::ConnectionStatus;
/// 操作按钮图标尺寸（像素），与其它面板的列表操作一致。
const ACTION_ICON_SIZE: f32 = ICON_SIZE;

/// 删除片段确认弹窗层：仅当存在待确认删除下标时返回遮罩层，否则 `None`。
/// 复用 `dialog_panel` + `overlay_wrap_dismissable`，与会话删除确认弹窗同源。
pub fn confirm_delete_overlay(app: &App) -> Option<Element<'_, Message>> {
    let idx = app.snippets.pending_delete?;
    let name = app
        .snippets
        .items
        .get(idx)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    let body: Element<'_, Message> =
        column![text(t!("snippet.delete_confirm", name => name)).size(14),]
            .spacing(8)
            .into();
    let panel = dialog_panel(
        t!("snippet.delete_title"),
        body,
        None,
        360.0,
        Some(DialogButton {
            label: t!("common.cancel").to_string(),
            on_press: Message::CancelDeleteSnippet,
            style: DialogBtnStyle::Neutral,
        }),
        DialogButton {
            label: t!("snippet.delete").to_string(),
            on_press: Message::ConfirmDeleteSnippet,
            style: DialogBtnStyle::Emphasis { danger: true },
        },
    );
    Some(crate::modal::overlay_wrap_dismissable(
        panel,
        Message::CancelDeleteSnippet,
    ))
}

/// 命令片段面板（中心视图之一，与"会话 / 文件 / 传输"并列切换）。
pub fn view(app: &App) -> Element<'_, Message> {
    let state = &app.snippets;
    let can_edit = state.can_edit();

    // 右键菜单的可发送条件：有活动标签才能发当前，有已连接标签才能广播。
    let send_current_ok = app.tabs.active().is_some();
    let send_all_ok = app
        .tabs
        .list()
        .iter()
        .any(|t| t.status == ConnectionStatus::Connected);

    let visible = crate::app::snippets::filtered_snippets(&state.items, &state.filter);
    let rows: Vec<Element<'_, Message>> = visible
        .iter()
        .map(|&idx| snippet_row(app, idx, send_current_ok, send_all_ok, can_edit))
        .collect();

    let body: Element<'_, Message> = if state.items.is_empty() {
        empty_state()
    } else if rows.is_empty() {
        no_match_state()
    } else {
        crate::ui::thin_vertical_scrollbar(column(rows).spacing(4))
            .height(Length::Fill)
            .into()
    };

    let mut content = column![toolbar(app, can_edit)].spacing(8).padding(10);
    if let Some(problem) = &state.load_problem {
        content = content.push(problem_banner(problem));
    }
    // 无命令时不显示过滤框（无处可筛），与会话面板同规。
    if !state.items.is_empty() {
        content = content.push(
            text_input(&t!("snippet.filter_placeholder"), &state.filter)
                .on_input(Message::FilterChanged)
                .size(13)
                .padding([5u16, 8u16])
                .style(crate::ui::search_input_style),
        );
    }
    content.push(body).into()
}

/// 顶部工具栏：右对齐的「快速命令唤起」与「新建」图标按钮（新建在文件不可用时禁用）。
fn toolbar(app: &App, can_edit: bool) -> Element<'static, Message> {
    // tooltip 顺带展示当前绑定（键位可重绑，不写死默认值）。
    let palette_tip = match app
        .shortcuts
        .chord_for(crate::app::shortcuts::Command::OpenSnippets)
    {
        Some(chord) => t!("snippet.palette_tooltip", chord => chord.to_string()),
        None => t!("snippet.open_palette"),
    };
    let palette = icon_button(
        Icon::Flash,
        ACTION_ICON_SIZE,
        palette_tip,
        Message::TogglePalette,
        Position::Bottom,
    );
    let add = button(Icon::Add.svg(ACTION_ICON_SIZE))
        .on_press_maybe(can_edit.then_some(Message::NewSnippet))
        .padding(ICON_BUTTON_DEFAULT_PADDING)
        .style(move |theme, st| crate::theme::icon_button_style(theme, st, false));
    let add: Element<'static, Message> = if can_edit {
        crate::ui::hover_tooltip(add, t!("snippet.new"), Position::Bottom)
    } else {
        add.into()
    };
    container(
        row![palette, add]
            .spacing(6)
            .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .align_x(iced::alignment::Horizontal::Right)
    .into()
}

/// 片段文件不可用（读不出 / 解析不了）的常驻提示：明说原因并要求用户先修文件。
fn problem_banner(problem: &str) -> Element<'_, Message> {
    container(text(problem.to_string()).size(12).color(crate::ui::DANGER))
        .width(Length::Fill)
        .padding([6u16, 8u16])
        .style(|theme: &Theme| {
            let p = crate::theme::custom_palette(theme);
            container::Style {
                background: Some(Background::Color(p.surface)),
                border: Border {
                    color: crate::ui::DANGER,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

/// 单条片段行：名称（主行）与文本预览（次行），悬浮高亮、单击选中、双击发送当前会话、右键菜单。
///
/// `send_current_ok` / `send_all_ok` / `can_edit` 决定菜单项的可用性（不可用项置灰而非隐藏）。
fn snippet_row<'a>(
    app: &'a App,
    idx: usize,
    send_current_ok: bool,
    send_all_ok: bool,
    can_edit: bool,
) -> Element<'a, Message> {
    let snippet = &app.snippets.items[idx];
    let hovered = app.snippets.hovered == Some(idx);
    let selected = app.snippets.selected == Some(idx);

    let content = container(
        column![
            text(snippet.display_name())
                .size(14)
                .wrapping(Wrapping::None),
            text(preview_line(&snippet.text))
                .size(12)
                .wrapping(Wrapping::None)
                .style(secondary_text),
        ]
        .spacing(2),
    )
    // 中栏可拖拽变窄，超长名称 / 文本一律单行截断，不撑宽面板。
    .width(Length::Fill)
    .clip(true);
    let item: Element<'a, Message> = mouse_area(
        container(content)
            .padding([6u16, 8u16])
            .width(Length::Fill)
            .style(move |theme| {
                // 片段无连接态，恒传 `Disconnected`（行底色不随任何状态着色）。
                let mut style = crate::theme::list_row_bg(
                    theme,
                    selected,
                    hovered,
                    ConnectionStatus::Disconnected,
                );
                style.border.radius = 6.0.into();
                style
            }),
    )
    .on_press(Message::SnippetSelect(idx))
    .on_enter(Message::SnippetEnter(idx))
    .on_exit(Message::SnippetExit(idx))
    .on_double_click(Message::SendCurrent(idx))
    .interaction(iced::mouse::Interaction::Pointer)
    .into();

    let total = app.snippets.items.len();
    let menu_overlay = move || snippet_menu(idx, total, send_current_ok, send_all_ok, can_edit);
    ContextMenu::new(item, menu_overlay).into()
}

/// 片段行的右键菜单（四组：新建 / 发送 / 编辑与排序 / 删除）。
fn snippet_menu(
    idx: usize,
    total: usize,
    send_current_ok: bool,
    send_all_ok: bool,
    can_edit: bool,
) -> Element<'static, Message> {
    let mut actions: Vec<Element<'static, Message>> = Vec::new();
    actions.push(menu_item(t!("common.new"), can_edit, Message::NewSnippet));
    actions.push(menu_separator());
    actions.push(menu_item(
        t!("snippet.send_current"),
        send_current_ok,
        Message::SendCurrent(idx),
    ));
    actions.push(menu_item(
        t!("snippet.send_all"),
        send_all_ok,
        Message::SendAll(idx),
    ));
    actions.push(menu_separator());
    actions.push(menu_item(
        t!("common.edit"),
        can_edit,
        Message::EditSnippet(idx),
    ));
    actions.push(menu_item(
        t!("snippet.copy"),
        can_edit,
        Message::DuplicateSnippet(idx),
    ));
    // 已在首 / 末行时对应的移动项置灰。
    actions.push(menu_item(
        t!("snippet.move_up"),
        can_edit && idx > 0,
        Message::MoveUp(idx),
    ));
    actions.push(menu_item(
        t!("snippet.move_down"),
        can_edit && idx + 1 < total,
        Message::MoveDown(idx),
    ));
    actions.push(menu_separator());
    actions.push(menu_item(
        t!("snippet.delete"),
        can_edit,
        Message::RequestDeleteSnippet(idx),
    ));
    crate::ui::menu_container(MeasuredColumn::with_children(actions))
}

/// 单个菜单项：`enabled` 为假时渲染为禁用项（无点击响应、次要文字色）。
fn menu_item(label: String, enabled: bool, msg: Message) -> Element<'static, Message> {
    if enabled {
        menu_entry(label, msg)
    } else {
        menu_entry_disabled(label)
    }
}

/// 行内文本预览：取首个非空行并 trim（多行命令只展示起始一行，不撑高行高）。
fn preview_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 片段清单为空时的占位提示。
fn empty_state<'a>() -> Element<'a, Message> {
    let content = column![
        text(t!("snippet.empty")).size(15).wrapping(Wrapping::Word),
        text(t!("snippet.empty_hint"))
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

/// 过滤后无命中时的占位提示。
fn no_match_state<'a>() -> Element<'a, Message> {
    container(crate::ui::hint_text(t!("snippet.filter_no_match")))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

/// 次要文本样式（行内预览、空态说明）。
fn secondary_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(crate::theme::custom_palette(theme).text_secondary),
    }
}
