//! GUI 主题相关常量与辅助。
//!
//! 主题系统参照 s3dm：以单一 [`custom_palette`] 基于背景感知亮度派生语义色，
//! 对任意 iced 内置主题（深色 / 浅色 / Dracula / Nord / Catppuccin 系列等）天然生效，
//! 无需为每套主题手工配色。样式函数均接收 `&Theme`，使主题切换即时生效。
//! 终端渲染区由内嵌终端组件自身的调色板绘制（`crate::widget::term`），其留白底色经
//! [`terminal_bg`] 随所选终端配色主题同步，不受本文件的程序主题语义色影响。

use iced::Color;
use iced::Theme;
use iced::widget::button;
use iced::widget::container;
use iced::widget::pick_list;
use iced::widget::scrollable;
use rterm_core::ConnectionStatus;
use std::sync::OnceLock;

/// 强调色取色点：取当前 iced 主题的 primary
pub fn accent_color(theme: &Theme) -> Color {
    theme.extended_palette().primary.base.color
}

/// 选中态强调底色：强调色以 0.28 不透明度叠加，其上文本仍用默认文本色即可保持可读。
pub fn accent_active_bg(theme: &Theme) -> Color {
    Color {
        a: 0.28,
        ..accent_color(theme)
    }
}

/// 深选中态强调底色。
pub fn accent_strong_bg(theme: &Theme) -> Color {
    Color {
        a: 0.45,
        ..accent_color(theme)
    }
}

/// 图标按钮圆角半径（像素）。
pub const ICON_BTN_RADIUS: f32 = 6.0;

/// 标签圆角半径（像素）。
pub const TAB_RADIUS: f32 = 8.0;

/// 终端背景兜底色（`#181818`），与终端配色板 `ColorPalette::default()` 的背景一致。
///
/// 仅作 [`terminal_bg`] 解析失败时的兜底：终端留白与活动标签底色均取当前终端主题的
/// `background`（活动标签经 [`tab_style`] 的 `merge_bg` 与终端区同色融合）。
pub const TERMINAL_BG: Color = Color::from_rgb(0.094, 0.094, 0.094);

/// 根据终端配色主题名返回终端背景色（解析调色板 `background` 字段的 `#RRGGBB` 十六进制）。
///
/// 终端视图的留白区域会跟随用户选择的终端配色主题，避免出现“深色终端 + 浅色留白”的割裂感；
/// 解析失败时回退到 [`TERMINAL_BG`]。
pub fn terminal_bg(name: &str) -> Color {
    let bg = crate::terminal_theme::resolve_terminal_theme(name).background;
    parse_hex_color(&bg).unwrap_or(TERMINAL_BG)
}

/// 将 `#RRGGBB` / `#RRGGBBAA` 十六进制字符串解析为 iced [`Color`]。
///
/// 解析失败时返回 `None`（调用方应回退到默认背景）。
fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.strip_prefix('#')?;
    let bytes = hex.as_bytes();
    let (r, g, b) = match bytes.len() {
        6 => (
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        ),
        8 => (
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        ),
        _ => return None,
    };
    Some(Color::from_rgb8(r, g, b))
}

/// tooltip 显示前的延迟（毫秒）。
pub const TOOLTIP_DELAY_MS: u64 = 200;

/// 左侧活动栏宽度（像素）。
pub const ACTIVITY_BAR_WIDTH: f32 = 48.0;

/// 按主题背景亮度把基准色推向「更亮 / 更暗」一档：深色主题取背景 +`up`，浅色主题取背景 -`down`。
///
/// 语义色派生的统一入口（[`custom_palette`] 之外的表单控件也复用），避免每处各写一遍亮度判定。
fn shift_from_bg(theme: &Theme, up: f32, down: f32) -> Color {
    let bg = theme.palette().background;
    let luminance = 0.299 * bg.r + 0.587 * bg.g + 0.114 * bg.b;
    let delta = if luminance > 0.5 { -down } else { up };
    Color::from_rgb(
        (bg.r + delta).clamp(0.0, 1.0),
        (bg.g + delta).clamp(0.0, 1.0),
        (bg.b + delta).clamp(0.0, 1.0),
    )
}

