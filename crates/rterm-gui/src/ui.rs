//! 跨面板复用的通用 UI 构件与样式（右键菜单项、输入框、模态弹窗、语义色）。
//!
//! 抽自 `session_panel` / `sftp_panel` / `settings_dialog` / `sftp_dialogs` /
//! `host_key_dialog` 中重复出现的样式与弹窗骨架，集中维护以避免多份实现失同步。

use crate::icons::Icon;
use crate::t;
use crate::theme;
use iced::widget::button;
use iced::widget::container;
use iced::widget::svg;
use iced::widget::text;
use iced::widget::text::Wrapping;
use iced::widget::text_input;
use iced::widget::tooltip;
use iced::widget::tooltip::Position;
use iced::{Background, Border, Color, Element, Length, Theme};

/// 右键上下文菜单宽度（像素）。
pub const MENU_WIDTH: f32 = 140.0;

/// 弹窗标题字号（像素）。
///
/// 各弹窗此前在 16 / 18 之间漂移，现统一：标题栏是同一套骨架，字号必须一致。
pub const DIALOG_TITLE_SIZE: f32 = 16.0;

/// 弹窗右上角关闭按钮的图标尺寸（像素）。
const DIALOG_CLOSE_ICON_SIZE: f32 = 16.0;
/// 弹窗标题栏内边距（上下 / 左右）。
const DIALOG_HEADER_PADDING: [f32; 2] = [12.0, 16.0];
/// 弹窗正文区内边距（上下 / 左右）。
///
/// 供各弹窗（含设置弹窗这种「双栏、正文自持滚动区」的形态）复用，避免同一个内边距
/// 在多个模块里各写一份而漂移。
pub const DIALOG_BODY_PADDING: [f32; 2] = [16.0, 20.0];
/// 弹窗面板圆角（像素）。
pub const DIALOG_PANEL_RADIUS: f32 = 10.0;
/// 内容面板标题字号（像素）。
const PANE_TITLE_SIZE: f32 = 15.0;
/// 弹窗底部按钮区内边距（上下 / 左右）。
const DIALOG_FOOTER_PADDING: [f32; 2] = [12.0, 16.0];
/// 弹窗内按钮文字字号（像素）。
const DIALOG_BTN_TEXT_SIZE: f32 = 13.0;
/// 主操作（强调）按钮内边距：比次要按钮略宽，强化主次对比。
const DIALOG_BTN_PADDING_EMPHASIS: [f32; 2] = [8.0, 20.0];
/// 次要（中性）按钮内边距。
const DIALOG_BTN_PADDING_NEUTRAL: [f32; 2] = [8.0, 16.0];
/// 内容区内联动作按钮的文字字号：比页脚按钮小一档，与字段标签（13px）/ 提示（12px）同区间。
const INLINE_BTN_TEXT_SIZE: f32 = 12.0;
/// 内联强调按钮内边距（上下 / 左右），竖向取 5 与 iced 默认按钮同一节奏。
const INLINE_BTN_PADDING_EMPHASIS: [f32; 2] = [5.0, 12.0];
/// 内联中性按钮内边距（上下 / 左右）。
const INLINE_BTN_PADDING_NEUTRAL: [f32; 2] = [5.0, 10.0];
/// 标题左侧强调条尺寸（宽 × 高，像素）。
const DIALOG_ACCENT_BAR: (f32, f32) = (3.0, 16.0);

/// 成功态（传输完成、导入成功等）绿色。
pub const SUCCESS: Color = Color::from_rgb(0.18, 0.6, 0.33);
/// 错误态红色。
pub const ERROR: Color = Color::from_rgb(0.8, 0.2, 0.2);
/// 警告态（钥匙串丢失、凭据可能失效等）琥珀色。
pub const WARNING: Color = Color::from_rgb(0.85, 0.6, 0.1);
/// 危险操作（删除 / 覆盖 / 指纹变更）红色。
pub const DANGER: Color = Color::from_rgb(0.85, 0.30, 0.30);

