//! 右侧终端区域：多标签页 + 内嵌终端视图（`crate::widget::term`）。
//!
//! 每个标签对应一个 [`TerminalTab`]。标签栏用于切换 / 关闭，
//! 主体为 `TerminalView`；键盘 / 鼠标事件由 `TerminalView` 自身捕获并回传
//! [`tabs::Message::Terminal`](crate::app::tabs::Message::Terminal)（经顶层 `Message::Tabs` 路由）。
//!
//! 标签栏首位是首页标签（收藏 + 最近连接，见 [`crate::home_tab`]），选中时主体显示首页内容。

use crate::t;

use crate::App;
use crate::app::search;
use crate::app::tabs;
use crate::icons::Icon;
use crate::message::Message;
use crate::state::TerminalTab;
use crate::widget::measured_column::MeasuredColumn;
use crate::widget::term::{Event as TerminalEvent, TerminalView};
use iced::alignment::Horizontal;
use iced::widget::svg::{Handle, Svg};
use iced::widget::tooltip::Position;
use iced::widget::{button, column, container, mouse_area, row, scrollable, text};
use iced::{Border, Color, Element, Length};
use iced_aw::widget::context_menu::ContextMenu;
use rterm_core::{ConnectionStatus, DisconnectReason};

/// 标签条高度（px）：标签满高贴合条带，活动标签底色与下方终端区零间距融合。
const TAB_BAR_HEIGHT: f32 = 34.0;
/// 活动标签顶部聚焦指示条高度（px）。
const TAB_INDICATOR_HEIGHT: f32 = 2.0;
/// 首页标签固定宽度（px）：标题恒定，定宽避免 Fill 指示条撑开内在宽度。
const HOME_TAB_WIDTH: f32 = 96.0;
/// 标签最大宽度（px）：标题文本、状态点、关闭按钮与间距的总和上限。
pub(crate) const TAB_MAX_WIDTH: f32 = 180.0;
/// 关闭按钮位宽度（px）：12px 图标 + 四边内边距 2px×2。未选中标签的关闭按钮隐藏时，
/// 以等宽透明占位顶上，标签宽度才不随悬浮变化。
const TAB_CLOSE_WIDTH: f32 = 16.0;
/// 标签内除标题文本外的固定宽度（px）：状态点 8、关闭按钮位 [`TAB_CLOSE_WIDTH`]、
/// 内容左右内边距 20（`padding([0, 10])`）、行内间距 12（`row.spacing(6)`，三个子元素两个间隙）；
/// 标题文本预算 = [`TAB_MAX_WIDTH`] − 本值。按常量拼出，改按钮宽度时估算自动同步。
const TAB_CHROME_WIDTH: f32 = 8.0 + TAB_CLOSE_WIDTH + 20.0 + 12.0;
/// 标题文本的最大显示宽度（px），超出截断加省略号。
const TAB_TEXT_MAX_WIDTH: f32 = TAB_MAX_WIDTH - TAB_CHROME_WIDTH;
/// 相邻标签的分隔线宽度（px）：标签紧贴（行间距 0），以 1px 发丝线分隔；
/// 每个标签元素 = 本体 + 分隔线，滚动偏移估算与渲染共用本常量。
pub(crate) const TAB_SPACING: f32 = 1.0;
/// 标签行左内边距（px）：条带从 0 起排，滚动偏移估算与渲染共用本常量。
pub(crate) const TAB_ROW_LEFT_PADDING: f32 = 0.0;

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

