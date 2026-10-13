//! 指针按下 / 移动 / 释放：选区起点、拖拽更新与链接悬停、打开。

use super::common::*;
use crate::widget::term::backend::{Command, LinkAction, MouseButton, RenderableContent};
use crate::widget::term::bindings::BindingsLayout;
use crate::widget::term::view::TerminalViewState;
use crate::widget::term::view::{TerminalGridPoint, TerminalView};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use iced::Point;
use iced_core::keyboard::Modifiers;

#[test]
fn handles_mouse_mode_with_left_click() {
    let mut state = TerminalViewState::new();
    let terminal_mode = TermMode::MOUSE_MODE;
    let layout_position = Point { x: 5.0, y: 5.0 };
    let cursor_position = Point { x: 100.0, y: 150.0 };
    let mut commands = Vec::new();
    let _modifiers = Modifiers::empty();

    TerminalView::handle_left_button_pressed(
        &mut state,
        &terminal_mode,
        cursor_position,
        layout_position,
        TEST_PADDING,
        &mut commands,
    );

    assert_eq!(commands.len(), 1);
    assert!(matches!(
        commands[0],
        Command::MouseReport(
            MouseButton::LeftButton,
            _modifiers,
            TerminalGridPoint {
                line: Line(0),
                column: Column(0),
            },
            true,
        )
    ));
    assert!(state.is_dragged);
}

#[test]
fn starts_simple_selection_with_left_click() {
    let terminal_mode = TermMode::SGR_MOUSE;
    let cursor_position = Point { x: 200.0, y: 150.0 };
    let layout_position = Point { x: 50.0, y: 50.0 };

    let cases = vec![
        SelectionType::Simple,
        SelectionType::Semantic,
        SelectionType::Lines,
    ];

    for _selection_type in cases {
        let mut state = TerminalViewState::new();
        state.keyboard_modifiers = Modifiers::SHIFT;
        let mut commands = Vec::new();

        TerminalView::handle_left_button_pressed(
            &mut state,
            &terminal_mode,
            cursor_position,
            layout_position,
            TEST_PADDING,
            &mut commands,
        );

        assert_eq!(commands.len(), 1);
        assert!(matches!(
            commands[0],
            Command::SelectStart(_selection_type, (146.0, 96.0))
        ),);
        assert!(state.is_dragged);
    }
}

#[test]
fn updates_mouse_position_on_grid() {
    let mut state = TerminalViewState::new();
    let terminal_content = RenderableContent::default();
    let mut commands = Vec::new();
    let cases = vec![
        (
            Point { x: 0.0, y: 0.0 },
            Point { x: 1.0, y: 1.0 },
            TerminalGridPoint {
                line: Line(0),
                column: Column(0),
            },
        ),
        (
            Point { x: 0.0, y: 0.0 },
            Point { x: 2.0, y: 2.0 },
            TerminalGridPoint {
                line: Line(0),
                column: Column(0),
            },
        ),
        (
            Point { x: 0.0, y: 0.0 },
            Point { x: 30.0, y: 2.0 },
            TerminalGridPoint {
                line: Line(0),
                column: Column(26),
            },
        ),
        (
            Point { x: 10.0, y: 0.0 },
            Point { x: 30.0, y: 2.0 },
            TerminalGridPoint {
                line: Line(0),
                column: Column(16),
            },
        ),
        (
            Point { x: 10.0, y: 10.0 },
            Point { x: 30.0, y: 2.0 },
            TerminalGridPoint {
                line: Line(0),
                column: Column(16),
            },
        ),
    ];

    for (layout_position, cursor_position, expected) in cases {
        TerminalView::handle_cursor_moved(
            &mut state,
            &terminal_content,
            &cursor_position,
            layout_position,
            TEST_PADDING,
            &mut commands,
        );

        assert_eq!(state.mouse_position_on_grid, expected);
    }
}

#[test]
fn generates_drag_update_command_when_dragged() {
    let mut state = TerminalViewState::new();
    state.is_dragged = true; // 模拟进行中的拖拽操作
    let terminal_content = RenderableContent::default();
    let layout_position = Point { x: 5.0, y: 5.0 };
    // 光标落在内容区内（默认终端 50 行），隔离出纯选区更新、不触发边缘滚动。
    let cursor_position = Point { x: 100.0, y: 45.0 };
    let mut commands = Vec::new();

    TerminalView::handle_cursor_moved(
        &mut state,
        &terminal_content,
        &cursor_position,
        layout_position,
        TEST_PADDING,
        &mut commands,
    );

    assert_eq!(commands.len(), 1);
    assert!(matches!(commands[0], Command::SelectUpdate((91.0, 36.0))));
}

