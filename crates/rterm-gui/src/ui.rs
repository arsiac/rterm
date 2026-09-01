//! 跨面板复用的通用 UI 构件与样式（右键菜单项、输入框、模态弹窗、语义色）。
//!
//! 抽自 `session_panel` / `sftp_panel` / `settings_dialog` / `sftp_dialogs` /
//! `host_key_dialog` 中重复出现的样式与弹窗骨架，集中维护以避免多份实现失同步。

use crate::icons::Icon;
use crate::sftp_dialogs::{dialog_btn_style, dialog_btn_style_neutral};
use crate::theme;
use iced::widget::button;
use iced::widget::container;
use iced::widget::svg;
use iced::widget::text;
use iced::widget::text_input;
use iced::{Background, Border, Color, Element, Length, Theme};

/// 右键上下文菜单宽度（像素）。
pub const MENU_WIDTH: f32 = 140.0;

/// 成功态（传输完成、导入成功等）绿色。
pub const SUCCESS: Color = Color::from_rgb(0.18, 0.6, 0.33);
/// 错误态红色。
pub const ERROR: Color = Color::from_rgb(0.8, 0.2, 0.2);
/// 警告态（钥匙串丢失、凭据可能失效等）琥珀色。
pub const WARNING: Color = Color::from_rgb(0.85, 0.6, 0.1);
/// 危险操作（删除 / 覆盖 / 指纹变更）红色。
pub const DANGER: Color = Color::from_rgb(0.85, 0.30, 0.30);

/// 弹窗右上角的关闭按钮（✕ 图标）：主题感知文字色 + 悬浮高亮，所有模态弹窗统一复用，
/// 避免各弹窗各自实现导致样式漂移。传入点击后触发的消息 `M`（如
/// `Message::Settings(settings::Message::Toggle)` / `Message::MasterPw(masterpw::Message::Cancel)` /
/// `Message::MasterPw(masterpw::Message::ChangeCancel)`，或经 `.map` 接入的子模块消息）。
pub fn dialog_close_button<M: Clone + 'static>(on_press: M) -> Element<'static, M> {
    let icon = Icon::Dismiss
        .svg(16.0)
        .style(|theme: &Theme, _status| svg::Style {
            color: Some(theme.extended_palette().background.strong.text),
        });
    button(icon)
        .on_press(on_press)
        .style(|theme, status| theme::icon_button_style(theme, status, false))
        .into()
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

/// 文本框 / 组合框输入区样式：跟随主题底色，圆角边框（名称 / 主机 / 路径等复用）。
///
/// 此前会话面板、设置弹窗各有一份逐字相同的输入样式、文件面板另有 4 处内联同款样式，
/// 现已统一复用此函数。
pub fn text_input_style(theme: &Theme, _status: text_input::Status) -> text_input::Style {
    let p = theme::custom_palette(theme);
    let pal = theme.extended_palette();
    text_input::Style {
        background: Background::Color(p.surface),
        border: Border {
            color: p.border,
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

/// 通用模态弹窗骨架：标题 + 分隔线 + 正文 + 左右两个按钮。
///
/// `left` 置于左侧、`right` 置于右侧；二者各自指定 [`DialogBtnStyle`]。
/// `border` 为 `Some(c)` 时以该色加粗描边（指纹变更警告形态）；`width` 指定面板宽度。
///
/// 抽自 `crate::sftp_dialogs::dialog_panel` 与 `host_key_dialog` 的 `panel`：
/// 二者弹窗内容、按钮消息、面板宽度、危险态位置均不同，故全部外置，仅共享骨架与样式。
pub fn dialog_panel<'a, M: Clone + 'a>(
    title: impl Into<String>,
    body: impl Into<Element<'a, M>>,
    border: Option<Color>,
    width: f32,
    left: DialogButton<M>,
    right: DialogButton<M>,
) -> Element<'a, M> {
    let panel = iced::widget::column![
        text(title.into()).size(18),
        iced::widget::rule::horizontal(1),
        body.into(),
        iced::widget::row![
            container(
                button(text(left.label).size(14))
                    .on_press(left.on_press)
                    .style(move |theme, st| dialog_btn_style_for(left.style, theme, st))
            )
            .width(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right),
            button(text(right.label).size(14))
                .on_press(right.on_press)
                .style(move |theme, st| dialog_btn_style_for(right.style, theme, st)),
        ]
        .spacing(10),
    ]
    .spacing(16)
    .padding(20);

    container(panel)
        .width(width)
        .style(move |theme: &Theme| {
            let p = theme::custom_palette(theme);
            container::Style {
                background: Some(Background::Color(p.surface_raised)),
                border: match border {
                    Some(c) => Border {
                        color: c,
                        width: 2.0,
                        radius: 8.0.into(),
                    },
                    None => Border {
                        color: p.border,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                },
                ..Default::default()
            }
        })
        .into()
}

/// 按 [`DialogBtnStyle`] 分派到具体的按钮样式函数。
fn dialog_btn_style_for(style: DialogBtnStyle, theme: &Theme, st: button::Status) -> button::Style {
    match style {
        DialogBtnStyle::Emphasis { danger } => dialog_btn_style(st, danger),
        DialogBtnStyle::Neutral => dialog_btn_style_neutral(theme, st),
    }
}
