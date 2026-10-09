//! 右侧终端区域：多标签页 + 内嵌终端视图（`crate::widget::term`）。
//!
//! 每个标签对应一个 [`TerminalTab`]。标签栏用于切换 / 关闭，
//! 主体为 `TerminalView`；键盘 / 鼠标事件由 `TerminalView` 自身捕获并回传
//! [`tabs::Message::Terminal`](crate::app::tabs::Message::Terminal)（经顶层 `Message::Tabs` 路由）。

use crate::t;

use crate::App;
use crate::app::tabs;
use crate::icons::Icon;
use crate::message::Message;
use crate::state::TerminalTab;
use crate::widget::term::{Event as TerminalEvent, TerminalView};
use iced::alignment::Horizontal;
use iced::widget::tooltip::Position;
use iced::widget::{button, column, container, mouse_area, row, scrollable, text};
use iced::{Border, Color, Element, Length, Padding};
use rterm_core::{ConnectionStatus, DisconnectReason};

/// 标签最大宽度（px）：标题文本、状态点、关闭按钮与间距的总和上限。
pub(crate) const TAB_MAX_WIDTH: f32 = 160.0;
/// 标签内除标题文本外的固定宽度（px）：状态点 8、关闭按钮 16（12px 图标 + 四边内边距 2）、
/// 标签按钮自身左右内边距 12（`padding([4, 6])`）、行内间距 8（`row.spacing(4)`），
/// 行内共三个子元素、两个间隙；标题文本预算 = [`TAB_MAX_WIDTH`] − 本值。
const TAB_CHROME_WIDTH: f32 = 44.0;
/// 标题文本的最大显示宽度（px），超出截断加省略号。
const TAB_TEXT_MAX_WIDTH: f32 = TAB_MAX_WIDTH - TAB_CHROME_WIDTH;
/// 相邻标签的间距（px），滚动偏移估算与渲染共用同一常量。
pub(crate) const TAB_SPACING: f32 = 4.0;
/// 标签行左内边距（px），滚动偏移估算与渲染共用同一常量。
pub(crate) const TAB_ROW_LEFT_PADDING: f32 = 4.0;
/// 标签行底部内边距（px）：给横向滚动条留位置；
/// 下拉按钮按行居中时会被这段留白压低，故用它做等量补偿（见 `view` 中 `list_button`）。
const TAB_ROW_BOTTOM_PADDING: f32 = 4.0;
/// 标签栏左内边距（px），即下拉按钮到左分界的距离。
const TAB_BAR_LEFT_PADDING: f32 = 8.0;

/// 标签显示标题：同会话多标签时按打开顺序追加序号（如 `会话名 #2`），单标签不冗余。
pub(crate) fn tab_label(tabs: &[TerminalTab], tab: &TerminalTab) -> String {
    let same_session_count = tabs
        .iter()
        .filter(|t| t.session_id == tab.session_id)
        .count();
    if same_session_count <= 1 {
        return tab.title.clone();
    }
    let seq = tabs
        .iter()
        .filter(|t| t.session_id == tab.session_id)
        .position(|t| t.id == tab.id)
        .map(|i| i + 1)
        .unwrap_or(1);
    format!("{} #{}", tab.title, seq)
}

/// 估算标签的渲染宽度：未触及宽度上限时按内容估，触顶按 [`TAB_MAX_WIDTH`]。
///
/// 供切换标签后按索引累加滚动偏移；估算系数与 [`truncate_label`] 同源（12px 字号下
/// 全角字符 12px、其余 6px），两侧若失同步会导致滚动定位偏移。
pub(crate) fn estimated_tab_width(label: &str) -> f32 {
    let text = label.chars().map(char_units).sum::<usize>() as f32 * 6.0;
    (TAB_CHROME_WIDTH + text).min(TAB_MAX_WIDTH)
}

/// 按估算像素宽度截断标题，超限时以省略号结尾。
///
/// iced 文本部件无内置省略号截断，这里手动按字符宽度累计（与 [`estimated_tab_width`]
/// 同一估算模型）。
fn truncate_label(label: &str, max_px: f32) -> String {
    // 预留 2 个半角单位：省略号 `…`（U+2026）本身按 1 个单位计，多留 1 个单位作余量，
    // 避免末尾字符与省略号挤在一起。
    let budget = max_px / 6.0 - 2.0;
    let mut units = 0.0;
    let mut out = String::new();
    for ch in label.chars() {
        let w = char_units(ch) as f32;
        if units + w > budget {
            out.push('…');
            break;
        }
        units += w;
        out.push(ch);
    }
    out
}

