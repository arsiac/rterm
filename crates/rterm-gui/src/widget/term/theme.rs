use crate::widget::term::settings::ThemeSettings;
use alacritty_terminal::vte::ansi::{self, NamedColor};
use iced::{Color, widget::container};
use std::collections::HashMap;

/// 终端样式接口：为容器提供背景等 iced 样式。
pub(crate) trait TerminalStyle {
    /// 返回该主题的容器样式（背景色等）。
    fn container_style(&self) -> container::Style;
}

#[derive(Debug, Clone)]
/// 终端配色板：命名颜色的十六进制字符串（`#RRGGBB`）。
///
/// 注意这份调色板**不喂给 alacritty**——alacritty 单元格颜色取自 ANSI 转义序列。它只供
/// iced 侧渲染使用（见本文件 [`Theme`] 与 `crate::terminal_theme` 的预设查表）。
pub struct ColorPalette {
    /// 前景色（默认文本颜色）。
    pub foreground: String,
    /// 背景色。
    pub background: String,
    /// 标准黑色。
    pub black: String,
    /// 标准红色。
    pub red: String,
    /// 标准绿色。
    pub green: String,
    /// 标准黄色。
    pub yellow: String,
    /// 标准蓝色。
    pub blue: String,
    /// 标准洋红。
    pub magenta: String,
    /// 标准青色。
    pub cyan: String,
    /// 标准白色。
    pub white: String,
    /// 加亮黑色。
    pub bright_black: String,
    /// 加亮红色。
    pub bright_red: String,
    /// 加亮绿色。
    pub bright_green: String,
    /// 加亮黄色。
    pub bright_yellow: String,
    /// 加亮蓝色。
    pub bright_blue: String,
    /// 加亮洋红。
    pub bright_magenta: String,
    /// 加亮青色。
    pub bright_cyan: String,
    /// 加亮白色。
    pub bright_white: String,
    /// 加亮前景色（可选，缺省回退到 `foreground`）。
    pub bright_foreground: Option<String>,
    /// 暗淡前景色。
    pub dim_foreground: String,
    /// 暗淡黑色。
    pub dim_black: String,
    /// 暗淡红色。
    pub dim_red: String,
    /// 暗淡绿色。
    pub dim_green: String,
    /// 暗淡黄色。
    pub dim_yellow: String,
    /// 暗淡蓝色。
    pub dim_blue: String,
    /// 暗淡洋红。
    pub dim_magenta: String,
    /// 暗淡青色。
    pub dim_cyan: String,
    /// 暗淡白色。
    pub dim_white: String,
}

impl Default for ColorPalette {
    /// 返回默认配色板（内置暗色预设）。
    fn default() -> Self {
        Self {
            foreground: String::from("#d8d8d8"),
            background: String::from("#181818"),
            black: String::from("#181818"),
            red: String::from("#ac4242"),
            green: String::from("#90a959"),
            yellow: String::from("#f4bf75"),
            blue: String::from("#6a9fb5"),
            magenta: String::from("#aa759f"),
            cyan: String::from("#75b5aa"),
            white: String::from("#d8d8d8"),
            bright_black: String::from("#6b6b6b"),
            bright_red: String::from("#c55555"),
            bright_green: String::from("#aac474"),
            bright_yellow: String::from("#feca88"),
            bright_blue: String::from("#82b8c8"),
            bright_magenta: String::from("#c28cb8"),
            bright_cyan: String::from("#93d3c3"),
            bright_white: String::from("#f8f8f8"),
            bright_foreground: None,
            dim_foreground: String::from("#828482"),
            dim_black: String::from("#0f0f0f"),
            dim_red: String::from("#712b2b"),
            dim_green: String::from("#5f6f3a"),
            dim_yellow: String::from("#a17e4d"),
            dim_blue: String::from("#456877"),
            dim_magenta: String::from("#704d68"),
            dim_cyan: String::from("#4d7770"),
            dim_white: String::from("#8e8e8e"),
        }
    }
}