/// 渲染右侧终端区：标签栏（切换 / 关闭 / 右键菜单）与当前活动标签的终端画布。
pub fn view(app: &App) -> Element<'_, Message> {
    // 活动标签底色与终端区同色融合；首页标签则与首页内容底色（pane 基色）融合。
    let merge_bg = crate::theme::terminal_bg(&app.config.terminal.theme);
    let tabs = app.tabs.list();
    let active_id = app.tabs.active();
    // 首页选中时终端标签均不显示选中态（首页才是当前选中标签）。
    let home_active = app.tabs.home_active();
    let focused = app.terminal_focused;

    // 每个标签 = 满高矩形本体 + 1px 分隔线；本体顶部 2px 聚焦指示条，整体可横向滚动。
    // 标签右键菜单的可用态边界：首标签左移 / 末标签右移不可用；批量关闭同理。
    let tab_count = tabs.len();
    let tab_row = row(tabs
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            let active = !home_active && active_id == Some(tab.id);
            let label = tab_label(tabs, tab);
            // 响铃视觉提示：本标签正在闪烁（见 `app::tabs`），标签样式短暂切为强调实底。
            let flash = tab.bell_flash.is_some();
            // 悬停态由模块跟踪（mouse_area 无内建视觉状态）：拖拽重排后错序到达的
            // 进入 / 离开事件也由模块按 id 判定，整行样式不会跟随错误目标。
            let hovered = app.tabs.hovered() == Some(tab.id);
            // 活动标签两侧的发丝线隐藏（VS Code 惯例），避免割裂融合底色。
            let next_active =
                !home_active && tabs.get(i + 1).is_some_and(|t| active_id == Some(t.id));

            let body_style = move |theme: &iced::Theme| {
                let style = tab_row_style(theme, active, focused, flash, hovered, merge_bg);
                container::Style {
                    background: style.background,
                    ..container::Style::default()
                }
            };
            // 右键菜单按被右击的标签自身生成（非当前活动标签），闭包持有本标签的 id / 会话 /
            // 位置，可用态在右击瞬间按当时布局计算。
            // container 不像 button 那样把 text_color 透传给子文本，标题颜色须显式给。
            let text_style = move |theme: &iced::Theme| iced::widget::text::Style {
                color: Some(
                    tab_row_style(theme, active, focused, flash, hovered, merge_bg).text_color,
                ),
            };
            let indicator_style = move |theme: &iced::Theme| {
                crate::theme::plain_background(tab_indicator_color(
                    theme, active, focused, flash, hovered, merge_bg,
                ))
            };
            // 整个标签（指示条 + 状态点 + 标题 + 关闭）由 mouse_area 包住：按下即激活并进入拖拽
            // （button 做不到按下即拖拽；其内部按压态与拖拽重排的状态机相斥）。
            // 关闭为嵌套按钮，按下即捕获，不会误触发标签的按下 / 拖拽。仅活动标签常显，
            // 其余标签悬浮才显（见 tab_close_slot），隐藏时留等宽占位避免整行跳动。
            // 指示条用「外层底色 + 顶部 2px 内边距」实现：内层铺标签底色盖住其余区域，
            // 仅露出顶部一条指示色。不能用 Fill 宽度的独立指示条——那会把标签内在宽度撑满。
            let face = container(
                container(
                    row![
                        // 圆点取本标签自己的连接状态；同会话其它标签的状态不影响本标签。
                        status_dot(tab),
                        text(truncate_label(&label, TAB_TEXT_MAX_WIDTH))
                            .size(12)
                            .style(text_style),
                        tab_close_slot(
                            active,
                            hovered,
                            move |theme| tab_close_icon_color(
                                theme, active, flash, hovered, merge_bg
                            ),
                            Message::Tabs(tabs::Message::CloseTab(tab.id)),
                        ),
                    ]
                    .spacing(6)
                    .align_y(iced::alignment::Vertical::Center)
                    .height(Length::Fill)
                    .padding([0, 10]),
                )
                .width(Length::Shrink)
                .height(Length::Fill)
                .style(body_style),
            )
            .width(Length::Shrink)
            .height(TAB_BAR_HEIGHT)
            .padding(iced::Padding {
                top: TAB_INDICATOR_HEIGHT,
                ..iced::Padding::ZERO
            })
            .style(indicator_style);

            row![
                tab_context_menu(
                    mouse_area(face)
                        .interaction(iced::mouse::Interaction::Pointer)
                        .on_press(Message::Tabs(tabs::Message::TabPressed(tab.id)))
                        .on_enter(Message::Tabs(tabs::Message::TabHoverEnter(tab.id)))
                        .on_exit(Message::Tabs(tabs::Message::TabHoverExit(tab.id))),
                    tab.id,
                    tab.session_id.clone(),
                    i,
                    tab_count,
                ),
                tab_separator(!(active || next_active)),
            ]
            .spacing(0)
            .into()
        })
        .collect::<Vec<Element<'_, Message>>>())
    .spacing(0)
    .height(TAB_BAR_HEIGHT);

    // 左侧触发按钮 + 抬升面板内的全量标签列表。
    let expanded = app.tabs.show_list();
    let chevron: Element<'_, Message> = if expanded {
        Icon::ChevronUp.svg(14.0).into()
    } else {
        Icon::ChevronDown.svg(14.0).into()
    };
    let list_button = button(chevron)
        .on_press(Message::Tabs(tabs::Message::ToggleTabList))
        .style(move |theme, st| crate::theme::icon_button_style(theme, st, expanded))
        .padding(4);
    let list_overlay = container(crate::ui::thin_vertical_scrollbar(
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

    // 标签条：surface 底色的满宽条带，标签满高贴合；滚动条隐藏（悬浮不占布局），
    // 溢出靠 Shift+滚轮 / 下拉列表 / 切换自动滚动到达，活动标签底缘直抵终端区。
    // 下拉列表触发按钮固定在条带最右端（滚动区 Fill 吃掉中间余量）。
    let tab_bar = container(
        row![
            home_slot(app),
            scrollable(tab_row)
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::new().width(0.0).scroller_width(0.0),
                ))
                .id(app.tabs.scroll_id())
                .style(crate::theme::tab_scrollable_style)
                .width(Length::Fill),
            tab_list,
        ]
        .spacing(0)
        .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .height(TAB_BAR_HEIGHT)
    .style(|theme| crate::theme::plain_background(crate::theme::custom_palette(theme).surface));

    let body: Element<'_, Message> = if app.tabs.home_active() {
        // 首页选中时显示首页内容（收藏 + 最近），终端画布让位。
        crate::home_tab::view(app)
    } else if app.tabs.list().is_empty() {
        // 无终端标签：首页已关闭，标签栏只留主页按钮（首页的唯一恢复入口）。
        container(text(t!("terminal.empty")).size(14))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    } else {
        match app
            .tabs
            .list()
            .iter()
            .find(|t| Some(t.id) == app.tabs.active())
        {
            Some(tab) => {
                // 终端区一律铺终端底色（含连接中 / 失败态），与活动标签底色无缝融合。
                let inner: Element<'_, Message> = match &tab.terminal {
                    Some(term) => {
                        let terminal_elem: Element<'_, TerminalEvent> =
                            TerminalView::show(term, app.terminal_focused, app.shortcuts.keymap())
                                .padding(4.0)
                                .scrollbar(app.config.terminal.show_scrollbar)
                                .into();
                        let terminal_elem: Element<'_, Message> =
                            terminal_elem.map(|e| Message::Tabs(tabs::Message::Terminal(e)));
                        // 远端程序开启了鼠标上报（tmux / vim 等）时由它接管鼠标，右键菜单让位
                        // （与中键粘贴让位于鼠标模式同理，见 `handle_middle_button_pressed`）。
                        if term.mouse_reporting() {
                            terminal_elem
                        } else {
                            terminal_context_menu(terminal_elem, term.has_selection(), tab.id)
                        }
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
                                let retry =
                                    button(text(t!("common.retry")).size(13).color(Color::WHITE))
                                        .on_press(Message::Tabs(tabs::Message::ReconnectRequested(
                                            tab.id,
                                        )))
                                        .padding([6, 16])
                                        .style(|_theme, _st| error_btn_style());
                                // 关闭按钮文字随终端底色派生（面板铺在终端色上，非程序主题色）。
                                let close_text = crate::theme::merge_text_colors(merge_bg).0;
                                let close = button(text(t!("common.close")).size(13))
                                    .on_press(Message::Tabs(tabs::Message::CloseTab(tab.id)))
                                    .padding([6, 16])
                                    .style(move |_theme, st| iced::widget::button::Style {
                                        background: match st {
                                            iced::widget::button::Status::Hovered
                                            | iced::widget::button::Status::Pressed => Some(
                                                crate::theme::merge_hover_color(merge_bg).into(),
                                            ),
                                            _ => None,
                                        },
                                        text_color: close_text,
                                        ..Default::default()
                                    });
                                container(
                                    column![
                                        text(t!("terminal.error_status"))
                                            .size(15)
                                            .color(crate::theme::error_title_color(merge_bg)),
                                        text(err)
                                            .size(13)
                                            .color(crate::theme::error_detail_color(merge_bg)),
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
                            _ => container(
                                text(t!("terminal.connecting"))
                                    .size(13)
                                    .color(crate::theme::merge_text_colors(merge_bg).1),
                            )
                            .center_x(Length::Fill)
                            .center_y(Length::Fill)
                            .padding(12)
                            .into(),
                        }
                    }
                };
                container(inner)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(move |_theme| crate::theme::plain_background(merge_bg))
                    .into()
            }
            None => container(text(t!("terminal.pick_tab")).size(13))
                .padding(6)
                .into(),
        }
    };

    let banner: Option<Element<'_, Message>> = (!app.tabs.home_active())
        .then(|| {
            app.tabs
                .list()
                .iter()
                .find(|t| Some(t.id) == app.tabs.active())
                .filter(|t| t.terminal.is_some() && t.disconnect_reason.is_some())
                .map(disconnected_banner)
        })
        .flatten();

    // 标签条（surface 底）→ 断开横幅 / 搜索条 → 终端区，三者零间距堆叠：
    // 活动标签底缘直抵终端顶缘，同色融合无割裂。横幅 / 搜索条出现时全宽铺满。
    let mut content = column![tab_bar].spacing(0);
    if let Some(banner) = banner {
        content = content.push(banner);
    }
    // 搜索条：打开时插在标签栏（及断开横幅）与终端区之间，输入框吃掉剩余宽度。
    if app.search.is_open() {
        content = content.push(
            container(search::view(&app.search).map(Message::Search))
                .width(Length::Fill)
                .padding([4, 8])
                .style(|theme| {
                    crate::theme::plain_background(crate::theme::custom_palette(theme).surface)
                }),
        );
    }
    content.push(container(body).height(Length::Fill)).into()
}

