//! 剪贴板与输入法链路：括号粘贴、中键粘贴取词、预编辑串与候选锚点。

use super::common::*;
use crate::widget::term::backend::{Command, MouseButton};
use crate::widget::term::view::input::{paste_bytes, read_middle_paste};
use crate::widget::term::view::{TerminalGridPoint, TerminalView, TerminalViewState};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use iced::Point;
use iced_core::clipboard::Kind as ClipboardKind;
use iced_core::input_method;
use iced_core::keyboard::Modifiers;

/// 剪贴板桩：分别预置 PRIMARY / 标准剪贴板的内容；写入忽略（本组用例只验证读取与决策）。
#[derive(Default)]
struct StubClipboard {
    primary: Option<String>,
    standard: Option<String>,
}

impl iced_graphics::core::Clipboard for StubClipboard {
    fn read(&self, kind: ClipboardKind) -> Option<String> {
        match kind {
            ClipboardKind::Primary => self.primary.clone(),
            ClipboardKind::Standard => self.standard.clone(),
        }
    }

    fn write(&mut self, _kind: ClipboardKind, _contents: String) {}
}

#[test]
fn paste_bytes_keeps_data_without_bracketed_paste() {
    assert_eq!(paste_bytes("ls\n", false), b"ls\n".to_vec());
}

#[test]
fn paste_bytes_wraps_data_in_bracketed_paste() {
    assert_eq!(
        paste_bytes("ls\n", true),
        b"\x1b[200~ls\n\x1b[201~".to_vec()
    );
}

#[test]
fn read_middle_paste_prefers_primary_over_standard() {
    let clipboard = StubClipboard {
        primary: Some("primary".into()),
        standard: Some("standard".into()),
    };

    assert_eq!(read_middle_paste(&clipboard).as_deref(), Some("primary"));
}

#[test]
fn read_middle_paste_falls_back_to_standard_when_primary_empty_or_unavailable() {
    let empty_primary = StubClipboard {
        primary: Some(String::new()),
        standard: Some("standard".into()),
    };
    assert_eq!(
        read_middle_paste(&empty_primary).as_deref(),
        Some("standard")
    );

    let missing_primary = StubClipboard {
        primary: None,
        standard: Some("standard".into()),
    };
    assert_eq!(
        read_middle_paste(&missing_primary).as_deref(),
        Some("standard")
    );
}

#[test]
fn read_middle_paste_yields_nothing_when_both_empty() {
    let empty_both = StubClipboard {
        primary: Some(String::new()),
        standard: Some(String::new()),
    };
    assert!(read_middle_paste(&empty_both).is_none());

    let missing_both = StubClipboard::default();
    assert!(read_middle_paste(&missing_both).is_none());
}

#[test]
fn middle_press_reports_in_mouse_mode() {
    let state = TerminalViewState::new();
    let clipboard = StubClipboard {
        primary: Some("ignored".into()),
        standard: None,
    };
    let mut commands = Vec::new();
    let mut paste = None;
    let _modifiers = Modifiers::empty();

    TerminalView::handle_middle_button_pressed(
        &state,
        &TermMode::MOUSE_MODE,
        &clipboard,
        &mut commands,
        &mut paste,
    );

    assert_eq!(commands.len(), 1);
    assert!(matches!(
        commands[0],
        Command::MouseReport(
            MouseButton::MiddleButton,
            _modifiers,
            TerminalGridPoint {
                line: Line(0),
                column: Column(0),
            },
            true,
        )
    ));
    assert!(paste.is_none(), "鼠标模式下中键是上报，不该产出粘贴请求");
}

#[test]
fn middle_press_yields_paste_text_outside_mouse_mode() {
    let state = TerminalViewState::new();
    let clipboard = StubClipboard {
        primary: Some("primary".into()),
        standard: None,
    };
    let mut commands = Vec::new();
    let mut paste = None;

    TerminalView::handle_middle_button_pressed(
        &state,
        &TermMode::empty(),
        &clipboard,
        &mut commands,
        &mut paste,
    );

    // 部件只上抛原始文本（括号粘贴编码改由宿主经 `Terminal::encode_paste` 完成）。
    assert!(commands.is_empty());
    assert_eq!(paste.as_deref(), Some("primary"));
}

