//! 终端配色主题预设。
//!
//! 终端主题模型是一份 [`ColorPalette`]（28 个 `#RRGGBB` 十六进制字符串：前景 / 背景 +
//! 8 标准色 + 8 加亮色 + 加亮前景 + 暗淡前景 + 8 暗淡色）。该类型自身没有 serde 能力，
//! 因此本模块只维护「预设名 -> 调色板」的查表，配置层（`AppConfig.terminal_theme`）
//! 只持久化预设名字符串，运行时再解析为具体调色板。
//!
//! 每个预设用 [`TerminalThemeDef`] 描述，预设颜色以 18 字段（背景 + 前景 + 16 ANSI）给出，
//! 其余字段（dim_*、bright_foreground）回退到 [`ColorPalette::default()`]。
//!
//! 颜色值主要抄自各上游权威仓库（Dracula、Solarized、Gruvbox、Nord、Catppuccin、Tokyo Night、
//! One、GitHub、Monokai、Ayu、Everforest、Kanagawa、PaperColor 等），与社区终端保持一致观感。

use crate::widget::term::ColorPalette;

/// 单个终端主题预设的静态定义。
pub struct TerminalThemeDef {
    /// 预设名，同时作为配置持久化键；切换时按此字符串查表解析调色板。
    pub name: &'static str,
    /// 解析出该主题完整调色板的构造函数（无捕获，可安全存为函数指针）。
    pub palette: fn() -> ColorPalette,
}

/// 终端主题默认预设名：与本文件内独立的 "One Dark" 预设不同，默认预设用的是
/// [`ColorPalette::default()`]（`#181818` 底的深色配色，见 `crate::widget::term::theme`）。
pub const TERMINAL_THEME_DEFAULT: &str = "Default";

/// 所有可用终端主题预设名，按字母排序，供设置 UI 的 `pick_list` 使用。
pub const TERMINAL_THEME_NAMES: &[&str] = &[
    "3024 Day",
    "Ayu Dark",
    "Ayu Light",
    "Catppuccin Latte",
    "Catppuccin Mocha",
    "Default",
    "Dracula",
    "Everforest",
    "GitHub Dark",
    "GitHub Light",
    "Gruvbox Dark",
    "Gruvbox Light",
    "Kanagawa",
    "Monokai",
    "Nord",
    "One Dark",
    "One Light",
    "Papercolor Light",
    "Solarized Dark",
    "Solarized Light",
    "Tokyo Night",
    "Ubuntu",
];

/// 全部主题预设的注册表，是「名字 -> 调色板」与「名字 -> 明暗」的唯一数据源。
///
/// 按明暗分组仅便于组织；运行期一律按 `name` 查找（见 [`resolve_terminal_theme`]），
/// 因此与 [`TERMINAL_THEME_NAMES`] 的字母序展示列表无需保持相同顺序。
pub const ALL_TERMINAL_THEMES: &[TerminalThemeDef] = &[
    // —— 深色 ——
    TerminalThemeDef {
        name: "Default",
        palette: default_palette,
    },
    TerminalThemeDef {
        name: "Ubuntu",
        palette: ubuntu_palette,
    },
    TerminalThemeDef {
        name: "Dracula",
        palette: dracula_palette,
    },
    TerminalThemeDef {
        name: "Solarized Dark",
        palette: solarized_dark_palette,
    },
    TerminalThemeDef {
        name: "Gruvbox Dark",
        palette: gruvbox_dark_palette,
    },
    TerminalThemeDef {
        name: "Nord",
        palette: nord_palette,
    },
    TerminalThemeDef {
        name: "Catppuccin Mocha",
        palette: catppuccin_mocha_palette,
    },
    TerminalThemeDef {
        name: "Tokyo Night",
        palette: tokyo_night_palette,
    },
    TerminalThemeDef {
        name: "One Dark",
        palette: one_dark_palette,
    },
    TerminalThemeDef {
        name: "GitHub Dark",
        palette: github_dark_palette,
    },
    TerminalThemeDef {
        name: "Monokai",
        palette: monokai_palette,
    },
    TerminalThemeDef {
        name: "Ayu Dark",
        palette: ayu_dark_palette,
    },
    TerminalThemeDef {
        name: "Everforest",
        palette: everforest_palette,
    },
    TerminalThemeDef {
        name: "Kanagawa",
        palette: kanagawa_palette,
    },
    // —— 浅色 ——
    TerminalThemeDef {
        name: "3024 Day",
        palette: day_3024_palette,
    },
    TerminalThemeDef {
        name: "Solarized Light",
        palette: solarized_light_palette,
    },
    TerminalThemeDef {
        name: "Gruvbox Light",
        palette: gruvbox_light_palette,
    },
    TerminalThemeDef {
        name: "Catppuccin Latte",
        palette: catppuccin_latte_palette,
    },
    TerminalThemeDef {
        name: "One Light",
        palette: one_light_palette,
    },
    TerminalThemeDef {
        name: "GitHub Light",
        palette: github_light_palette,
    },
    TerminalThemeDef {
        name: "Ayu Light",
        palette: ayu_light_palette,
    },
    TerminalThemeDef {
        name: "Papercolor Light",
        palette: papercolor_light_palette,
    },
];

