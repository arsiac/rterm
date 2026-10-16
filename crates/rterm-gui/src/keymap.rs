//! iced 键盘事件与自有和弦（[`Chord`]）之间的转换与判据。
//!
//! 全局按键订阅与终端部件共用：把 iced 的 `Key` + `Modifiers` 归一为 [`Chord`]，
//! 与 `keybindings.toml` 的键位表同一套表示，使「按用户配置匹配」得以在 `update` 里查表完成。

use crate::app::shortcuts::Command;
use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};
use rterm_config::keybindings::{Chord, ChordKey, Keymap};

/// 把 iced 的按键与修饰键转成和弦；无法表示（纯修饰键、媒体键等）返回 `None`。
pub(crate) fn chord_from_key(key: &Key, modifiers: Modifiers) -> Option<Chord> {
    Some(Chord {
        ctrl: modifiers.control(),
        shift: modifiers.shift(),
        alt: modifiers.alt(),
        logo: modifiers.logo(),
        key: chord_key(key)?,
    })
}

/// 该按键是否值得转成和弦交上层查表：带任意修饰键，或为具名键。
///
/// 纯字符键（无修饰）是普通打字，不生成消息，避免按键洪泛。
pub(crate) fn is_shortcut_candidate(key: &Key, modifiers: Modifiers) -> bool {
    !modifiers.is_empty() || matches!(key, Key::Named(_))
}

/// 该和弦是否为终端聚焦时需由客户端拦下的保留键（不写入远端）。
///
/// 判据为「和弦绑定的命令属于终端保留集」，见 [`Command::terminal_intercepts`]。
pub(crate) fn is_terminal_reserved(chord: &Chord, keymap: &Keymap) -> bool {
    keymap
        .command_for(chord)
        .and_then(Command::from_id)
        .is_some_and(Command::terminal_intercepts)
}

