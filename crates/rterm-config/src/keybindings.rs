//! 键位绑定的数据模型与持久化。
//!
//! [`Chord`] 描述「修饰键 + 一个按键」的单和弦；[`Keybindings`] 负责 `keybindings.toml`
//! 的读写（仅保存与默认不同的稀疏覆盖）；[`Keymap`] 把默认表与覆盖合并为「命令 → 和弦」
//! 的查询表。本模块不依赖 GUI 与 iced：命令仅以字符串 id 表示，默认表由调用方提供。

use crate::ConfigError;
use log::{debug, warn};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// 和弦中的一个按键（自有枚举，不依赖 iced）。
///
/// 覆盖可绑定的具名键与单字符键；字母统一以大写存储，使 `Display` / 解析互为逆运算。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChordKey {
    /// 单字符键（字母、数字、符号）；字母已规范为大写。
    Char(char),
    /// 功能键 `F1`..=`F24`。
    F(u8),
    /// 回车 / 换行键。
    Enter,
    /// Esc 键。
    Escape,
    /// Tab 键。
    Tab,
    /// 空格键。
    Space,
    /// 退格键。
    Backspace,
    /// Delete 键。
    Delete,
    /// Insert 键。
    Insert,
    /// Home 键。
    Home,
    /// End 键。
    End,
    /// Page Up 键。
    PageUp,
    /// Page Down 键。
    PageDown,
    /// 上方向键。
    ArrowUp,
    /// 下方向键。
    ArrowDown,
    /// 左方向键。
    ArrowLeft,
    /// 右方向键。
    ArrowRight,
}

impl fmt::Display for ChordKey {
    /// 输出可被 [`Chord::from_str`] 读回的规范键名（字母大写，方向键用 `Up`/`Down`/…）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChordKey::Char(c) => write!(f, "{c}"),
            ChordKey::F(n) => write!(f, "F{n}"),
            ChordKey::Enter => f.write_str("Enter"),
            ChordKey::Escape => f.write_str("Esc"),
            ChordKey::Tab => f.write_str("Tab"),
            ChordKey::Space => f.write_str("Space"),
            ChordKey::Backspace => f.write_str("Backspace"),
            ChordKey::Delete => f.write_str("Delete"),
            ChordKey::Insert => f.write_str("Insert"),
            ChordKey::Home => f.write_str("Home"),
            ChordKey::End => f.write_str("End"),
            ChordKey::PageUp => f.write_str("PageUp"),
            ChordKey::PageDown => f.write_str("PageDown"),
            ChordKey::ArrowUp => f.write_str("Up"),
            ChordKey::ArrowDown => f.write_str("Down"),
            ChordKey::ArrowLeft => f.write_str("Left"),
            ChordKey::ArrowRight => f.write_str("Right"),
        }
    }
}

/// 单个快捷键和弦：四个逻辑修饰键 + 一个按键。
///
/// 修饰键为**逻辑**语义（`logo` 在 macOS 显示为 `Cmd`、其它平台为 `Super`），
/// 使同一份 `keybindings.toml` 可在各平台复用。`Display` / [`Chord::from_str`] 互为逆运算。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Chord {
    /// 是否按下 Ctrl。
    pub ctrl: bool,
    /// 是否按下 Shift。
    pub shift: bool,
    /// 是否按下 Alt。
    pub alt: bool,
    /// 是否按下 Logo / Super / Cmd 键。
    pub logo: bool,
    /// 主按键。
    pub key: ChordKey,
}

impl fmt::Display for Chord {
    /// 输出规范文本，修饰键顺序固定为 `Ctrl+Shift+Alt+Super`。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.logo {
            f.write_str("Super+")?;
        }
        write!(f, "{}", self.key)
    }
}

impl FromStr for Chord {
    type Err = String;

    /// 解析形如 `Ctrl+Shift+F` / `F2` / `Alt+Up` 的文本。
    ///
    /// 修饰键名大小写不敏感，并接受常见别名（`Control`/`Option`/`Cmd`/`Win` 等）；
    /// 空串不是合法和弦（解绑由 [`Keybindings`] 的空串覆盖表达）。
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err("empty string is not a valid chord".into());
        }
        let (mut ctrl, mut shift, mut alt, mut logo) = (false, false, false, false);
        let mut key: Option<ChordKey> = None;
        for part in trimmed.split('+') {
            let token = part.trim();
            if token.is_empty() {
                return Err(format!("chord `{s}` contains an empty segment"));
            }
            match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "shift" => shift = true,
                "alt" | "option" | "opt" => alt = true,
                "super" | "cmd" | "command" | "meta" | "win" | "logo" => logo = true,
                _ => {
                    if key.is_some() {
                        return Err(format!(
                            "chord `{s}` must contain exactly one non-modifier key"
                        ));
                    }
                    key = Some(parse_key(token)?);
                }
            }
        }
        let key = key.ok_or_else(|| format!("chord `{s}` is missing a key"))?;
        Ok(Chord {
            ctrl,
            shift,
            alt,
            logo,
            key,
        })
    }
}