/// 表单输入区常态边框色：比通用 `border` 更明确一档（深色背景 +0.24 / 浅色 -0.22），
/// 使内嵌输入框在抬升面板上边界可辨（面板底与通用 `border` 仅差 0.08，边界几乎看不见）。
pub fn input_border_color(theme: &Theme) -> Color {
    shift_from_bg(theme, 0.24, 0.22)
}

/// 输入区悬浮边框色：在 [`input_border_color`] 基础上再亮 / 暗一档，作为悬浮反馈。
pub fn input_border_hover(theme: &Theme) -> Color {
    shift_from_bg(theme, 0.34, 0.32)
}

/// 自定义调色板，在 iced 主题默认颜色之外补充 UI 专用语义色。
///
/// 通过感知亮度公式判断背景深浅，对任意 iced 主题天然生效，无需为每个主题手工配色
/// （思路来自 s3dm）。
pub struct CustomPalette {
    /// 表面背景色（用于面板、列表行常态底等）。
    pub surface: Color,
    /// 抬升表面色（用于弹窗、悬浮元素、行悬浮态等）。
    pub surface_raised: Color,
    /// 次要文本 / 图标颜色（字段标签、图标等）。
    pub text_secondary: Color,
    /// 弱文本色（说明 / 提示文字）：比 `text_secondary` 再弱一档。
    pub text_tertiary: Color,
    /// 悬浮态背景色（行 / 按钮悬停时，较表面更突出）。
    pub hover: Color,
    /// 抬升表面（弹窗、dropdown 面板）内的行悬浮色。
    ///
    /// [`Self::hover`] 与 [`Self::surface_raised`] 是同一档派生值，在抬升表面上用
    /// `hover` 做悬浮反馈会与面板底色完全重合、看不出变化，故需再抬一档。
    pub hover_raised: Color,
    /// 边框 / 分隔线颜色。
    pub border: Color,
}

/// 可用主题列表，每项为（显示名称, iced `Theme` 枚举）。
///
/// 覆盖 iced 0.14 内置的全部 22 套主题；配置以显示名称字符串持久化，
/// 切换时无需为每套主题单独配色。
pub const AVAILABLE_THEMES: &[(&str, Theme)] = &[
    ("Dark", Theme::Dark),
    ("Light", Theme::Light),
    ("Dracula", Theme::Dracula),
    ("Nord", Theme::Nord),
    ("Solarized Light", Theme::SolarizedLight),
    ("Solarized Dark", Theme::SolarizedDark),
    ("Gruvbox Light", Theme::GruvboxLight),
    ("Gruvbox Dark", Theme::GruvboxDark),
    ("Catppuccin Latte", Theme::CatppuccinLatte),
    ("Catppuccin Frappé", Theme::CatppuccinFrappe),
    ("Catppuccin Macchiato", Theme::CatppuccinMacchiato),
    ("Catppuccin Mocha", Theme::CatppuccinMocha),
    ("Tokyo Night", Theme::TokyoNight),
    ("Tokyo Night Storm", Theme::TokyoNightStorm),
    ("Tokyo Night Light", Theme::TokyoNightLight),
    ("Kanagawa Wave", Theme::KanagawaWave),
    ("Kanagawa Dragon", Theme::KanagawaDragon),
    ("Kanagawa Lotus", Theme::KanagawaLotus),
    ("Moonfly", Theme::Moonfly),
    ("Nightfly", Theme::Nightfly),
    ("Oxocarbon", Theme::Oxocarbon),
    ("Ferra", Theme::Ferra),
];

