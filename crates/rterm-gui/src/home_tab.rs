//! 首页标签内容：标题区、收藏卡片网格与最近连接列表。
//!
//! 终端区标签栏的首个标签，默认打开；收藏随会话文件走，最近连接随配置走，点击卡片即连接。

use crate::icons::Icon;
use crate::t;

use crate::App;
use crate::app::session::Message as SessionMessage;
use crate::app::tabs;
use crate::message::Message;
use crate::theme;
use iced::widget::grid::{Grid, Sizing};
use iced::widget::svg;
use iced::widget::text::Wrapping;
use iced::widget::{Column, Space, button, column, container, responsive, row, text};
use iced::{Background, Border, Color, Element, Length, Padding, Size, Theme};
use rterm_config::SessionConfig;

/// 标题区高度（像素）。
const TITLE_AREA_HEIGHT: f32 = 88.0;
/// 内容区左右内边距（像素）。
const CONTENT_PADDING_X: f32 = 32.0;
/// 首个分区距分割线的距离（像素）。
const CONTENT_PADDING_TOP: f32 = 24.0;
/// 收藏卡片网格的卡片最小宽度（像素）：卡片窄于此宽度会挤，窄窗时改少排几列。
const CARD_MIN_WIDTH: f32 = 320.0;
/// 收藏卡片网格的行列间距（像素）。
const GRID_SPACING: f32 = 16.0;
/// 收藏卡片高度（像素）：容纳首行（头像）与底部分组标签两行内容。
const CARD_HEIGHT: f32 = 84.0;
/// 收藏卡片圆角（像素）。
const CARD_RADIUS: f32 = 10.0;
/// 头像边长（像素）。
const AVATAR_SIZE: f32 = 32.0;
/// 状态点直径（像素）。
const STATUS_DOT_SIZE: f32 = 6.0;
/// 星标按钮边长（像素）。
const STAR_SLOT_SIZE: f32 = 18.0;
/// 右上角星标与状态点的间距（像素）。
const CORNER_SPACING: f32 = 8.0;
/// 最近连接行高（像素）。
const RECENT_ROW_HEIGHT: f32 = 40.0;
/// 「最近连接」区展示的最大条数。
const RECENT_DISPLAY_LIMIT: usize = 5;
/// 会话总数不超过该值时不展示「最近连接」区（小数据量下最近区纯噪音）。
const RECENT_SECTION_MIN_SESSIONS: usize = 10;

/// 渲染首页内容：标题区 + 收藏区 + 最近连接区。
pub fn view(app: &App) -> Element<'_, Message> {
    let favorites = favorite_sessions(app);
    let recent = recent_sessions(app);

    let mut sections: Vec<Element<'_, Message>> = Vec::new();

    // 收藏区：用户显式收藏的会话，按名称排序。
    sections.push(section(
        app,
        Icon::StarOutline,
        t!("session.favorites"),
        favorites.len(),
        favorites_area(app, favorites),
    ));

    // 最近连接区：会话总数不足门槛时不展示（小数据量下最近区纯噪音）。
    if app.session.sessions.len() > RECENT_SECTION_MIN_SESSIONS {
        sections.push(section(
            app,
            Icon::Clock,
            t!("session.recent"),
            recent.len(),
            if recent.is_empty() {
                empty_state(Icon::Clock, t!("session.recent_empty"))
            } else {
                recent_list(app, &recent)
            },
        ));
    }

    let body = content_box(
        Column::with_children(sections)
            .spacing(32)
            .padding([CONTENT_PADDING_TOP, 0.0])
            .width(Length::Fill),
    );
    let divider: Element<'_, Message> = content_box(crate::ui::hairline()).into();
    container(
        column![
            title_area(app),
            divider,
            crate::ui::thin_vertical_scrollbar(body).height(Length::Fill),
        ]
        .width(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// 内容盒：左右各留 [`CONTENT_PADDING_X`]，内容随终端区宽度铺满。
///
/// 标题行、分割线、正文三块共用同一栅格，宽度一致。
fn content_box<'a>(content: impl Into<Element<'a, Message>>) -> container::Container<'a, Message> {
    container(content)
        .width(Length::Fill)
        .padding([0.0, CONTENT_PADDING_X])
}

