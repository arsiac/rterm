//! 构建脚本：locales 翻译文件变化时触发重编译（i18n! 宏在编译期内嵌 yml）。

fn main() {
    println!("cargo:rerun-if-changed=locales");
    // 只盯 yml；目录级声明兜住文件的增删。
    for entry in std::fs::read_dir("locales").into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "yml") {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
