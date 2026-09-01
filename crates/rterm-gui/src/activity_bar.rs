//! 最左侧固定活动栏。
//!
//! 提供三个切换按钮：会话管理（[`CenterView::Sessions`]）、文件管理（[`CenterView::Files`]）
//! 与传输（[`CenterView::Transfers`]），对应 VSCode 风格的活动栏。文件管理依附于终端标签自己的
//! SSH 连接，故未连接任何会话时该按钮禁用，并以图标禁止符号 + 鼠标禁止光标提示。

use crate::t;

use crate::App;
use crate::icons::Icon;
use crate::message::Message;
use crate::state::CenterView;
use crate::theme;
use iced::mouse::Interaction;
use iced::widget::space::vertical;
use iced::widget::tooltip;
use iced::widget::tooltip::Position;
use iced::widget::{button, column, container, mouse_area, text};
use iced::{Color, Element, Length};
use rterm_core::ConnectionStatus;

/// 活动栏按钮图标尺寸（px）。
const ACTIVITY_ICON_SIZE: f32 = 24.0;

/// 禁用态图标颜色（低对比灰，提示按钮当前不可用）。
const DISABLED_ICON_COLOR: Color = Color::from_rgb(0.45, 0.47, 0.50);

/// 渲染最左侧活动栏视图。
///
/// 含会话管理、文件管理、传输队列三个切换按钮及固定在底部的设置按钮；
/// 未连接任何终端会话时“文件管理”按钮禁用。
pub fn view(app: &App) -> Element<'_, Message> {
    // 是否存在已连接的标签：决定“文件管理”按钮是否可用（SFTP 建立在终端标签自己的连接上）。
    let any_connected = app
        .tabs
        .list()
        .iter()
        .any(|t| t.status == ConnectionStatus::Connected);
    let sessions_btn = activity_button(
        Icon::ListBar,
        app.center == CenterView::Sessions,
        true,
        t!("activity.sessions"),
        Message::SwitchCenter(CenterView::Sessions),
    );
    let files_btn = activity_button(
        Icon::FolderMultiple,
        app.center == CenterView::Files,
        any_connected,
        t!("activity.files"),
        Message::SwitchCenter(CenterView::Files),
    );
    // 传输按钮：切换到中心面板的传输队列视图（与“会话 / 文件”并列）。
    let transfer_btn = activity_button(
        Icon::ArrowSort,
        app.center == CenterView::Transfers,
        true,
        t!("activity.transfers"),
        Message::SwitchCenter(CenterView::Transfers),
    );
    // 设置按钮固定在活动栏最底部：用可拉伸空白把上方按钮与设置按钮分隔开。
    let settings_btn = settings_button(app.settings.show_settings);

    let content = column![
        sessions_btn,
        files_btn,
        transfer_btn,
        vertical(),
        settings_btn
    ]
    .spacing(4);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|theme| theme::plain_background(crate::theme::custom_palette(theme).surface))
        .padding(6)
        .into()
}

/// 使用 `settings-16-filled` 图标，点击切换设置弹窗。
///
/// `active` 表示设置弹窗当前是否打开，用于高亮当前选中态。
fn settings_button(active: bool) -> Element<'static, Message> {
    let btn = button(Icon::Settings.svg(ACTIVITY_ICON_SIZE))
        .on_press(Message::Settings(crate::app::settings::Message::Toggle))
        .width(Length::Fill)
        .style(move |theme, status| theme::icon_button_style(theme, status, active));
    tooltip(btn, text(t!("activity.settings")), Position::Right)
        .delay(iced::time::Duration::from_millis(theme::TOOLTIP_DELAY_MS))
        .style(theme::tooltip_style)
        .into()
}

/// 单个活动栏按钮（SVG 图标）。
///
/// `enabled` 为 `false` 时按钮禁用：图标灰显、点击无响应，并以鼠标禁止光标
/// （`Interaction::NotAllowed`）作为状态提示，不显示 tooltip。
fn activity_button(
    icon: Icon,
    active: bool,
    enabled: bool,
    tooltip_label: impl Into<String>,
    on_press: Message,
) -> Element<'static, Message> {
    // 禁用时改用带禁止符号的 folder-junk 图标灰显，作为不可用状态提示；
    // 启用时恢复传入的正常图标（如 FolderMultiple）。
    let icon_svg = if enabled {
        icon.svg(ACTIVITY_ICON_SIZE)
    } else {
        Icon::FolderJunk.svg_with_color(ACTIVITY_ICON_SIZE, DISABLED_ICON_COLOR)
    };
    let btn = button(icon_svg)
        .on_press_maybe(if enabled { Some(on_press) } else { None })
        .width(Length::Fill)
        .style(move |theme, status| {
            if enabled {
                theme::icon_button_style(theme, status, active)
            } else {
                iced::widget::button::Style::default()
            }
        });

    // 禁用时以鼠标禁止光标作为状态提示（不显示 tooltip）。
    if enabled {
        tooltip(btn, text(tooltip_label.into()), Position::Right)
            .delay(iced::time::Duration::from_millis(theme::TOOLTIP_DELAY_MS))
            .style(theme::tooltip_style)
            .into()
    } else {
        mouse_area(btn).interaction(Interaction::NotAllowed).into()
    }
}