/// 弹窗右上角的关闭按钮（✕ 图标）：主题感知文字色 + 悬浮高亮 + 提示气泡，所有模态弹窗统一复用，
/// 避免各弹窗各自实现导致样式漂移。传入点击后触发的消息 `M`（如
/// `Message::Settings(settings::Message::Toggle)` / `Message::MasterPw(masterpw::Message::Cancel)` /
/// `Message::MasterPw(masterpw::Message::ChangeCancel)`，或经 `.map` 接入的子模块消息）。
///
/// 关闭按钮与底部「取消」是同一语义的两条发现路径（有人扫标题栏、有人扫按钮区），
/// 传入的消息应当一致：不要为关闭按钮单独接一个不同的处理分支。
pub fn dialog_close_button<M: Clone + 'static>(on_press: M) -> Element<'static, M> {
    let icon = Icon::Dismiss
        .svg(DIALOG_CLOSE_ICON_SIZE)
        .style(|theme: &Theme, _status| svg::Style {
            color: Some(theme.extended_palette().background.strong.text),
        });
    let btn = button(icon)
        .on_press(on_press)
        .style(|theme, status| theme::icon_button_style(theme, status, false));
    hover_tooltip(btn, t!("common.close"), Position::Bottom)
}

/// 弹窗面板样式
pub fn dialog_panel_style(border: Option<Color>) -> impl Fn(&Theme) -> container::Style {
    move |theme: &Theme| {
        let p = theme::custom_palette(theme);
        container::Style {
            background: Some(Background::Color(p.surface_raised)),
            border: match border {
                Some(c) => Border {
                    color: c,
                    width: 2.0,
                    radius: DIALOG_PANEL_RADIUS.into(),
                },
                None => Border {
                    color: p.border,
                    width: 1.0,
                    radius: DIALOG_PANEL_RADIUS.into(),
                },
            },
            shadow: iced::Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
                offset: iced::Vector::new(0.0, 8.0),
                blur_radius: 24.0,
            },
            ..Default::default()
        }
    }
}

/// 弹窗标题栏
pub fn dialog_title_bar<'a, M: Clone + 'static>(
    title: impl Into<String>,
    close: Option<M>,
) -> Element<'a, M> {
    let accent_bar = container(text("").size(DIALOG_TITLE_SIZE))
        .width(DIALOG_ACCENT_BAR.0)
        .height(DIALOG_ACCENT_BAR.1)
        .style(|theme: &Theme| container::Style {
            background: Some(Background::Color(theme::accent_color(theme))),
            border: Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    let heading: Element<'a, M> = text(title.into())
        .size(DIALOG_TITLE_SIZE)
        .width(Length::Fill)
        .into();
    let mut bar = iced::widget::row![accent_bar, heading]
        .align_y(iced::alignment::Vertical::Center)
        .spacing(8)
        .padding(DIALOG_HEADER_PADDING);
    if let Some(msg) = close {
        bar = bar.push(dialog_close_button(msg));
    }
    bar.into()
}

/// 弹窗内的低对比度细分割线（
pub fn hairline<'a, M: Clone + 'a>() -> Element<'a, M> {
    iced::widget::rule::horizontal(1)
        .style(|theme: &Theme| iced::widget::rule::Style {
            color: theme::custom_palette(theme).border,
            radius: 0.0.into(),
            fill_mode: iced::widget::rule::FillMode::Full,
            snap: true,
        })
        .into()
}

/// 表单字段标签
pub fn field_label<'a, M: Clone + 'a>(label: impl Into<String>) -> Element<'a, M> {
    text(label.into())
        .size(13)
        .style(|theme: &Theme| iced::widget::text::Style {
            color: Some(theme::custom_palette(theme).text_secondary),
        })
        .into()
}

/// 表单小节标题
pub fn section_title<'a, M: Clone + 'a>(label: impl Into<String>) -> Element<'a, M> {
    text(label.into())
        .size(13)
        .style(|theme: &Theme| iced::widget::text::Style {
            color: Some(theme.extended_palette().background.base.text),
        })
        .into()
}

/// 内容面板标题
pub fn pane_title<'a, M: Clone + 'a>(label: impl Into<String>) -> Element<'a, M> {
    text(label.into())
        .size(PANE_TITLE_SIZE)
        .style(|theme: &Theme| iced::widget::text::Style {
            color: Some(theme.extended_palette().background.base.text),
        })
        .into()
}

/// 必填字段标签
pub fn required_label<'a, M: Clone + 'a>(label: impl Into<String>) -> Element<'a, M> {
    let label: Element<'a, M> = field_label(label);
    let star: Element<'a, M> = text("*")
        .size(13)
        .style(|theme: &Theme| iced::widget::text::Style {
            color: Some(theme::accent_color(theme)),
        })
        .into();
    iced::widget::row![label, star].spacing(3).into()
}

