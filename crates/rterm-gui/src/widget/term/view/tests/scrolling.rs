//! 终端滚动：滚轮增量与拖拽越界时的边缘自动滚动。

use super::common::*;
use crate::widget::term::backend::{Command, RenderableContent};
use crate::widget::term::font::TermFont;
use crate::widget::term::settings::FontSettings;
use crate::widget::term::view::input::{
    SELECTION_AUTOSCROLL_MAX_LINES, selection_autoscroll_delta,
};
use crate::widget::term::view::{TerminalView, TerminalViewState};
use alacritty_terminal::term::TermMode;
use iced::Point;
use iced::mouse::ScrollDelta;

#[test]
fn scroll_wheel_up_by_lines() {
    let mut state = TerminalViewState::new();
    let font = TermFont::new(FontSettings::default());
    let mut commands = Vec::new();

    TerminalView::handle_wheel_scrolled(
        &mut state,
        ScrollDelta::Lines { y: 3.0, x: 0.0 }, // 滚轮上滚 3 行（y 为正 = 回滚历史）
        &font.measure,
        &mut commands,
    );

    assert_eq!(commands.len(), 1);
    assert!(matches!(commands[0], Command::Scroll(3)));
}

#[test]
fn scroll_wheel_down_by_lines() {
    let mut state = TerminalViewState::new();
    let font = TermFont::new(FontSettings::default());
    let mut commands = Vec::new();

    TerminalView::handle_wheel_scrolled(
        &mut state,
        ScrollDelta::Lines { y: -2.0, x: 0.0 },
        &font.measure,
        &mut commands,
    );

    assert_eq!(commands.len(), 1);
    assert!(matches!(commands[0], Command::Scroll(-2)));
}

#[test]
fn scroll_wheel_up_by_pixels() {
    let mut state = TerminalViewState::new();
    let font = TermFont::new(FontSettings::default());
    let mut commands = Vec::new();

    TerminalView::handle_wheel_scrolled(
        &mut state,
        ScrollDelta::Pixels { y: 45.0, x: 0.0 },
        &font.measure,
        &mut commands,
    );

    assert_eq!(commands.len(), 1);
    assert!(matches!(commands[0], Command::Scroll(2)));
    assert_eq!(state.scroll_pixels, 8.600002);
}

#[test]
fn scroll_wheel_down_by_pixels() {
    let mut state = TerminalViewState::new();
    let font = TermFont::new(FontSettings::default());
    let mut commands = Vec::new();

    TerminalView::handle_wheel_scrolled(
        &mut state,
        ScrollDelta::Pixels { y: -60.0, x: 0.0 },
        &font.measure,
        &mut commands,
    );

    assert_eq!(commands.len(), 1);
    assert!(matches!(commands[0], Command::Scroll(-3)));
    assert_eq!(state.scroll_pixels, -5.4000034);
}

#[test]
fn delta_returns_none_inside_content() {
    assert_eq!(selection_autoscroll_delta(0.0, 50, 1.0), None);
    assert_eq!(selection_autoscroll_delta(49.9, 50, 1.0), None);
}

#[test]
fn delta_scrolls_up_proportional_to_overshoot() {
    assert_eq!(selection_autoscroll_delta(-0.5, 50, 1.0), Some(1));
    assert_eq!(selection_autoscroll_delta(-2.0, 50, 1.0), Some(3));
}

#[test]
fn delta_scrolls_down_proportional_to_overshoot() {
    assert_eq!(selection_autoscroll_delta(50.0, 50, 1.0), Some(-1));
    assert_eq!(selection_autoscroll_delta(53.0, 50, 1.0), Some(-4));
}

#[test]
fn delta_caps_overshoot_at_max_lines() {
    assert_eq!(
        selection_autoscroll_delta(-100.0, 50, 1.0),
        Some(SELECTION_AUTOSCROLL_MAX_LINES)
    );
    assert_eq!(
        selection_autoscroll_delta(1000.0, 50, 1.0),
        Some(-SELECTION_AUTOSCROLL_MAX_LINES)
    );
}