#[test]
fn middle_press_yields_raw_text_even_when_bracketed() {
    let state = TerminalViewState::new();
    let clipboard = StubClipboard {
        primary: Some("primary".into()),
        standard: None,
    };
    let mut commands = Vec::new();
    let mut paste = None;

    TerminalView::handle_middle_button_pressed(
        &state,
        &TermMode::BRACKETED_PASTE,
        &clipboard,
        &mut commands,
        &mut paste,
    );

    assert!(commands.is_empty());
    assert_eq!(paste.as_deref(), Some("primary"));
}

#[test]
fn middle_press_with_empty_clipboard_yields_nothing() {
    let state = TerminalViewState::new();
    let mut commands = Vec::new();
    let mut paste = None;

    TerminalView::handle_middle_button_pressed(
        &state,
        &TermMode::empty(),
        &StubClipboard::default(),
        &mut commands,
        &mut paste,
    );

    assert!(commands.is_empty());
    assert!(paste.is_none());
}

#[test]
fn middle_release_reports_only_in_mouse_mode() {
    let state = TerminalViewState::new();

    let mut commands = Vec::new();
    TerminalView::handle_middle_button_released(&state, &TermMode::MOUSE_MODE, &mut commands);
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        commands[0],
        Command::MouseReport(MouseButton::MiddleButton, _, _, false)
    ));

    let mut commands = Vec::new();
    TerminalView::handle_middle_button_released(&state, &TermMode::empty(), &mut commands);
    assert!(commands.is_empty());
}

#[test]
fn commit_writes_utf8_and_ends_composition() {
    let mut state = TerminalViewState::new();
    state.preedit = Some("nihao".to_owned());

    let command = TerminalView::handle_input_method_event(
        &mut state,
        &input_method::Event::Commit("你好".to_owned()),
    );

    assert!(matches!(
        command,
        Some(Command::Write(bytes)) if bytes == "你好".as_bytes()
    ));
    assert_eq!(state.preedit, None);
}

#[test]
fn commit_without_text_writes_nothing() {
    let mut state = TerminalViewState::new();

    let command = TerminalView::handle_input_method_event(
        &mut state,
        &input_method::Event::Commit(String::new()),
    );

    assert!(command.is_none());
}

#[test]
fn preedit_is_stored_until_emptied() {
    let mut state = TerminalViewState::new();

    TerminalView::handle_input_method_event(
        &mut state,
        &input_method::Event::Preedit("nihao".to_owned(), None),
    );
    assert_eq!(state.preedit.as_deref(), Some("nihao"));

    // 空串表示组合被清空（提交前输入法会先发一次空预编辑串）。
    let command = TerminalView::handle_input_method_event(
        &mut state,
        &input_method::Event::Preedit(String::new(), None),
    );
    assert!(command.is_none());
    assert_eq!(state.preedit, None);
}

#[test]
fn closed_clears_preedit() {
    let mut state = TerminalViewState::new();
    state.preedit = Some("nihao".to_owned());

    let command = TerminalView::handle_input_method_event(&mut state, &input_method::Event::Closed);

    assert!(command.is_none());
    assert_eq!(state.preedit, None);
}

#[test]
fn caret_anchors_to_cursor_cell() {
    let bounds = iced::Rectangle::new(Point::new(100.0, 50.0), iced::Size::new(400.0, 300.0));

    let rect = TerminalView::caret_rect(bounds, TEST_PADDING, &content_at(2, 3));

    assert_eq!(rect.x, 100.0 + TEST_PADDING + 3.0);
    assert_eq!(rect.y, 50.0 + TEST_PADDING + 2.0);
}

#[test]
fn caret_clamps_into_content_area_when_cursor_is_out_of_view() {
    let bounds = iced::Rectangle::new(Point::ORIGIN, iced::Size::new(200.0, 100.0));

    // 光标滚出可视区（回滚历史）时把锚点收回内容区底边，避免候选框落到窗口外。
    let rect = TerminalView::caret_rect(bounds, TEST_PADDING, &content_at(1000, 0));

    assert_eq!(rect.y, 100.0 - TEST_PADDING - 1.0);
}