/// 全部可用主题显示名（与 [`AVAILABLE_THEMES`] 同源派生，避免两份字面量清单失同步）。
///
/// 供设置弹窗的主题下拉框使用；按字母排序使下拉框有序；返回 `'static` 切片以满足 `pick_list` 的签名。
pub fn theme_names() -> &'static [&'static str] {
    /// 主题名缓存（首次调用 `theme_names` 时按字母序填充，进程内复用）。
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names: Vec<&'static str> = AVAILABLE_THEMES.iter().map(|(name, _)| *name).collect();
        names.sort();
        names
    })
}

/// 根据当前主题计算自定义调色板。
///
/// 取主题背景色，按感知亮度公式 `0.299R + 0.587G + 0.114B` 判定深浅：
/// 深色主题时整体提亮，浅色主题时整体压暗，从而对任意主题都派生出协调的语义色。
pub fn custom_palette(theme: &Theme) -> CustomPalette {
    let bg = theme.palette().background;
    let luminance = 0.299 * bg.r + 0.587 * bg.g + 0.114 * bg.b;
    if luminance > 0.5 {
        // 浅色主题：表面 / 边框压暗，次要文本用中灰。
        CustomPalette {
            surface: Color::from_rgb(
                (bg.r - 0.04).max(0.0),
                (bg.g - 0.04).max(0.0),
                (bg.b - 0.04).max(0.0),
            ),
            surface_raised: Color::from_rgb(
                (bg.r - 0.08).max(0.0),
                (bg.g - 0.08).max(0.0),
                (bg.b - 0.08).max(0.0),
            ),
            text_secondary: Color::from_rgb(0.45, 0.45, 0.45),
            text_tertiary: Color::from_rgb(0.60, 0.60, 0.60),
            hover: Color::from_rgb(
                (bg.r - 0.08).max(0.0),
                (bg.g - 0.08).max(0.0),
                (bg.b - 0.08).max(0.0),
            ),
            hover_raised: Color::from_rgb(
                (bg.r - 0.14).max(0.0),
                (bg.g - 0.14).max(0.0),
                (bg.b - 0.14).max(0.0),
            ),
            border: Color::from_rgb(
                (bg.r - 0.15).max(0.0),
                (bg.g - 0.15).max(0.0),
                (bg.b - 0.15).max(0.0),
            ),
        }
    } else {
        // 深色主题：表面 / 边框提亮，次要文本用浅灰。
        CustomPalette {
            surface: Color::from_rgb(
                (bg.r + 0.06).min(1.0),
                (bg.g + 0.06).min(1.0),
                (bg.b + 0.06).min(1.0),
            ),
            surface_raised: Color::from_rgb(
                (bg.r + 0.10).min(1.0),
                (bg.g + 0.10).min(1.0),
                (bg.b + 0.10).min(1.0),
            ),
            text_secondary: Color::from_rgb(0.6, 0.6, 0.6),
            text_tertiary: Color::from_rgb(0.45, 0.45, 0.45),
            hover: Color::from_rgb(
                (bg.r + 0.10).min(1.0),
                (bg.g + 0.10).min(1.0),
                (bg.b + 0.10).min(1.0),
            ),
            hover_raised: Color::from_rgb(
                (bg.r + 0.16).min(1.0),
                (bg.g + 0.16).min(1.0),
                (bg.b + 0.16).min(1.0),
            ),
            border: Color::from_rgb(
                (bg.r + 0.18).min(1.0),
                (bg.g + 0.18).min(1.0),
                (bg.b + 0.18).min(1.0),
            ),
        }
    }
}

/// 将主题显示名解析为 iced `Theme`。
///
/// 兼容旧版配置值 `"dark"` / `"light"`（分别映射 `Dark` / `Light`）；
/// 否则在 [`AVAILABLE_THEMES`] 中按显示名查找，未匹配时回退 `Theme::Dark`。
pub fn resolve_theme(name: &str) -> Theme {
    match name {
        "dark" => return Theme::Dark,
        "light" => return Theme::Light,
        _ => {}
    }
    AVAILABLE_THEMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| t.clone())
        .unwrap_or(Theme::Dark)
}

