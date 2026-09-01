//! 右侧终端区域：多标签页 + 内嵌终端视图（`crate::widget::term`）。
//!
//! 每个标签对应一个 [`TerminalTab`]。标签栏用于切换 / 关闭，
//! 主体为 `TerminalView`；键盘 / 鼠标事件由 `TerminalView` 自身捕获并回传
//! [`tabs::Message::Terminal`](crate::app::tabs::Message::Terminal)（经顶层 `Message::Tabs` 路由）。

use crate::t;

use crate::App;
use crate::app::session::Message as SessionMessage;
use crate::app::tabs;
use crate::icons::Icon;
use crate::message::Message;
use crate::state::TerminalTab;
use crate::widget::term::TerminalView;
use iced::alignment::Horizontal;
use iced::widget::tooltip::Position;
use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Border, Color, Element, Length, Padding};
use rterm_core::ConnectionStatus;

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
            // 标签自己的连接状态，驱动行首状态圆点；同会话其它标签的状态不影响本标签。
            let status = tab.status;
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
            // 整个标签（状态点 + 标题 + 关闭）做成单一按钮，悬浮反馈覆盖全部区域；
            // 关闭为嵌套按钮，会捕获自身区域的点击，不会误触发切换。
            let focused = app.terminal_focused;
            button(
                row![
                    status_dot(status),
                    text(truncate_label(&label, TAB_TEXT_MAX_WIDTH)).size(12),
                    close
                ]
                .spacing(4)
                .align_y(iced::alignment::Vertical::Center),
            )
            .on_press(Message::Tabs(tabs::Message::SelectTab(tab.id)))
            .style(move |theme, st| crate::theme::tab_style(theme, st, active, focused))
            .padding([4, 6])
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
                    let status = tab.status;
                    button(
                        row![status_dot(status), text(label).size(13)]
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
            Some(term) => container(
                TerminalView::show(term, app.terminal_focused)
                    .map(|e| Message::Tabs(tabs::Message::Terminal(e))),
            )
            .padding(4)
            .style(|_theme| container::Style {
                background: Some(crate::theme::terminal_bg(&app.config.terminal_theme).into()),
                border: Border {
                    radius: 4.0.into(),
                    ..Border::default()
                },
                ..container::Style::default()
            })
            .into(),
            None => {
                // 标签已建但终端尚未就绪：按本标签的连接状态显示“连接中”或失败原因。
                match tab.status {
                    ConnectionStatus::Error => {
                        // 失败原因记在该标签自己身上（同会话其它标签可能仍然连着）。
                        let err = tab
                            .error
                            .clone()
                            .unwrap_or_else(|| t!("terminal.connect_failed"));
                        let retry = button(text(t!("common.retry")).size(13).color(Color::WHITE))
                            .on_press(Message::Session(SessionMessage::ConnectSession(
                                tab.session_id.clone(),
                            )))
                            .padding([6, 16])
                            .style(|_theme, _st| error_btn_style());
                        let close = button(text(t!("common.close")).size(13))
                            .on_press(Message::Tabs(tabs::Message::CloseTab(tab.id)))
                            .padding([6, 16])
                            .style(crate::theme::tab_close_style);
                        container(
                            column![
                                text(t!("terminal.error_status"))
                                    .size(15)
                                    .color(Color::from_rgb(0.95, 0.45, 0.45)),
                                text(err).size(13).color(Color::from_rgb(0.85, 0.7, 0.7)),
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

    column![
        // 标签栏自身不设背景（透明，透出 pane 底色）；区分度靠活动标签的 `tab_style` 高亮，
        // 而非标签栏底色。
        container(tab_bar),
        // 终端区域：仅左右下侧留白，背景取当前终端配色主题的 `terminal_bg`
        // （跟随用户所选终端主题，22 套预设中含浅色方案），使终端本体在面板中形成内嵌边框视觉。
        container(body).height(Length::Fill).padding(Padding {
            left: 6.0,
            right: 6.0,
            top: 0.0,
            bottom: 6.0,
        })
    ]
    .spacing(2)
    .into()
}

/// 标签连接状态圆点：8px 圆，颜色编码 已连接 / 连接中 / 失败 / 未连接，
/// 附中文 tooltip 照顾色弱用户。
///
/// 状态色是**固定的**状态语义色、不跟随程序主题（连接中 / 未连接两态就地写 RGB 字面量，
/// 已连接 / 失败复用 `crate::ui::SUCCESS` / `ERROR`）——语义色若随主题漂移会丢失「红=出错」的直觉。
fn status_dot(status: ConnectionStatus) -> Element<'static, Message> {
    let (color, label) = match status {
        ConnectionStatus::Connected => (crate::ui::SUCCESS, t!("terminal.connected")),
        ConnectionStatus::Connecting => (
            Color::from_rgb(0.85, 0.65, 0.2),
            t!("terminal.connecting_status"),
        ),
        ConnectionStatus::Error => (crate::ui::ERROR, t!("terminal.error_status")),
        ConnectionStatus::Disconnected => (
            Color::from_rgb(0.55, 0.55, 0.55),
            t!("terminal.disconnected"),
        ),
    };
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