#[test]
fn generates_drag_update_command_when_dragged_in_mouse_motion_mode() {
    let mut state = TerminalViewState::new();
    state.is_dragged = true; // 模拟进行中的拖拽操作
    let terminal_content = RenderableContent {
        terminal_mode: TermMode::MOUSE_MOTION,
        ..Default::default()
    };
    let layout_position = Point { x: 5.0, y: 5.0 };
    let cursor_position = Point { x: 100.0, y: 150.0 };
    let mut commands = Vec::new();
    let _modifiers = Modifiers::empty();

    TerminalView::handle_cursor_moved(
        &mut state,
        &terminal_content,
        &cursor_position,
        layout_position,
        TEST_PADDING,
        &mut commands,
    );

    assert_eq!(commands.len(), 1);
    assert!(matches!(
        commands[0],
        Command::MouseReport(
            MouseButton::LeftMove,
            _modifiers,
            TerminalGridPoint {
                line: Line(49),
                column: Column(79),
            },
            true,
        )
    ));
}

#[test]
fn generates_drag_update_command_when_dragged_in_srg_mode_with_key_mods() {
    let mut state = TerminalViewState::new();
    state.keyboard_modifiers = Modifiers::SHIFT;
    state.is_dragged = true; // 模拟进行中的拖拽操作
    let terminal_content = RenderableContent {
        terminal_mode: TermMode::SGR_MOUSE,
        ..Default::default()
    };
    let layout_position = Point { x: 5.0, y: 5.0 };
    // 光标落在内容区内，隔离出纯选区更新、不触发边缘滚动。
    let cursor_position = Point { x: 100.0, y: 45.0 };
    let mut commands = Vec::new();

    TerminalView::handle_cursor_moved(
        &mut state,
        &terminal_content,
        &cursor_position,
        layout_position,
        TEST_PADDING,
        &mut commands,
    );

    assert_eq!(commands.len(), 1);
    assert!(matches!(commands[0], Command::SelectUpdate((91.0, 36.0))));
}

#[test]
fn generates_drag_update_and_link_open() {
    let mut state = TerminalViewState::new();
    state.keyboard_modifiers = Modifiers::COMMAND;
    state.is_dragged = true; // 模拟进行中的拖拽操作
    let terminal_content = RenderableContent {
        terminal_mode: TermMode::SGR_MOUSE,
        ..Default::default()
    };
    let layout_position = Point { x: 5.0, y: 5.0 };
    // 光标落在内容区内，隔离出纯选区更新、不触发边缘滚动。
    let cursor_position = Point { x: 100.0, y: 45.0 };
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
    assert!(matches!(commands[0], Command::SelectUpdate((91.0, 36.0))));
    assert!(matches!(
        commands[1],
        Command::ProcessLink(
            LinkAction::Hover,
            TerminalGridPoint {
                line: Line(36),
                column: Column(79),
            },
        )
    ));
}

#[test]
fn mouse_mode_activated() {
    let mut state = TerminalViewState::new();
    let terminal_mode = TermMode::MOUSE_MODE;
    let bindings = BindingsLayout::new();
    let mut commands = Vec::new();
    let _modifiers = Modifiers::empty();

    TerminalView::handle_button_released(&mut state, &terminal_mode, &bindings, &mut commands);

    assert_eq!(commands.len(), 1);
    assert!(matches!(
        commands[0],
        Command::MouseReport(
            MouseButton::LeftButton,
            _modifiers,
            TerminalGridPoint {
                line: Line(0),
                column: Column(0)
            },
            false
        )
    ));
}

#[test]
fn link_open_on_button_release() {
    let mut state = TerminalViewState::new();
    state.keyboard_modifiers = Modifiers::COMMAND;
    let terminal_mode = TermMode::MOUSE_MODE;
    let bindings = BindingsLayout::new();
    let mut commands = Vec::new();
    let _modifiers = Modifiers::empty();

    TerminalView::handle_button_released(&mut state, &terminal_mode, &bindings, &mut commands);

    assert_eq!(commands.len(), 2);
    assert!(matches!(
        commands[0],
        Command::MouseReport(
            MouseButton::LeftButton,
            _modifiers,
            TerminalGridPoint {
                line: Line(0),
                column: Column(0)
            },
            false
        )
    ));
    assert!(matches!(
        commands[1],
        Command::ProcessLink(
            LinkAction::Open,
            TerminalGridPoint {
                line: Line(0),
                column: Column(0)
            }
        ),
    ));
}

#[test]
fn link_open_on_button_release_in_non_mouse_mode() {
    let mut state = TerminalViewState::new();
    state.keyboard_modifiers = Modifiers::COMMAND;
    state.mouse_position_on_grid = TerminalGridPoint {
        line: Line(4),
        column: Column(10),
    };
    let terminal_mode = TermMode::empty(); // 假定 SGR_MOUSE 模式不影响链接打开
    let bindings = BindingsLayout::new();
    let mut commands = Vec::new();

    TerminalView::handle_button_released(&mut state, &terminal_mode, &bindings, &mut commands);

    assert_eq!(commands.len(), 1);
    assert!(matches!(
        commands[0],
        Command::ProcessLink(
            LinkAction::Open,
            TerminalGridPoint {
                line: Line(4),
                column: Column(10)
            }
        ),
    ));
}
