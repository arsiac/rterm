//! 字符键回落的单元测试：Ctrl 控制字符与 Alt 的 ESC 前缀。

use crate::widget::term::view::input::char_key_bytes;
use iced_core::keyboard::Modifiers;

/// 无修饰键：原样采用系统给出的文本，不额外包装。
#[test]
fn plain_char_keeps_text() {
    assert_eq!(
        char_key_bytes(".", Modifiers::empty(), Some(".")),
        Some(b".".to_vec())
    );
}

/// Alt+句点必须带 ESC 前缀：bash 的 M-.（yank-last-arg）靠它识别 Alt 组合。
#[test]
fn alt_period_prefixes_escape() {
    assert_eq!(
        char_key_bytes(".", Modifiers::ALT, Some(".")),
        Some(b"\x1b.".to_vec())
    );
}

/// 部分平台在按住 Alt 时不给 text，此时用物理键字符补 ESC 前缀。
#[test]
fn alt_char_without_text_uses_key() {
    assert_eq!(
        char_key_bytes(".", Modifiers::ALT, None),
        Some(b"\x1b.".to_vec())
    );
}

/// Ctrl+字母且无 text：退回 ASCII 控制字符。
#[test]
fn ctrl_char_without_text_maps_to_control_byte() {
    assert_eq!(char_key_bytes("c", Modifiers::CTRL, None), Some(vec![0x03]));
}

/// Ctrl+Alt 两者叠加：控制字符前再加 ESC 前缀。
#[test]
fn ctrl_alt_without_text_stacks_escape() {
    assert_eq!(
        char_key_bytes("c", Modifiers::CTRL | Modifiers::ALT, None),
        Some(vec![0x1b, 0x03])
    );
}

/// 既无 text 又无法换算（多字符 / 无 Ctrl）：不写任何字节。
#[test]
fn unrepresentable_key_returns_none() {
    assert_eq!(char_key_bytes("xyz", Modifiers::empty(), None), None);
}