/// 将 [`Color`] 转为与 `theme::palette` 无关的背景样式（仅返回纯色）。
pub fn plain_background(color: Color) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(color.into()),
        ..Default::default()
    }
}

/// 依据连接状态返回列表行常态背景色。
///
/// 注意：列表行不随“已连接”变绿，连接状态仅用于错误提示；
/// 同一会话可重复连接并开多个标签，故此处不对 `Connected` 特别上色。
fn row_base_bg(theme: &Theme, status: ConnectionStatus) -> Color {
    let p = custom_palette(theme);
    match status {
        ConnectionStatus::Error => Color::from_rgb(0.45, 0.20, 0.20),
        _ => p.surface,
    }
}

/// 列表行样式：选中优先（主题强调色淡底），其次悬浮高亮，否则取按连接状态的常态底色。
///
/// `theme` 为当前主题，用于派生语义色；`selected` 为是否当前选中项；`hovered` 为是否鼠标
/// 悬浮；`status` 用于错误态底色。会话列表无选中概念时传 `selected = false`，SFTP 无连接
/// 错误态时传 [`ConnectionStatus::Disconnected`]。
///
/// 选中态背景取 [`accent_active_bg`]（当前主题强调色以 0.28 不透明度叠加），与设置弹窗选中
/// 分类一致、淡而克制；文本色沿用主题默认文本色，确保淡底上文字清晰可读。
pub fn list_row_bg(
    theme: &Theme,
    selected: bool,
    hovered: bool,
    status: ConnectionStatus,
) -> iced::widget::container::Style {
    if selected {
        plain_background(accent_active_bg(theme))
    } else if hovered {
        plain_background(custom_palette(theme).hover)
    } else {
        plain_background(row_base_bg(theme, status))
    }
}

/// 把带 alpha 的前景色合成到不透明背景色上，返回不透明结果色。
///
/// 仅用于语义色派生；`foreground.a` 先夹到 `0.0..=1.0`，结果的 alpha 恒为 1。
fn blend_over(foreground: Color, background: Color) -> Color {
    let a = foreground.a.clamp(0.0, 1.0);
    Color::from_rgb(
        foreground.r * a + background.r * (1.0 - a),
        foreground.g * a + background.g * (1.0 - a),
        foreground.b * a + background.b * (1.0 - a),
    )
}

/// 列表行的**不透明**底色：与 [`list_row_bg`] 取色一致，但把选中态的半透明强调色
/// 预先合成到常态行底上。
///
/// [`accent_active_bg`] 带 0.28 不透明度；若直接拿它当文字淡出遮罩的终点色，遮罩
/// 末端最多只能遮住 28% 的文字，会残留半截字。行底与淡出终点共用本函数后两者
/// 严格同色，遮罩也能完全淡入背景。默认 / 悬停态本就不透明，取色不变。
///
/// 只影响需要精确衔接文字淡出的视图（如 SFTP 文件列表）；会话列表仍走
/// [`list_row_bg`]，外观不受影响。
pub fn list_row_solid_bg(
    theme: &Theme,
    selected: bool,
    hovered: bool,
    status: ConnectionStatus,
) -> Color {
    let base = row_base_bg(theme, status);
    if selected {
        blend_over(accent_active_bg(theme), base)
    } else if hovered {
        custom_palette(theme).hover
    } else {
        base
    }
}

