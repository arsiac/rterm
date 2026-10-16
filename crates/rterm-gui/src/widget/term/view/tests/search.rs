//! 终端保留键拦截：默认键位表下只有搜索和弦被客户端拦下。

use crate::app::shortcuts::Command;
use crate::keymap::{chord_from_key, is_terminal_reserved};
use crate::widget::term::settings::Settings;
use crate::widget::term::terminal::{Event, Terminal};
use crate::widget::term::view::input::update;
use crate::widget::term::view::{TerminalView, TerminalViewState};
use iced::{Point, Size};
use iced_core::Shell;
use iced_core::clipboard::Null;
use iced_core::keyboard::{Key, Location, Modifiers, key::Code, key::Named, key::Physical};
use iced_core::layout::{Layout, Node};
use iced_core::mouse::Cursor;
use rterm_config::keybindings::{Chord, Keymap};
use std::collections::BTreeMap;

/// 默认键位表：由各命令的默认和弦构建（不触盘）。
fn default_keymap() -> Keymap {
    let defaults: Vec<(&str, Option<Chord>)> = Command::ALL
        .iter()
        .map(|c| (c.as_str(), c.default_chord()))
        .collect();
    Keymap::build(&defaults, &BTreeMap::new())
}

/// 测试用和弦转换；按键必可表示。
fn chord(key: &Key, modifiers: Modifiers) -> Chord {
    chord_from_key(key, modifiers).expect("测试用键应可转为和弦")
}

#[test]
fn terminal_reserves_only_the_search_binding() {
    let keymap = default_keymap();

    // 默认绑定 Ctrl+Shift+F：终端聚焦时由客户端拦下开搜索条，远端收不到字节。
    assert!(is_terminal_reserved(
        &chord(
            &Key::Character("F".into()),
            Modifiers::CTRL | Modifiers::SHIFT
        ),
        &keymap
    ));
    // Shift 使 iced 上报大写，仍命中同一和弦。
    assert!(is_terminal_reserved(
        &chord(
            &Key::Character("f".into()),
            Modifiers::CTRL | Modifiers::SHIFT
        ),
        &keymap
    ));
    // 裸 Ctrl+F 未绑定，不拦（仍透传 `\x06`，nano / vim 前向搜索不受影响）。
    assert!(!is_terminal_reserved(
        &chord(&Key::Character("f".into()), Modifiers::CTRL),
        &keymap
    ));
    // 叠加 Alt / Logo 不命中，不拦（让位桌面组合键）。
    assert!(!is_terminal_reserved(
        &chord(
            &Key::Character("f".into()),
            Modifiers::CTRL | Modifiers::SHIFT | Modifiers::ALT
        ),
        &keymap
    ));
    // F2 绑到 SFTP 重命名，不属终端保留集：终端聚焦时仍照旧写 `\x1bOQ` 给远端。
    assert!(!is_terminal_reserved(
        &chord(&Key::Named(Named::F2), Modifiers::empty()),
        &keymap
    ));
}

#[test]
fn reserved_chord_is_published_and_captured() {
    let terminal = Terminal::new(1, Settings::default()).expect("离线构造终端");
    let keymap = default_keymap();
    let mut view = TerminalView::show(&terminal, true, &keymap);
    let mut state = TerminalViewState::new();
    let size = Size {
        width: 800.0,
        height: 600.0,
    };
    // 预置与布局一致的视口尺寸，避免 handle_resize 先发一条 BackendCall 干扰断言。
    state.size = size;
    let node = Node::new(size);
    let layout = Layout::new(&node);
    let cursor = Cursor::Available(Point { x: 10.0, y: 10.0 });
    let mut clipboard = Null;

    // 终端保留集里的两个默认和弦各驱动一次：搜索条（F）与常用命令弹窗（P）。
    for c in ["F", "P"] {
        let mut messages = Vec::new();
        let code = match c {
            "F" => Code::KeyF,
            _ => Code::KeyP,
        };
        let event = iced_core::Event::Keyboard(iced_core::keyboard::Event::KeyPressed {
            key: Key::Character(c.into()),
            modified_key: Key::Character(c.into()),
            physical_key: Physical::Code(code),
            location: Location::Standard,
            modifiers: Modifiers::CTRL | Modifiers::SHIFT,
            text: Some(c.into()),
            repeat: false,
        });
        let captured = {
            let mut shell = Shell::new(&mut messages);
            update(
                &mut view,
                &mut state,
                &event,
                layout,
                cursor,
                &mut clipboard,
                &mut shell,
            );
            shell.is_event_captured()
        };

        let expected = chord(
            &Key::Character(c.into()),
            Modifiers::CTRL | Modifiers::SHIFT,
        );
        assert_eq!(messages.len(), 1, "key={c}");
        assert!(
            matches!(&messages[0], Event::ReservedChord { chord } if *chord == expected),
            "key={c}"
        );
        assert!(captured, "key={c}");
    }
}
