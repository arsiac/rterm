//! 中心面板的传输队列视图（与“会话 / 文件”并列切换）。
//!
//! 聚合展示所有终端标签的 SFTP 传输队列（上传 / 下载）。每个标签的 SFTP 通道非并发安全，
//! 故单标签内顺序执行，但面板跨标签汇总。每条传输提供进度条、速度 / 剩余时间；可用操作随
//! 状态不同——进行中 / 排队中只可取消，失败可重试或移除，已完成只可移除。

use crate::t;

use crate::App;
use crate::app::transfer::Message;
use crate::icons::{Icon, icon_button};
use crate::sftp_panel::format_size;
use crate::state::{Transfer, TransferDirection, TransferStatus};
use crate::theme;
use iced::alignment::Vertical;
use iced::widget::text::Wrapping;
use iced::widget::tooltip::Position;
use iced::widget::{Space, column, container, row, scrollable, text};
use iced::{Color, Element, Length, Padding};

/// 传输项方向（上传 / 下载）图标尺寸（像素）。
const ITEM_ICON_SIZE: f32 = 16.0;
/// 操作按钮（取消 / 重试 / 移除）图标尺寸（像素），略小于方向图标以保持克制。
const ACTION_ICON_SIZE: f32 = 14.0;

/// 传输队列面板（作为中心面板的一个视图，与“会话 / 文件”并列切换）。
pub fn view(app: &App) -> Element<'_, Message> {
    let transfers: Vec<&Transfer> = app.transfer.all_transfers();

    let header = row![
        Icon::ArrowSort.svg(18.0),
        text(t!("transfer.title", count => transfers.len()))
            .size(13)
            .style(secondary_text),
    ]
    .spacing(6)
    .align_y(Vertical::Center);

    let body: Element<'_, Message> = if transfers.is_empty() {
        container(text(t!("transfer.empty")).size(12).style(secondary_text))
            .padding(12)
            .into()
    } else {
        let items: Vec<Element<'_, Message>> = transfers.iter().map(|t| transfer_item(t)).collect();
        scrollable(column(items).spacing(8).padding(Padding {
            top: 0.0,
            right: 10.0,
            bottom: 0.0,
            left: 0.0,
        }))
        .height(Length::Fill)
        .into()
    };

    // 整面板留白，避免内容贴边。
    column![
        container(header).padding(Padding {
            top: 4.0,
            right: 12.0,
            bottom: 8.0,
            left: 12.0,
        }),
        body,
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(10)
    .into()
}