#[derive(Debug, Clone)]
/// 终端主题：由配色板与 256 色表解析出 iced [`Color`]，供渲染取色。
pub struct Theme {
    /// 配色板（命名颜色十六进制字符串）。
    palette: Box<ColorPalette>,
    /// 256 色 ANSI 索引到 iced [`Color`] 的查表。
    ansi256_colors: HashMap<u8, Color>,
}

impl Default for Theme {
    /// 返回默认主题：内置配色板与 256 色表。
    fn default() -> Self {
        Self {
            palette: Box::<ColorPalette>::default(),
            ansi256_colors: build_ansi256_colors(),
        }
    }
}

impl Theme {
    /// 以主题设置（配色板）构造主题。
    pub fn new(settings: ThemeSettings) -> Self {
        Self {
            palette: settings.color_pallete,
            ansi256_colors: build_ansi256_colors(),
        }
    }

    /// 把 alacritty 的 ANSI 颜色（命名 / 索引 / 直接 RGB）解析为 iced [`Color`]。
    pub fn get_color(&self, c: ansi::Color) -> Color {
        match c {
            ansi::Color::Spec(rgb) => Color::from_rgb8(rgb.r, rgb.g, rgb.b),
            ansi::Color::Indexed(index) => {
                if index <= 15 {
                    let color = match index {
                        // 常规终端颜色
                        0 => &self.palette.black,
                        1 => &self.palette.red,
                        2 => &self.palette.green,
                        3 => &self.palette.yellow,
                        4 => &self.palette.blue,
                        5 => &self.palette.magenta,
                        6 => &self.palette.cyan,
                        7 => &self.palette.white,
                        // 高亮终端颜色
                        8 => &self.palette.bright_black,
                        9 => &self.palette.bright_red,
                        10 => &self.palette.bright_green,
                        11 => &self.palette.bright_yellow,
                        12 => &self.palette.bright_blue,
                        13 => &self.palette.bright_magenta,
                        14 => &self.palette.bright_cyan,
                        15 => &self.palette.bright_white,
                        _ => &self.palette.background,
                    };

                    return hex_to_color(color)
                        .unwrap_or_else(|_| panic!("invalid color {}", color));
                }

                // 其他颜色
                match self.ansi256_colors.get(&index) {
                    Some(color) => *color,
                    None => Color::from_rgb8(0, 0, 0),
                }
            }
            ansi::Color::Named(c) => {
                let color = match c {
                    NamedColor::Foreground => &self.palette.foreground,
                    NamedColor::Background => &self.palette.background,
                    // 常规终端颜色
                    NamedColor::Black => &self.palette.black,
                    NamedColor::Red => &self.palette.red,
                    NamedColor::Green => &self.palette.green,
                    NamedColor::Yellow => &self.palette.yellow,
                    NamedColor::Blue => &self.palette.blue,
                    NamedColor::Magenta => &self.palette.magenta,
                    NamedColor::Cyan => &self.palette.cyan,
                    NamedColor::White => &self.palette.white,
                    // 高亮终端颜色
                    NamedColor::BrightBlack => &self.palette.bright_black,
                    NamedColor::BrightRed => &self.palette.bright_red,
                    NamedColor::BrightGreen => &self.palette.bright_green,
                    NamedColor::BrightYellow => &self.palette.bright_yellow,
                    NamedColor::BrightBlue => &self.palette.bright_blue,
                    NamedColor::BrightMagenta => &self.palette.bright_magenta,
                    NamedColor::BrightCyan => &self.palette.bright_cyan,
                    NamedColor::BrightWhite => &self.palette.bright_white,
                    NamedColor::BrightForeground => match &self.palette.bright_foreground {
                        Some(color) => color,
                        None => &self.palette.foreground,
                    },
                    // 暗淡终端颜色
                    NamedColor::DimForeground => &self.palette.dim_foreground,
                    NamedColor::DimBlack => &self.palette.dim_black,
                    NamedColor::DimRed => &self.palette.dim_red,
                    NamedColor::DimGreen => &self.palette.dim_green,
                    NamedColor::DimYellow => &self.palette.dim_yellow,
                    NamedColor::DimBlue => &self.palette.dim_blue,
                    NamedColor::DimMagenta => &self.palette.dim_magenta,
                    NamedColor::DimCyan => &self.palette.dim_cyan,
                    NamedColor::DimWhite => &self.palette.dim_white,
                    _ => &self.palette.background,
                };

                hex_to_color(color).unwrap_or_else(|_| panic!("invalid color {}", color))
            }
        }
    }
}

