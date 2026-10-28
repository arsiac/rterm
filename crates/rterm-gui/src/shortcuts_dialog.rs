//! 快捷键管理弹窗。
//!
//! 单栏分组列出全部快捷键：可编辑区（点键位字段进入录制、可重置单条 / 全部，冲突时行内
//! 提示并可「重新分配」）与固定只读区（终端复制 / 粘贴）。纯渲染，交互经
//! [`crate::message::Message::Shortcuts`] 路由进 [`crate::app::shortcuts`]。

use crate::app::App;
use crate::app::shortcuts::{self, Command, CommandGroup, RecordingNotice};
use crate::icons::{Icon, icon_button};
use crate::message::Message;
use crate::modal;
use crate::t;
use crate::ui::DialogBtnStyle;
use iced::widget::text::Wrapping;
use iced::widget::{button, column, container, row, text, text_input};
use iced::{Background, Border, Element, Length, Theme};

/// 快捷键弹窗面板占窗口宽 / 高的比例。
const SHORTCUTS_SIZE_RATIO: f32 = 0.8;
/// 面板与窗口边缘的留白（像素）：用于封顶面板尺寸，避免贴边。
const DIALOG_MARGIN: f32 = 16.0;
/// 键位字段宽度（像素）。
const CHORD_FIELD_W: f32 = 160.0;
/// 「恢复默认」图标尺寸（像素）。
const RESET_ICON_SIZE: f32 = 14.0;

/// 根据当前是否显示快捷键弹窗返回遮罩层元素；未显示时返回 `None`。
pub fn view(app: &App) -> Option<Element<'_, Message>> {
    if !app.shortcuts.show_shortcuts {
        return None;
    }
    let panel = iced::widget::responsive(move |size| {
        // 宽 / 高均按窗口占比取，并以「窗口 - 两侧留白」封顶，避免贴边。
        let max_w = (size.width - DIALOG_MARGIN * 2.0).max(0.0);
        let max_h = (size.height - DIALOG_MARGIN * 2.0).max(0.0);
        let width = (size.width * SHORTCUTS_SIZE_RATIO).clamp(0.0, max_w);
        let height = (size.height * SHORTCUTS_SIZE_RATIO).clamp(0.0, max_h);
        container(
            container(body(app))
                .width(width)
                .height(height)
                .style(crate::ui::dialog_panel_style(None))
                .padding(0),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    });
    Some(modal::overlay_wrap_dismissable(
        panel.into(),
        Message::Shortcuts(shortcuts::Message::Toggle),
    ))
}

/// 弹窗主体：标题栏（含强调条与关闭按钮）+ 分隔线 + 分组快捷键列表 + 页脚。
fn body(app: &App) -> Element<'_, Message> {
    column![
        crate::ui::dialog_title_bar(
            t!("shortcuts.title"),
            Some(Message::Shortcuts(shortcuts::Message::Toggle)),
        ),
        crate::ui::hairline(),
        content(app),
        crate::ui::hairline(),
        footer(),
    ]
    .into()
}