/// 标题区：房子图标 + 标题 / 副标题。
fn title_area(app: &App) -> Element<'_, Message> {
    let house = Icon::Home.svg(16.0).style(|theme: &Theme, _| svg::Style {
        color: Some(theme::accent_color(theme)),
    });
    let heading = column![
        text(t!("home.title")).size(18).font(semibold_font(app)),
        text(t!("home.subtitle"))
            .size(12)
            .style(|theme: &Theme| iced::widget::text::Style {
                color: Some(theme::custom_palette(theme).text_secondary),
            }),
    ]
    .spacing(2);

    let title_row = row![house, heading]
        .spacing(10)
        .align_y(iced::alignment::Vertical::Center)
        .width(Length::Fill);

    content_box(title_row)
        .height(TITLE_AREA_HEIGHT)
        .align_y(iced::alignment::Vertical::Center)
        .into()
}

/// 一个分区：分区头（图标 + 标题 + 计数）+ 内容。
fn section<'a>(
    app: &'a App,
    icon: Icon,
    title: String,
    count: usize,
    content: Element<'a, Message>,
) -> Element<'a, Message> {
    column![section_head(app, icon, title, count), content]
        .spacing(12)
        .width(Length::Fill)
        .into()
}

/// 分区头：图标 + 标题 + 计数徽标。
fn section_head<'a>(app: &'a App, icon: Icon, title: String, count: usize) -> Element<'a, Message> {
    row![
        icon.svg(16.0),
        text(title).size(14).font(semibold_font(app)),
        count_pill(app, count),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center)
    .into()
}

/// 计数徽标：强调色文字 + 淡强调底。
fn count_pill(app: &App, count: usize) -> Element<'_, Message> {
    let font = semibold_font(app);
    container(text(count.to_string()).size(12).font(font))
        .padding([2, 6])
        .style(|theme: &Theme| container::Style {
            background: Some(Background::Color(Color {
                a: 0.16,
                ..theme::accent_color(theme)
            })),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            text_color: Some(theme::accent_color(theme)),
            ..Default::default()
        })
        .into()
}

/// 收藏区内容：卡片网格始终渲染，无收藏时高度为零（保留悬浮态载体），空时另附提示。
fn favorites_area<'a>(app: &'a App, favorites: Vec<&'a SessionConfig>) -> Element<'a, Message> {
    let empty = favorites.is_empty();
    let mut children = vec![favorites_grid(app, favorites)];
    if empty {
        children.push(empty_state(
            Icon::StarOutline,
            t!("session.favorites_empty"),
        ));
    }
    Column::with_children(children).width(Length::Fill).into()
}

/// 收藏卡片网格：卡片内含头像、名称 / 地址、分组标签与状态点。
///
/// 列数随终端区可用宽度自适应，卡片宽度保持在 [`CARD_MIN_WIDTH`] 上下。
fn favorites_grid<'a>(app: &'a App, favorites: Vec<&'a SessionConfig>) -> Element<'a, Message> {
    // `Responsive` 在布局时把可用宽度交给闭包，据此推算列数。
    responsive(move |size: Size| {
        let columns = grid_columns(size.width);
        let grid = Grid::with_children(
            favorites
                .iter()
                .enumerate()
                .map(|(slot, s)| favorite_card(app, s, slot)),
        )
        .columns(columns)
        .spacing(GRID_SPACING)
        .height(Sizing::EvenlyDistribute(Length::Shrink));

        // 网格整体套一层 mouse_area：光标离开收藏区时清空悬浮槽位。
        iced::widget::mouse_area(grid)
            .on_exit(Message::Tabs(tabs::Message::HomeLeave))
            .into()
    })
    .height(Length::Shrink)
    .into()
}

/// 网格列数：按可用宽度与卡片最小宽度推算，至少一列。
fn grid_columns(width: f32) -> usize {
    let per_column = CARD_MIN_WIDTH + GRID_SPACING;
    (((width + GRID_SPACING) / per_column).floor().max(1.0)) as usize
}