/// 单字符的半角单位宽度：CJK / 全角区段记 2，其余记 1。
fn char_units(ch: char) -> usize {
    let c = ch as u32;
    let wide = (0x1100..=0x115F).contains(&c)
        || (0x2E80..=0xA4CF).contains(&c)
        || (0xAC00..=0xD7A3).contains(&c)
        || (0xF900..=0xFAFF).contains(&c)
        || (0xFE30..=0xFE4F).contains(&c)
        || (0xFF00..=0xFF60).contains(&c)
        || (0x20000..=0x3FFFD).contains(&c);
    if wide { 2 } else { 1 }
}

/// 渲染右侧终端区：标签栏（切换 / 关闭）与当前活动标签的终端画布。
pub fn view(app: &App) -> Element<'_, Message> {
    if app.tabs.list().is_empty() {
        return container(text(t!("terminal.empty")).size(14))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    }

    // 每个标签一个标题按钮 + 关闭按钮，标题超宽截断，整体可横向滚动。
    let tab_row = row(app
        .tabs
        .list()
        .iter()
        .map(|tab| {
            let active = app.tabs.active() == Some(tab.id);
            let label = tab_label(app.tabs.list(), tab);
            // 响铃视觉提示：本标签正在闪烁（见 `app::tabs`），标签样式短暂切为强调实底。
            let flash = tab.bell_flash.is_some();
            // 悬停态由模块跟踪（mouse_area 无内建视觉状态）：拖拽重排后错序到达的
            // 进入 / 离开事件也由模块按 id 判定，整行样式不会跟随错误目标。
            let hovered = app.tabs.hovered() == Some(tab.id);
            // 关闭按钮常态透明，仅悬停 / 按下时显示红色背景（见 `theme::tab_close_style`，
            // 它忽略主题参数）；未着色是刻意让图标在活动 / 非活动标签上都保持低调。
            let close = button(Icon::Dismiss.svg(12.0))
                .on_press(Message::Tabs(tabs::Message::CloseTab(tab.id)))
                .style(crate::theme::tab_close_style)
                .padding(2);
            let close =
                iced::widget::tooltip(close, text(t!("terminal.close_tab")), Position::Bottom)
                    .delay(iced::time::Duration::from_millis(
                        crate::theme::TOOLTIP_DELAY_MS,
                    ))
                    .style(crate::theme::tooltip_style);
            // 整个标签（状态点 + 标题 + 关闭）由 mouse_area 包住：按下即激活并进入拖拽
            // （button 做不到按下即拖拽；其内部按压态与拖拽重排的状态机相斥）。
            // 关闭为嵌套按钮，按下即捕获，不会误触发标签的按下 / 拖拽。
            let focused = app.terminal_focused;
            let tab_container_style = move |theme: &iced::Theme| {
                let style = tab_row_style(theme, active, focused, flash, hovered);
                container::Style {
                    background: style.background,
                    border: style.border,
                    ..container::Style::default()
                }
            };
            // container 不像 button 那样把 text_color 透传给子文本，标题颜色须显式给。
            let tab_text_style = move |theme: &iced::Theme| iced::widget::text::Style {
                color: Some(tab_row_style(theme, active, focused, flash, hovered).text_color),
            };
            mouse_area(
                container(
                    row![
                        // 圆点取本标签自己的连接状态；同会话其它标签的状态不影响本标签。
                        status_dot(tab),
                        text(truncate_label(&label, TAB_TEXT_MAX_WIDTH))
                            .size(12)
                            .style(tab_text_style),
                        close
                    ]
                    .spacing(4)
                    .align_y(iced::alignment::Vertical::Center),
                )
                .padding([4, 6])
                .style(tab_container_style),
            )
            .interaction(iced::mouse::Interaction::Pointer)
            .on_press(Message::Tabs(tabs::Message::TabPressed(tab.id)))
            .on_enter(Message::Tabs(tabs::Message::TabHoverEnter(tab.id)))
            .on_exit(Message::Tabs(tabs::Message::TabHoverExit(tab.id)))
            .into()
        })
        .collect::<Vec<Element<'_, Message>>>())
    .spacing(TAB_SPACING)
    .padding(Padding {
        top: 0f32,
        right: 6f32,
        bottom: TAB_ROW_BOTTOM_PADDING,
        left: TAB_ROW_LEFT_PADDING,
    });

    // 左侧触发按钮 + 抬升面板内的全量标签列表。
    let expanded = app.tabs.show_list();
    let chevron: Element<'_, Message> = if expanded {
        Icon::ChevronUp.svg(14.0).into()
    } else {
        Icon::ChevronDown.svg(14.0).into()
    };
    // 标签行整体比标签本身高出一段底部留白，按 row 居中会把按钮压低半个留白；
    // 给按钮补上等量底部留白，其图标中心才与标签中心重合（补偿量与按钮自身高度无关）。
    let list_button = container(
        button(chevron)
            .on_press(Message::Tabs(tabs::Message::ToggleTabList))
            .style(move |theme, st| crate::theme::icon_button_style(theme, st, expanded))
            .padding(4),
    )
    .padding(Padding {
        top: 0.0,
        right: 0.0,
        bottom: TAB_ROW_BOTTOM_PADDING,
        left: 0.0,
    });
    let list_overlay = container(scrollable(
        column(
            app.tabs
                .list()
                .iter()
                .map(|tab| {
                    let active = app.tabs.active() == Some(tab.id);
                    let label = tab_label(app.tabs.list(), tab);
                    button(
                        row![status_dot(tab), text(label).size(13)]
                            .spacing(6)
                            .align_y(iced::alignment::Vertical::Center),
                    )
                    .on_press(Message::Tabs(tabs::Message::SwitchTab(tab.id)))
                    .style(move |theme, st| crate::theme::tab_list_row_style(theme, st, active))
                    .width(Length::Fill)
                    .padding([6, 10])
                    .into()
                })
                .collect::<Vec<Element<'_, Message>>>(),
        )
        .spacing(2)
        .width(Length::Fill),
    ))
    .width(Length::Fill)
    .style(crate::theme::dropdown_panel_style);
    let tab_list =
        iced_aw::widget::drop_down::DropDown::new(list_button, list_overlay, app.tabs.show_list())
            // 面板宽度须在 DropDown 上指定：overlay 的布局上限取此处的值，未指定则退化为
            // 触发按钮宽度（约 22px），内部部件再设宽度也会被该上限裁掉。
            .width(Length::Fixed(240.0))
            .alignment(iced_aw::core::alignment::Alignment::BottomEnd)
            .on_dismiss(Message::Tabs(tabs::Message::ToggleTabList));

    let tab_bar = row![
        tab_list,
        scrollable(tab_row)
            .direction(scrollable::Direction::Horizontal(
                // 细滚动条：轨道宽 4px、滑块宽 6px（iced 中 `width` 指轨道、`scroller_width`
                // 指滑块，两者语义相反，勿对调），配色见 tab_scrollable_style。
                scrollable::Scrollbar::new().width(4.0).scroller_width(6.0),
            ))
            .id(app.tabs.scroll_id())
            .style(crate::theme::tab_scrollable_style)
            .width(Length::Fill)
    ]
    .spacing(4)
    .align_y(iced::alignment::Vertical::Center)
    .padding(Padding {
        top: 6f32,
        right: 6f32,
        bottom: 2f32,
        left: TAB_BAR_LEFT_PADDING,
    });

    let body: Element<'_, Message> = match app
        .tabs
        .list()
        .iter()
        .find(|t| Some(t.id) == app.tabs.active())
    {
        Some(tab) => match &tab.terminal {
            Some(term) => {
                let terminal_elem: Element<'_, TerminalEvent> =
                    TerminalView::show(term, app.terminal_focused)
                        .padding(4.0)
                        .into();
                let terminal_elem =
                    terminal_elem.map(|e| Message::Tabs(tabs::Message::Terminal(e)));
                container(terminal_elem)
                    .style(|_theme| container::Style {
                        background: Some(
                            crate::theme::terminal_bg(&app.config.terminal.theme).into(),
                        ),
                        border: Border {
                            radius: 4.0.into(),
                            ..Border::default()
                        },
                        ..container::Style::default()
                    })
                    .into()
            }
            None => {
                // 标签已建但终端尚未就绪：按本标签的连接状态显示“连接中”或失败原因。
                match tab.status {
                    ConnectionStatus::Error => {
                        // 失败原因记在该标签自己身上（同会话其它标签可能仍然连着）。
                        let err = tab
                            .error
                            .clone()
                            .unwrap_or_else(|| t!("terminal.connect_failed"));
                        // 重试在**本标签原地**发起，与横幅「重新连接」同一条消息、同一套防重复。
                        let retry = button(text(t!("common.retry")).size(13).color(Color::WHITE))
                            .on_press(Message::Tabs(tabs::Message::ReconnectRequested(tab.id)))
                            .padding([6, 16])
                            .style(|_theme, _st| error_btn_style());
                        let close = button(text(t!("common.close")).size(13))
                            .on_press(Message::Tabs(tabs::Message::CloseTab(tab.id)))
                            .padding([6, 16])
                            .style(crate::theme::error_close_style);
                        container(
                            column![
                                text(t!("terminal.error_status")).size(15).style(
                                    |theme: &iced::Theme| iced::widget::text::Style {
                                        color: Some(crate::theme::error_title_color(theme)),
                                    }
                                ),
                                text(err).size(13).style(|theme: &iced::Theme| {
                                    iced::widget::text::Style {
                                        color: Some(crate::theme::error_detail_color(theme)),
                                    }
                                }),
                                row![retry, close].spacing(10),
                            ]
                            .spacing(10)
                            .align_x(Horizontal::Center),
                        )
                        .center_x(Length::Fill)
                        .center_y(Length::Fill)
                        .padding(20)
                        .into()
                    }
                    _ => container(text(t!("terminal.connecting")).size(13))
                        .center_x(Length::Fill)
                        .center_y(Length::Fill)
                        .padding(12)
                        .into(),
                }
            }
        },
        None => container(text(t!("terminal.pick_tab")).size(13))
            .padding(6)
            .into(),
    };

    let banner: Option<Element<'_, Message>> = app
        .tabs
        .list()
        .iter()
        .find(|t| Some(t.id) == app.tabs.active())
        .filter(|t| t.terminal.is_some() && t.disconnect_reason.is_some())
        .map(disconnected_banner);

    // 标签栏自身不设背景（透明，透出 pane 底色）；区分度靠活动标签的 `tab_style` 高亮，
    // 而非标签栏底色。断开横幅插在标签栏与终端区之间，与终端区左右对齐。
    let mut content = column![container(tab_bar)].spacing(2);
    if let Some(banner) = banner {
        content = content.push(container(banner).padding(Padding {
            left: 4.0,
            right: 2.0,
            top: 0.0,
            bottom: 0.0,
        }));
    }
    content
        // 终端区域：仅左右下侧留白
        .push(container(body).height(Length::Fill).padding(Padding {
            left: 4.0,
            right: 2.0,
            top: 0.0,
            bottom: 2.0,
        }))
        .into()
}

