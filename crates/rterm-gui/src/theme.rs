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

/// 强调色（按钮、选中态等）。
pub const ACCENT: Color = Color::from_rgb(0.20, 0.55, 0.95);

/// 选中态（活动栏当前视图 / 聚焦标签 / 设置导航当前项 / 设置弹窗当前分类）的强调底色：
/// 主题强调色以低不透明度叠加，其上文本仍用默认文本色即可保持可读。
pub fn primary_active_bg(theme: &Theme) -> Color {
    Color {
        a: 0.28,
        ..theme.extended_palette().primary.base.color
    }
}

/// 图标按钮圆角半径（像素）。
pub const ICON_BTN_RADIUS: f32 = 6.0;

/// 标签圆角半径（像素）。
pub const TAB_RADIUS: f32 = 8.0;

/// 终端背景兜底色（`#181818`），与终端配色板 `ColorPalette::default()` 的背景一致。
///
/// 仅作 [`terminal_bg`] 解析失败时的兜底：终端留白实际取当前终端主题的 `background`，
/// 故本常量与「选中标签底色」无关（活动标签底色走 [`primary_active_bg`] / `hover`）。
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

/// 标签关闭按钮悬停时的红色背景。
pub const TAB_CLOSE_HOVER: Color = Color::from_rgba(0.85, 0.30, 0.30, 0.65);

/// tooltip 显示前的延迟（毫秒）。
pub const TOOLTIP_DELAY_MS: u64 = 200;

/// 左侧活动栏宽度（像素）。
pub const ACTIVITY_BAR_WIDTH: f32 = 48.0;

/// 自定义调色板，在 iced 主题默认颜色之外补充 UI 专用语义色。
///
/// 通过感知亮度公式判断背景深浅，对任意 iced 主题天然生效，无需为每个主题手工配色
/// （思路来自 s3dm）。
pub struct CustomPalette {
    /// 表面背景色（用于面板、列表行常态底等）。
    pub surface: Color,
    /// 抬升表面色（用于弹窗、悬浮元素、行悬浮态等）。
    pub surface_raised: Color,
    /// 次要文本 / 图标颜色（说明文字、图标等）。
    pub text_secondary: Color,
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

/// 列表行背景颜色：选中优先（强调色），其次悬浮高亮，否则取按连接状态的常态底色。
///
/// `theme` 为当前主题，用于派生语义色；`selected` 为是否当前选中项；`hovered` 为是否鼠标
/// 悬浮；`status` 用于错误态底色。会话列表无选中概念时传 `selected = false`，SFTP 无连接
/// 错误态时传 [`ConnectionStatus::Disconnected`]。
pub fn list_row_bg(
    theme: &Theme,
    selected: bool,
    hovered: bool,
    status: ConnectionStatus,
) -> Color {
    if selected {
        ACCENT
    } else if hovered {
        custom_palette(theme).hover
    } else {
        row_base_bg(theme, status)
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
        Some(primary_active_bg(theme).into())
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

/// 下拉框样式：跟随主题底色 + 圆角边框（设置弹窗 / 会话编辑弹窗等表单复用）。
pub fn pick_list_style(theme: &Theme, _status: pick_list::Status) -> pick_list::Style {
    let p = custom_palette(theme);
    let pal = theme.extended_palette();
    pick_list::Style {
        text_color: pal.background.base.text,
        placeholder_color: p.text_secondary,
        handle_color: p.text_secondary,
        background: iced::Background::Color(p.surface),
        border: iced::Border {
            color: p.border,
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

/// `pane_grid` 样式：分隔条以 [`ACCENT`] 着色，悬停 / 拖拽时高亮，与整体主题一致。
pub fn pane_grid_style(_theme: &Theme) -> iced::widget::pane_grid::Style {
    iced::widget::pane_grid::Style {
        hovered_region: iced::widget::pane_grid::Highlight {
            background: iced::Background::Color(iced::Color::TRANSPARENT),
            border: iced::Border {
                width: 2.0,
                color: ACCENT,
                ..Default::default()
            },
        },
        picked_split: iced::widget::pane_grid::Line {
            color: ACCENT,
            width: 3.0,
        },
        hovered_split: iced::widget::pane_grid::Line {
            color: ACCENT,
            width: 3.0,
        },
    }
}

/// 左右分割线的颜色：比通用 `border` 更暗 / 更淡（深色 `bg+0.10`、浅色 `bg-0.08`），
/// 降低分隔线的存在感，避免其抢夺视觉焦点。
pub fn pane_divider_color(theme: &Theme) -> Color {
    let bg = theme.palette().background;
    let luminance = 0.299 * bg.r + 0.587 * bg.g + 0.114 * bg.b;
    if luminance > 0.5 {
        Color::from_rgb(
            (bg.r - 0.08).max(0.0),
            (bg.g - 0.08).max(0.0),
            (bg.b - 0.08).max(0.0),
        )
    } else {
        Color::from_rgb(
            (bg.r + 0.10).min(1.0),
            (bg.g + 0.10).min(1.0),
            (bg.b + 0.10).min(1.0),
        )
    }
}

/// 标签按钮样式：整个标签（状态点 + 标题 + 关闭按钮）为单一按钮，悬浮 / 按下反馈覆盖全部区域。
///
/// 三态区分「选中」与「键盘聚焦」：
/// - 非活动：透明背景，悬浮 / 按下以主题 hover 色反馈。
/// - 活动且聚焦：强调色半透明底，表示按键会落入此终端。
/// - 活动但未聚焦：中性低饱和底（随主题 surface 派生），与「聚焦」态在色相 / 饱和度上明显不同，
///   避免用户误以为焦点仍在终端（如焦点已落在文件管理输入框）。
pub fn tab_style(
    theme: &Theme,
    status: button::Status,
    active: bool,
    focused: bool,
) -> iced::widget::button::Style {
    let background = if active {
        // 活动标签常亮高亮，不再叠加悬浮反馈。
        if focused {
            Some(primary_active_bg(theme).into())
        } else {
            Some(custom_palette(theme).hover.into())
        }
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
        text_color: theme.palette().text,
        border: iced::Border {
            radius: TAB_RADIUS.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// 标签列表 dropdown 内的行样式：活动行常显强调底色，悬浮 / 按下再加深一档。
///
/// 面板底色取 `surface_raised`，与 [`tab_style`] 用的 `hover` 同值，故此处不能用
/// `tab_style`：活动行（未聚焦分支取 `hover`）会与面板底色重合、悬浮行也看不出变化。
pub fn tab_list_row_style(
    theme: &Theme,
    status: button::Status,
    active: bool,
) -> iced::widget::button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed if active => {
            // 活动行加深：强调色不透明度由 0.28 提到 0.45，文本色不变仍可读。
            Some(Color {
                a: 0.45,
                ..theme.extended_palette().primary.base.color
            })
        }
        button::Status::Hovered | button::Status::Pressed => {
            Some(custom_palette(theme).hover_raised)
        }
        _ if active => Some(primary_active_bg(theme)),
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

/// 标签关闭按钮（dismiss 图标）样式：悬停 / 按下时显示红色背景并带圆角，常态透明。
pub fn tab_close_style(_theme: &Theme, status: button::Status) -> iced::widget::button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Some(TAB_CLOSE_HOVER.into()),
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