/// 首页标签：标签栏首位。打开时是与终端标签同形的满高矩形（指示条 + 图标 + 标题 + 关闭），
/// 活动底色与首页内容底色（pane 基色）融合；关闭后留一个主页图标按钮，点击即重新打开。
/// 关闭按钮沿用终端标签的显隐规则：活动时常显，未活动时悬浮才显。
fn home_slot(app: &App) -> Element<'_, Message> {
    if app.tabs.home_open() {
        let active = app.tabs.home_active();
        // button 的 hover 只在 style 闭包里可见，撑不起子元素的显隐：由 mouse_area
        // 把悬浮态上报给 app::tabs，再回流到这里的渲染。
        let hovered = app.tabs.home_tab_hovered();
        // 首页内容透明透出 pane 基色，故融合底色取程序主题背景（随主题切换）。
        let indicator_style = move |theme: &iced::Theme| {
            crate::theme::plain_background(if active {
                crate::theme::accent_color(theme)
            } else {
                Color::TRANSPARENT
            })
        };
        let home_icon = Svg::new(Handle::from_memory(Icon::Home.bytes()))
            .width(Length::Fixed(12.0))
            .height(Length::Fixed(12.0))
            .style(move |theme, _| iced::widget::svg::Style {
                color: Some(home_text_color(theme, active)),
            });
        let text_style = move |theme: &iced::Theme| iced::widget::text::Style {
            color: Some(home_text_color(theme, active)),
        };
        // 关闭按钮嵌在标签内：其按下会捕获事件，不会连带触发标签的 OpenHome。
        // 标题文本必须撑满剩余宽度：row 的 Fill 只撑满自身边界，子元素仍从左流式排布，
        // 余量会堆在关闭按钮之后（行内边距 [0, 10] 不等于右边距）；让文本吃掉余量，
        // 关闭按钮才被推到右缘 10px 处，与终端标签一致。
        // column / row 撑满按钮定宽（HOME_TAB_WIDTH）同理，且 Fill 让顶部指示条恢复
        // 全宽（Shrink 父级下 Fill 解算为 intrinsic，指示条会缩成 0 宽不可见）。
        let home_button = button(
            column![
                container("")
                    .width(Length::Fill)
                    .height(TAB_INDICATOR_HEIGHT)
                    .style(indicator_style),
                row![
                    home_icon,
                    text(t!("terminal.home_tab"))
                        .size(12)
                        .width(Length::Fill)
                        .style(text_style),
                    tab_close_slot(
                        active,
                        hovered,
                        move |theme| {
                            if active {
                                crate::theme::merge_text_colors(theme.palette().background).1
                            } else {
                                // 未活动的关闭按钮只在悬浮时出现，用次要文本色与终端标签对齐
                                // （第三级太淡，浮现出来反而不易看见）。
                                crate::theme::custom_palette(theme).text_secondary
                            }
                        },
                        Message::Tabs(tabs::Message::CloseHome),
                    ),
                ]
                .spacing(6)
                .align_y(iced::alignment::Vertical::Center)
                .height(Length::Fill)
                .width(Length::Fill)
                .padding([0, 10]),
            ]
            .width(Length::Fill)
            .height(TAB_BAR_HEIGHT),
        )
        .on_press(Message::Tabs(tabs::Message::OpenHome))
        .style(move |theme: &iced::Theme, status| {
            crate::theme::tab_style(
                theme,
                status,
                active,
                true,
                false,
                theme.palette().background,
            )
        })
        .width(Length::Fixed(HOME_TAB_WIDTH))
        .padding(0);
        // 悬浮态上报 app::tabs 再回流到渲染（button 的 hover 只在 style 闭包里可见）。
        // 不挂 on_press：按下仍由内层 button 与嵌套的关闭按钮各自捕获。
        mouse_area(home_button)
            .interaction(iced::mouse::Interaction::Pointer)
            .on_enter(Message::Tabs(tabs::Message::HomeTabHover(true)))
            .on_exit(Message::Tabs(tabs::Message::HomeTabHover(false)))
            .into()
    } else {
        let btn = button(Icon::Home.svg(14.0))
            .on_press(Message::Tabs(tabs::Message::OpenHome))
            .style(|theme, st| crate::theme::icon_button_style(theme, st, false))
            .padding(4);
        btn.into()
    }
}