/// 断开态横幅：终端顶部窄条，告知连接已断并提供两条重连入口（Enter / 按钮）。
///
/// 只在终端已存在（连上过）且收到断开归因时渲染；首次连接失败没有终端可承载横幅，
/// 走全屏 Error 覆盖层，两者因 `terminal` 是否为 `None` 而互斥。
///
/// 文案按标签状态分级（见 [`banner_label`]）；重连在途隐藏按钮（触发会被防重复挡下）。
fn disconnected_banner(tab: &TerminalTab) -> Element<'_, Message> {
    let flashing = tab.banner_flash.is_some();
    let reconnecting = tab.status == ConnectionStatus::Connecting;
    let label = banner_label(
        reconnecting,
        tab.error.as_deref(),
        flashing,
        tab.disconnect_reason,
    );
    let mut items: Vec<Element<'_, Message>> = vec![
        text(label)
            .size(12)
            .style(move |theme: &iced::Theme| iced::widget::text::Style {
                color: Some(banner_text_color(theme, flashing)),
            })
            .width(Length::Fill)
            .into(),
    ];
    if !reconnecting {
        items.push(
            button(
                text(t!("terminal.reconnect_button"))
                    .size(12)
                    .color(Color::WHITE),
            )
            .on_press(Message::Tabs(tabs::Message::ReconnectRequested(tab.id)))
            .padding([2, 10])
            .style(|_theme, _st| error_btn_style())
            .into(),
        );
    }
    container(
        row(items)
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center),
    )
    .padding([4, 8])
    .width(Length::Fill)
    .style(move |theme: &iced::Theme| banner_style(theme, flashing))
    .into()
}