/// 解析单个按键名（`F2` / `Enter` / `Esc` / 单字符等）。
fn parse_key(token: &str) -> Result<ChordKey, String> {
    let lower = token.to_ascii_lowercase();
    if let Some(num) = lower.strip_prefix('f')
        && let Ok(n) = num.parse::<u8>()
        && (1..=24).contains(&n)
    {
        return Ok(ChordKey::F(n));
    }
    let named = match lower.as_str() {
        "enter" | "return" => ChordKey::Enter,
        "escape" | "esc" => ChordKey::Escape,
        "tab" => ChordKey::Tab,
        "space" => ChordKey::Space,
        "backspace" | "bs" => ChordKey::Backspace,
        "delete" | "del" => ChordKey::Delete,
        "insert" | "ins" => ChordKey::Insert,
        "home" => ChordKey::Home,
        "end" => ChordKey::End,
        "pageup" | "pgup" => ChordKey::PageUp,
        "pagedown" | "pgdn" => ChordKey::PageDown,
        "up" | "arrowup" => ChordKey::ArrowUp,
        "down" | "arrowdown" => ChordKey::ArrowDown,
        "left" | "arrowleft" => ChordKey::ArrowLeft,
        "right" | "arrowright" => ChordKey::ArrowRight,
        _ => {
            let mut chars = token.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) => Ok(ChordKey::Char(c.to_ascii_uppercase())),
                _ => Err(format!("unrecognized key `{token}`")),
            };
        }
    };
    Ok(named)
}

/// 用户键位覆盖的持久化存储（配置目录下的 `keybindings.toml`）。
///
/// 文件仅保存与默认不同的**稀疏覆盖**（`命令 id = "和弦文本"`）：空串表示显式解绑，
/// 未出现的命令沿用默认。默认值随版本演进，故「恢复默认」只需删除条目。
#[derive(Clone)]
pub struct Keybindings {
    /// 覆盖文件在磁盘上的绝对路径（`keybindings.toml`）。
    path: PathBuf,
    /// 内存中的覆盖表，键为命令 id、值为和弦文本（空串表示解绑）。
    overrides: BTreeMap<String, String>,
}

impl Keybindings {
    /// 创建存储实例并准备好配置目录，覆盖表初始为空（需调用 [`Keybindings::load`] 填充）。
    ///
    /// 目录由 [`crate::paths::config_dir`] 解析（开发沙箱下为 `.dev/config`）。
    ///
    /// # 错误
    /// 无法定位配置目录或创建目录失败时返回 [`ConfigError::ConfigDir`] / [`ConfigError::Store`]。
    pub fn new() -> Result<Self, ConfigError> {
        let dir = crate::paths::config_dir()
            .ok_or_else(|| ConfigError::ConfigDir("cannot locate XDG config directory".into()))?;
        fs::create_dir_all(&dir)
            .map_err(|e| ConfigError::Store(format!("failed to create config directory: {e}")))?;
        let path = dir.join("keybindings.toml");
        debug!("Keybindings file path: {}", path.display());
        Ok(Self {
            path,
            overrides: BTreeMap::new(),
        })
    }

    /// 返回覆盖文件的绝对路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取覆盖表；文件不存在或为空时视为无覆盖。
    ///
    /// # 错误
    /// 读取或解析失败时返回 [`ConfigError::Store`] / [`ConfigError::TomlDe`]。
    pub fn load(&mut self) -> Result<(), ConfigError> {
        if !self.path.exists() {
            self.overrides = BTreeMap::new();
            return Ok(());
        }
        let content = fs::read_to_string(&self.path)
            .map_err(|e| ConfigError::Store(format!("failed to read keybindings file: {e}")))?;
        if content.trim().is_empty() {
            self.overrides = BTreeMap::new();
            return Ok(());
        }
        self.overrides = toml::from_str(&content)?;
        debug!("Loaded {} key override(s)", self.overrides.len());
        Ok(())
    }

    /// 将当前覆盖表写入 `keybindings.toml`。
    ///
    /// # 错误
    /// 序列化或写入失败时返回 [`ConfigError::Store`]。
    pub fn save(&self) -> Result<(), ConfigError> {
        let content = toml::to_string_pretty(&self.overrides).map_err(|e| {
            ConfigError::Store(format!("failed to serialize keybindings file: {e}"))
        })?;
        fs::write(&self.path, content)
            .map_err(|e| ConfigError::Store(format!("failed to write keybindings file: {e}")))?;
        debug!("Saved {} key override(s)", self.overrides.len());
        Ok(())
    }

