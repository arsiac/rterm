//! 命令片段的持久化。
//!
//! 片段是**条数不限**的用户内容清单，落在配置目录下的 `snippets.toml`（经 [`crate::paths`] 解析），
//! 与 `config.toml` / `sessions.toml` 并列；纯文本不含凭据，故不加密、不参与会话导出。

use crate::ConfigError;
use log::debug;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// 片段名称的字符数上限。
pub const MAX_NAME_CHARS: usize = 64;

/// 片段文本的字节数上限（8 KiB）。
pub const MAX_TEXT_BYTES: usize = 8 * 1024;

/// 渲染用显示名的字符数上限。
pub const MAX_DISPLAY_CHARS: usize = 40;

/// 单条命令片段。
///
/// `name` / `text` 缺键取空串、`submit` 缺键取 `true`（发送后回车），手工写的文件只需写出关心的字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    /// 片段名称，非空且不超过 [`MAX_NAME_CHARS`] 个字符。
    #[serde(default)]
    pub name: String,
    /// 片段文本（发送到终端的原始内容），不空白且不超过 [`MAX_TEXT_BYTES`] 字节。
    #[serde(default)]
    pub text: String,
    /// 发送后是否补一个回车，使命令直接执行。
    #[serde(default = "default_true")]
    pub submit: bool,
}

/// `submit` 缺键时的默认值（发送后回车执行）。
fn default_true() -> bool {
    true
}

/// 空片段：名与文本为空、`submit` 为 `true`，与缺键加载语义一致。
impl Default for Snippet {
    fn default() -> Self {
        Self {
            name: String::new(),
            text: String::new(),
            submit: true,
        }
    }
}

/// 片段校验失败的原因。
///
/// 以枚举而非文案返回，使调用方（GUI）自行映射为本地化提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnippetIssue {
    /// 名称 trim 后为空。
    EmptyName,
    /// 名称超过 [`MAX_NAME_CHARS`] 个字符。
    NameTooLong,
    /// 文本 trim 后为空。
    EmptyText,
    /// 文本超过 [`MAX_TEXT_BYTES`] 字节。
    TextTooLong,
}

impl Snippet {
    /// 校验名称与文本是否可保存；**不限制条数**。
    pub fn validate(&self) -> Result<(), SnippetIssue> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(SnippetIssue::EmptyName);
        }
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(SnippetIssue::NameTooLong);
        }
        if self.text.trim().is_empty() {
            return Err(SnippetIssue::EmptyText);
        }
        if self.text.len() > MAX_TEXT_BYTES {
            return Err(SnippetIssue::TextTooLong);
        }
        Ok(())
    }

    /// 渲染用显示名：名称非空时取名称，否则回落取文本首行；两者都截断到 [`MAX_DISPLAY_CHARS`] 个字符。
    ///
    /// 仅用于列表渲染——手工编辑出的空名条目也能显示，而保存仍要求名称非空（见 [`Snippet::validate`]）。
    pub fn display_name(&self) -> String {
        let name = self.name.trim();
        let source = if name.is_empty() {
            self.text.lines().next().unwrap_or("").trim()
        } else {
            name
        };
        source.chars().take(MAX_DISPLAY_CHARS).collect()
    }
}

/// `snippets.toml` 的文档根结构。
///
/// TOML 不允许文档根直接为数组，故用此表包裹片段列表，序列化为数组表 `[[snippet]]`。
#[derive(Serialize, Deserialize)]
struct SnippetsFile {
    /// 片段清单，按用户排序顺序落盘。
    #[serde(default)]
    snippet: Vec<Snippet>,
}

/// 命令片段的持久化存储（配置目录下的 `snippets.toml`）。
///
/// 只负责文件读写；清单本体由调用方持有，故 [`Snippets::load`] 返回条目而非存入自身。
#[derive(Clone)]
pub struct Snippets {
    /// 片段文件在磁盘上的绝对路径（`snippets.toml`）。
    path: PathBuf,
    /// 上一次 [`Snippets::load`] 是否因读取或解析失败而置位。
    unusable: bool,
}

impl Snippets {
    /// 创建存储实例并准备好配置目录（目录由 [`crate::paths::config_dir`] 解析）。
    ///
    /// 无法定位配置目录或创建目录失败时返回 [`ConfigError::ConfigDir`] / [`ConfigError::Store`]。
    pub fn new() -> Result<Self, ConfigError> {
        let dir = crate::paths::config_dir()
            .ok_or_else(|| ConfigError::ConfigDir("无法定位 XDG 配置目录".into()))?;
        fs::create_dir_all(&dir)
            .map_err(|e| ConfigError::Store(format!("创建配置目录失败: {e}")))?;
        let path = dir.join("snippets.toml");
        debug!("Snippets file path: {}", path.display());
        Ok(Self {
            path,
            unusable: false,
        })
    }