/// 弹窗正文：过滤框 + 分组可滚动列表（可编辑区 + 固定只读区）。
fn content(app: &App) -> Element<'_, Message> {
    let filter_input = text_input(&t!("shortcuts.filter_placeholder"), &app.shortcuts.filter)
        .on_input(|s| Message::Shortcuts(shortcuts::Message::FilterChanged(s)))
        .size(13)
        .padding([5u16, 8u16])
        .width(Length::Fill)
        .style(crate::ui::search_input_style);

    let filter = app.shortcuts.filter.trim().to_lowercase();
    let mut items: Vec<Element<'_, Message>> = Vec::new();

    for group in CommandGroup::ALL {
        let mut rows: Vec<Element<'_, Message>> = Vec::new();
        for cmd in group.commands() {
            let label = command_label(cmd);
            let chord = app.shortcuts.chord_for(cmd).map(|c| c.to_string());
            if !matches(&filter, &label, chord.as_deref()) {
                continue;
            }
            rows.push(command_row(app, cmd));
        }
        if !rows.is_empty() {
            items.push(crate::ui::section_title(group_label(group)));
            items.extend(rows);
        }
    }

    let mut readonly: Vec<Element<'_, Message>> = Vec::new();
    for (label, chord) in readonly_bindings() {
        if !matches(&filter, &label, Some(&chord)) {
            continue;
        }
        readonly.push(readonly_row(&label, &chord));
    }
    if !readonly.is_empty() {
        items.push(crate::ui::section_title(t!("shortcuts.group_readonly")));
        items.push(crate::ui::hint_text(t!("shortcuts.readonly_hint")));
        items.extend(readonly);
    }

    let list: Element<'_, Message> = if items.is_empty() {
        crate::ui::hint_text(t!("shortcuts.no_match"))
    } else {
        crate::ui::thin_vertical_scrollbar(column(items).spacing(4))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    };

    let top_pad = crate::ui::DIALOG_BODY_PADDING;
    column![
        container(filter_input).padding(iced::Padding {
            top: top_pad[0],
            right: top_pad[1],
            bottom: 8.0,
            left: top_pad[1],
        }),
        container(list)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(iced::Padding {
                top: 0.0,
                right: top_pad[1],
                bottom: 8.0,
                left: top_pad[1],
            }),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// 页脚：一条「全部恢复默认」动作，整体靠右。
fn footer() -> Element<'static, Message> {
    container(crate::ui::inline_action_button(
        t!("shortcuts.reset_all"),
        Message::Shortcuts(shortcuts::Message::ResetAll),
        DialogBtnStyle::Neutral,
    ))
    .width(Length::Fill)
    .align_x(iced::alignment::Horizontal::Right)
    .padding(iced::Padding {
        top: 8.0,
        right: crate::ui::DIALOG_BODY_PADDING[1],
        bottom: 8.0,
        left: crate::ui::DIALOG_BODY_PADDING[1],
    })
    .into()
}

/// 可编辑区单行：命令名靠左、键位字段与重置按钮靠右；录制受阻时下方加一行提示。
fn command_row(app: &App, command: Command) -> Element<'_, Message> {
    let label = command_label(command);
    let recording = app.shortcuts.recording() == Some(command);
    let chord = app.shortcuts.chord_for(command).map(|c| c.to_string());

    let mut tail = row![chord_field(command, chord, recording)]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center);
    if app.shortcuts.is_overridden(command) {
        tail = tail.push(icon_button(
            Icon::ArrowClockwise,
            RESET_ICON_SIZE,
            t!("shortcuts.reset"),
            Message::Shortcuts(shortcuts::Message::Reset(command)),
            iced::widget::tooltip::Position::Left,
        ));
    }

    let head = row![text(label).size(14).width(Length::Fill), tail]
        .spacing(12)
        .align_y(iced::alignment::Vertical::Center)
        .padding([4, 8]);

    match app.shortcuts.notice_for(command) {
        Some(notice) => column![head, notice_row(command, notice)].spacing(2).into(),
        None => head.into(),
    }
}

/// 键位字段（可点）：显示当前和弦，未绑定显示占位、录制中显示提示；点按进入录制。
fn chord_field(
    command: Command,
    chord: Option<String>,
    recording: bool,
) -> Element<'static, Message> {
    let bound = chord.is_some();
    let value = if recording {
        t!("shortcuts.record_prompt")
    } else {
        chord.unwrap_or_else(|| t!("shortcuts.unbound"))
    };
    let label = text(value)
        .size(13)
        .wrapping(Wrapping::None)
        .style(move |theme: &Theme| text::Style {
            color: Some(if recording {
                crate::theme::accent_color(theme)
            } else if bound {
                theme.extended_palette().background.base.text
            } else {
                crate::theme::custom_palette(theme).text_tertiary
            }),
        });
    button(label)
        .on_press(Message::Shortcuts(shortcuts::Message::StartRecording(
            command,
        )))
        .width(Length::Fixed(CHORD_FIELD_W))
        .padding([4, 8])
        .style(move |theme, status| chord_field_style(theme, status, recording))
        .into()
}

