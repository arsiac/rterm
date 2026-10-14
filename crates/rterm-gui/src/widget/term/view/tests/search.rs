//! 终端保留键拦截：默认键位表下只有搜索和弦被客户端拦下。

use crate::app::shortcuts::Command;
use crate::keymap::{chord_from_key, is_terminal_reserved};
use iced_core::keyboard::{Key, Modifiers, key::Named};
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