/// 首页标签图标 / 标题色：活动时按 pane 基色亮度派生主色，非活动用次要文本色。
fn home_text_color(theme: &iced::Theme, active: bool) -> Color {
    if active {
        crate::theme::merge_text_colors(theme.palette().background).0
    } else {
        crate::theme::custom_palette(theme).text_secondary
    }
}

/// 终端右键菜单：包住终端画布，提供「查找 / 快速命令 / 复制 / 粘贴」。`copy_enabled` 为否时复制项以禁用态呈现。
/// 「查找」恒可用（不依赖选区），打开携带本标签 id 的搜索条。
fn terminal_context_menu(
    terminal: Element<'_, Message>,
    copy_enabled: bool,
    tab_id: u64,
) -> Element<'_, Message> {
    let overlay = move || {
        let copy: Element<'_, Message> = if copy_enabled {
            crate::ui::menu_entry(t!("terminal.menu_copy"), Message::TerminalCopy)
        } else {
            crate::ui::menu_entry_disabled(t!("terminal.menu_copy"))
        };
        crate::ui::menu_container(
            MeasuredColumn::with_children(vec![
                crate::ui::menu_entry(
                    t!("terminal.menu_search"),
                    Message::Search(search::Message::Open { tab_id }),
                ),
                crate::ui::menu_entry(
                    t!("terminal.menu_palette"),
                    Message::Snippets(crate::app::snippets::Message::TogglePalette),
                ),
                copy,
                crate::ui::menu_entry(t!("terminal.menu_paste"), Message::TerminalPaste),
            ])
            .spacing(2),
        )
    };
    ContextMenu::new(terminal, overlay).into()
}