    /// 返回片段文件的绝对路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取片段清单；文件不存在或为空时视为空清单。
    ///
    /// 读取或解析失败（[`ConfigError::Store`] / [`ConfigError::TomlDe`]）时记为不可用并返回错误：
    /// 此时 [`Snippets::save`] 拒绝写入，以免用空清单覆盖掉尚可手工修复的文件。
    pub fn load(&mut self) -> Result<Vec<Snippet>, ConfigError> {
        self.unusable = false;
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path).map_err(|e| {
            self.unusable = true;
            ConfigError::Store(format!("读取片段文件失败: {e}"))
        })?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        match toml::from_str::<SnippetsFile>(&content) {
            Ok(file) => {
                debug!("Loaded {} snippet(s)", file.snippet.len());
                Ok(file.snippet)
            }
            Err(e) => {
                self.unusable = true;
                Err(ConfigError::TomlDe(e))
            }
        }
    }

    /// 全量写入片段清单（覆盖原文件）。
    ///
    /// 文件不可用（读取或解析失败后）、序列化或写入失败时返回 [`ConfigError`]。
    pub fn save(&self, items: &[Snippet]) -> Result<(), ConfigError> {
        if self.unusable {
            return Err(ConfigError::Store(format!(
                "片段文件不可用，拒绝覆盖写入: {}",
                self.path.display()
            )));
        }
        let file = SnippetsFile {
            snippet: items.to_vec(),
        };
        let content = toml::to_string_pretty(&file)
            .map_err(|e| ConfigError::Store(format!("序列化片段文件失败: {e}")))?;
        fs::write(&self.path, content)
            .map_err(|e| ConfigError::Store(format!("写入片段文件失败: {e}")))?;
        debug!("Saved {} snippet(s)", items.len());
        Ok(())
    }
}

