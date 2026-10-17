//! 拖选释放的自动复制决策：三档策略 → 写入目标与「已复制到 …」提示信号。

use crate::widget::term::backend::Command as BackendCommand;
use crate::widget::term::settings::Settings;
use crate::widget::term::terminal::{Command, Terminal};
use crate::widget::term::view::TerminalView;
use crate::widget::term::view::input::auto_copy_target;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::vte::ansi::Handler;
use iced_core::clipboard::Kind as ClipboardKind;
use rterm_config::CopyOnSelect;

/// 决策映射表：档位 + 选区 → (写入目标, 文本)。
#[test]
fn auto_copy_target_maps_each_strategy() {
    assert_eq!(
        auto_copy_target(CopyOnSelect::Primary, Some("sel")),
        Some((ClipboardKind::Primary, "sel"))
    );
    // clipboard 档写标准剪贴板：宿主按写入目标分流提示文案。
    assert_eq!(
        auto_copy_target(CopyOnSelect::Clipboard, Some("sel")),
        Some((ClipboardKind::Standard, "sel"))
    );
    // off 档不写、不发提示。
    assert_eq!(auto_copy_target(CopyOnSelect::Off, Some("sel")), None);
}

/// 选区为空 / 缺失时三档一律不写（无内容可复制）。
#[test]
fn auto_copy_target_skips_empty_or_missing_selection() {
    assert_eq!(auto_copy_target(CopyOnSelect::Primary, Some("")), None);
    assert_eq!(auto_copy_target(CopyOnSelect::Clipboard, Some("")), None);
    assert_eq!(auto_copy_target(CopyOnSelect::Primary, None), None);
    assert_eq!(auto_copy_target(CopyOnSelect::Clipboard, None), None);
}

/// 离线构造终端并向网格注入一行 `hello`，再在第 0 行 0..3 列拉出选区。
fn terminal_with_selection() -> Terminal {
    let mut terminal = Terminal::new(1, Settings::default()).expect("离线构造终端");
    {
        let mut term = terminal.backend.test_term().lock();
        for c in "hello".chars() {
            term.input(c);
        }
    }
    // 默认 TerminalSize 单元格 1×1 像素：像素坐标即网格坐标。
    terminal.backend.handle(BackendCommand::SelectStart(
        SelectionType::Simple,
        (0.0, 0.0),
    ));
    terminal
        .backend
        .handle(BackendCommand::SelectUpdate((3.0, 0.0)));
    terminal
}

/// 记录写入的剪贴板桩：分槽留存，供三档断言。
#[derive(Default)]
struct RecordingClipboard {
    primary: Option<String>,
    standard: Option<String>,
}

impl iced_graphics::core::Clipboard for RecordingClipboard {
    fn read(&self, _kind: ClipboardKind) -> Option<String> {
        None
    }

    fn write(&mut self, kind: ClipboardKind, contents: String) {
        match kind {
            ClipboardKind::Primary => self.primary = Some(contents),
            ClipboardKind::Standard => self.standard = Some(contents),
        }
    }
}

/// 拖选释放执行路径：primary 写 PRIMARY、clipboard 写标准剪贴板、off 不写；
/// 热应用 `SetCopyOnSelect` 随之切换档位。
#[test]
fn drag_release_auto_copies_per_strategy() {
    for (strategy, primary_expected, standard_expected) in [
        (CopyOnSelect::Primary, Some("hel"), None),
        (CopyOnSelect::Clipboard, None, Some("hel")),
        (CopyOnSelect::Off, None, None),
    ] {
        let mut terminal = terminal_with_selection();
        terminal.handle(Command::SetCopyOnSelect(strategy));
        assert_eq!(terminal.backend.copy_on_select(), strategy);

        let keymap =
            rterm_config::keybindings::Keymap::build(&[], &std::collections::BTreeMap::new());
        let view = TerminalView::show(&terminal, true, &keymap);
        let mut clipboard = RecordingClipboard::default();
        let wrote = view.auto_copy_on_drag_release(&mut clipboard);

        let expected_kind = match strategy {
            CopyOnSelect::Primary => Some(ClipboardKind::Primary),
            CopyOnSelect::Clipboard => Some(ClipboardKind::Standard),
            CopyOnSelect::Off => None,
        };
        assert_eq!(wrote, expected_kind);
        assert_eq!(clipboard.primary.as_deref(), primary_expected);
        assert_eq!(clipboard.standard.as_deref(), standard_expected);
    }
}
