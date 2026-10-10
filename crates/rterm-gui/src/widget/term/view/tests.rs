use super::input::{
    SELECTION_AUTOSCROLL_MAX_LINES, paste_bytes, read_middle_paste, selection_autoscroll_delta,
};
use super::scrollbar::{
    SCROLLBAR_INSET, SCROLLBAR_MIN_THUMB, SCROLLBAR_WIDTH, scrollbar_drag_step,
    scrollbar_drag_target, scrollbar_geometry, scrollbar_releases_drag,
};
use super::*;
use crate::widget::term::backend::{Command, LinkAction, MouseButton, RenderableContent};
use crate::widget::term::bindings::BindingsLayout;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::{TermMode, cell};
use iced::mouse::ScrollDelta;
use iced::{Point, Rectangle};
use iced_core::clipboard::Kind as ClipboardKind;
use iced_core::input_method;

const TEST_PADDING: f32 = 4.0;

mod handle_left_button_pressed_tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line};

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
}

mod handle_cursor_moved_tests {
    use alacritty_terminal::index::{Column, Line};

    use super::*;

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
}

mod selection_autoscroll_delta_tests {
    use super::*;

    #[test]
    fn returns_none_inside_content() {
        assert_eq!(selection_autoscroll_delta(0.0, 50, 1.0), None);
        assert_eq!(selection_autoscroll_delta(49.9, 50, 1.0), None);
    }

    #[test]
    fn scrolls_up_proportional_to_overshoot() {
        assert_eq!(selection_autoscroll_delta(-0.5, 50, 1.0), Some(1));
        assert_eq!(selection_autoscroll_delta(-2.0, 50, 1.0), Some(3));
    }

    #[test]
    fn scrolls_down_proportional_to_overshoot() {
        assert_eq!(selection_autoscroll_delta(50.0, 50, 1.0), Some(-1));
        assert_eq!(selection_autoscroll_delta(53.0, 50, 1.0), Some(-4));
    }

    #[test]
    fn caps_overshoot_at_max_lines() {
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
    fn returns_none_for_degenerate_geometry() {
        assert_eq!(selection_autoscroll_delta(-5.0, 0, 1.0), None);
        assert_eq!(selection_autoscroll_delta(-5.0, 50, 0.0), None);
    }
}

mod handle_button_released_tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line};

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
}

mod middle_paste_tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line};

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
    fn middle_paste_prefers_primary_over_standard() {
        let clipboard = StubClipboard {
            primary: Some("primary".into()),
            standard: Some("standard".into()),
        };

        assert_eq!(read_middle_paste(&clipboard).as_deref(), Some("primary"));
    }

    #[test]
    fn middle_paste_falls_back_to_standard_when_primary_empty_or_unavailable() {
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
    fn middle_paste_yields_nothing_when_both_empty() {
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
        let _modifiers = Modifiers::empty();

        TerminalView::handle_middle_button_pressed(
            &state,
            &TermMode::MOUSE_MODE,
            &clipboard,
            &mut commands,
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
    }

    #[test]
    fn middle_press_pastes_outside_mouse_mode() {
        let state = TerminalViewState::new();
        let clipboard = StubClipboard {
            primary: Some("primary".into()),
            standard: None,
        };
        let mut commands = Vec::new();

        TerminalView::handle_middle_button_pressed(
            &state,
            &TermMode::empty(),
            &clipboard,
            &mut commands,
        );

        assert_eq!(commands.len(), 1);
        assert!(matches!(
            &commands[0],
            Command::Write(bytes) if bytes == b"primary"
        ));
    }

    #[test]
    fn middle_press_wraps_paste_when_bracketed() {
        let state = TerminalViewState::new();
        let clipboard = StubClipboard {
            primary: Some("primary".into()),
            standard: None,
        };
        let mut commands = Vec::new();

        TerminalView::handle_middle_button_pressed(
            &state,
            &TermMode::BRACKETED_PASTE,
            &clipboard,
            &mut commands,
        );

        assert_eq!(commands.len(), 1);
        assert!(matches!(
            &commands[0],
            Command::Write(bytes) if bytes == b"\x1b[200~primary\x1b[201~"
        ));
    }

    #[test]
    fn middle_press_with_empty_clipboard_writes_nothing() {
        let state = TerminalViewState::new();
        let mut commands = Vec::new();

        TerminalView::handle_middle_button_pressed(
            &state,
            &TermMode::empty(),
            &StubClipboard::default(),
            &mut commands,
        );

        assert!(commands.is_empty());
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
}

mod handle_wheel_scrolled_tests {
    use super::*;
    use crate::widget::term::font::TermFont;
    use crate::widget::term::settings::FontSettings;

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
}

mod handle_input_method_event_tests {
    use super::*;

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

        let command =
            TerminalView::handle_input_method_event(&mut state, &input_method::Event::Closed);

        assert!(command.is_none());
        assert_eq!(state.preedit, None);
    }
}

mod caret_rect_tests {
    use alacritty_terminal::index::{Column, Line};

    use super::*;

    /// 构造光标位于指定网格点的渲染内容；默认单元格为 1×1 像素，故断言可直接按格数书写。
    fn content_at(line: i32, column: usize) -> RenderableContent {
        RenderableContent {
            cursor_point: TerminalGridPoint {
                line: Line(line),
                column: Column(column),
            },
            ..Default::default()
        }
    }

    #[test]
    fn anchors_to_cursor_cell() {
        let bounds = Rectangle::new(Point::new(100.0, 50.0), Size::new(400.0, 300.0));

        let rect = TerminalView::caret_rect(bounds, TEST_PADDING, &content_at(2, 3));

        assert_eq!(rect.x, 100.0 + TEST_PADDING + 3.0);
        assert_eq!(rect.y, 50.0 + TEST_PADDING + 2.0);
    }

    #[test]
    fn clamps_into_content_area_when_cursor_is_out_of_view() {
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(200.0, 100.0));

        // 光标滚出可视区（回滚历史）时把锚点收回内容区底边，避免候选框落到窗口外。
        let rect = TerminalView::caret_rect(bounds, TEST_PADDING, &content_at(1000, 0));

        assert_eq!(rect.y, 100.0 - TEST_PADDING - 1.0);
    }
}