/// 用 18 个 `#RRGGBB` 字符串构造 [`ColorPalette`]（背景、前景、16 个 ANSI 色），其余字段回退默认值。
///
/// 该辅助函数统一收敛所有预设的构造细节，避免每套主题重复书写 `..Default::default()` 样板。
#[allow(clippy::too_many_arguments)]
fn palette(
    background: &str,
    foreground: &str,
    black: &str,
    red: &str,
    green: &str,
    yellow: &str,
    blue: &str,
    magenta: &str,
    cyan: &str,
    white: &str,
    bright_black: &str,
    bright_red: &str,
    bright_green: &str,
    bright_yellow: &str,
    bright_blue: &str,
    bright_magenta: &str,
    bright_cyan: &str,
    bright_white: &str,
) -> ColorPalette {
    ColorPalette {
        background: String::from(background),
        foreground: String::from(foreground),
        black: String::from(black),
        red: String::from(red),
        green: String::from(green),
        yellow: String::from(yellow),
        blue: String::from(blue),
        magenta: String::from(magenta),
        cyan: String::from(cyan),
        white: String::from(white),
        bright_black: String::from(bright_black),
        bright_red: String::from(bright_red),
        bright_green: String::from(bright_green),
        bright_yellow: String::from(bright_yellow),
        bright_blue: String::from(bright_blue),
        bright_magenta: String::from(bright_magenta),
        bright_cyan: String::from(bright_cyan),
        bright_white: String::from(bright_white),
        ..Default::default()
    }
}

/// 默认主题调色板：即 [`ColorPalette::default()`]（`#181818` 底的深色配色），
/// 与下面独立的 "One Dark" 预设不是同一套。
fn default_palette() -> ColorPalette {
    ColorPalette::default()
}

/// Ubuntu 主题调色板（紫底 `#300A24`），抄自上游 iced_term 示例（该依赖已并入 `crate::widget::term`）。
fn ubuntu_palette() -> ColorPalette {
    palette(
        "#300A24", "#FFFFFF", "#2E3436", "#CC0000", "#4E9A06", "#C4A000", "#3465A4", "#75507B",
        "#06989A", "#D3D7CF", "#555753", "#EF2929", "#8AE234", "#FCE94F", "#729FCF", "#AD7FA8",
        "#34E2E2", "#EEEEEC",
    )
}

/// Dracula 主题调色板（紫调高对比），抄自 Dracula 上游配色。
fn dracula_palette() -> ColorPalette {
    palette(
        "#282A36", "#F8F8F2", "#21222C", "#FF5555", "#50FA7B", "#F1FA8C", "#BD93F9", "#FF79C6",
        "#8BE9FD", "#F8F8F2", "#6272A4", "#FF6E6E", "#69FF94", "#FFFFA5", "#D6ACFF", "#FF92DF",
        "#A4FFFF", "#FFFFFF",
    )
}

/// Solarized Dark 主题调色板（低饱和护眼），抄自 Solarized 上游配色。
fn solarized_dark_palette() -> ColorPalette {
    palette(
        "#002B36", "#839496", "#073642", "#DC322F", "#859900", "#B58900", "#268BD2", "#D33682",
        "#2AA198", "#EEE8D5", "#002B36", "#CB4B16", "#586E75", "#657B83", "#839496", "#6C71C4",
        "#93A1A1", "#FDF6E3",
    )
}

/// Gruvbox Dark 主题调色板（复古暖色），抄自 Gruvbox 上游配色。
fn gruvbox_dark_palette() -> ColorPalette {
    palette(
        "#282828", "#EBDBB2", "#282828", "#CC241D", "#98971A", "#D79921", "#458588", "#B16286",
        "#689D6A", "#A89984", "#928374", "#FB4934", "#B8BB26", "#FABD2F", "#83A598", "#D3869B",
        "#8EC07C", "#EBDBB2",
    )
}

/// Nord 主题调色板（北欧冷蓝灰），抄自 Nord 上游配色。
fn nord_palette() -> ColorPalette {
    palette(
        "#2E3440", "#D8DEE9", "#3B4252", "#BF616A", "#A3BE8C", "#EBCB8B", "#81A1C1", "#B48EAD",
        "#88C0D0", "#E5E9F0", "#4C566A", "#BF616A", "#A3BE8C", "#EBCB8B", "#81A1C1", "#B48EAD",
        "#8FBCBB", "#ECEFF4",
    )
}