/// 说明 / 提示文字
pub fn hint_text<'a, M: Clone + 'a>(label: impl Into<String>) -> Element<'a, M> {
    // 弱文字色、按词换行
    text(label.into())
        .size(12)
        .wrapping(Wrapping::Word)
        .style(|theme: &Theme| iced::widget::text::Style {
            color: Some(theme.extended_palette().background.weak.text),
        })
        .into()
}

/// 表单小节
pub fn form_section<'a, M: Clone + 'a>(
    title: impl Into<String>,
    content: impl Into<Element<'a, M>>,
) -> Element<'a, M> {
    // 小节标题 + 内容
    let head: Element<'a, M> = section_title(title);
    let body: Element<'a, M> = content.into();
    iced::widget::column![head, body].spacing(12).into()
}

/// 弹窗底部按钮行
pub fn dialog_footer<'a, M: Clone + 'a>(
    error: Option<String>,
    secondary: Option<DialogButton<M>>,
    primary: DialogButton<M>,
) -> Element<'a, M> {
    // 左侧校验错误提示 + 右侧「次要 / 主操作」按钮组（整体靠右）
    let message = container(
        text(error.unwrap_or_default())
            .size(13)
            .wrapping(Wrapping::Word)
            .color(DANGER),
    )
    .width(Length::Fill)
    .align_y(iced::alignment::Vertical::Center);

    let mut buttons = iced::widget::row![]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center);
    if let Some(secondary) = secondary {
        buttons = buttons.push(dialog_button(secondary));
    }
    buttons = buttons.push(dialog_button(primary));

    let row = iced::widget::row![message, buttons].align_y(iced::alignment::Vertical::Center);
    container(row)
        .width(Length::Fill)
        .padding(DIALOG_FOOTER_PADDING)
        .into()
}

/// 弹窗底部单按钮行（居中）：只读信息框的「关闭」等没有次要操作的场景。
pub fn dialog_footer_centered<'a, M: Clone + 'a>(primary: DialogButton<M>) -> Element<'a, M> {
    container(dialog_button(primary))
        .width(Length::Fill)
        .align_x(iced::alignment::Horizontal::Center)
        .padding(DIALOG_FOOTER_PADDING)
        .into()
}

/// 按 [`DialogButton`] 配置构造按钮部件：样式分派、字号与内边距在此单点统一。
///
/// 这是**弹窗页脚**按钮的尺寸——取消 / 保存这类弹窗级主次操作需要撑起注意力。
pub fn dialog_button<'a, M: Clone + 'a>(cfg: DialogButton<M>) -> iced::widget::Button<'a, M> {
    let DialogButton {
        label,
        on_press,
        style,
    } = cfg;
    let padding = match style {
        DialogBtnStyle::Emphasis { .. } => DIALOG_BTN_PADDING_EMPHASIS,
        DialogBtnStyle::Neutral => DIALOG_BTN_PADDING_NEUTRAL,
    };
    button(text(label).size(DIALOG_BTN_TEXT_SIZE))
        .on_press(on_press)
        .style(move |theme, st| dialog_btn_style_for(style, theme, st))
        .padding(padding)
}

/// 内容区内联动作按钮：设置项等「表单内容里的操作」用它，比页脚按钮小一档。
///
/// 页脚按钮是弹窗级操作，尺寸要撑起注意力；内联动作是某个字段的附属操作（设置 / 更改 /
/// 关闭主密码、检查更新），应当与同级字段标签相称而非与页脚按钮相称，否则一排按钮会把
/// 整个分类页的视觉重心吸走。配色与悬浮反馈仍复用 [`DialogBtnStyle`] 及其样式函数，
/// 强调色走同一取色点，不产生第二套实现。
pub fn inline_action_button<'a, M: Clone + 'a>(
    label: impl Into<String>,
    on_press: M,
    style: DialogBtnStyle,
) -> iced::widget::Button<'a, M> {
    let padding = match style {
        DialogBtnStyle::Emphasis { .. } => INLINE_BTN_PADDING_EMPHASIS,
        DialogBtnStyle::Neutral => INLINE_BTN_PADDING_NEUTRAL,
    };
    button(text(label.into()).size(INLINE_BTN_TEXT_SIZE))
        .on_press(on_press)
        .style(move |theme, st| dialog_btn_style_for(style, theme, st))
        .padding(padding)
}