/// 标签右键菜单：包住单个标签，按功能分三组、组间以横线分隔（与其它面板菜单同一套构件）。
///
/// 菜单按被右击的标签自身生成（非当前活动标签）：闭包持有该标签的 id / 会话 / 位置，
/// 可用态在右击瞬间按当时布局计算（菜单打开期间列表不会变化，语义正确）。
///
/// - 会话组：连接此会话（同会话另开一个标签，走会话模块的连接链路）。
/// - 移动组：左移 / 右移 / 移到开头 / 移到末尾，已在对应边界时呈禁用态。
/// - 关闭组：关闭标签 / 关闭其他 / 关闭右侧 / 全部关闭，对应集合为空时呈禁用态。
fn tab_context_menu<'a>(
    underlay: impl Into<Element<'a, Message>>,
    tab_id: u64,
    session_id: String,
    index: usize,
    count: usize,
) -> Element<'a, Message> {
    let first = index == 0;
    let last = index + 1 >= count;
    let menu = move || {
        // 会话组：复制标签（同会话另开一个标签，Chrome / Windows Terminal 的 Duplicate 惯例，
        // 走会话模块的连接链路；用词与会话列表的 Duplicate 一致）。
        let connect: Element<'_, Message> = crate::ui::menu_entry(
            t!("terminal.menu_tab_duplicate"),
            Message::Session(crate::app::session::Message::ConnectSession(
                session_id.clone(),
            )),
        );
        // 移动组：方向指向的边界不可用（首不能左移 / 移到开头，末不能右移 / 移到末尾）。
        let move_left: Element<'_, Message> = if first {
            crate::ui::menu_entry_disabled(t!("terminal.menu_tab_move_left"))
        } else {
            crate::ui::menu_entry(
                t!("terminal.menu_tab_move_left"),
                Message::Tabs(tabs::Message::MoveTabRelative(tab_id, tabs::MoveDir::Left)),
            )
        };
        let move_right: Element<'_, Message> = if last {
            crate::ui::menu_entry_disabled(t!("terminal.menu_tab_move_right"))
        } else {
            crate::ui::menu_entry(
                t!("terminal.menu_tab_move_right"),
                Message::Tabs(tabs::Message::MoveTabRelative(tab_id, tabs::MoveDir::Right)),
            )
        };
        let move_first: Element<'_, Message> = if first {
            crate::ui::menu_entry_disabled(t!("terminal.menu_tab_move_first"))
        } else {
            crate::ui::menu_entry(
                t!("terminal.menu_tab_move_first"),
                Message::Tabs(tabs::Message::MoveTabRelative(tab_id, tabs::MoveDir::First)),
            )
        };
        let move_last: Element<'_, Message> = if last {
            crate::ui::menu_entry_disabled(t!("terminal.menu_tab_move_last"))
        } else {
            crate::ui::menu_entry(
                t!("terminal.menu_tab_move_last"),
                Message::Tabs(tabs::Message::MoveTabRelative(tab_id, tabs::MoveDir::Last)),
            )
        };
        // 关闭组：集合为空时禁用（仅此一个标签时不能「关其他」，已是最右时不能「关右侧」）。
        let close_others: Element<'_, Message> = if count <= 1 {
            crate::ui::menu_entry_disabled(t!("terminal.menu_tab_close_others"))
        } else {
            crate::ui::menu_entry(
                t!("terminal.menu_tab_close_others"),
                Message::Tabs(tabs::Message::CloseOthers(tab_id)),
            )
        };
        let close_right: Element<'_, Message> = if last {
            crate::ui::menu_entry_disabled(t!("terminal.menu_tab_close_right"))
        } else {
            crate::ui::menu_entry(
                t!("terminal.menu_tab_close_right"),
                Message::Tabs(tabs::Message::CloseToRight(tab_id)),
            )
        };
        crate::ui::menu_container(
            MeasuredColumn::with_children(vec![
                connect,
                crate::ui::menu_separator(),
                move_left,
                move_right,
                move_first,
                move_last,
                crate::ui::menu_separator(),
                crate::ui::menu_entry(
                    t!("terminal.menu_tab_close"),
                    Message::Tabs(tabs::Message::CloseTab(tab_id)),
                ),
                close_others,
                close_right,
                crate::ui::menu_entry(
                    t!("terminal.menu_tab_close_all"),
                    Message::Tabs(tabs::Message::CloseAll),
                ),
            ])
            .spacing(2),
        )
    };
    ContextMenu::new(underlay, menu).into()
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
/// 直角满宽铺在标签条与终端区之间，与两者零间距衔接。
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
            ..Border::default()
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