/// 把 iced 的单个按键转成 [`ChordKey`]；不在可绑定范围内的键返回 `None`。
fn chord_key(key: &Key) -> Option<ChordKey> {
    Some(match key {
        // 字母统一大写存储；Shift 使 iced 上报大写字符，大小写两种上报归一到同一和弦。
        Key::Character(s) => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            ChordKey::Char(c.to_ascii_uppercase())
        }
        Key::Named(named) => match named {
            Named::Enter => ChordKey::Enter,
            Named::Escape => ChordKey::Escape,
            Named::Tab => ChordKey::Tab,
            Named::Space => ChordKey::Space,
            Named::Backspace => ChordKey::Backspace,
            Named::Delete => ChordKey::Delete,
            Named::Insert => ChordKey::Insert,
            Named::Home => ChordKey::Home,
            Named::End => ChordKey::End,
            Named::PageUp => ChordKey::PageUp,
            Named::PageDown => ChordKey::PageDown,
            Named::ArrowUp => ChordKey::ArrowUp,
            Named::ArrowDown => ChordKey::ArrowDown,
            Named::ArrowLeft => ChordKey::ArrowLeft,
            Named::ArrowRight => ChordKey::ArrowRight,
            Named::F1 => ChordKey::F(1),
            Named::F2 => ChordKey::F(2),
            Named::F3 => ChordKey::F(3),
            Named::F4 => ChordKey::F(4),
            Named::F5 => ChordKey::F(5),
            Named::F6 => ChordKey::F(6),
            Named::F7 => ChordKey::F(7),
            Named::F8 => ChordKey::F(8),
            Named::F9 => ChordKey::F(9),
            Named::F10 => ChordKey::F(10),
            Named::F11 => ChordKey::F(11),
            Named::F12 => ChordKey::F(12),
            Named::F13 => ChordKey::F(13),
            Named::F14 => ChordKey::F(14),
            Named::F15 => ChordKey::F(15),
            Named::F16 => ChordKey::F(16),
            Named::F17 => ChordKey::F(17),
            Named::F18 => ChordKey::F(18),
            Named::F19 => ChordKey::F(19),
            Named::F20 => ChordKey::F(20),
            Named::F21 => ChordKey::F(21),
            Named::F22 => ChordKey::F(22),
            Named::F23 => ChordKey::F(23),
            Named::F24 => ChordKey::F(24),
            _ => return None,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// 默认键位表：终端搜索绑 `Ctrl+Shift+F`、常用命令弹窗绑 `Ctrl+Shift+P`，SFTP 重命名绑 `F2`。
    fn keymap() -> Keymap {
        Keymap::build(
            &[
                (
                    "open_search",
                    Some(Chord {
                        ctrl: true,
                        shift: true,
                        alt: false,
                        logo: false,
                        key: ChordKey::Char('F'),
                    }),
                ),
                (
                    "open_snippets",
                    Some(Chord {
                        ctrl: true,
                        shift: true,
                        alt: false,
                        logo: false,
                        key: ChordKey::Char('P'),
                    }),
                ),
                (
                    "sftp_rename",
                    Some(Chord {
                        ctrl: false,
                        shift: false,
                        alt: false,
                        logo: false,
                        key: ChordKey::F(2),
                    }),
                ),
            ],
            &BTreeMap::new(),
        )
    }

    #[test]
    fn chord_from_key_normalizes_letters_and_modifiers() {
        let chord = chord_from_key(
            &Key::Character("f".into()),
            Modifiers::CTRL | Modifiers::SHIFT,
        )
        .expect("字符键应可转换");
        assert_eq!(
            chord,
            Chord {
                ctrl: true,
                shift: true,
                alt: false,
                logo: false,
                key: ChordKey::Char('F'),
            }
        );
        // Shift 使 iced 上报大写，仍归一到同一和弦。
        assert_eq!(
            chord_from_key(
                &Key::Character("F".into()),
                Modifiers::CTRL | Modifiers::SHIFT
            ),
            Some(chord)
        );
        // 具名键按名转换。
        assert_eq!(
            chord_from_key(&Key::Named(Named::F2), Modifiers::empty()),
            Some(Chord {
                ctrl: false,
                shift: false,
                alt: false,
                logo: false,
                key: ChordKey::F(2),
            })
        );
        // 无法表示的键（如多媒体键）不产生和弦。
        assert_eq!(
            chord_from_key(&Key::Named(Named::MediaPlayPause), Modifiers::empty()),
            None
        );
    }

    #[test]
    fn candidate_filter_excludes_plain_typing() {
        // 纯字符键是打字，不预筛。
        assert!(!is_shortcut_candidate(
            &Key::Character("a".into()),
            Modifiers::empty()
        ));
        // 带修饰键的字符键是候选。
        assert!(is_shortcut_candidate(
            &Key::Character("a".into()),
            Modifiers::CTRL
        ));
        // 具名键无修饰也是候选（Esc / Enter / 方向键等）。
        assert!(is_shortcut_candidate(
            &Key::Named(Named::Escape),
            Modifiers::empty()
        ));
    }

    #[test]
    fn terminal_reserves_intercepted_chords_only() {
        let keymap = keymap();
        let search = Chord {
            ctrl: true,
            shift: true,
            alt: false,
            logo: false,
            key: ChordKey::Char('F'),
        };
        assert!(is_terminal_reserved(&search, &keymap));
        // 常用命令弹窗的 Ctrl+Shift+P 同属终端保留集（不拦会当 `\x10` 写进远端）。
        assert!(is_terminal_reserved(
            &Chord {
                key: ChordKey::Char('P'),
                ..search
            },
            &keymap
        ));
        // 裸 Ctrl+F 未绑定，不拦（仍透传 `\x06`）。
        assert!(!is_terminal_reserved(
            &Chord {
                shift: false,
                ..search
            },
            &keymap
        ));
        // 叠加 Alt / Logo 不命中，不拦（让位桌面组合键）。
        assert!(!is_terminal_reserved(
            &Chord {
                alt: true,
                ..search
            },
            &keymap
        ));
        assert!(!is_terminal_reserved(
            &Chord {
                logo: true,
                ..search
            },
            &keymap
        ));
        // 其它字符键不误伤。
        assert!(!is_terminal_reserved(
            &Chord {
                key: ChordKey::Char('G'),
                ..search
            },
            &keymap
        ));
        // F2 绑到 SFTP 重命名，但不属终端保留集：仍透传给远端。
        assert!(!is_terminal_reserved(
            &Chord {
                ctrl: false,
                shift: false,
                alt: false,
                logo: false,
                key: ChordKey::F(2),
            },
            &keymap
        ));
        // 具名键不误伤。
        assert!(!is_terminal_reserved(
            &Chord {
                ctrl: false,
                shift: false,
                alt: false,
                logo: false,
                key: ChordKey::Enter,
            },
            &keymap
        ));
    }
}