/// Catppuccin Mocha 主题调色板（柔和莫兰迪深色），抄自 Catppuccin 上游配色。
fn catppuccin_mocha_palette() -> ColorPalette {
    palette(
        "#1E1E2E", "#CDD6F4", "#45475A", "#F38BA8", "#A6E3A1", "#F9E2AF", "#89B4FA", "#F5C2E7",
        "#94E2D5", "#BAC2DE", "#585B70", "#F38BA8", "#A6E3A1", "#F9E2AF", "#89B4FA", "#F5C2E7",
        "#94E2D5", "#CDD6F4",
    )
}

/// Tokyo Night 主题调色板（赛博蓝紫），抄自 Tokyo Night 上游配色。
fn tokyo_night_palette() -> ColorPalette {
    palette(
        "#1A1B26", "#C0CAF5", "#15161E", "#F7768E", "#9ECE6A", "#E0AF68", "#7AA2F7", "#BB9AF7",
        "#7DCFA1", "#A9B1D6", "#414868", "#F7768E", "#9ECE6A", "#E0AF68", "#7AA2F7", "#BB9AF7",
        "#7DCFA1", "#C0CAF5",
    )
}

/// One Dark 主题调色板（经典 One 系），抄自 One Dark 上游配色。
fn one_dark_palette() -> ColorPalette {
    palette(
        "#282C34", "#ABB2BF", "#282C34", "#E06C75", "#98C379", "#E5C07B", "#61AFEF", "#C678DD",
        "#56B6C2", "#ABB2BF", "#5C6370", "#E06C75", "#98C379", "#E5C07B", "#61AFEF", "#C678DD",
        "#56B6C2", "#FFFFFF",
    )
}

/// GitHub Dark 主题调色板（类 GitHub 界面），抄自 GitHub 上游配色。
fn github_dark_palette() -> ColorPalette {
    palette(
        "#0D1117", "#C9D1D9", "#484F58", "#FF7B72", "#3FB950", "#D29922", "#58A6FF", "#DB61A2",
        "#39C5CF", "#B1BAC4", "#6E7681", "#FFA198", "#56D364", "#E3B341", "#79C0FF", "#F778BA",
        "#56D4DD", "#F0F6FC",
    )
}

/// Monokai 主题调色板（高饱和霓虹），抄自 Monokai 上游配色。
fn monokai_palette() -> ColorPalette {
    palette(
        "#272822", "#F8F8F2", "#272822", "#F92672", "#A6E22E", "#F4BF75", "#66D9EF", "#AE81FF",
        "#A1EFE4", "#F8F8F2", "#75715E", "#F92672", "#A6E22E", "#F4BF75", "#66D9EF", "#AE81FF",
        "#A1EFE4", "#F9F8F5",
    )
}

/// Ayu Dark 主题调色板（极简深色），抄自 Ayu 上游配色。
fn ayu_dark_palette() -> ColorPalette {
    palette(
        "#0A0E14", "#B3B1AD", "#0A0E14", "#F07178", "#AAD94C", "#FFB454", "#59C2FF", "#D2A6FF",
        "#95E6CB", "#FFFFFF", "#000000", "#FF8B92", "#C2D94C", "#FFC66D", "#84C0FF", "#E0AAFF",
        "#95E6CB", "#FFFFFF",
    )
}

/// Everforest 主题调色板（自然绿调深色），抄自 Everforest 上游配色。
fn everforest_palette() -> ColorPalette {
    palette(
        "#2D353B", "#D3C6AA", "#475258", "#E67E80", "#A7C080", "#DBBC7F", "#7FBBB3", "#D699B6",
        "#83C092", "#D3C6AA", "#475258", "#E67E80", "#A7C080", "#DBBC7F", "#7FBBB3", "#D699B6",
        "#83C092", "#D3C6AA",
    )
}

/// Kanagawa 主题调色板（和风深蓝），抄自 Kanagawa 上游配色。
fn kanagawa_palette() -> ColorPalette {
    palette(
        "#1F1F28", "#DCD7BA", "#1F1F28", "#C34043", "#76946A", "#C0A36E", "#7E9CD8", "#957FB8",
        "#6CA0A8", "#C8C093", "#363646", "#E82424", "#98BB6C", "#E6C384", "#7FB4CA", "#B8A1E3",
        "#7FC4CA", "#E6E1C0",
    )
}

