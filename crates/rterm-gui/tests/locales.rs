//! 校验 locales 中的 key 与代码中的 `t!` 调用一致。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use yaml_rust2::{Yaml, YamlLoader};

/// 由代码动态拼接、字面量扫描无法发现的 key。
const DYNAMIC_KEYS: &[&str] = &[];

#[test]
fn locale_keys_match_usage() {
    let gui_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = gui_dir
        .parent()
        .and_then(Path::parent)
        .expect("工作区根目录");

    let en = locale_keys(&gui_dir.join("locales/en.yml"));
    let zh = locale_keys(&gui_dir.join("locales/zh-CN.yml"));
    assert_eq!(en, zh, "en 与 zh-CN 的 key 集不一致");

    let used = used_keys(workspace);

    let unused: Vec<&str> = en
        .difference(&used)
        .map(String::as_str)
        .filter(|key| !DYNAMIC_KEYS.contains(key))
        .collect();
    assert!(unused.is_empty(), "locales 中存在未使用的 key: {unused:?}");

    let undefined: Vec<&str> = used.difference(&en).map(String::as_str).collect();
    assert!(
        undefined.is_empty(),
        "代码引用但 locales 中缺失的 key: {undefined:?}"
    );
}

fn locale_keys(path: &Path) -> BTreeSet<String> {
    let text =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {} 失败: {e}", path.display()));
    let docs = YamlLoader::load_from_str(&text)
        .unwrap_or_else(|e| panic!("解析 {} 失败: {e}", path.display()));
    assert_eq!(docs.len(), 1, "{} 应只包含一个 YAML 文档", path.display());

    let mut keys = BTreeSet::new();
    collect_keys(&docs[0], "", &mut keys);
    keys
}

fn collect_keys(node: &Yaml, prefix: &str, keys: &mut BTreeSet<String>) {
    if let Yaml::Hash(map) = node {
        for (key, value) in map {
            let key = key
                .as_str()
                .unwrap_or_else(|| panic!("{prefix} 下存在非字符串 key"));
            let full = if prefix.is_empty() {
                key.to_owned()
            } else {
                format!("{prefix}.{key}")
            };
            collect_keys(value, &full, keys);
        }
    } else if !prefix.is_empty() {
        keys.insert(prefix.to_owned());
    }
}

fn used_keys(root: &Path) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for dir in ["src", "crates"] {
        collect_t_literals(&root.join(dir), &mut keys);
    }
    keys
}

fn collect_t_literals(dir: &Path, keys: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if entry.file_name().to_str() != Some("target") {
                collect_t_literals(&path, keys);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let text = fs::read_to_string(&path).unwrap();
            keys.extend(t_literals(&text).map(str::to_owned));
        }
    }
}

fn t_literals(text: &str) -> impl Iterator<Item = &str> {
    // 拆开写，避免扫描到本文件时把 needle 自身当成一次调用。
    let needle = concat!("t", "!(");
    text.match_indices(needle).filter_map(move |(start, _)| {
        let prev = text[..start].chars().next_back();
        if prev.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        let rest = text[start + needle.len()..].trim_start();
        let rest = rest.strip_prefix('r').unwrap_or(rest).strip_prefix('"')?;
        let end = rest.find('"')?;
        Some(&rest[..end])
    })
}