/// 标签行当前样式：底 / 文本色统一取自 [`crate::theme::tab_style`]（`merge_bg` 为终端底色）。
///
/// mouse_area 包住的标签没有内建按压态，悬停档由模块的悬停跟踪补上（未活动标签的
/// `Hovered` 与 `Pressed` 在 `tab_style` 里同档，一个悬停位即可）。
fn tab_row_style(
    theme: &iced::Theme,
    active: bool,
    focused: bool,
    flash: bool,
    hovered: bool,
    merge_bg: Color,
) -> iced::widget::button::Style {
    let status = if hovered {
        iced::widget::button::Status::Hovered
    } else {
        iced::widget::button::Status::Active
    };
    crate::theme::tab_style(theme, status, active, focused, flash, merge_bg)
}

/// 标签顶部 2px 色带颜色（纯决策）：活动聚焦取强调色、活动未聚焦取融合底色次要色（灰条）；
/// 其余情形与标签本体底色一致（闪烁=强调、非活动悬停=hover、常态=透明），使色带不显突兀。
fn tab_indicator_color(
    theme: &iced::Theme,
    active: bool,
    focused: bool,
    flash: bool,
    hovered: bool,
    merge_bg: Color,
) -> Color {
    if flash {
        return crate::theme::accent_color(theme);
    }
    if active {
        return if focused {
            crate::theme::accent_color(theme)
        } else {
            crate::theme::merge_text_colors(merge_bg).1
        };
    }
    if hovered {
        crate::theme::custom_palette(theme).hover
    } else {
        Color::TRANSPARENT
    }
}