/// 断开横幅的文案（纯决策）：重连在途 → 重连提示；重连失败 → 具体原因（按键闪烁时
/// 让位于按键提示）；常态按退出原因分流（远端会话已结束 / 连接已断开）。
fn banner_label(
    reconnecting: bool,
    error: Option<&str>,
    flashing: bool,
    reason: Option<DisconnectReason>,
) -> String {
    if reconnecting {
        return t!("terminal.reconnecting").to_string();
    }
    if let Some(err) = error {
        return if flashing {
            t!("terminal.press_enter_reconnect").to_string()
        } else {
            err.to_string()
        };
    }
    if flashing {
        return t!("terminal.press_enter_reconnect").to_string();
    }
    if matches!(reason, Some(DisconnectReason::ChannelEof)) {
        t!("terminal.session_ended_banner").to_string()
    } else {
        t!("terminal.disconnected_banner").to_string()
    }
}

/// 断开横幅的背景样式：常态沿用右键菜单基调（strong 底 + 细边框）；
/// 闪烁时换成悬停底 + 错误红边框，作为「按键收到了、但写不进去」的强调。
fn banner_style(theme: &iced::Theme, flashing: bool) -> container::Style {
    let p = crate::theme::custom_palette(theme);
    let (background, border_color) = if flashing {
        (p.hover, crate::ui::ERROR)
    } else {
        (theme.extended_palette().background.strong.color, p.border)
    };
    container::Style {
        background: Some(background.into()),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    }
}