/// 收藏卡片：点击即连接；卡片悬浮时右上角浮现星标（点击取消收藏）。
fn favorite_card<'a>(app: &'a App, s: &'a SessionConfig, slot: usize) -> Element<'a, Message> {
    let id = s.id.clone();
    let hovered = app.tabs.home_hovered() == Some(slot);

    let body = column![
        row![
            row![avatar(app, s), info_column(app, s, 3.0)]
                .spacing(12)
                .align_y(iced::alignment::Vertical::Center)
                .width(Length::Fill),
            // 右上角簇：星标与状态点顶对齐到卡片内容顶边。
            row![star_slot(&id, hovered), status_dot(app, &id)]
                .spacing(CORNER_SPACING)
                .align_y(iced::alignment::Vertical::Top),
        ]
        .spacing(12)
        .align_y(iced::alignment::Vertical::Top),
        // 弹性留白把分组标签压到卡片底部。
        Space::new().height(Length::Fill),
        row![Space::new().width(Length::Fill), group_tag(s)].spacing(6),
    ]
    .spacing(4);

    let card = button(
        container(body)
            .height(CARD_HEIGHT)
            .width(Length::Fill)
            .padding([12, 14]),
    )
    .on_press(Message::Session(SessionMessage::ConnectSession(id.clone())))
    .width(Length::Fill)
    .style(card_style);

    // 悬浮态由 mouse_area 单独跟踪：卡片按钮自身只负责点击与按压反馈。
    iced::widget::mouse_area(card)
        .on_enter(Message::Tabs(tabs::Message::HomeHover(slot)))
        .on_exit(Message::Tabs(tabs::Message::HomeUnhover(slot)))
        .interaction(iced::mouse::Interaction::Pointer)
        .into()
}

/// 收藏卡片样式：常态卡片底 + 细边框，悬浮提亮，按下压暗并以强调色描边。
fn card_style(theme: &Theme, status: iced::widget::button::Status) -> iced::widget::button::Style {
    let p = theme::custom_palette(theme);
    let (background, border) = match status {
        iced::widget::button::Status::Hovered => (p.hover, p.border),
        iced::widget::button::Status::Pressed => (p.surface, theme::accent_color(theme)),
        _ => (p.surface, p.border),
    };
    iced::widget::button::Style {
        background: Some(Background::Color(background)),
        text_color: theme.palette().text,
        border: Border {
            radius: CARD_RADIUS.into(),
            width: 1.0,
            color: border,
        },
        ..Default::default()
    }
}

