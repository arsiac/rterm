//! 搜索快捷键拦截。

use crate::widget::term::view::input::is_search_shortcut;
use iced_core::keyboard::{Key, Modifiers, key::Named};

#[test]
fn search_shortcut_matches_ctrl_shift_f_only() {
    let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
    // 小写与大写都命中（Shift 使物理键上报 'F'）。
    assert!(is_search_shortcut(&Key::Character("f".into()), ctrl_shift));
    assert!(is_search_shortcut(&Key::Character("F".into()), ctrl_shift));
    // 裸 Ctrl+F 不拦（仍透传 \x06）。
    assert!(!is_search_shortcut(
        &Key::Character("f".into()),
        Modifiers::CTRL
    ));
    // 叠加 Alt / Logo 不拦（让位桌面组合键；`COMMAND` 在非 macOS 上就是 `CONTROL` 位）。
    assert!(!is_search_shortcut(
        &Key::Character("f".into()),
        ctrl_shift | Modifiers::ALT
    ));
    assert!(!is_search_shortcut(
        &Key::Character("f".into()),
        ctrl_shift | Modifiers::LOGO
    ));
    // 其他字符 / 命名键不误伤。
    assert!(!is_search_shortcut(&Key::Character("g".into()), ctrl_shift));
    assert!(!is_search_shortcut(&Key::Named(Named::Enter), ctrl_shift));
}