/// 断开横幅的文字色：常态随主题正文色；闪烁时用错误红与边框呼应。
fn banner_text_color(theme: &iced::Theme, flashing: bool) -> Color {
    if flashing {
        crate::ui::ERROR
    } else {
        theme.palette().text
    }
}

/// 标签行当前样式：底 / 边框 / 文本色统一取自 [`crate::theme::tab_style`]。
///
/// mouse_area 包住的标签没有内建按压态，悬停档由模块的悬停跟踪补上（未活动标签的
/// `Hovered` 与 `Pressed` 在 `tab_style` 里同档，一个悬停位即可）。
fn tab_row_style(
    theme: &iced::Theme,
    active: bool,
    focused: bool,
    flash: bool,
    hovered: bool,
) -> iced::widget::button::Style {
    let status = if hovered {
        iced::widget::button::Status::Hovered
    } else {
        iced::widget::button::Status::Active
    };
    crate::theme::tab_style(theme, status, active, focused, flash)
}

/// 标签连接状态圆点：8px 圆 + tooltip（配色与文案见 [`status_dot_visual`]）。
fn status_dot(tab: &TerminalTab) -> Element<'static, Message> {
    let connecting = tab.status == ConnectionStatus::Connecting;
    let errored = tab.status == ConnectionStatus::Error;
    let (color, label) = status_dot_visual(
        tab.status,
        connecting && tab.terminal.is_some(),
        errored && tab.terminal.is_some() && tab.error.is_none(),
        tab.disconnect_reason,
    );
    let dot = container("")
        .width(Length::Fixed(8.0))
        .height(Length::Fixed(8.0))
        .style(move |_theme| container::Style {
            background: Some(color.into()),
            border: Border {
                radius: 4.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });
    iced::widget::tooltip(dot, text(label), Position::Bottom)
        .delay(iced::time::Duration::from_millis(
            crate::theme::TOOLTIP_DELAY_MS,
        ))
        .style(crate::theme::tooltip_style)
        .into()
}