/// 键位字段的按钮样式：形似输入框，录制态以强调色描边。
fn chord_field_style(theme: &Theme, status: button::Status, recording: bool) -> button::Style {
    let border_color = if recording {
        crate::theme::accent_color(theme)
    } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        crate::theme::input_border_hover(theme)
    } else {
        crate::theme::input_border_color(theme)
    };
    button::Style {
        background: Some(Background::Color(theme.palette().background)),
        text_color: theme.extended_palette().background.base.text,
        border: Border {
            color: border_color,
            width: if recording { 2.0 } else { 1.0 },
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

/// 录制受阻提示行：裸键为纯文字，冲突附「重新分配」按钮。
fn notice_row(command: Command, notice: RecordingNotice) -> Element<'static, Message> {
    let row = match notice {
        RecordingNotice::NeedsModifier => {
            row![notice_text(t!("shortcuts.notice_needs_modifier"))]
        }
        RecordingNotice::Conflict { occupant, .. } => row![
            notice_text(t!("shortcuts.notice_conflict", name => command_label(occupant))),
            crate::ui::inline_action_button(
                t!("shortcuts.reassign"),
                Message::Shortcuts(shortcuts::Message::Reassign(command)),
                DialogBtnStyle::Emphasis { danger: false },
            ),
        ]
        .spacing(8),
    };
    container(row.align_y(iced::alignment::Vertical::Center))
        .padding(iced::Padding {
            top: 0.0,
            right: 8.0,
            bottom: 4.0,
            left: 8.0,
        })
        .into()
}

/// 录制受阻提示文字（警告色）。
fn notice_text(label: String) -> Element<'static, Message> {
    text(label)
        .size(12)
        .wrapping(Wrapping::Word)
        .style(|_theme: &Theme| text::Style {
            color: Some(crate::ui::WARNING),
        })
        .into()
}

/// 只读区单行：与可编辑行同构，但灰显且无任何交互。
fn readonly_row(label: &str, chord: &str) -> Element<'static, Message> {
    row![
        text(label.to_string())
            .size(14)
            .width(Length::Fill)
            .style(|theme: &Theme| text::Style {
                color: Some(crate::theme::custom_palette(theme).text_secondary),
            }),
        text(chord.to_string())
            .size(13)
            .style(|theme: &Theme| text::Style {
                color: Some(crate::theme::custom_palette(theme).text_tertiary),
            }),
    ]
    .spacing(12)
    .align_y(iced::alignment::Vertical::Center)
    .padding([4, 8])
    .into()
}

/// 固定只读绑定：终端复制 / 粘贴（不可配置，仅展示，按平台取默认和弦）。
fn readonly_bindings() -> Vec<(String, String)> {
    let (copy, paste) = if cfg!(target_os = "macos") {
        ("Cmd+C", "Cmd+V")
    } else {
        ("Ctrl+Shift+C", "Ctrl+Shift+V")
    };
    vec![
        (t!("shortcuts.copy").to_string(), copy.to_string()),
        (t!("shortcuts.paste").to_string(), paste.to_string()),
    ]
}

/// 命令的展示名。
fn command_label(command: Command) -> String {
    match command {
        Command::OpenSettings => t!("shortcuts.cmd_open_settings"),
        Command::OpenShortcuts => t!("shortcuts.cmd_open_shortcuts"),
        Command::ToggleSidebar => t!("shortcuts.cmd_toggle_sidebar"),
        Command::NewSession => t!("shortcuts.cmd_new_session"),
        Command::OpenSessions => t!("shortcuts.cmd_open_sessions"),
        Command::OpenSearch => t!("shortcuts.cmd_open_search"),
        Command::OpenSnippets => t!("shortcuts.cmd_open_snippets"),
        Command::SftpRename => t!("shortcuts.cmd_sftp_rename"),
    }
    .to_string()
}

/// 分组的展示名。
fn group_label(group: CommandGroup) -> String {
    match group {
        CommandGroup::General => t!("shortcuts.group_general"),
        CommandGroup::Session => t!("shortcuts.group_session"),
        CommandGroup::Terminal => t!("shortcuts.group_terminal"),
        CommandGroup::Files => t!("shortcuts.group_files"),
    }
    .to_string()
}

/// 过滤命中判定：命令名或和弦文本包含过滤词（均已小写）；空过滤词恒命中。
fn matches(filter: &str, label: &str, chord: Option<&str>) -> bool {
    if filter.is_empty() {
        return true;
    }
    label.to_lowercase().contains(filter)
        || chord.is_some_and(|c| c.to_lowercase().contains(filter))
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn empty_filter_matches_everything() {
        assert!(matches("", "Any command", None));
        assert!(matches("", "Any command", Some("Ctrl+F")));
    }

    #[test]
    fn filter_matches_label_or_chord_case_insensitively() {
        assert!(matches("search", "Terminal Search", Some("Ctrl+Shift+F")));
        assert!(matches("shift+f", "Terminal Search", Some("Ctrl+Shift+F")));
        assert!(matches("ctrl", "Terminal Search", Some("ctrl+shift+f")));
        assert!(!matches("rename", "Terminal Search", Some("Ctrl+Shift+F")));
        assert!(!matches("f2", "Terminal Search", None));
    }
}
