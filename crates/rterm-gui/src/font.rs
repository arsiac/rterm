//! 字体辅助
//!
//! 枚举系统已安装的字体系列，供设置弹窗的下拉框使用。
//!
//! 内存约束：iced 0.14 的 `Font` 是 `Copy`，其 `Family::Name` 仅接受 `&'static str`，
//! 故任意“命名字体”必然引用一个 `'static` 字符串。为在支持字体切换的同时避免内存
//! 随切换次数无限增长，这里用「按字体名去重的 `'static` 缓存」——每个不同的族名只泄漏
//! 一次并永久复用，总泄漏量有界（≈用过的不同字体名数量，与切换次数无关）；字体系列
//! 列表本身以 `Vec<String>` 持有（不泄漏），仅扫描一次。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use iced::Font;
use log::debug;

use crate::t;

/// 字体下拉框中表示「使用系统默认字体」的标签（随语言切换，故为运行时取值而非常量）。
pub fn default_font_label() -> String {
    t!("settings.ui_font_default")
}

/// 不同字体名 → `'static str` 的进程级缓存（同一名称只泄漏一次）。
static NAME_CACHE: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();

/// 将字体族名提升为 `&'static str`（进程级缓存，同名只泄漏一次，复用不再增长）。
fn to_static_name(name: &str) -> &'static str {
    let mut cache = NAME_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("font name cache poisoned");
    cache
        .entry(name.to_string())
        .or_insert_with(|| Box::leak(name.to_string().into_boxed_str()))
}

/// 一次扫描得到的全部字体系列（去重、排序），由 `Vec` 持有而非泄漏。
struct FontLists {
    /// 全部字体系列名（去重、排序）。
    all: Vec<String>,
    /// 等宽字体系列名（去重、排序），供终端字体选择使用。
    mono: Vec<String>,
}

/// 进程级字体列表缓存：仅扫描一次系统字体，同时产出全部与等宽两份列表。
fn font_lists() -> &'static FontLists {
    /// 进程级字体列表缓存（首次调用 `font_lists` 时扫描系统字体并填充）。
    static LISTS: OnceLock<FontLists> = OnceLock::new();
    LISTS.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let mut all: HashSet<String> = HashSet::new();
        let mut mono: HashSet<String> = HashSet::new();
        for face in db.faces() {
            // 每个字面只取主系列名，避免同一字体的本地化别名重复出现。
            if let Some((family, _)) = face.families.first() {
                let trimmed = family.trim();
                if !trimmed.is_empty() {
                    all.insert(trimmed.to_string());
                    // 终端只能使用等宽字体，否则字符网格会错位；以字体自身的
                    // `monospaced` 标注为准筛选，避免依赖族名猜测。
                    if face.monospaced {
                        mono.insert(trimmed.to_string());
                    }
                }
            }
        }
        let mut all: Vec<String> = all.into_iter().collect();
        all.sort_unstable();
        let mut mono: Vec<String> = mono.into_iter().collect();
        mono.sort_unstable();
        debug!(
            "扫描到 {} 个系统字体系列（其中 {} 个等宽）",
            all.len(),
            mono.len()
        );
        FontLists { all, mono }
    })
}

/// 系统已安装的全部字体系列列表（去重、排序；进程级缓存，仅首次扫描系统字体）。
pub fn installed_families() -> &'static [String] {
    &font_lists().all
}

/// 系统已安装的**等宽**字体系列列表（供终端字体选择，避免非等宽字体破坏字符网格）。
pub fn installed_monospace_families() -> &'static [String] {
    &font_lists().mono
}

/// 界面字体下拉框选项：默认标签 + 已安装的全部字体列表。
///
/// 若当前已保存了一个不在已安装列表中的自定义字体族名，也把它加入选项，
/// 避免已有配置在下拉框中“丢失”。
pub fn ui_font_options(current: &str) -> Vec<String> {
    let mut options = vec![default_font_label()];
    for family in installed_families() {
        options.push(family.clone());
    }
    if !current.trim().is_empty() && !options.iter().any(|o| o == current) {
        options.push(current.to_string());
    }
    options
}

/// 终端字体下拉框选项：默认标签 + 已安装的等宽字体列表（终端仅接受等宽字体）。
pub fn terminal_font_options(current: &str) -> Vec<String> {
    let mut options = vec![default_font_label()];
    for family in installed_monospace_families() {
        options.push(family.clone());
    }
    if !current.trim().is_empty() && !options.iter().any(|o| o == current) {
        options.push(current.to_string());
    }
    options
}

/// 将字体族名解析为 Iced `Font`；空/空白名称回退到 iced 默认字体。
///
/// 因 iced 0.14 的 `Font` 为 `Copy` 且仅接受 `&'static` 族名，族名经 `to_static_name`
/// 去重缓存后引用；同一名称只泄漏一次，切换字体不会使内存随次数增长。
pub fn resolve_font(name: &str) -> Font {
    if name.trim().is_empty() {
        Font::DEFAULT
    } else {
        Font::with_name(to_static_name(name))
    }
}

/// 终端字体解析：空名回退到 iced 等宽别名 `Font::MONOSPACE`（保证网格对齐），
/// 否则同 [`resolve_font`] 经去重缓存引用 `'static` 族名，切换零增长。
pub fn resolve_terminal_font(name: &str) -> Font {
    if name.trim().is_empty() {
        Font::MONOSPACE
    } else {
        Font::with_name(to_static_name(name))
    }
}