#[cfg(test)]
impl Snippets {
    /// 以显式文件路径构造存储（仅测试用）：绕开 [`crate::paths::set_test_root`] 的全局状态根
    /// 改写，与其它改写状态根的测试可并行跑而不相互覆盖临时目录。
    fn at(path: PathBuf) -> Self {
        Self {
            path,
            unusable: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 串行化使用同一临时目录的测试。
    static DIR_LOCK: Mutex<()> = Mutex::new(());

    /// 在临时目录下建一个独立的片段文件，返回存储与其所在目录。
    fn temp_store(tag: &str) -> (Snippets, PathBuf) {
        let dir = std::env::temp_dir().join(format!("rterm_snips_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("应能创建临时目录");
        (Snippets::at(dir.join("snippets.toml")), dir)
    }

    /// 便捷构造一条片段。
    fn snippet(name: &str, text: &str, submit: bool) -> Snippet {
        Snippet {
            name: name.to_string(),
            text: text.to_string(),
            submit,
        }
    }

    #[test]
    fn new_resolves_config_dir_file() {
        let store = Snippets::new().expect("应能创建存储");
        assert_eq!(
            store.path().file_name().and_then(|s| s.to_str()),
            Some("snippets.toml")
        );
    }

    #[test]
    fn roundtrip_preserves_fields_and_order() {
        let _guard = DIR_LOCK.lock().unwrap();
        let (store, dir) = temp_store("roundtrip");
        let items = vec![
            snippet("z-last", "echo z\n", true),
            snippet("a-first", "ls -la", false),
            snippet("m-mid", "git status", true),
        ];
        store.save(&items).expect("应能保存");

        let mut reader = Snippets::at(store.path().to_path_buf());
        let loaded = reader.load().expect("应能加载");
        assert_eq!(loaded, items, "往返应保持各字段与顺序");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_keys_fall_back_to_defaults() {
        let _guard = DIR_LOCK.lock().unwrap();
        let (store, dir) = temp_store("defaults");
        fs::write(
            store.path(),
            "[[snippet]]\nname = \"only name\"\n\n[[snippet]]\ntext = \"whoami\"\n",
        )
        .expect("应能写入片段文件");

        let mut reader = Snippets::at(store.path().to_path_buf());
        let loaded = reader.load().expect("应能加载");
        assert_eq!(loaded.len(), 2);
        // 缺 text 与缺 name 都取空串，缺 submit 取 true。
        assert_eq!(loaded[0].text, "");
        assert!(loaded[0].submit);
        assert_eq!(loaded[1].name, "");
        assert_eq!(loaded[1].text, "whoami");
        assert!(loaded[1].submit, "缺 submit 应默认回车执行");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_loads_empty_and_blank_file_loads_empty() {
        let _guard = DIR_LOCK.lock().unwrap();
        let (store, dir) = temp_store("empty");

        let mut reader = Snippets::at(store.path().to_path_buf());
        assert_eq!(reader.load().expect("缺文件应返回空清单"), Vec::new());

        fs::write(store.path(), "   \n").expect("应能写入空白文件");
        assert_eq!(
            reader.load().expect("空白文件应返回空清单"),
            Vec::new(),
            "空白内容不应报错"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_is_unusable_and_save_is_refused() {
        let _guard = DIR_LOCK.lock().unwrap();
        let (store, dir) = temp_store("corrupt");
        let broken = "[[snippet]\nname = \"没闭合\"\n";
        fs::write(store.path(), broken).expect("应能写入损坏文件");

        let mut reader = Snippets::at(store.path().to_path_buf());
        assert!(reader.load().is_err(), "损坏文件应报错");

        let err = reader.save(&[snippet("new", "echo hi", true)]);
        assert!(err.is_err(), "不可用时必须拒绝写入");
        assert_eq!(
            fs::read_to_string(store.path()).expect("应能读回"),
            broken,
            "拒绝写入后原文件应逐字节不变"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unusable_flag_resets_after_a_successful_load() {
        let _guard = DIR_LOCK.lock().unwrap();
        let (store, dir) = temp_store("recover");
        fs::write(store.path(), "not = [toml").expect("应能写入损坏文件");

        let mut reader = Snippets::at(store.path().to_path_buf());
        assert!(reader.load().is_err());
        assert!(
            reader.save(&[snippet("x", "echo x", true)]).is_err(),
            "读失败后必须拒绝写入，直到文件被修好"
        );

        // 用户手工修好文件后，重新加载应恢复可用并允许写盘。
        fs::write(
            store.path(),
            "[[snippet]]\nname = \"fixed\"\ntext = \"echo ok\"\n",
        )
        .expect("应能写入修复后的文件");
        let items = reader.load().expect("修复后应能加载");
        assert_eq!(items.len(), 1);
        reader.save(&items).expect("恢复后应能保存");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_covers_name_and_text_tiers() {
        // 合法：名称与文本都非空且在限内。
        assert_eq!(
            snippet("valid", "echo hi", true).validate(),
            Ok(()),
            "合法片段应通过"
        );
        // 名称 trim 后为空（含纯空白）拒。
        assert_eq!(
            snippet("   ", "echo hi", true).validate(),
            Err(SnippetIssue::EmptyName)
        );
        assert_eq!(
            snippet("", "echo hi", true).validate(),
            Err(SnippetIssue::EmptyName)
        );
        // 名称按字符计上限。
        let at_limit = "名".repeat(MAX_NAME_CHARS);
        assert_eq!(snippet(&at_limit, "echo hi", true).validate(), Ok(()));
        let over_limit = "名".repeat(MAX_NAME_CHARS + 1);
        assert_eq!(
            snippet(&over_limit, "echo hi", true).validate(),
            Err(SnippetIssue::NameTooLong)
        );
        // 文本 trim 后为空拒。
        assert_eq!(
            snippet("valid", "  \n\t ", true).validate(),
            Err(SnippetIssue::EmptyText)
        );
        // 文本按字节计上限。
        let text_at_limit = "x".repeat(MAX_TEXT_BYTES);
        assert_eq!(snippet("valid", &text_at_limit, true).validate(), Ok(()));
        let text_over_limit = "x".repeat(MAX_TEXT_BYTES + 1);
        assert_eq!(
            snippet("valid", &text_over_limit, true).validate(),
            Err(SnippetIssue::TextTooLong)
        );
    }

    #[test]
    fn no_count_cap_accepts_many_items() {
        let _guard = DIR_LOCK.lock().unwrap();
        let (store, dir) = temp_store("many");
        let items: Vec<Snippet> = (0..101)
            .map(|i| snippet(&format!("snippet-{i}"), &format!("echo {i}"), i % 2 == 0))
            .collect();
        for item in &items {
            assert_eq!(item.validate(), Ok(()), "第 {item:?} 条应合法");
        }
        store.save(&items).expect("应能保存 101 条");

        let mut reader = Snippets::at(store.path().to_path_buf());
        assert_eq!(
            reader.load().expect("应能加载").len(),
            101,
            "条数不应被截断"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn display_name_falls_back_to_first_text_line() {
        assert_eq!(snippet("named", "echo hi", true).display_name(), "named");
        assert_eq!(
            snippet("  padded  ", "echo hi", true).display_name(),
            "padded"
        );
        // 空名回落取文本首行并 trim。
        assert_eq!(
            snippet("", "  git log --oneline  \nsecond", true).display_name(),
            "git log --oneline"
        );
        // 回落名同样截断到上限。
        let long = "y".repeat(MAX_DISPLAY_CHARS + 10);
        assert_eq!(
            snippet("", &long, true).display_name().chars().count(),
            MAX_DISPLAY_CHARS
        );
        // 名与文本都为空时返回空串，不 panic。
        assert_eq!(snippet("", "", true).display_name(), "");
    }
}