/// 头像：名称首字母 + 淡强调底。
fn avatar<'a>(app: &App, s: &'a SessionConfig) -> Element<'a, Message> {
    let font = semibold_font(app);
    container(
        text(initial_letter(&s.name))
            .size(14)
            .font(font)
            .style(|theme: &Theme| iced::widget::text::Style {
                color: Some(theme::accent_color(theme)),
            }),
    )
    .width(AVATAR_SIZE)
    .height(AVATAR_SIZE)
    .align_x(iced::alignment::Horizontal::Center)
    .align_y(iced::alignment::Vertical::Center)
    .style(|theme: &Theme| container::Style {
        background: Some(Background::Color(Color {
            a: 0.14,
            ..theme::accent_color(theme)
        })),
        border: Border {
            radius: 8.0.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

/// 名称 + `user@host` 两行文本，超宽截断。
fn info_column<'a>(app: &'a App, s: &'a SessionConfig, spacing: f32) -> Element<'a, Message> {
    container(
        column![
            text(&s.name)
                .size(14)
                .font(semibold_font(app))
                .wrapping(Wrapping::None),
            text(format!("{}@{}", s.username, s.host))
                .size(12)
                .wrapping(Wrapping::None)
                .style(|theme: &Theme| iced::widget::text::Style {
                    color: Some(theme::custom_palette(theme).text_secondary),
                }),
        ]
        .spacing(spacing),
    )
    .width(Length::Fill)
    .clip(true)
    .into()
}

/// 星标槽位：悬浮时显示星标按钮（取消收藏），否则留空占位。
///
/// 槽位 18×18，图标 12×12 顶对齐槽位（按钮下内边距 6px 抵掉居中余量）。
fn star_slot(id: &str, hovered: bool) -> Element<'static, Message> {
    let content: Element<'static, Message> = if !hovered {
        Space::new().into()
    } else {
        let star = Icon::Star.svg(12.0).style(|theme: &Theme, _| svg::Style {
            color: Some(theme::accent_color(theme)),
        });
        let btn = button(star)
            .on_press(Message::Session(SessionMessage::ToggleFavorite(
                id.to_string(),
            )))
            .padding(Padding::new(0.0).top(0.0).right(3.0).bottom(6.0).left(3.0))
            .style(|theme: &Theme, st| theme::icon_button_style(theme, st, false))
            .width(Length::Fixed(STAR_SLOT_SIZE))
            .height(Length::Fixed(STAR_SLOT_SIZE));
        crate::ui::hover_tooltip(
            btn,
            t!("session.unfavorite"),
            iced::widget::tooltip::Position::Bottom,
        )
    };
    container(content)
        .width(Length::Fixed(STAR_SLOT_SIZE))
        .height(Length::Fixed(STAR_SLOT_SIZE))
        .align_x(iced::alignment::Horizontal::Center)
        .align_y(iced::alignment::Vertical::Center)
        .into()
}

/// 连接状态点：已连接为绿、连接中为黄；未连接时留同样大小的空位，避免状态变化时卡片内容抖动。
fn status_dot(app: &App, id: &str) -> Element<'static, Message> {
    let color = match app.session_status(id) {
        rterm_core::ConnectionStatus::Connected => crate::ui::SUCCESS,
        rterm_core::ConnectionStatus::Connecting => crate::ui::WARNING,
        _ => {
            return Space::new()
                .width(Length::Fixed(STATUS_DOT_SIZE))
                .height(Length::Fixed(STATUS_DOT_SIZE))
                .into();
        }
    };
    Icon::Circle
        .svg_with_color(STATUS_DOT_SIZE, color)
        .width(Length::Fixed(STATUS_DOT_SIZE))
        .height(Length::Fixed(STATUS_DOT_SIZE))
        .into()
}

/// 分组标签：会话所属分组（未分组则不显示）。
fn group_tag(s: &SessionConfig) -> Element<'static, Message> {
    let Some(group) = s.group.as_deref().filter(|g| !g.is_empty()) else {
        return Space::new().into();
    };
    container(text(group.to_string()).size(11))
        .padding([2, 6])
        .style(|theme: &Theme| container::Style {
            background: Some(Background::Color(overlay_color(theme, 0.06))),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            text_color: Some(theme::custom_palette(theme).text_secondary),
            ..Default::default()
        })
        .into()
}

