//! 轻量、无生命周期约束的 toast 通知组件（基于 `iced_toaster` 0.2.0 源码，纳入本 crate 定制）。
//!
//! 与上游的差异：
//! - 关闭按钮（×）复用弹窗关闭按钮的同款样式（扁平、主题感知文字色 + 悬浮高亮，见
//!   [`crate::theme::icon_button_style`]），与所有模态弹窗的关闭按钮视觉一致；
//! - 保留定时自动消失（见 [`Toaster::dismiss_expired`]），并在悬浮时暂停计时。
//!
//! 状态管理由 [`Toaster`] 承担，UI 在 [`Toaster::view`] 中叠加到内容之上。

use crate::theme;

use iced::widget::svg::{self, Handle, Svg};
use iced::widget::{Column, button, column, container, mouse_area, row, stack, text};
use iced::{Alignment, Border, Color, Length, Theme};

/// 本模块内统一的 `iced` 元素类型别名（绑定默认 `Theme`）。
type Element<'a, M> = iced::Element<'a, M, Theme>;

/// 存放所有 toast 的管理器。
#[derive(Debug)]
pub struct Toaster<M> {
    /// 当前所有 toast 及其标识的有序列表。
    toasts: Vec<(ToastId, Toast<M>)>,
    /// 自增的下一个 toast 标识计数器。
    next_id: u64,
    /// 同时可见的最大 toast 数，超出后挤掉最旧一条。
    max_toasts: usize,
    /// toast 的显示位置（屏幕四角之一）。
    position: Position,
}

/// 创建一个 [`Toaster`]。
pub fn toaster<M>() -> Toaster<M> {
    Toaster {
        toasts: Vec::new(),
        next_id: 0,
        max_toasts: 5,
        position: Position::default(),
    }
}

impl<M> Toaster<M> {
    /// 设置 toast 的显示位置。
    pub fn set_position(&mut self, position: Position) {
        self.position = position;
    }

    /// 获取 toast 的显示位置。
    pub fn get_position(&self) -> Position {
        self.position
    }

    /// 设置同时可见的最大 toast 数，超出后挤掉最旧的一条。
    pub fn max_toasts(mut self, max: usize) -> Self {
        self.max_toasts = max;
        self
    }

    /// 向管理器推入一条 toast，返回其 [`ToastId`]。
    pub fn push(&mut self, toast: Toast<M>) -> ToastId {
        let id = ToastId(self.next_id);
        self.next_id += 1;

        if self.toasts.len() >= self.max_toasts {
            self.toasts.remove(0);
        }

        self.toasts.push((id, toast));

        id
    }

    /// 设置某条 toast 的悬浮状态（悬浮时暂停其自动消失计时）。
    pub fn set_hovered(&mut self, id: ToastId, hovered: bool) {
        let Some((_, toast)) = self.toasts.iter_mut().find(|(tid, _)| *tid == id) else {
            return;
        };

        toast.hovered = hovered;
        if !hovered {
            // 重置起始时刻，使离开后用户仍有完整时长查看。
            toast.spawned_at = std::time::Instant::now();
        }
    }

    /// 关闭某条 toast。
    pub fn dismiss(&mut self, id: ToastId) {
        self.toasts.retain(|(toast_id, _)| *toast_id != id);
    }

    /// 关闭所有已到期的 toast（悬浮中的除外）。
    pub fn dismiss_expired(&mut self) {
        self.toasts
            .retain(|(_, toast)| toast.hovered || toast.spawned_at.elapsed() < toast.duration);
    }

