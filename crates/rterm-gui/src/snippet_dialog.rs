//! 常用命令的新建 / 编辑弹窗。
//!
//! 与其余模态框共用遮罩（[`crate::modal::overlay_wrap_dismissable`]，单击遮罩空白关闭），由 [`crate::layout`] 叠到最上层；命令输入按 bash 词法着色（见 [`crate::bash_highlight`]），校验原因落在页脚、保存按钮保持可用。

use crate::t;

use crate::App;
use crate::app::snippets::{EditorDraft, Message};
use crate::bash_highlight::{BashHighlighter, Settings, bash_format};
use iced::widget::text::Wrapping;
use iced::widget::{checkbox, column, container, text_editor, text_input};
use iced::{Background, Border, Element, Font, Length, Theme};
use rterm_config::{MAX_NAME_CHARS, MAX_TEXT_BYTES, SnippetIssue};

/// 弹窗宽度（像素）；比会话编辑器更宽，给命令行留出横向空间。
const DIALOG_W: f32 = 740.0;
/// 命令输入区高度（像素）。
const TEXT_AREA_HEIGHT: f32 = 160.0;

/// 常用命令编辑弹窗；无草稿时返回 `None`（不占据叠加层）。
pub fn view(app: &App) -> Option<Element<'_, Message>> {
    let draft = app.snippets.editor.as_ref()?;
    let panel = container(body(draft))
        .width(DIALOG_W)
        .style(crate::ui::dialog_panel_style(None))
        .padding(0);
    Some(crate::modal::overlay_wrap_dismissable(
        panel.into(),
        Message::CancelEdit,
    ))
}

/// 弹窗主体：标题栏 + 表单 + 固定页脚（校验提示与按钮）。
fn body(draft: &EditorDraft) -> Element<'_, Message> {
    let target = draft.target();
    // 点过保存才提示原因：新建草稿必然是空名，一打开就报错属于误报。
    let issue = if draft.save_attempted {
        draft.issue()
    } else {
        None
    };

    let header = crate::ui::dialog_title_bar(
        if draft.is_new() {
            t!("snippet.new")
        } else {
            t!("snippet.edit")
        },
        Some(Message::CancelEdit),
    );

    let command_area = text_editor(&draft.text)
        .font(Font::MONOSPACE)
        .size(12)
        .padding(8)
        .height(Length::Fixed(TEXT_AREA_HEIGHT))
        .wrapping(Wrapping::WordOrGlyph)
        .highlight_with::<BashHighlighter>(Settings, bash_format)
        .on_action(move |action| Message::EditorAction(target, action))
        .style(editor_style);

    let mut fields: Vec<Element<'_, Message>> = Vec::new();
    fields.push(crate::ui::field_label(name_label()));
    fields.push(
        text_input(&t!("snippet.name_placeholder"), &draft.name)
            .on_input(Message::EditorName)
            .style(crate::ui::text_input_style)
            .into(),
    );
    fields.push(
        column![crate::ui::field_label(t!("snippet.text")), command_area]
            .spacing(4)
            .into(),
    );
    fields.push(
        column![
            checkbox(draft.submit)
                .label(t!("snippet.submit"))
                .on_toggle(|_| Message::EditorToggleSubmit)
                .spacing(8),
            crate::ui::hint_text(t!("snippet.submit_hint")),
        ]
        .spacing(4)
        .into(),
    );
    fields.push(crate::ui::hint_text(t!("snippet.plaintext_hint")));
    let form = column(fields).spacing(10);

    let footer = crate::ui::dialog_footer(
        issue.map(issue_text),
        Some(crate::ui::DialogButton {
            label: t!("common.cancel"),
            on_press: Message::CancelEdit,
            style: crate::ui::DialogBtnStyle::Neutral,
        }),
        crate::ui::DialogButton {
            label: t!("snippet.save"),
            on_press: Message::EditorSave,
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

/// 命令输入区的样式：随主题的表面色与边框，与本应用其余输入框一致。
fn editor_style(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let mut style = text_editor::default(theme, status);
    let p = crate::theme::custom_palette(theme);
    style.background = Background::Color(p.surface);
    style.border = Border {
        color: p.border,
        width: 1.0,
        radius: 6.0.into(),
    };
    style
}

/// 名称字段标签：带上限提示，使「名称 ≤ N 字符」这条规则在编辑处可见。
fn name_label() -> String {
    t!("snippet.name", max => MAX_NAME_CHARS)
}

/// 校验失败原因的提示文案。
fn issue_text(issue: SnippetIssue) -> String {
    match issue {
        SnippetIssue::EmptyName => t!("snippet.name_required"),
        SnippetIssue::NameTooLong => t!("snippet.name_too_long", max => MAX_NAME_CHARS),
        SnippetIssue::EmptyText => t!("snippet.empty_text"),
        SnippetIssue::TextTooLong => t!("snippet.text_too_long", max => MAX_TEXT_BYTES),
    }
}