/// 标签关闭图标色（纯决策）：闪烁白字、活动取融合底色次要色、非活动悬停提亮一档。
///
/// 悬停反馈由按钮的中性 hover 底承担，图标自身不随悬停变色。
fn tab_close_icon_color(
    theme: &iced::Theme,
    active: bool,
    flash: bool,
    hovered: bool,
    merge_bg: Color,
) -> Color {
    if flash {
        Color::WHITE
    } else if active {
        crate::theme::merge_text_colors(merge_bg).1
    } else if hovered {
        crate::theme::custom_palette(theme).text_secondary
    } else {
        crate::theme::custom_palette(theme).text_tertiary
    }
}

/// 关闭按钮是否可见（纯决策）：选中的标签常显，未选中的标签仅悬浮时显。
fn close_button_visible(active: bool, hovered: bool) -> bool {
    active || hovered
}

/// 标签栏的关闭按钮位：可见时渲染真实按钮，隐藏时用等宽透明占位顶上。
///
/// 占位不可省：标签本体宽度是 `Length::Shrink`，抽掉 16px 按钮会让悬浮瞬间整行标签
/// 横向跳动；[`estimated_tab_width`] 的滚动偏移估算也按恒定的 [`TAB_CLOSE_WIDTH`]
/// 计算，两处失同步会让下拉列表切签后的滚动定位偏移。
fn tab_close_slot(
    active: bool,
    hovered: bool,
    icon_color: impl Fn(&iced::Theme) -> Color + 'static,
    on_close: Message,
) -> Element<'static, Message> {
    if close_button_visible(active, hovered) {
        tab_close_button(icon_color, on_close)
    } else {
        container("")
            .width(Length::Fixed(TAB_CLOSE_WIDTH))
            .height(Length::Fixed(TAB_CLOSE_WIDTH))
            .into()
    }
}

/// 标签关闭按钮：12px dismiss 图标 + 中性悬停底，终端标签与首页标签共用。
///
/// [`crate::theme::tab_close_style`] 在悬停 / 按下时铺中性底提供反馈，图标色恒为
/// `icon_color`（活动标签随融合底色派生），不随悬停切换；也不挂 tooltip，避免标签栏里
/// 密集的悬停气泡遮挡相邻标签。按下即捕获，不会连带触发外层标签的激活 / 拖拽。
fn tab_close_button(
    icon_color: impl Fn(&iced::Theme) -> Color + 'static,
    on_close: Message,
) -> Element<'static, Message> {
    let icon = Svg::new(Handle::from_memory(Icon::Dismiss.bytes()))
        .width(Length::Fixed(12.0))
        .height(Length::Fixed(12.0))
        .style(move |theme, _status| iced::widget::svg::Style {
            color: Some(icon_color(theme)),
        });
    button(icon)
        .on_press(on_close)
        .style(crate::theme::tab_close_style)
        .padding(2)
        .into()
}