/// 构建 256 色 ANSI 调色板（16 之后为 6x6x6 立方与 24 级灰度）。
fn build_ansi256_colors() -> HashMap<u8, Color> {
    let mut ansi256_colors = HashMap::new();

    for r in 0..6 {
        for g in 0..6 {
            for b in 0..6 {
                // 预留前 16 个颜色供配置使用。
                let index = 16 + r * 36 + g * 6 + b;
                let color = Color::from_rgb8(
                    if r == 0 { 0 } else { r * 40 + 55 },
                    if g == 0 { 0 } else { g * 40 + 55 },
                    if b == 0 { 0 } else { b * 40 + 55 },
                );
                ansi256_colors.insert(index, color);
            }
        }
    }

    let index: u8 = 232;
    for i in 0..24 {
        let value = i * 10 + 8;
        ansi256_colors.insert(index + i, Color::from_rgb8(value, value, value));
    }

    ansi256_colors
}

/// 将 `#RRGGBB` 十六进制字符串解析为 iced [`Color`]。
fn hex_to_color(hex: &str) -> anyhow::Result<Color> {
    if hex.len() != 7 {
        return Err(anyhow::format_err!("input string is in non valid format"));
    }

    let r = u8::from_str_radix(&hex[1..3], 16)?;
    let g = u8::from_str_radix(&hex[3..5], 16)?;
    let b = u8::from_str_radix(&hex[5..7], 16)?;

    Ok(Color::from_rgb8(r, g, b))
}

impl TerminalStyle for Theme {
    /// 以主题背景色构造容器样式。
    fn container_style(&self) -> container::Style {
        container::Style {
            background: Some(
                hex_to_color(&self.palette.background)
                    .unwrap_or_else(|_| {
                        panic!("invalid background color {}", self.palette.background)
                    })
                    .into(),
            ),
            ..container::Style::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi;
    use std::collections::HashMap;

    #[test]
    fn hex_to_color_valid_convertion() {
        assert!(hex_to_color("#000000").is_ok())
    }

    #[test]
    fn hex_to_color_short_string() {
        assert!(hex_to_color("GG").is_err());
    }

    #[test]
    fn hex_to_color_long_string() {
        assert!(hex_to_color("GG000000").is_err());
    }

    #[test]
    fn hex_to_color_non_valid_hex_string() {
        assert!(hex_to_color("#KKLLOO").is_err());
    }

    #[test]
    fn get_basic_indexed_colors() {
        let default_theme = Theme::default();
        let basic_indexed_colors_map: HashMap<u8, String> = HashMap::from([
            (0, default_theme.palette.black.clone()),
            (1, default_theme.palette.red.clone()),
            (2, default_theme.palette.green.clone()),
            (3, default_theme.palette.yellow.clone()),
            (4, default_theme.palette.blue.clone()),
            (5, default_theme.palette.magenta.clone()),
            (6, default_theme.palette.cyan.clone()),
            (7, default_theme.palette.white.clone()),
            (8, default_theme.palette.bright_black.clone()),
            (9, default_theme.palette.bright_red.clone()),
            (10, default_theme.palette.bright_green.clone()),
            (11, default_theme.palette.bright_yellow.clone()),
            (12, default_theme.palette.bright_blue.clone()),
            (13, default_theme.palette.bright_magenta.clone()),
            (14, default_theme.palette.bright_cyan.clone()),
            (15, default_theme.palette.bright_white.clone()),
        ]);

        for index in 0..16 {
            let color = default_theme.get_color(ansi::Color::Indexed(index));
            let expected_color = basic_indexed_colors_map.get(&index).unwrap();
            assert_eq!(color, hex_to_color(expected_color).unwrap())
        }
    }
}