mod scrollbar_tests {
    use super::*;

    /// 测试用快照：`rows` 行视口、`columns` 列、`history` 行历史。
    fn content_with(
        rows: usize,
        columns: usize,
        history: usize,
        display_offset: usize,
    ) -> RenderableContent {
        RenderableContent {
            cells: vec![cell::Cell::default(); rows * columns],
            columns,
            display_offset,
            history_size: history,
            ..Default::default()
        }
    }

    /// 轨道贴住内容右缘、上下各内缩 `padding + SCROLLBAR_INSET`。
    #[test]
    fn track_hugs_the_right_content_edge() {
        let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));
        let (track, _thumb) =
            scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 50, 0)).unwrap();

        assert_eq!(track.x, 10.0 + 400.0 - TEST_PADDING - SCROLLBAR_WIDTH);
        assert_eq!(track.y, 20.0 + TEST_PADDING + SCROLLBAR_INSET);
        assert_eq!(track.width, SCROLLBAR_WIDTH);
        assert_eq!(track.height, 300.0 - (TEST_PADDING + SCROLLBAR_INSET) * 2.0);
    }

    /// 贴住实时输出（偏移 0）时滑块在轨道最下；滚到历史顶端时在最上。
    #[test]
    fn thumb_travels_between_track_ends() {
        let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));

        let (_track, bottom) =
            scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 50, 0)).unwrap();
        assert_eq!(bottom.y + bottom.height, 26.0 + 288.0);

        let (_track, top) =
            scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 50, 50)).unwrap();
        assert_eq!(top.y, 26.0);
    }

    /// 历史越长滑块越短，但不低于 [`SCROLLBAR_MIN_THUMB`]。
    #[test]
    fn thumb_shrinks_with_history_but_keeps_a_grabable_minimum() {
        let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));

        // 视口 50 行、历史 50 行 ⇒ 占总行数一半。
        let (_track, half) =
            scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 50, 0)).unwrap();
        assert_eq!(half.height, 288.0 / 2.0);

        // 历史极长时钳到最小高度，否则滑块细到抓不住。
        let (_track, tiny) =
            scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 100_000, 0)).unwrap();
        assert_eq!(tiny.height, SCROLLBAR_MIN_THUMB);
    }

    /// 无历史 / 备用屏 / 视口为空时不画滚动条。
    #[test]
    fn no_geometry_without_scrollback_or_on_the_alternate_screen() {
        let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));

        assert!(scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 0, 0)).is_none());
        assert!(scrollbar_geometry(bounds, TEST_PADDING, &content_with(0, 80, 50, 0)).is_none());

        let alt_screen = RenderableContent {
            terminal_mode: TermMode::ALT_SCREEN,
            ..content_with(50, 80, 50, 0)
        };
        assert!(scrollbar_geometry(bounds, TEST_PADDING, &alt_screen).is_none());
    }

    /// 默认终端模式（含 [`TermMode::ALTERNATE_SCROLL`]）仍须显示滚动条。
    ///
    /// 该位是 alacritty 默认就置位的修饰位而非「备用屏」信号，曾据它排除致普通会话无滚动条。
    #[test]
    fn default_term_mode_still_shows_scrollbar() {
        let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));
        assert!(TermMode::default().contains(TermMode::ALTERNATE_SCROLL));

        let content = RenderableContent {
            terminal_mode: TermMode::default(),
            ..content_with(50, 80, 50, 0)
        };
        assert!(scrollbar_geometry(bounds, TEST_PADDING, &content).is_some());
    }

    /// 拖动映射：抓取点跟手（端点与中点），越界钳到两端。
    #[test]
    fn drag_target_maps_cursor_to_scroll_offset() {
        let track = Rectangle::new(Point::new(400.0, 26.0), Size::new(6.0, 288.0));
        let thumb_height = 144.0;
        let span = 288.0 - thumb_height;

        // 抓在滑块顶部拖到轨道顶 ⇒ 完全回滚（偏移 = 历史长度）。
        assert_eq!(
            scrollbar_drag_target(track, thumb_height, 50, track.y, 0.0),
            50
        );
        // 拖到轨道底 ⇒ 贴住实时输出。
        assert_eq!(
            scrollbar_drag_target(track, thumb_height, 50, track.y + span, 0.0),
            0
        );
        // 中点 ⇒ 一半。
        assert_eq!(
            scrollbar_drag_target(track, thumb_height, 50, track.y + span / 2.0, 0.0),
            25
        );
        // 抓取偏移抵消光标位移：光标相对滑块顶部的落点未变则偏移不变。
        assert_eq!(
            scrollbar_drag_target(track, thumb_height, 50, track.y + 72.0, 72.0),
            50
        );
        // 拖出轨道两端钳到极限。
        assert_eq!(
            scrollbar_drag_target(track, thumb_height, 50, track.y - 500.0, 0.0),
            50
        );
        assert_eq!(
            scrollbar_drag_target(track, thumb_height, 50, track.y + 5000.0, 0.0),
            0
        );
    }

    /// 一步增量以「上次已下达的偏移」为基准，故一批多次移动的增量之和恰为最终目标。
    ///
    /// 若改用晚一轮才更新的后端 `display_offset` 作基准，同批增量叠加会冲过目标再回弹——即抖动成因。
    #[test]
    fn drag_step_telescopes_across_a_batch() {
        let seed = 12usize;
        let mut tracked = seed;
        let mut applied = 0i32;

        for target in [30usize, 8, 45, 45] {
            let (delta, next) = scrollbar_drag_step(target, tracked);
            applied += delta;
            tracked = next;
        }

        // 总和即最终目标相对种子的位移，中途的来回都被抵消。
        assert_eq!(applied, 45 - seed as i32);
        assert_eq!(tracked, 45);
        // 目标与基准相同则不发命令。
        assert_eq!(scrollbar_drag_step(45, 45), (0, 45));
    }

    /// 仅「拖拽中 + 左键释放」解除锁存；未拖拽的释放留给选区 / 鼠标上报。
    #[test]
    fn only_left_release_while_dragging_ends_the_drag() {
        let release = iced_core::mouse::Event::ButtonReleased(iced_core::mouse::Button::Left);
        assert!(scrollbar_releases_drag(&release, true));
        assert!(!scrollbar_releases_drag(&release, false));

        let moved = iced_core::mouse::Event::CursorMoved {
            position: Point::ORIGIN,
        };
        assert!(!scrollbar_releases_drag(&moved, true));
    }

    /// 离窗 / 失焦收尾：清空拖拽与悬停锁存并报告需要重绘；全空时不动重绘。
    #[test]
    fn ending_pointer_interactions_clears_latches() {
        let mut state = TerminalViewState {
            scrollbar_drag: Some(3.0),
            is_dragged: true,
            scrollbar_hovered: true,
            ..TerminalViewState::new()
        };
        assert!(state.end_pointer_interactions());
        assert!(state.scrollbar_drag.is_none());
        assert!(!state.is_dragged);
        assert!(!state.scrollbar_hovered);

        // 已无任何锁存 / 悬停：不要求重绘。
        assert!(!state.end_pointer_interactions());
    }
}