    /// 管理器是否为空。
    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    /// 渲染入口：将 `content` 与 toast 层叠加，并传入关闭 / 悬浮消息构造器。
    pub fn view<'a>(
        &'a self,
        content: impl Into<Element<'a, M>>,
        on_dismiss: impl Fn(ToastId) -> M + 'a,
        on_hover: impl Fn(ToastId, bool) -> M + 'a,
    ) -> Element<'a, M>
    where
        M: Clone + 'a,
    {
        let toast_column = Column::with_children(self.toasts.iter().map(|(id, toast)| {
            let id = *id;

            let level = toast.level;

            let mut toast_row = row![
                column![
                    text(&toast.title).size(14).style(move |theme: &Theme| {
                        text::Style {
                            color: Some(level.color(theme)),
                        }
                    }),
                    text(&toast.text).size(12),
                ]
                .spacing(4)
                .width(Length::Fill),
            ];

            for (label, msg) in &toast.actions {
                toast_row = toast_row.push(button(text(label).size(12)).on_press(msg.clone()));
            }

            // 关闭按钮：复用弹窗关闭按钮的同款图标（`Icon::Dismiss`）与样式（扁平 + 悬浮高亮），
            // 与所有模态弹窗的关闭按钮视觉一致。
            // `Icon::Dismiss` 的 SVG 字节为 `'static`（内嵌资源），`Handle::from_memory` 仅要求数据
            // 本身 `'static`；`Svg<'a>` 的生命周期 `'a` 来自其 `style` 闭包（此处为非捕获闭包，可匹配
            // 任意 `'a`），故按钮整体为 `Element<'a, M>`，无需把 `M` 收紧为 `'static` 即可直接推入
            // `view<'a>` 的 `Row<'a>`（与上游 `toast` 用文字 “×” 时的生命周期约束一致）。
            let dismiss_bytes: &'static [u8] = crate::icons::Icon::Dismiss.bytes();
            let close_icon: Svg<'a> = Svg::new(Handle::from_memory(dismiss_bytes))
                .width(Length::Fixed(16.0))
                .height(Length::Fixed(16.0))
                .style(|theme: &Theme, _status| svg::Style {
                    color: Some(theme.extended_palette().background.strong.text),
                });
            toast_row = toast_row.push(
                button(close_icon)
                    .on_press(on_dismiss(id))
                    .style(|theme, status| theme::icon_button_style(theme, status, false)),
            );

            let progress = toast.spawned_at.elapsed().as_secs_f32() / toast.duration.as_secs_f32();
            let border_width = 4.0 * (1.0 - progress.min(1.0)).max(0.5);

            mouse_area(
                container(toast_row.spacing(8).align_y(Alignment::Center))
                    .padding(12)
                    .width(300)
                    .style(move |theme: &Theme| {
                        let palette = theme.extended_palette();
                        container::Style {
                            background: Some(match toast.hovered {
                                false => palette.background.weak.color.into(),
                                true => palette.background.strong.color.into(),
                            }),
                            border: Border {
                                color: level.color(theme),
                                width: border_width,
                                radius: 8.0.into(),
                            },
                            ..Default::default()
                        }
                    }),
            )
            .on_enter(on_hover(id, true))
            .on_exit(on_hover(id, false))
            .into()
        }))
        .spacing(8)
        .padding(12)
        .width(Length::Shrink)
        .align_x(match self.position {
            Position::TopLeft | Position::BottomLeft => Alignment::Start,
            Position::TopRight | Position::BottomRight => Alignment::End,
        });

        let (align_x, align_y) = match self.position {
            Position::TopLeft => (Alignment::Start, Alignment::Start),
            Position::TopRight => (Alignment::End, Alignment::Start),
            Position::BottomLeft => (Alignment::Start, Alignment::End),
            Position::BottomRight => (Alignment::End, Alignment::End),
        };

        let toast_container = container(toast_column)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(align_x)
            .align_y(align_y);

        stack![content.into(), toast_container].into()
    }
}

/// toast 的标识（内部自增 `u64`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToastId(u64);

/// toast 显示位置，用 [`Toaster::get_position`] / [`Toaster::set_position`] 读取 / 设置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    /// 左上。
    TopLeft,
    /// 右上。
    TopRight,
    /// 左下。
    BottomLeft,
    /// 右下（默认）。
    #[default]
    BottomRight,
}

impl Position {
    /// 顺时针切到下一个位置。
    pub fn next(self) -> Self {
        match self {
            Position::TopLeft => Position::TopRight,
            Position::TopRight => Position::BottomRight,
            Position::BottomRight => Position::BottomLeft,
            Position::BottomLeft => Position::TopLeft,
        }
    }
}

/// toast 级别，决定边框颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    /// 提示（蓝）。
    Info,
    /// 成功（绿）。
    Success,
    /// 警告（琥珀）。
    Warning,
    /// 错误（红）。
    Error,
}

impl ToastLevel {
    /// 按级别返回对应的边框颜色（蓝、绿、琥珀、红）。
    fn color(&self, theme: &Theme) -> Color {
        let palette = theme.extended_palette();
        match self {
            ToastLevel::Info => palette.primary.strong.color,
            ToastLevel::Success => palette.success.strong.color,
            ToastLevel::Warning => palette.warning.strong.color,
            ToastLevel::Error => palette.danger.strong.color,
        }
    }
}

/// 单条 toast。
#[derive(Debug, Clone)]
pub struct Toast<M> {
    /// 正文内容（主标题下的描述文字）。
    text: String,
    /// 级别，决定边框颜色。
    level: ToastLevel,
    /// 标题（主行，按级别着色）。
    title: String,
    /// 存活时长，超时后自动消失。
    duration: std::time::Duration,
    /// 创建时刻，用于计算剩余存活时间与悬浮重置计时。
    spawned_at: std::time::Instant,
    /// 附加的操作按钮（文案 + 消息）。
    actions: Vec<(String, M)>,
    /// 是否处于悬浮态，悬浮时暂停自动消失计时。
    hovered: bool,
}

/// 创建一条 toast。
pub fn toast<M>(text: impl Into<String>) -> Toast<M> {
    Toast {
        text: text.into(),
        level: ToastLevel::Info,
        title: "Info".to_string(),
        duration: std::time::Duration::from_secs(5),
        spawned_at: std::time::Instant::now(),
        actions: Vec::new(),
        hovered: false,
    }
}

impl<M> Toast<M> {
    /// 设置标题。
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// 设置级别（[`ToastLevel`]）。
    pub fn level(mut self, level: ToastLevel) -> Self {
        self.level = level;
        self
    }

    /// 设置存活时长（秒）。
    pub fn duration(mut self, duration: u64) -> Self {
        self.duration = std::time::Duration::from_secs(duration);
        self
    }

    /// 附加一个操作按钮（文案 + 消息）。
    pub fn action(mut self, label: impl Into<String>, message: M) -> Self {
        self.actions.push((label.into(), message));
        self
    }
}