#[test]
fn delta_returns_none_for_degenerate_geometry() {
    assert_eq!(selection_autoscroll_delta(-5.0, 0, 1.0), None);
    assert_eq!(selection_autoscroll_delta(-5.0, 50, 0.0), None);
}

#[test]
fn scrolls_up_when_dragged_above_content() {
    let mut state = TerminalViewState::new();
    state.is_dragged = true;
    // 默认终端 50 行、单元格 1×1 像素：内容区为 y ∈ [padding, padding + 50)。
    let terminal_content = RenderableContent::default();
    let layout_position = Point { x: 0.0, y: 0.0 };
    // 光标越过顶边（y = padding）2px => 越界 2 行 + 1 = 3 行。
    let cursor_position = Point { x: 0.0, y: 2.0 };
    let mut commands = Vec::new();

    TerminalView::handle_cursor_moved(
        &mut state,
        &terminal_content,
        &cursor_position,
        layout_position,
        TEST_PADDING,
        &mut commands,
    );

    // 先滚动再更新选区，端点才能跟着新揭示的行延伸。
    assert_eq!(commands.len(), 2);
    assert!(matches!(commands[0], Command::Scroll(3)));
    assert!(matches!(commands[1], Command::SelectUpdate(_)));
}

#[test]
fn scrolls_down_when_dragged_below_content() {
    let mut state = TerminalViewState::new();
    state.is_dragged = true;
    let terminal_content = RenderableContent::default();
    let layout_position = Point { x: 0.0, y: 0.0 };
    // 光标恰落在底边（y = padding + 50）上 => 越界 0 行 + 1 = -1 行。
    let cursor_position = Point { x: 0.0, y: 54.0 };
    let mut commands = Vec::new();

    TerminalView::handle_cursor_moved(
        &mut state,
        &terminal_content,
        &cursor_position,
        layout_position,
        TEST_PADDING,
        &mut commands,
    );

    assert_eq!(commands.len(), 2);
    assert!(matches!(commands[0], Command::Scroll(-1)));
    assert!(matches!(commands[1], Command::SelectUpdate(_)));
}

#[test]
fn caps_autoscroll_at_max_lines() {
    let mut state = TerminalViewState::new();
    state.is_dragged = true;
    let terminal_content = RenderableContent::default();
    let layout_position = Point { x: 0.0, y: 0.0 };
    // 光标越过内容区底边极远，增量应被封顶为单次最大行数。
    let cursor_position = Point { x: 0.0, y: 5004.0 };
    let mut commands = Vec::new();

    TerminalView::handle_cursor_moved(
        &mut state,
        &terminal_content,
        &cursor_position,
        layout_position,
        TEST_PADDING,
        &mut commands,
    );

    assert_eq!(commands.len(), 2);
    assert!(matches!(
        commands[0],
        Command::Scroll(n) if n == -SELECTION_AUTOSCROLL_MAX_LINES
    ));
}

#[test]
fn skips_autoscroll_in_alt_screen() {
    let mut state = TerminalViewState::new();
    state.is_dragged = true;
    let terminal_content = RenderableContent {
        terminal_mode: TermMode::ALT_SCREEN,
        ..Default::default()
    };
    let layout_position = Point { x: 0.0, y: 0.0 };
    let cursor_position = Point { x: 0.0, y: 54.0 };
    let mut commands = Vec::new();

    TerminalView::handle_cursor_moved(
        &mut state,
        &terminal_content,
        &cursor_position,
        layout_position,
        TEST_PADDING,
        &mut commands,
    );

    // 备用屏下 `Command::Scroll` 会改发方向键给远端，故只更新选区、不产生滚动。
    assert_eq!(commands.len(), 1);
    assert!(matches!(commands[0], Command::SelectUpdate(_)));
}