/// 右键菜单中的单条可点击项：文字铺满行宽，悬停 / 按下以主题 `hover` 色反馈。
///
/// 不捕获任何环境，可在任意面板间复用（此前各面板各自维护一份逐字相同的实现，现已统一到此处）。
pub fn menu_entry<'a, M: Clone + 'a>(label: impl Into<String>, msg: M) -> Element<'a, M> {
    button(text(label.into()).width(Length::Fill))
        .on_press(msg)
        .style(|theme, st| {
            let hbg = theme::custom_palette(theme).hover;
            let (bg, border) = match st {
                button::Status::Hovered | button::Status::Pressed => (
                    Some(Background::Color(hbg)),
                    Border {
                        color: hbg,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                ),
                _ => (None, Border::default().width(0)),
            };
            button::Style {
                background: bg,
                border,
                text_color: theme.extended_palette().background.strong.text,
                ..Default::default()
            }
        })
        .width(Length::Fill)
        .padding([4, 8])
        .into()
}

/// 右键菜单容器：固定宽度、内边距与抬升边框，承载 [`menu_entry`] 列表。
///
/// 此前各面板各写一遍相同的容器样式（背景强色 + 边框 + 圆角 6），现已统一到此处。
pub fn menu_container<'a, M: Clone + 'a>(content: Element<'a, M>) -> Element<'a, M> {
    container(content)
        .width(Length::Fixed(MENU_WIDTH))
        .padding(4)
        .style(|theme: &Theme| container::Style {
            background: Some(Background::Color(
                theme.extended_palette().background.strong.color,
            )),
            border: Border {
                color: theme::custom_palette(theme).border,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// 右键菜单中的分组分割线：一行低对比度细线，用于把菜单项按功能分类。
///
/// 与 [`menu_entry`] / [`menu_container`] 同属右键菜单构件，泛型 `M` 仅用于推断菜单消息类型。
/// 线本身复用 [`hairline`]，与弹窗内的分隔线同一取色来源。
pub fn menu_separator<'a, M: Clone + 'a>() -> Element<'a, M> {
    let line: Element<'a, M> = hairline();
    container(line).padding([2, 4]).width(Length::Fill).into()
}

/// 文本框 / 组合框输入区样式：跟随主题底色，圆角边框（名称 / 主机 / 路径等复用）。
pub fn text_input_style(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let p = theme::custom_palette(theme);
    let pal = theme.extended_palette();
    let border_color = match status {
        text_input::Status::Focused { .. } => theme::accent_color(theme),
        text_input::Status::Hovered => theme::input_border_hover(theme),
        _ => theme::input_border_color(theme),
    };
    text_input::Style {
        background: Background::Color(pal.background.base.color),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 6.0.into(),
        },
        icon: pal.background.base.text,
        placeholder: p.text_secondary,
        value: pal.background.base.text,
        selection: pal.primary.strong.color,
    }
}

/// 弹窗按钮的视觉样式。
#[derive(Debug, Clone, Copy)]
pub enum DialogBtnStyle {
    /// 强调按钮：常态与悬停用强调色（或危险红），白字。
    Emphasis {
        /// 是否为危险操作（删除 / 覆盖 / 指纹变更）：危险按钮用 [`DANGER`] 红而非强调蓝。
        danger: bool,
    },
    /// 中性按钮：跟随表面底色、次要文字色（取消 / 拒绝等）。
    Neutral,
}

/// 弹窗内单个按钮配置。
pub struct DialogButton<M> {
    /// 按钮文字。
    pub label: String,
    /// 点击触发的消息（消息类型 `M` 由调用方决定，故对 `M` 泛型）。
    pub on_press: M,
    /// 视觉样式（见 [`DialogBtnStyle`]）。
    pub style: DialogBtnStyle,
}

/// 通用模态弹窗骨架：标题栏（含强调条）+ 分隔线 + 正文 + 底部按钮行。
///
/// `left` 为次要按钮、`right` 为主操作按钮，二者各自指定 [`DialogBtnStyle`]，整体靠右；
/// `left` 为 `None` 时只渲染 `right` 一个按钮并居中（只读信息框只需「关闭」）。
/// `border` 为 `Some(c)` 时以该色加粗描边（指纹变更警告形态）；`width` 指定面板宽度。
///
/// 抽自 `crate::sftp_dialogs::dialog_panel` 与 `host_key_dialog` 的 `panel`：
/// 二者弹窗内容、按钮消息、面板宽度、危险态位置均不同，故全部外置，仅共享骨架与样式。
/// 骨架自身（标题栏 / 分割线 / 正文内边距 / 按钮行）与会话编辑器保持一致，
/// 调用方不再需要各自拼装这些层级。
pub fn dialog_panel<'a, M: Clone + 'static>(
    title: impl Into<String>,
    body: impl Into<Element<'a, M>>,
    border: Option<Color>,
    width: f32,
    left: Option<DialogButton<M>>,
    right: DialogButton<M>,
) -> Element<'a, M> {
    let footer = match left {
        Some(left) => dialog_footer(None, Some(left), right),
        None => dialog_footer_centered(right),
    };
    let body: Element<'a, M> = body.into();
    let panel = iced::widget::column![
        dialog_title_bar(title, None::<M>),
        hairline(),
        container(body)
            .width(Length::Fill)
            .padding(DIALOG_BODY_PADDING),
        hairline(),
        footer,
    ];

    container(panel)
        .width(width)
        .style(dialog_panel_style(border))
        .into()
}

/// 按 [`DialogBtnStyle`] 分派到具体的按钮样式函数。
fn dialog_btn_style_for(style: DialogBtnStyle, theme: &Theme, st: button::Status) -> button::Style {
    match style {
        DialogBtnStyle::Emphasis { danger } => dialog_btn_style(theme, st, danger),
        DialogBtnStyle::Neutral => dialog_btn_style_neutral(theme, st),
    }
}

/// 对话框强调（确认 / 主操作）按钮样式：常态为纯色实底，悬停 / 按下降到 85% 不透明度，
/// 文字恒为白色。
///
/// `danger` 为 `true` 时底色用 [`DANGER`] 红（删除 / 覆盖 / 指纹变更），否则用统一取色点
/// [`theme::accent_color`]（随用户主题色 / 当前主题生效）。原定义在 `sftp_dialogs`，
/// 因它已是全部弹窗共用的按钮样式，故上移到本模块（顺带消除 `ui` ↔ `sftp_dialogs` 互相依赖）。
pub fn dialog_btn_style(theme: &Theme, status: button::Status, danger: bool) -> button::Style {
    let base = if danger {
        DANGER
    } else {
        theme::accent_color(theme)
    };
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => {
            Color::from_rgba(base.r, base.g, base.b, 0.85)
        }
        _ => base,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: Color::WHITE,
        border: Border::default().rounded(6.0),
        ..Default::default()
    }
}

/// 对话框中性（取消 / 拒绝）按钮样式：表面底色 + 次要文字色 + 细边框。
///
/// 悬停 / 按下抬升一档到 `hover_raised`：中性按钮嵌在抬升面板上，用 `hover` 会与面板底色
/// 完全重合（二者是同一档派生值），看不出反馈。
pub fn dialog_btn_style_neutral(theme: &Theme, status: button::Status) -> button::Style {
    let p = theme::custom_palette(theme);
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => p.hover_raised,
        _ => p.surface,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: p.text_secondary,
        border: Border {
            color: p.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

/// 为任意部件包裹一个悬停 `tooltip`，在 `label` 被裁剪时展示其完整内容（如超长文件名）。
///
/// 延迟与样式与 [`crate::icons::icon_button`] 内置的 tooltip 保持一致。tooltip 文本限宽并
/// 自动换行，避免超长内容横向撑出视口；`position` 由调用处指定（长列表行推荐
/// [`Position::FollowCursor`]），并启用 `snap_within_viewport` 防止浮层溢出可视区。
pub fn hover_tooltip<'a, M>(
    content: impl Into<Element<'a, M>>,
    label: impl Into<String>,
    position: Position,
) -> Element<'a, M>
where
    M: 'a,
{
    /// tooltip 文本最大宽度（像素），超出则换行，避免单行过长。
    const TOOLTIP_MAX_WIDTH: f32 = 400.0;
    let body = container(text(label.into()).size(12).wrapping(Wrapping::WordOrGlyph))
        .max_width(TOOLTIP_MAX_WIDTH);
    tooltip(content, body, position)
        .delay(iced::time::Duration::from_millis(theme::TOOLTIP_DELAY_MS))
        .style(theme::tooltip_style)
        .gap(6.0)
        .snap_within_viewport(true)
        .into()
}