    /// 当前覆盖表（命令 id → 和弦文本）。
    pub fn overrides(&self) -> &BTreeMap<String, String> {
        &self.overrides
    }

    /// 设置某命令的覆盖值；`chord` 为空串表示显式解绑。
    pub fn set(&mut self, command: &str, chord: &str) {
        self.overrides
            .insert(command.to_string(), chord.to_string());
    }

    /// 清除某命令的覆盖，回落到默认绑定。
    pub fn reset(&mut self, command: &str) {
        self.overrides.remove(command);
    }

    /// 清除全部覆盖（恢复默认绑定）。
    pub fn reset_all(&mut self) {
        self.overrides.clear();
    }
}

/// 解析后的键位表：命令 id → 和弦，由默认表叠加用户覆盖得到。
///
/// 构建期解析覆盖文本并丢弃非法项（记日志、保留默认），同时建立和弦 → 命令的反向索引
/// 供按键匹配使用；同一和弦被多个命令占用时后者覆盖前者（冲突检测由 GUI 层负责）。
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    /// 命令 id → 和弦。
    chords: BTreeMap<String, Chord>,
    /// 和弦 → 命令 id 的反向索引。
    by_chord: BTreeMap<Chord, String>,
}

impl Keymap {
    /// 构建键位表。
    ///
    /// `defaults` 为全部**已知命令**的默认绑定：`None` 表示该命令默认无绑定（仍可被覆盖
    /// 绑定）；`overrides` 中：空串解绑，合法和弦文本覆盖默认，非法文本或未知命令 id 被
    /// 丢弃并记日志。
    pub fn build(defaults: &[(&str, Option<Chord>)], overrides: &BTreeMap<String, String>) -> Self {
        let known: HashSet<&str> = defaults.iter().map(|(id, _)| *id).collect();
        let mut chords: BTreeMap<String, Chord> = BTreeMap::new();
        for (id, chord) in defaults {
            if let Some(chord) = chord {
                chords.insert((*id).to_string(), *chord);
            }
        }
        for (id, text) in overrides {
            if !known.contains(id.as_str()) {
                warn!("keybindings: ignoring override for unknown command `{id}`");
                continue;
            }
            if text.trim().is_empty() {
                chords.remove(id);
                continue;
            }
            match text.parse::<Chord>() {
                Ok(chord) => {
                    chords.insert(id.clone(), chord);
                }
                Err(e) => warn!(
                    "keybindings: invalid chord `{text}` for command `{id}` ({e}); keeping default"
                ),
            }
        }
        let by_chord = chords
            .iter()
            .map(|(id, chord)| (*chord, id.clone()))
            .collect();
        debug!("Keymap built with {} binding(s)", chords.len());
        Self { chords, by_chord }
    }

    /// 查询某命令当前绑定的和弦；无绑定返回 `None`。
    pub fn chord_for(&self, command: &str) -> Option<&Chord> {
        self.chords.get(command)
    }

    /// 查询某和弦绑定的命令；未占用返回 `None`。
    pub fn command_for(&self, chord: &Chord) -> Option<&str> {
        self.by_chord.get(chord).map(String::as_str)
    }