/// 相邻标签间的发丝分隔线：1px 宽、条带满高；活动标签两侧隐藏，避免割裂融合底色。
fn tab_separator(visible: bool) -> Element<'static, Message> {
    container("")
        .width(Length::Fixed(TAB_SPACING))
        .height(Length::Fixed(TAB_BAR_HEIGHT))
        .style(move |theme| {
            crate::theme::plain_background(if visible {
                crate::theme::pane_divider_color(theme)
            } else {
                Color::TRANSPARENT
            })
        })
        .into()
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

    /// 顶部色带：活动聚焦强调色、未聚焦融合次要色；其余与本体底色一致（闪烁强调 / 悬停 hover / 常态透明）。
    #[test]
    fn indicator_band_matches_body_or_marks_active() {
        let theme = iced::Theme::Dark;
        let dark_bg = crate::theme::TERMINAL_BG;
        let p = crate::theme::custom_palette(&theme);
        // 非活动常态透明；非活动悬停与本体 hover 同色。
        assert_eq!(
            tab_indicator_color(&theme, false, true, false, false, dark_bg),
            Color::TRANSPARENT
        );
        assert_eq!(
            tab_indicator_color(&theme, false, true, false, true, dark_bg),
            p.hover
        );
        // 闪烁与本体强调实底同色。
        assert_eq!(
            tab_indicator_color(&theme, true, true, true, false, dark_bg),
            crate::theme::accent_color(&theme)
        );
        // 活动聚焦取强调色；未聚焦取融合底色上的次要色。
        assert_eq!(
            tab_indicator_color(&theme, true, true, false, false, dark_bg),
            crate::theme::accent_color(&theme)
        );
        assert_eq!(
            tab_indicator_color(&theme, true, false, false, false, dark_bg),
            crate::theme::merge_text_colors(dark_bg).1
        );
    }

    /// 关闭图标常态色：闪烁白、活动取融合次要色、非活动按悬停提亮一档。
    #[test]
    fn close_icon_color_follows_tab_state() {
        let theme = iced::Theme::Dark;
        let dark_bg = crate::theme::TERMINAL_BG;
        let p = crate::theme::custom_palette(&theme);
        assert_eq!(
            tab_close_icon_color(&theme, true, true, false, dark_bg),
            Color::WHITE
        );
        assert_eq!(
            tab_close_icon_color(&theme, true, false, false, dark_bg),
            crate::theme::merge_text_colors(dark_bg).1
        );
        assert_eq!(
            tab_close_icon_color(&theme, false, false, true, dark_bg),
            p.text_secondary
        );
        assert_eq!(
            tab_close_icon_color(&theme, false, false, false, dark_bg),
            p.text_tertiary
        );
    }

    /// 关闭按钮显隐：选中标签常显，未选中标签仅悬浮时显。
    #[test]
    fn close_button_only_shows_for_active_or_hovered_tabs() {
        assert!(close_button_visible(true, false), "选中标签恒显示关闭按钮");
        assert!(close_button_visible(true, true));
        assert!(close_button_visible(false, true), "未选中标签悬浮才显示");
        assert!(!close_button_visible(false, false));
    }

    /// 隐藏时的占位宽度必须等于真实按钮宽度：悬浮才不会让整行标签横向跳动，
    /// 宽度估算也要跟着同一个常量走。
    #[test]
    fn close_placeholder_width_matches_chrome_estimate() {
        // 12px 图标 + 左右各 2px padding = 16px。
        assert_eq!(TAB_CLOSE_WIDTH, 12.0 + 2.0 * 2.0);
        assert_eq!(TAB_CHROME_WIDTH, 8.0 + TAB_CLOSE_WIDTH + 20.0 + 12.0);
    }

    /// 融合底色文本派生：深底浅字、浅底深字，主色对比强于次要色。
    #[test]
    fn merge_text_colors_flip_with_background_luminance() {
        let (dark_primary, dark_secondary) =
            crate::theme::merge_text_colors(crate::theme::TERMINAL_BG);
        let (light_primary, light_secondary) =
            crate::theme::merge_text_colors(Color::from_rgb(0.99, 0.97, 0.9));
        // 深底取浅字、浅底取深字。
        assert!(dark_primary.r > 0.5 && dark_primary.g > 0.5);
        assert!(light_primary.r < 0.5 && light_primary.g < 0.5);
        // 次要色比主色更贴近底色（对比更弱）。
        assert!(dark_secondary.r < dark_primary.r);
        assert!(light_secondary.r > light_primary.r);
    }

    /// 标签宽度估算：短标题按内容估、长标题触顶 [`TAB_MAX_WIDTH`]。
    #[test]
    fn estimated_tab_width_caps_at_max() {
        assert!(estimated_tab_width("a") < TAB_MAX_WIDTH);
        assert_eq!(
            estimated_tab_width(&"very-long-session-name".repeat(4)),
            TAB_MAX_WIDTH
        );
    }
}
