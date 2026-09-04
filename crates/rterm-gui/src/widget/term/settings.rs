//! 终端部件的可配置项：字体、主题与后端（PTY / shell）参数。

use std::{collections::HashMap, path::PathBuf};

use crate::widget::term::ColorPalette;
use iced::Font;

/// 默认启动的 shell 程序（Windows 下使用 WSL）。
#[cfg(target_os = "windows")]
const DEFAULT_SHELL: &str = "wsl.exe";

/// 默认启动的 shell 程序（非 Windows 平台使用系统 bash）。
#[cfg(not(target_os = "windows"))]
const DEFAULT_SHELL: &str = "/bin/bash";

#[derive(Default, Clone)]
/// 终端部件的整体设置：字体、主题与后端三项。
pub struct Settings {
    /// 字体相关设置。
    pub font: FontSettings,
    /// 主题（配色）相关设置。
    pub theme: ThemeSettings,
    /// 后端（shell / PTY）相关设置。
    pub backend: BackendSettings,
}

#[derive(Debug, Clone)]
/// 后端（PTY）设置：启动的 shell 程序、参数、环境变量与工作目录。
pub struct BackendSettings {
    /// 启动的可执行程序（默认 shell）。
    pub program: String,
    /// 传给程序的命令行参数。
    pub args: Vec<String>,
    /// 追加 / 覆盖的环境变量。
    pub env: HashMap<String, String>,
    /// 起始工作目录（`None` 表示沿用当前目录）。
    pub working_directory: Option<PathBuf>,
    /// 历史缓冲行数（透传给 alacritty `scrolling_history`），0 表示不保留历史。
    pub scrollback: usize,
}

impl Default for BackendSettings {
    /// 以 `DEFAULT_SHELL` 作为默认程序，参数为空、无环境变量、无起始目录、默认 10000 行历史。
    fn default() -> Self {
        Self {
            program: DEFAULT_SHELL.to_string(),
            args: vec![],
            env: HashMap::new(),
            working_directory: None,
            scrollback: 10000,
        }
    }
}

#[derive(Debug, Clone)]
/// 字体设置：字号、缩放系数与字体族。
pub struct FontSettings {
    /// 字号（像素）。
    pub size: f32,
    /// 额外缩放系数（适配高分屏字形大小）。
    pub scale_factor: f32,
    /// 使用的字体族。
    pub font_type: Font,
}

impl Default for FontSettings {
    /// 字体默认 14px、缩放系数 1.3、等宽字体（`Font::MONOSPACE`）。
    fn default() -> Self {
        Self {
            size: 14.0,
            scale_factor: 1.3,
            font_type: Font::MONOSPACE,
        }
    }
}

#[derive(Default, Debug, Clone)]
/// 主题设置：当前配色板。
pub struct ThemeSettings {
    /// 当前使用的配色板。
    pub color_pallete: Box<ColorPalette>,
}

impl ThemeSettings {
    /// 以给定配色板构造主题设置。
    pub fn new(color_pallete: Box<ColorPalette>) -> Self {
        Self { color_pallete }
    }
}