    /// 遍历全部已绑定项（命令 id, 和弦），按命令 id 排序。
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Chord)> {
        self.chords.iter().map(|(id, chord)| (id.as_str(), chord))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 便捷构造：无修饰键的和弦。
    fn plain(key: ChordKey) -> Chord {
        Chord {
            ctrl: false,
            shift: false,
            alt: false,
            logo: false,
            key,
        }
    }

    #[test]
    fn chord_text_roundtrips_and_normalizes() {
        // 解析结果字段正确。
        let chord = "Ctrl+Shift+F".parse::<Chord>().expect("应能解析");
        assert!(chord.ctrl && chord.shift && !chord.alt && !chord.logo);
        assert_eq!(chord.key, ChordKey::Char('F'));

        // Display 与解析互为逆运算。
        for text in [
            "Ctrl+Shift+F",
            "F2",
            "Ctrl+Alt+Delete",
            "Super+C",
            "Esc",
            "Shift+Enter",
            "Alt+Up",
            "Ctrl+Space",
        ] {
            let parsed = text.parse::<Chord>().expect("应能解析");
            assert_eq!(parsed.to_string(), text, "`{text}` 往返应不变");
        }

        // 修饰键别名与大小写：规范化为同一和弦。
        let canonical = "Ctrl+Shift+F".parse::<Chord>().expect("应能解析");
        for alias in ["control+shift+f", "SHIFT+CTRL+f", "ctrl+shift+F"] {
            assert_eq!(
                alias.parse::<Chord>().expect("应能解析"),
                canonical,
                "别名 `{alias}` 应归一"
            );
        }
        // Cmd / Win 等别名统一落到 logo。
        assert_eq!(
            "cmd+c".parse::<Chord>().expect("应能解析"),
            "Super+C".parse::<Chord>().expect("应能解析")
        );

        // 单字符键：字母转大写，数字与符号原样。
        assert_eq!(plain(ChordKey::Char('A')), "a".parse().unwrap());
        assert_eq!(plain(ChordKey::Char('1')), "1".parse().unwrap());
        assert_eq!(plain(ChordKey::Char('#')), "#".parse().unwrap());
    }

    #[test]
    fn invalid_chord_texts_are_rejected() {
        for text in [
            "",
            "   ",
            "Ctrl+",
            "+F",
            "Ctrl+F+G",
            "F25",
            "F0",
            "Ctrl+Foo",
            "ctrl shift f",
        ] {
            assert!(
                text.parse::<Chord>().is_err(),
                "`{text}` 不应被接受为合法和弦"
            );
        }
    }

    #[test]
    fn keymap_applies_sparse_override_and_unbind() {
        let defaults = [
            ("open_search", Some("Ctrl+F".parse::<Chord>().unwrap())),
            ("sftp_rename", Some(plain(ChordKey::F(2)))),
            ("toggle_sidebar", None),
        ];
        let mut overrides = BTreeMap::new();
        overrides.insert("open_search".to_string(), "Ctrl+G".to_string());
        overrides.insert("sftp_rename".to_string(), String::new()); // 显式解绑
        overrides.insert("toggle_sidebar".to_string(), "F5".to_string()); // 默认无绑定 → 绑定
        overrides.insert("nope".to_string(), "F6".to_string()); // 未知命令 → 丢弃

        let keymap = Keymap::build(&defaults, &overrides);

        assert_eq!(
            keymap.chord_for("open_search"),
            Some(&"Ctrl+G".parse::<Chord>().unwrap())
        );
        assert!(keymap.chord_for("sftp_rename").is_none(), "空串应解绑");
        assert_eq!(
            keymap.chord_for("toggle_sidebar"),
            Some(&plain(ChordKey::F(5)))
        );
        assert!(keymap.chord_for("nope").is_none());

        // 反向索引随之更新。
        assert_eq!(
            keymap.command_for(&"Ctrl+G".parse().unwrap()),
            Some("open_search")
        );
        assert!(keymap.command_for(&"Ctrl+F".parse().unwrap()).is_none());
        assert!(keymap.command_for(&plain(ChordKey::F(2))).is_none());
        assert_eq!(
            keymap.command_for(&plain(ChordKey::F(5))),
            Some("toggle_sidebar")
        );
    }

    #[test]
    fn keymap_invalid_override_keeps_default() {
        let defaults = [("open_search", Some("Ctrl+F".parse::<Chord>().unwrap()))];
        let mut overrides = BTreeMap::new();
        overrides.insert("open_search".to_string(), "bogus".to_string());

        let keymap = Keymap::build(&defaults, &overrides);

        assert_eq!(
            keymap.chord_for("open_search"),
            Some(&"Ctrl+F".parse::<Chord>().unwrap()),
            "非法覆盖应被丢弃并保留默认"
        );
    }

    /// 直接以临时路径构造存储，避免动全局状态根（无需持锁）。
    fn store_at(path: PathBuf) -> Keybindings {
        Keybindings {
            path,
            overrides: BTreeMap::new(),
        }
    }

    #[test]
    fn keybindings_load_missing_file_is_empty() {
        let path =
            std::env::temp_dir().join(format!("rterm_kb_missing_{}.toml", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut store = store_at(path.clone());
        store.load().expect("缺失文件应视为无覆盖");
        assert!(store.overrides().is_empty());
    }

    #[test]
    fn keybindings_save_load_roundtrip() {
        let path = std::env::temp_dir().join(format!("rterm_kb_rt_{}.toml", std::process::id()));
        let _ = fs::remove_file(&path);

        let mut store = store_at(path.clone());
        store.set("open_search", "Ctrl+Shift+G");
        store.set("sftp_rename", ""); // 解绑
        store.set("toggle_sidebar", "F5");
        store.reset("toggle_sidebar"); // 再清掉一条覆盖
        store.save().expect("保存应成功");

        let mut loaded = store_at(path.clone());
        loaded.load().expect("加载应成功");
        assert_eq!(
            loaded.overrides().get("open_search"),
            Some(&"Ctrl+Shift+G".to_string())
        );
        assert_eq!(loaded.overrides().get("sftp_rename"), Some(&String::new()));
        assert!(loaded.overrides().get("toggle_sidebar").is_none());

        // 全清后落盘，读回为空。
        loaded.reset_all();
        loaded.save().expect("保存应成功");
        let mut empty = store_at(path.clone());
        empty.load().expect("加载应成功");
        assert!(empty.overrides().is_empty());

        let _ = fs::remove_file(&path);
    }
}