/// 单条传输项的卡片（方向图标 + 名称 + 状态 / 操作 + 进度条 + 详情）。
fn transfer_item(t: &Transfer) -> Element<'_, Message> {
    let dir_icon = match t.direction {
        TransferDirection::Upload => Icon::CloudArrowUp,
        TransferDirection::Download => Icon::CloudArrowDown,
    };
    let name = text(t.name.clone())
        .size(13)
        .width(Length::Fill)
        .wrapping(Wrapping::None);
    let name_clip = container(name).width(Length::Fill).clip(true);

    let bar_color = match t.status {
        TransferStatus::Done => crate::ui::SUCCESS,
        TransferStatus::Error => crate::ui::ERROR,
        _ => theme::ACCENT,
    };
    // 未知总量且进行中：以满条（低对比）表示“进行中但无法量化”，避免空条误导为 stalled。
    let fill = if t.total > 0 {
        (t.transferred as f32 / t.total as f32).clamp(0.0, 1.0)
    } else if t.status == TransferStatus::Active {
        1.0
    } else {
        0.0
    };
    let bar = thin_bar(fill, bar_color);

    let mut header_row: Vec<Element<'_, Message>> = Vec::new();
    header_row.push(dir_icon.svg(ITEM_ICON_SIZE).into());
    header_row.push(name_clip.into());
    header_row.push(Space::new().width(Length::Fill).into());
    match t.status {
        TransferStatus::Done => {
            header_row.push(
                Icon::Checkmark
                    .svg_with_color(ACTION_ICON_SIZE, crate::ui::SUCCESS)
                    .into(),
            );
        }
        TransferStatus::Error => {
            header_row.push(
                Icon::Warning
                    .svg_with_color(ACTION_ICON_SIZE, crate::ui::ERROR)
                    .into(),
            );
        }
        _ => {}
    }
    match t.status {
        TransferStatus::Active | TransferStatus::Queued => {
            header_row.push(icon_button(
                Icon::Dismiss,
                ACTION_ICON_SIZE,
                t!("common.cancel"),
                crate::app::transfer::Message::CancelTransfer(t.id),
                Position::Left,
            ));
        }
        TransferStatus::Error => {
            header_row.push(icon_button(
                Icon::ArrowClockwise,
                ACTION_ICON_SIZE,
                t!("common.retry"),
                crate::app::transfer::Message::RetryTransfer(t.id),
                Position::Left,
            ));
            header_row.push(icon_button(
                Icon::Dismiss,
                ACTION_ICON_SIZE,
                t!("common.remove"),
                crate::app::transfer::Message::RemoveTransfer(t.id),
                Position::Left,
            ));
        }
        TransferStatus::Done => {
            header_row.push(icon_button(
                Icon::Dismiss,
                ACTION_ICON_SIZE,
                t!("common.remove"),
                crate::app::transfer::Message::RemoveTransfer(t.id),
                Position::Left,
            ));
        }
    }
    let header = row(header_row).spacing(4).align_y(Vertical::Center);

    // 每条传输渲染为带背景与圆角的卡片，风格对齐会话 / 文件列表行。
    container(column![header, bar, detail_text(t)].spacing(4))
        .padding([8, 10])
        .style(|theme| {
            let p = crate::theme::custom_palette(theme);
            container::Style {
                background: Some(p.surface_raised.into()),
                border: iced::Border {
                    color: p.border,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

/// 单条传输的次要详情文字：进度百分比、已传 / 总量、实时速率与剩余时间。
fn detail_text(t: &Transfer) -> Element<'_, Message> {
    let s = match t.status {
        TransferStatus::Queued => t!("transfer.queued"),
        _ => {
            let size_part = if t.total > 0 {
                format!("{} / {}", format_size(t.transferred), format_size(t.total))
            } else {
                format_size(t.transferred)
            };
            let pct = if t.total > 0 {
                format!(
                    "{}%",
                    (t.transferred as f32 / t.total as f32 * 100.0) as u32
                )
            } else {
                String::new()
            };
            let speed = if t.status == TransferStatus::Active && t.speed > 0.0 {
                format!(" · {}", format_rate(t.speed))
            } else {
                String::new()
            };
            let eta = if t.status == TransferStatus::Active && t.speed > 0.0 && t.total > 0 {
                let rem = (t.total.saturating_sub(t.transferred)) as f64 / t.speed;
                t!("transfer.eta", time => format_eta(rem))
            } else {
                String::new()
            };
            let mut s = pct;
            if !s.is_empty() {
                s.push(' ');
            }
            s.push_str(&size_part);
            s.push_str(&speed);
            s.push_str(&eta);
            s
        }
    };
    text(s).size(11).style(secondary_text).into()
}

/// 矮进度条（高度固定 5px）。
///
/// iced 0.14 的 `ProgressBar` 有 `girth()` 可调粗细，但它同时决定轨道与滑块样式、且与卡片内的
/// 圆角背景不易对齐，故这里以两层容器自绘：外层铺满低对比灰底，内层按 `fill` 比例占据宽度，
/// 用 `FillPortion` 配剩余占位实现比例填充。
fn thin_bar(fill: f32, color: Color) -> Element<'static, Message> {
    let pct = (fill.clamp(0.0, 1.0) * 100.0) as u16;
    let bar = container("")
        .width(Length::FillPortion(pct))
        .height(Length::Fixed(5.0))
        .style(move |_t| container::Style {
            background: Some(color.into()),
            ..Default::default()
        });
    let track = container(
        if pct >= 100 {
            row![bar]
        } else {
            row![
                bar,
                container("")
                    .width(Length::FillPortion(100 - pct))
                    .height(Length::Fixed(5.0))
            ]
        }
        .spacing(0),
    )
    .width(Length::Fill)
    .height(Length::Fixed(5.0))
    .style(|_t| container::Style {
        background: Some(Color::from_rgba(0.5, 0.5, 0.5, 0.25).into()),
        ..Default::default()
    });
    track.into()
}

/// 次要文本样式（说明文字、图标等），随主题自动协调。
fn secondary_text(theme: &iced::Theme) -> text::Style {
    text::Style {
        color: Some(theme::custom_palette(theme).text_secondary),
    }
}

/// 将速度（字节/秒）格式化为可读速率。
fn format_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", format_size(bytes_per_sec as u64))
}

/// 将剩余秒数格式化为 `NhNm` / `NmNs` / `Ns`。
fn format_eta(sec: f64) -> String {
    let s = sec as u64;
    if s >= 3600 {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{}s", s / 60, s % 60)
    } else {
        format!("{}s", s)
    }
}