/// 3024 Day 主题调色板（浅色 `#F7F7F7`），抄自上游 iced_term 示例（该依赖已并入 `crate::widget::term`）。
fn day_3024_palette() -> ColorPalette {
    palette(
        "#F7F7F7", "#4A4543", "#090300", "#DB2D20", "#01A252", "#FDED02", "#01A0E4", "#A16A94",
        "#B5E4F4", "#A5A2A2", "#5C5855", "#E8BBD0", "#3A3432", "#4A4543", "#807D7C", "#D6D5D4",
        "#CDAB53", "#F7F7F7",
    )
}

/// Solarized Light 主题调色板（低饱和护眼浅色），抄自 Solarized 上游配色。
fn solarized_light_palette() -> ColorPalette {
    palette(
        "#FDF6E3", "#657B83", "#073642", "#DC322F", "#859900", "#B58900", "#268BD2", "#D33682",
        "#2AA198", "#EEE8D5", "#002B36", "#CB4B16", "#586E75", "#657B83", "#839496", "#6C71C4",
        "#93A1A1", "#FDF6E3",
    )
}

/// Gruvbox Light 主题调色板（复古暖色浅色），抄自 Gruvbox 上游配色。
fn gruvbox_light_palette() -> ColorPalette {
    palette(
        "#FBF1C7", "#3C3836", "#FBF1C7", "#CC241D", "#98971A", "#D79921", "#458588", "#B16286",
        "#689D6A", "#7C6F64", "#928374", "#FB4934", "#B8BB26", "#FABD2F", "#83A598", "#D3869B",
        "#8EC07C", "#EBDBB2",
    )
}

/// Catppuccin Latte 主题调色板（柔和莫兰迪浅色），抄自 Catppuccin 上游配色。
fn catppuccin_latte_palette() -> ColorPalette {
    palette(
        "#EFF1F5", "#4C4F69", "#5C5F77", "#D20F39", "#40A02B", "#DF8E1D", "#1E66F5", "#EA76CB",
        "#179299", "#ACB0BE", "#6C6F85", "#D20F39", "#40A02B", "#DF8E1D", "#1E66F5", "#EA76CB",
        "#179299", "#4C4F69",
    )
}

/// One Light 主题调色板（经典浅色），抄自 One Light 上游配色。
fn one_light_palette() -> ColorPalette {
    palette(
        "#FAFAFA", "#383A42", "#383A42", "#E45649", "#50A14F", "#C18401", "#4078F2", "#A626A4",
        "#0184BC", "#F0F0F0", "#4F4F4F", "#E06C75", "#98C379", "#E5C07B", "#61AFEF", "#C678DD",
        "#56B6C2", "#FFFFFF",
    )
}

/// GitHub Light 主题调色板（类 GitHub 浅色界面），抄自 GitHub 上游配色。
fn github_light_palette() -> ColorPalette {
    palette(
        "#FFFFFF", "#24292F", "#57606A", "#CF222E", "#1A7F37", "#9A6700", "#0969DA", "#BF3989",
        "#0598BC", "#6E7781", "#8B949E", "#A40E26", "#2DA44E", "#BF8700", "#218BFF", "#D4A72C",
        "#0AA1C0", "#6E7781",
    )
}

/// Ayu Light 主题调色板（极简浅色），抄自 Ayu 上游配色。
fn ayu_light_palette() -> ColorPalette {
    palette(
        "#FAFAFA", "#5C6773", "#5C6773", "#F07178", "#AAD94C", "#FFB454", "#59C2FF", "#D2A6FF",
        "#95E6CB", "#FFFFFF", "#5C6773", "#F07178", "#AAD94C", "#FFB454", "#59C2FF", "#D2A6FF",
        "#95E6CB", "#FFFFFF",
    )
}

/// Papercolor Light 主题调色板（纸感浅色），抄自 PaperColor 上游配色。
fn papercolor_light_palette() -> ColorPalette {
    palette(
        "#F8F8F8", "#444444", "#EEEEEE", "#AF0000", "#008700", "#5F8700", "#0087FF", "#AF00AF",
        "#00AFAF", "#FFFFFF", "#BCBCBC", "#D70000", "#5FAF00", "#AF8700", "#5FAFFF", "#AF00AF",
        "#00D7AF", "#FFFFFF",
    )
}

/// 根据预设名解析出对应的终端调色板。
///
/// 优先在 [`ALL_TERMINAL_THEMES`] 注册表中按名查找；未知名字回退到
/// [`TERMINAL_THEME_DEFAULT`]（`ColorPalette::default()` 的深色方案），因此调用方无需担心
/// 配置中残留了已被移除的主题名。
pub fn resolve_terminal_theme(name: &str) -> ColorPalette {
    for def in ALL_TERMINAL_THEMES {
        if def.name == name {
            return (def.palette)();
        }
    }
    ColorPalette::default()
}