/// 最近连接列表：紧凑行，右侧显示相对时间与分组标签。
fn recent_list<'a>(app: &'a App, recent: &[(&'a SessionConfig, i64)]) -> Element<'a, Message> {
    Column::with_children(recent.iter().map(|(s, unix)| recent_row(app, s, *unix)))
        .spacing(2)
        .into()
}

/// 最近连接行：点击即连接。
fn recent_row<'a>(app: &'a App, s: &'a SessionConfig, unix: i64) -> Element<'a, Message> {
    let id = s.id.clone();
    let clock = Icon::Clock.svg(16.0);
    let time =
        text(relative_time(unix))
            .size(12)
            .style(|theme: &Theme| iced::widget::text::Style {
                color: Some(theme::custom_palette(theme).text_secondary),
            });

    button(
        row![
            clock,
            info_column(app, s, 1.0),
            row![time, group_tag(s)].spacing(10),
        ]
        .spacing(12)
        .align_y(iced::alignment::Vertical::Center),
    )
    .on_press(Message::Session(SessionMessage::ConnectSession(id)))
    .width(Length::Fill)
    .height(RECENT_ROW_HEIGHT)
    .padding([0, 10])
    .style(recent_row_style)
    .into()
}

/// 最近连接行样式：常态透明，悬浮铺卡片底色，按下压暗。
fn recent_row_style(
    theme: &Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    let p = theme::custom_palette(theme);
    let background = match status {
        iced::widget::button::Status::Hovered => Some(Background::Color(p.hover)),
        iced::widget::button::Status::Pressed => Some(Background::Color(p.surface)),
        _ => None,
    };
    iced::widget::button::Style {
        background,
        text_color: theme.palette().text,
        border: Border {
            radius: 8.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// 空状态：图标 + 一行提示。
fn empty_state(icon: Icon, hint: String) -> Element<'static, Message> {
    let icon = icon.svg(24.0).opacity(0.3_f32);
    container(
        column![
            icon,
            text(hint)
                .size(12)
                .style(|theme: &Theme| iced::widget::text::Style {
                    color: Some(theme::custom_palette(theme).text_tertiary),
                }),
        ]
        .spacing(10)
        .width(Length::Fill)
        .align_x(iced::alignment::Horizontal::Center),
    )
    .width(Length::Fill)
    .padding([40, 0])
    .into()
}

/// 收藏的会话：用户显式收藏者，按名称排序。
fn favorite_sessions(app: &App) -> Vec<&SessionConfig> {
    let mut favorites: Vec<&SessionConfig> =
        app.session.sessions.iter().filter(|s| s.favorite).collect();
    favorites.sort_by(|a, b| a.name.cmp(&b.name));
    favorites
}

/// 最近连接条目：取记录前若干条，与现存会话求交（已删除的会话不再出现），保持时间倒序。
fn recent_sessions(app: &App) -> Vec<(&SessionConfig, i64)> {
    app.config
        .session_list
        .recent_sessions
        .iter()
        .take(RECENT_DISPLAY_LIMIT)
        .filter_map(|r| {
            app.session
                .sessions
                .iter()
                .find(|s| s.id == r.id)
                .map(|s| (s, r.unix))
        })
        .collect()
}

/// 相对时间文案：按本地日历把连接时间折算成「刚刚 / N 分钟前 / 昨天 HH:MM / N 天前 …」。
fn relative_time(unix: i64) -> String {
    use chrono::{Datelike, TimeZone};

    // 夏令时回拨那一小时有两个本地时刻，取较早者；超出本地时间范围才算未知。
    let Some(dt) = chrono::Local.timestamp_opt(unix, 0).earliest() else {
        return t!("home.time_unknown");
    };
    let now = chrono::Local::now();
    let diff = now - dt;
    // 时钟回拨导致记录时间落在未来时，按「刚刚」处理。
    if diff < chrono::Duration::zero() {
        return t!("home.time_now");
    }
    let today = now.date_naive();
    let day = dt.date_naive();
    if day == today {
        let minutes = diff.num_minutes();
        if minutes < 1 {
            return t!("home.time_now");
        }
        if minutes < 60 {
            return t!("home.time_minutes", count => minutes);
        }
        return t!("home.time_hours", count => diff.num_hours());
    }
    if day == today - chrono::Duration::days(1) {
        return t!("home.time_yesterday", time => dt.format("%H:%M").to_string());
    }
    let days = diff.num_days();
    if days < 7 {
        return t!("home.time_days", count => days);
    }
    let months = (today.year() - day.year()) * 12 + today.month() as i32 - day.month() as i32;
    if days < 30 {
        return t!("home.time_weeks", count => diff.num_weeks());
    }
    if months < 12 {
        return t!("home.time_months", count => months.max(1));
    }
    t!("home.time_years", count => months / 12)
}

/// 名称首字母：头像里显示的字符。
fn initial_letter(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
}

/// 标题 / 名称用的半粗字重：沿用用户选择的界面字体族，仅加重字重。
fn semibold_font(app: &App) -> iced::Font {
    iced::Font {
        weight: iced::font::Weight::Semibold,
        ..crate::font::resolve_font(&app.config.appearance.ui_font)
    }
}

/// 半透明色块：深色主题叠白、浅色主题叠黑。
fn overlay_color(theme: &Theme, alpha: f32) -> Color {
    let bg = theme.palette().background;
    let luminance = 0.299 * bg.r + 0.587 * bg.g + 0.114 * bg.b;
    if luminance > 0.5 {
        Color::from_rgba(0.0, 0.0, 0.0, alpha)
    } else {
        Color::from_rgba(1.0, 1.0, 1.0, alpha)
    }
}