/// 图标按钮样式：以背景色区分选中态与悬停态，并带圆角。
///
/// `theme` 用于派生悬停态的语义色；`active` 表示当前选中（如活动栏当前视图）；其余情况
/// 随 [`button::Status`] 在悬停 / 按下时显示浅色背景。
pub fn icon_button_style(
    theme: &Theme,
    status: button::Status,
    active: bool,
) -> iced::widget::button::Style {
    let background = if active {
        Some(accent_active_bg(theme).into())
    } else {
        match status {
            button::Status::Hovered | button::Status::Pressed => {
                Some(custom_palette(theme).hover.into())
            }
            _ => None,
        }
    };
    iced::widget::button::Style {
        background,
        border: iced::Border {
            radius: ICON_BTN_RADIUS.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// 卡片式按钮样式：底色与边框均取同区卡片色（`surface` / `border`），悬停 / 按下时底色提亮一档。
///
/// 用于表单内嵌的次要动作按钮（如「新增跳板机」）：与周围卡片同底同框而不喧宾夺主，
/// 悬停时以底色变化提示可点击。
pub fn card_button_style(theme: &Theme, status: button::Status) -> iced::widget::button::Style {
    let p = custom_palette(theme);
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => p.hover,
        _ => p.surface,
    };
    iced::widget::button::Style {
        background: Some(background.into()),
        border: iced::Border {
            color: p.border,
            width: 1.0,
            radius: ICON_BTN_RADIUS.into(),
        },
        text_color: p.text_secondary,
        ..Default::default()
    }
}

/// 下拉框样式：跟随主题底色 + 圆角边框（设置弹窗 / 会话编辑弹窗等表单复用）。
///
/// 三态区分（按 `status` 取值）：展开时以强调色描边并把右侧
/// 箭头一并染成强调色（与下拉层呼应），悬浮时边框提亮一档，常态用输入区边框色。
pub fn pick_list_style(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let p = custom_palette(theme);
    let pal = theme.extended_palette();
    let accent = accent_color(theme);
    let opened = matches!(status, pick_list::Status::Opened { .. });
    let border_color = match status {
        pick_list::Status::Opened { .. } => accent,
        pick_list::Status::Hovered => input_border_hover(theme),
        pick_list::Status::Active => input_border_color(theme),
    };
    pick_list::Style {
        text_color: pal.background.base.text,
        placeholder_color: p.text_secondary,
        handle_color: if opened { accent } else { p.text_secondary },
        // 与输入框同底（窗口主背景色）：在抬升面板上呈内嵌观感，边界清晰可辨。
        background: iced::Background::Color(pal.background.base.color),
        border: iced::Border {
            color: border_color,
            width: 1.0,
            radius: 6.0.into(),
        },
    }
}

/// Tooltip 样式：抬升表面背景 + 次要文本色 + 细边框，随主题自适应。
pub fn tooltip_style(theme: &Theme) -> iced::widget::container::Style {
    let p = custom_palette(theme);
    iced::widget::container::Style {
        text_color: Some(p.text_secondary),
        background: Some(p.surface_raised.into()),
        border: iced::Border {
            radius: 6.0.into(),
            width: 1.0,
            color: p.border,
        },
        ..Default::default()
    }
}

/// `pane_grid` 样式：分隔条以 [`accent_color`] 着色，悬停 / 拖拽时高亮，与整体主题一致。
pub fn pane_grid_style(_theme: &Theme) -> iced::widget::pane_grid::Style {
    iced::widget::pane_grid::Style {
        hovered_region: iced::widget::pane_grid::Highlight {
            background: iced::Background::Color(iced::Color::TRANSPARENT),
            border: iced::Border {
                width: 2.0,
                color: accent_color(_theme),
                ..Default::default()
            },
        },
        picked_split: iced::widget::pane_grid::Line {
            color: accent_color(_theme),
            width: 3.0,
        },
        hovered_split: iced::widget::pane_grid::Line {
            color: accent_color(_theme),
            width: 3.0,
        },
    }
}

/// 左右分割线的颜色：比通用 `border` 更暗 / 更淡（深色 `bg+0.10`、浅色 `bg-0.08`），
/// 降低分隔线的存在感，避免其抢夺视觉焦点。
pub fn pane_divider_color(theme: &Theme) -> Color {
    shift_from_bg(theme, 0.10, 0.08)
}

/// 标签条样式（VS Code / Zed 形态）：矩形标签满高贴合条带，活动标签与下方内容区同色融合。
///
/// - 非活动：透明底（透出条带 surface 底色），标题用次要文本色；悬浮 / 按下以 hover 色反馈。
/// - 活动：底色取 `merge_bg`（终端背景或首页内容底色），与下方内容区连成一体；标题色按
///   `merge_bg` 亮度派生（深底浅字 / 浅底深字），未聚焦时降一档次要色，示意焦点不在终端。
/// - `flash`（响铃视觉提示）优先级最高：实底强调色盖过上述所有状态。
///
/// 顶部聚焦指示条由渲染层拼装（iced `Border` 无法单边，见 `terminal_pane`），此处不画边框。
pub fn tab_style(
    theme: &Theme,
    status: button::Status,
    active: bool,
    focused: bool,
    flash: bool,
    merge_bg: Color,
) -> iced::widget::button::Style {
    if flash {
        return iced::widget::button::Style {
            background: Some(accent_color(theme).into()),
            // 实底强调色下固定白字：跟随主题的文本色在浅色主题是深字，会与实底糊在一起。
            text_color: Color::WHITE,
            ..Default::default()
        };
    }
    if active {
        let (primary, secondary) = merge_text_colors(merge_bg);
        return iced::widget::button::Style {
            background: Some(merge_bg.into()),
            text_color: if focused { primary } else { secondary },
            ..Default::default()
        };
    }
    let p = custom_palette(theme);
    let (background, text_color) = match status {
        button::Status::Hovered | button::Status::Pressed => (Some(p.hover), theme.palette().text),
        _ => (None, p.text_secondary),
    };
    iced::widget::button::Style {
        background: background.map(iced::Background::Color),
        text_color,
        ..Default::default()
    }
}

/// 融合底色上的主 / 次文本色：按底色亮度取深字或浅字，供活动标签与终端底色上的错误面板使用。
///
/// 终端背景随终端配色主题独立于程序主题，文本不能取程序主题文本色——深色程序 + 浅色终端
/// 会把浅字铺在浅底上，故按融合底色自身亮度判定。
pub fn merge_text_colors(bg: Color) -> (Color, Color) {
    let luminance = 0.299 * bg.r + 0.587 * bg.g + 0.114 * bg.b;
    if luminance > 0.5 {
        (
            Color::from_rgb(0.13, 0.13, 0.15),
            Color::from_rgb(0.42, 0.42, 0.45),
        )
    } else {
        (
            Color::from_rgb(0.93, 0.93, 0.95),
            Color::from_rgb(0.68, 0.68, 0.72),
        )
    }
}

/// 融合底色上的中性悬停底：按传入底色亮度推一档（深 +0.10 / 浅 -0.06），与程序主题无关。
pub fn merge_hover_color(bg: Color) -> Color {
    let luminance = 0.299 * bg.r + 0.587 * bg.g + 0.114 * bg.b;
    let delta = if luminance > 0.5 { -0.06 } else { 0.10 };
    Color::from_rgb(
        (bg.r + delta).clamp(0.0, 1.0),
        (bg.g + delta).clamp(0.0, 1.0),
        (bg.b + delta).clamp(0.0, 1.0),
    )
}

/// 标签列表 dropdown 内的行样式：活动行常显强调底色，悬浮 / 按下再加深一档。
///
/// 不复用 [`tab_style`]：其活动底是终端融合色、非活动底透明，均无法在抬升面板内
/// 提供「强调选中 + 悬浮加深」的层级（面板底 `surface_raised` 与 `hover` 同值）。
pub fn tab_list_row_style(
    theme: &Theme,
    status: button::Status,
    active: bool,
) -> iced::widget::button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed if active => {
            // 活动行加深：强调色不透明度由 0.28 提到 0.45，文本色不变仍可读。
            Some(accent_strong_bg(theme))
        }
        button::Status::Hovered | button::Status::Pressed => {
            Some(custom_palette(theme).hover_raised)
        }
        _ if active => Some(accent_active_bg(theme)),
        _ => None,
    };
    iced::widget::button::Style {
        background: background.map(iced::Background::Color),
        text_color: theme.palette().text,
        border: iced::Border {
            radius: TAB_RADIUS.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// 标签关闭按钮（dismiss 图标）样式：悬停 / 按下铺中性 hover 底，常态透明。
pub fn tab_close_style(theme: &Theme, status: button::Status) -> iced::widget::button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => {
            Some(custom_palette(theme).hover.into())
        }
        _ => None,
    };
    iced::widget::button::Style {
        background,
        border: iced::Border {
            radius: 4.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// 错误面板标题颜色：按所铺底色亮度取红——深底亮红、浅底标准红。
///
/// 错误面板铺在终端背景上（独立于程序主题），故按传入底色而非程序主题判定。
pub fn error_title_color(bg: Color) -> Color {
    let luminance = 0.299 * bg.r + 0.587 * bg.g + 0.114 * bg.b;
    if luminance > 0.5 {
        Color::from_rgb(0.8, 0.2, 0.2)
    } else {
        Color::from_rgb(0.95, 0.45, 0.45)
    }
}

/// 错误详情文字颜色：按所铺底色亮度取红——深底浅红、浅底深红。
pub fn error_detail_color(bg: Color) -> Color {
    let luminance = 0.299 * bg.r + 0.587 * bg.g + 0.114 * bg.b;
    if luminance > 0.5 {
        Color::from_rgb(0.55, 0.35, 0.35)
    } else {
        Color::from_rgb(0.85, 0.7, 0.7)
    }
}

/// 标签栏水平滚动条样式：无轨道底色、细圆角滑块，默认态低调、悬停 / 拖拽时加深。
pub fn tab_scrollable_style(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let p = custom_palette(theme);
    let scroller = match status {
        scrollable::Status::Active { .. } => p.border,
        scrollable::Status::Hovered { .. } | scrollable::Status::Dragged { .. } => p.text_secondary,
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: scrollable::Rail {
            background: None,
            border: iced::Border::default(),
            scroller: scrollable::Scroller {
                background: Color::TRANSPARENT.into(),
                border: iced::Border::default(),
            },
        },
        horizontal_rail: scrollable::Rail {
            background: None,
            border: iced::Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            scroller: scrollable::Scroller {
                background: scroller.into(),
                border: iced::Border {
                    radius: 2.0.into(),
                    ..Default::default()
                },
            },
        },
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: p.surface_raised.into(),
            border: iced::Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            shadow: iced::Shadow::default(),
            icon: p.text_secondary,
        },
    }
}

/// 细滚动条样式：不画轨道底色，滑块半透明，悬停 / 拖拽时加深。
pub fn thin_scrollbar_style(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let p = custom_palette(theme);
    let (color, alpha) = match status {
        scrollable::Status::Active { .. } => (p.border, 0.4),
        scrollable::Status::Hovered {
            is_vertical_scrollbar_hovered: true,
            ..
        }
        | scrollable::Status::Dragged {
            is_vertical_scrollbar_dragged: true,
            ..
        } => (p.text_secondary, 0.75),
        _ => (p.border, 0.5),
    };
    let rail = scrollable::Rail {
        background: None,
        border: iced::Border::default(),
        scroller: scrollable::Scroller {
            background: Color { a: alpha, ..color }.into(),
            border: iced::Border {
                radius: 3.0.into(),
                ..Default::default()
            },
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: p.surface_raised.into(),
            border: iced::Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            shadow: iced::Shadow::default(),
            icon: p.text_secondary,
        },
    }
}

/// 标签列表 dropdown 面板样式：抬升表面 + 细边框 + 圆角，与 tooltip 视觉一致。
pub fn dropdown_panel_style(theme: &Theme) -> container::Style {
    let p = custom_palette(theme);
    container::Style {
        background: Some(p.surface_raised.into()),
        border: iced::Border {
            radius: 8.0.into(),
            width: 1.0,
            color: p.border,
        },
        ..Default::default()
    }
}