/// 状态圆点的（颜色, tooltip 文案）（纯决策）。
///
/// 状态色固定不随程序主题（连接中 / 未连接就地写 RGB 字面量，已连接 / 失败复用
/// `crate::ui::SUCCESS` / `ERROR`）——语义色随主题漂移会丢失「红=出错」的直觉。
/// tooltip 按情境细分：重连在途（`reconnecting`）区别于首次连接；断开掉线
/// （`disconnected`）按退出原因分流，与连接失败区分。
fn status_dot_visual(
    status: ConnectionStatus,
    reconnecting: bool,
    disconnected: bool,
    reason: Option<DisconnectReason>,
) -> (Color, String) {
    match status {
        ConnectionStatus::Connected => (crate::ui::SUCCESS, t!("terminal.connected")),
        ConnectionStatus::Connecting => (
            Color::from_rgb(0.85, 0.65, 0.2),
            if reconnecting {
                t!("terminal.reconnecting")
            } else {
                t!("terminal.connecting_status")
            },
        ),
        ConnectionStatus::Error => (
            crate::ui::ERROR,
            if disconnected {
                match reason {
                    Some(DisconnectReason::ChannelEof) => t!("terminal.session_ended"),
                    _ => t!("terminal.connection_lost"),
                }
            } else {
                t!("terminal.error_status")
            },
        ),
        ConnectionStatus::Disconnected => (
            Color::from_rgb(0.55, 0.55, 0.55),
            t!("terminal.disconnected"),
        ),
    }
}

/// 失败面板中“重试”按钮的样式：红色底以提示可重新发起连接。
fn error_btn_style() -> iced::widget::button::Style {
    iced::widget::button::Style {
        background: Some(Color::from_rgb(0.7, 0.25, 0.25).into()),
        text_color: Color::WHITE,
        border: iced::Border {
            radius: 4.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 横幅文案分流：重连在途 / 重连失败 / 按键闪烁 / 退出原因（远端结束 vs 连接断开）。
    #[test]
    fn banner_label_splits_reasons_and_failures() {
        let died = DisconnectReason::TransportDied;
        let eof = DisconnectReason::ChannelEof;
        // 重连在途与失败原因优先于退出原因分流。
        assert_eq!(
            banner_label(true, None, false, Some(died)),
            t!("terminal.reconnecting")
        );
        assert_eq!(banner_label(false, Some("boom"), false, Some(died)), "boom");
        // 按键闪烁让位于「按 Enter」提示（失败原因同样让位）。
        assert_eq!(
            banner_label(false, None, true, Some(died)),
            t!("terminal.press_enter_reconnect")
        );
        assert_eq!(
            banner_label(false, Some("boom"), true, Some(died)),
            t!("terminal.press_enter_reconnect")
        );
        // 常态按退出原因分流；无归因兜底为「连接已断开」。
        assert_eq!(
            banner_label(false, None, false, Some(eof)),
            t!("terminal.session_ended_banner")
        );
        assert_eq!(
            banner_label(false, None, false, Some(died)),
            t!("terminal.disconnected_banner")
        );
        assert_eq!(
            banner_label(false, None, false, None),
            t!("terminal.disconnected_banner")
        );
    }

    /// 圆点分流：重连在途区别于首连；断开掉线按退出原因，连接失败另发文案。
    #[test]
    fn status_dot_visual_splits_reconnect_and_disconnect_reasons() {
        use ConnectionStatus::{Connected, Connecting, Disconnected, Error};
        // 首次连接（无终端）与重连在途（有终端）同色不同文案。
        assert_eq!(
            status_dot_visual(Connecting, false, false, None).1,
            t!("terminal.connecting_status")
        );
        assert_eq!(
            status_dot_visual(Connecting, true, false, None),
            (
                Color::from_rgb(0.85, 0.65, 0.2),
                t!("terminal.reconnecting")
            )
        );
        // 断开掉线：红点，文案按退出原因分流。
        assert_eq!(
            status_dot_visual(Error, true, true, Some(DisconnectReason::ChannelEof)),
            (crate::ui::ERROR, t!("terminal.session_ended"))
        );
        assert_eq!(
            status_dot_visual(Error, true, true, Some(DisconnectReason::TransportDied)).1,
            t!("terminal.connection_lost")
        );
        // 连接失败（首连 / 重连 attempt）：不按退出原因分流。
        assert_eq!(
            status_dot_visual(Error, true, false, Some(DisconnectReason::ChannelEof)).1,
            t!("terminal.error_status")
        );
        // 已连接 / 未连接两态不受分流影响。
        assert_eq!(
            status_dot_visual(Connected, true, false, None),
            (crate::ui::SUCCESS, t!("terminal.connected"))
        );
        assert_eq!(
            status_dot_visual(Disconnected, false, false, None).1,
            t!("terminal.disconnected")
        );
    }
}
