//! 中心面板的传输队列视图（与“会话 / 文件”并列切换）。
//!
//! 聚合展示所有终端标签的 SFTP 传输队列（上传 / 下载），按全局并发 N 调度（跨标签、跨方向
//! 共享额度，见 `app::transfer`）。每条传输提供进度条、速度 / 剩余时间；可用操作随状态不同，
//! 失败与等待重试的行会附上原因。
//!
//! **琥珀色（WARNING）标记「还没恢复」的行**，有两种形态：
//!
//! 1. **等待重试**：退避计时中，行上写「等待重试（第 n/N 次） · Ns 后重试」；
//! 2. **重试中、但尚无数据**：[`is_retrying_without_data`]——worker 已跑起来（`Active`）却
//!    一个字节都还没落地。核心层在每次尝试的读写循环**之前**先回调一次 `(0, total)`，故
//!    「尚无数据」在 UI 侧是可见的事实而非猜测。
//!
//! 琥珀在**第一个字节到达时结束**（`transferred > 0`）：它的可见时长由「故障持续多久」决定，
//! 与退避步长无关。整行取 [`crate::ui::WARNING`]（卡片描边、空轨底色、状态图标与文字），与
//! 最终失败的红色区分。只染「空轨」而不填满进度条：染轨道只声明状态，不谎报进度。
//!
//! 自动重试的次数（上限读自 `[transfer] retry_attempts`）**只出现在「坏消息」里**——等待重试、
//! 重试中尚无数据、最终失败；数据一到，那一行就回到干净样子。标题栏只放总数，各态计数由每行
//! 自己表达（汇总行在窄面板上必然折行）。

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
use iced::widget::{column, container, row, scrollable, text};
use iced::{Color, Element, Length, Padding};
use std::time::Instant;

/// 传输项方向（上传 / 下载）图标尺寸（像素）。
const ITEM_ICON_SIZE: f32 = 16.0;
/// 操作按钮（取消 / 重试 / 移除）图标尺寸（像素），略小于方向图标以保持克制。
const ACTION_ICON_SIZE: f32 = 14.0;

/// 传输队列面板（作为中心面板的一个视图，与“会话 / 文件”并列切换）。
pub fn view(app: &App) -> Element<'_, Message> {
    let transfers: Vec<&Transfer> = app.transfer.all_transfers();
    // 自动重试的次数上限（`[transfer] retry_attempts`）用于渲染每行的「第 n/N 次」分母。
    let retry_limit = app.config.transfer.retry_attempts;

    let header = row![
        Icon::ArrowSort.svg(18.0),
        text(t!("transfer.title", count => transfers.len()))
            .size(13)
            .style(secondary_text),
    ]
    .spacing(8)
    .align_y(Vertical::Center);

    let body: Element<'_, Message> = if transfers.is_empty() {
        container(text(t!("transfer.empty")).size(12).style(secondary_text))
            .padding(12)
            .into()
    } else {
        let items: Vec<Element<'_, Message>> = transfers
            .iter()
            .map(|t| transfer_item(t, retry_limit))
            .collect();
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
///
/// `retry_limit` 为自动重试的次数上限，仅用于等待重试 / 重试中尚无数据 / 最终失败态的
/// 「第 n/N 次」文案。
fn transfer_item(t: &Transfer, retry_limit: u32) -> Element<'_, Message> {
    let dir_icon = match t.direction {
        TransferDirection::Upload => Icon::CloudArrowUp,
        TransferDirection::Download => Icon::CloudArrowDown,
    };
    // 借用 `t` 中的名称而非克隆：并发后队列更长，每帧全量克隆会持续累积分配。
    // 悬浮提示仍需 owned 字符串（tooltip 的 'static 约束），故只在这里保留那次克隆。
    let name = text(t.name.as_str())
        .size(13)
        .width(Length::Fill)
        .wrapping(Wrapping::None);
    let name_clip = container(name).width(Length::Fill).clip(true);
    let name_clip = crate::ui::hover_tooltip(name_clip, t.name.clone(), Position::FollowCursor);

    // 配色只有一个入口：`Tone`（由状态 + 「重试中尚无数据」派生）。强调色分支走统一取色点
    // `accent_color`（随用户主题色 / 当前主题生效）；SUCCESS / ERROR / WARNING 为固定语义色，
    // 不随主题漂移。琥珀而非红：还没失败到底，会自动恢复，用红会让用户以为要重传。
    let tone = tone_of(t);
    let hue = tone_color(tone);
    let bar_color: Box<dyn Fn(&iced::Theme) -> Color + 'static> = match hue {
        Some(c) => Box::new(move |_t: &iced::Theme| c),
        None => Box::new(crate::theme::accent_color),
    };
    // 见 `progress_fraction`：总量未知画空条，满条会被读成「已经下完了」。
    let fill = progress_fraction(t);
    // 琥珀态最常以**空轨**面目出现（总量未知 / 尚无数据 → 没有可填的段），只染轨道才能让整行
    // 都带上状态色，而非只剩一个 14px 图标。
    let track = match tone {
        Tone::Retry => with_alpha(crate::ui::WARNING, 0.30),
        _ => TRACK_BASE,
    };
    let bar = thin_bar(fill, bar_color, track);

    let mut header_row: Vec<Element<'_, Message>> = Vec::new();
    header_row.push(dir_icon.svg(ITEM_ICON_SIZE).into());
    header_row.push(name_clip);
    // 状态图标同样由 `Tone` 决定（等待重试与「重试中尚无数据」共用琥珀循环箭头：稍后会自己再跑，
    // 不用警告三角——它还没失败到底）。
    match tone {
        Tone::Success => {
            header_row.push(
                Icon::Checkmark
                    .svg_with_color(ACTION_ICON_SIZE, crate::ui::SUCCESS)
                    .into(),
            );
        }
        Tone::Failed => {
            header_row.push(
                Icon::Warning
                    .svg_with_color(ACTION_ICON_SIZE, crate::ui::ERROR)
                    .into(),
            );
        }
        Tone::Retry => {
            header_row.push(
                Icon::ArrowClockwise
                    .svg_with_color(ACTION_ICON_SIZE, crate::ui::WARNING)
                    .into(),
            );
        }
        Tone::Accent => {}
    }
    match t.status {
        // 等待重试只提供「取消」：退避到点会自动继续，不需要（也不该有）「立即重试」——
        // 那等于绕过退避，让用户在服务端刚出问题时立刻再撞一次。
        TransferStatus::Active | TransferStatus::Queued | TransferStatus::WaitingRetry => {
            header_row.push(icon_button(
                Icon::Dismiss,
                ACTION_ICON_SIZE,
                t!("common.cancel"),
                crate::app::transfer::Message::CancelTransfer(t.id),
                Position::Left,
            ));
        }
        TransferStatus::Error => {
            // 有断点时说「继续下载 / 继续上传」而非「重试」：发的仍是同一条 `RetryTransfer`，
            // 差别只在用户知道自己不必从头再来。
            let label = if can_continue(t) {
                continue_label(t)
            } else {
                t!("common.retry")
            };
            header_row.push(icon_button(
                Icon::ArrowClockwise,
                ACTION_ICON_SIZE,
                label,
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
                Icon::FolderOpen,
                ACTION_ICON_SIZE,
                t!("common.open_folder"),
                crate::app::transfer::Message::OpenContainingFolder(t.local.clone()),
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
    }
    let header = row(header_row).spacing(4).align_y(Vertical::Center);

    // 每条传输渲染为带背景与圆角的卡片，风格对齐会话 / 文件列表行。
    // 琥珀态与最终失败的描边染状态色：文字与图标都只有 11–14px，描边是唯一能覆盖整行的色彩
    // 通道，扫一眼面板就能分出「还没恢复」与「已失败」。成功态不染（一批下载完成时满屏绿框
    // 反而吵闹，绿色已由满条进度条与对勾表达）。
    let border_tint = match tone {
        Tone::Retry => Some(crate::ui::WARNING),
        Tone::Failed => Some(crate::ui::ERROR),
        Tone::Success | Tone::Accent => None,
    };
    container(column![header, bar, detail_text(t, retry_limit)].spacing(4))
        .padding([8, 10])
        .style(move |theme| {
            let p = crate::theme::custom_palette(theme);
            container::Style {
                background: Some(p.surface_raised.into()),
                border: iced::Border {
                    color: border_tint.map_or(p.border, |c| with_alpha(c, 0.55)),
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

/// 一行的配色基调。渲染层唯一的配色输入：由 `Transfer` 派生，行内所有着色元素（进度条填充、
/// 轨道、卡片描边、状态图标、两行文字）都从这里取，故同一行不可能出现两种基调。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tone {
    /// 排队中 / 传输中：跟随主题强调色。
    Accent,
    /// 琥珀：还没恢复（等待重试，或重试中尚无数据）。
    Retry,
    /// 红：最终失败。
    Failed,
    /// 绿：已完成。
    Success,
}

/// 派生一行的基调。唯一的分支在 `Accent` 里：[`is_retrying_without_data`] 把「重试中但尚无数据」
/// 也归入琥珀。判据刻意在渲染层派生而不新增 `TransferStatus` 变体——调度器全部键在状态上，
/// 而这里需要的只是同一状态下的两种呈现。
fn tone_of(t: &Transfer) -> Tone {
    match t.status {
        TransferStatus::Done => Tone::Success,
        TransferStatus::Error => Tone::Failed,
        TransferStatus::WaitingRetry => Tone::Retry,
        TransferStatus::Active | TransferStatus::Queued => {
            if is_retrying_without_data(t) {
                Tone::Retry
            } else {
                Tone::Accent
            }
        }
    }
}

/// 该行是否处于「重试尝试进行中、但一个字节都还没拿到」：`Active` && `attempts > 0`（首次尝试
/// 不适用，否则每次下载起手都闪琥珀）&& `transferred == 0`。
///
/// 第三个条件是可观测的事实而非猜测：worker 起步时会把 stat 出来的真实起点经 `AttemptStarted`
/// 记到行上，续传的重试因此 `transferred` 直接就是断点（> 0），天然不显琥珀。
fn is_retrying_without_data(t: &Transfer) -> bool {
    t.status == TransferStatus::Active && t.attempts > 0 && t.transferred == 0
}

/// 失败行的按钮该说「继续（下载 / 上传）」还是「重试」：上一轮起点大于 0 才算有断点。
///
/// **只是文案判据**——真能不能续由下一轮的 stat 说了算（源端变了照样从头写）。判据与方向无关，
/// 两侧的 `.part` 是同一套地基，方向词由 [`continue_label`] 给出。
fn can_continue(t: &Transfer) -> bool {
    t.resume.is_some_and(|r| r.offset > 0)
}

/// 可续传的行上按钮的文案：说清「继续」的是哪一侧，免得被读成重新来一遍。
fn continue_label(t: &Transfer) -> String {
    match t.direction {
        TransferDirection::Download => t!("transfer.resume_download"),
        TransferDirection::Upload => t!("transfer.resume_upload"),
    }
}

/// 基调对应的语义色；`Accent` 无固定色（走 `theme::accent_color`）。固定常量不随主题漂移，
/// 保持「红 = 出错、琥珀 = 稍后自己再试、绿 = 完成」的直觉。
fn tone_color(tone: Tone) -> Option<Color> {
    match tone {
        Tone::Success => Some(crate::ui::SUCCESS),
        Tone::Failed => Some(crate::ui::ERROR),
        Tone::Retry => Some(crate::ui::WARNING),
        Tone::Accent => None,
    }
}

/// 语义色加透明度：描边（0.55）与空轨底色（0.30）共用，保证同一状态在不同元素上是同一色相。
fn with_alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

/// 进度条填充比例（0.0..=1.0）。
///
/// 总量未知（总量取自远端元数据，取不到时为 0）时返回 **0**，即空条——满条会被读成「已经
/// 下完了」，空条虽不表达进度但绝不说谎。唯一的例外是 `Done`（总量为 0 的完成态，满条即
/// 「已完成」）。
fn progress_fraction(t: &Transfer) -> f32 {
    if t.status == TransferStatus::Done {
        1.0
    } else if t.total > 0 {
        (t.transferred as f32 / t.total as f32).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// 单条传输的次要详情：主行（状态 / 进度 / 速率）+ 失败原因 + 半成品提示。
///
/// 失败与等待重试必须把 `error` 显示出来——否则用户只能看到一个红色图标，无从判断
/// 是网络抖动还是权限不足（自动重试到底值不值得等，全看这句话）。
fn detail_text(t: &Transfer, retry_limit: u32) -> Element<'_, Message> {
    let main = detail_line(t, retry_limit);
    // 琥珀态的两行文字都取琥珀：失败原因若仍是红色，整行看上去与最终失败无异。
    let retrying = tone_of(t) == Tone::Retry;
    let (main_style, note_style): (TextStyle, TextStyle) = if retrying {
        (warning_text, warning_text)
    } else {
        (secondary_text, error_text)
    };

    let mut col = column![text(main).size(11).style(main_style)].spacing(2);
    // 失败原因在琥珀态与最终失败两处都要给出：琥珀态下它回答「为什么在重试」，而重试尚无数据
    // 的那一段里 `error` 是刻意保留的（见 `app::transfer::Message::RetryDue`）。
    if (retrying || t.status == TransferStatus::Error)
        && let Some(err) = &t.error
    {
        col = col.push(text(err.as_str()).size(11).style(note_style));
    }
    // 半成品清理失败（仅下载侧会走到这里）：给出具体路径，让用户知道该手动删哪个文件。
    if let Some(path) = &t.partial {
        col = col.push(
            text(t!("transfer.partial_left", path => path.as_str()))
                .size(11)
                .style(error_text),
        );
    }
    col.into()
}

/// 详情主行的文本（纯函数，便于单测）。
fn detail_line(t: &Transfer, retry_limit: u32) -> String {
    match t.status {
        TransferStatus::Queued => t!("transfer.queued"),
        TransferStatus::WaitingRetry => {
            // 倒计时由 `not_before` 与**渲染时刻**作差得到。渲染是惰性的，但 iced 在每条消息后
            // 都会重建界面，而 `Message::ToastTick` 心跳每 500 ms 必然触发一次，故读数最多滞后
            // 一个心跳——不必为此新增订阅。
            // 向上取整：还剩 0.4 s 时显示「1s 后重试」而不是「0s」（后者会被读成卡住）。
            let secs = t
                .not_before
                .map(|deadline| {
                    deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis()
                        .div_ceil(1000) as u64
                })
                .unwrap_or(0);
            t!(
                "transfer.retry_wait",
                attempt => t.attempts,
                max => retry_limit,
                secs => secs
            )
        }
        // 自动重试的计数只属于「坏消息」：等待重试（上一支自带 n/N）与最终失败。挂在「重试中 /
        // 成功」上会让一次网络抖动留下永久徽标。`attempts` 本身不清零（重试预算要用它），
        // 清掉的只是显示。
        TransferStatus::Error => {
            let mut s = progress_line(t);
            if t.attempts > 0 {
                if !s.is_empty() {
                    s.push_str(" · ");
                }
                s.push_str(&t!(
                    "transfer.retried_of",
                    attempt => t.attempts,
                    max => retry_limit
                ));
            }
            s
        }
        // 重试已启动、但一个字节都还没拿到：不显示「0 B / 总量 0%」（「0 B」会被读成「下完了
        // 0 字节」），改为明说现状——用户由此知道琥珀为什么还挂着，也知道它会在第一个字节到达
        // 时消失。
        TransferStatus::Active if is_retrying_without_data(t) => t!(
            "transfer.retrying",
            attempt => t.attempts,
            max => retry_limit
        ),
        // 传输中 / 已完成：正常态不挂重试计数（手动重试会把 `attempts` 归零，故失败再出现时是新数）。
        _ => progress_line(t),
    }
}

/// 「已传 / 总量 · 百分比 · 速率 · 剩余时间」这一段（排队 / 重试 / 完成各态共用）。
///
/// 总量未知且一个字节都还没动（刚启动、首个进度回调之前）时**不显示「0 B」**：
/// 它会被读成「下完了 0 字节」。无总量而有已传字节时（元数据取不到）只显示已传。
fn progress_line(t: &Transfer) -> String {
    let size_part = if t.total > 0 {
        format!("{} / {}", format_size(t.transferred), format_size(t.total))
    } else if t.transferred > 0 {
        format_size(t.transferred)
    } else {
        String::new()
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
    let mut s = String::new();
    for part in [pct, size_part] {
        if part.is_empty() {
            continue;
        }
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(&part);
    }
    // 都空 = 刚启动且总量未知：给一句状态词，避免这一行整个空掉。
    if s.is_empty() && t.status == TransferStatus::Active {
        s.push_str(&t!("transfer.transferring"));
    }
    s.push_str(&speed);
    s.push_str(&eta);
    s
}

/// 矮进度条（高度固定 5px）。
///
/// iced 0.14 的 `ProgressBar` 有 `girth()` 可调粗细，但它同时决定轨道与滑块样式、且与卡片内的
/// 圆角背景不易对齐，故这里以两层容器自绘：外层铺满低对比底（`track`），内层按 `fill` 比例占据
/// 宽度，用 `FillPortion` 配剩余占位实现比例填充。
///
/// `track` 给调用方一个只染「轨道」的通道（等待重试用半透明琥珀），使空条本身也能携带状态。
fn thin_bar(
    fill: f32,
    color: impl Fn(&iced::Theme) -> Color + 'static,
    track: Color,
) -> Element<'static, Message> {
    let pct = (fill.clamp(0.0, 1.0) * 100.0) as u16;
    let bar = container("")
        .width(Length::FillPortion(pct))
        .height(Length::Fixed(5.0))
        .style(move |t| container::Style {
            background: Some(color(t).into()),
            ..Default::default()
        });
    // 空占位：与填充条共同按 `FillPortion` 比例分配轨道宽度。
    let placeholder = |portion: u16| {
        container("")
            .width(Length::FillPortion(portion))
            .height(Length::Fixed(5.0))
    };
    let rail = container(
        if pct == 0 {
            // 排队中：仅渲染空轨道。不可用 `FillPortion(0)` 表示 0 宽——iced 会把它当作
            // 非流式元素并按可用全宽解析，导致空条被误绘为满条。
            row![placeholder(100)]
        } else if pct >= 100 {
            row![bar]
        } else {
            row![bar, placeholder(100 - pct)]
        }
        .spacing(0),
    )
    .width(Length::Fill)
    .height(Length::Fixed(5.0))
    .style(move |_t| container::Style {
        background: Some(track.into()),
        ..Default::default()
    });
    rail.into()
}

/// 进度条轨道的默认底色（低对比中性灰，深浅主题通用）。
const TRACK_BASE: Color = Color::from_rgba(0.5, 0.5, 0.5, 0.25);

/// 文本样式函数指针：详情区需要按状态在「次要灰 / 语义色」之间切换，用别名避免类型噪声。
type TextStyle = fn(&iced::Theme) -> text::Style;

/// 次要文本样式（说明文字、图标等），随主题自动协调。
fn secondary_text(theme: &iced::Theme) -> text::Style {
    text::Style {
        color: Some(theme::custom_palette(theme).text_secondary),
    }
}

/// 失败原因 / 半成品提示的文字样式：固定语义色（红 = 出错，不随主题漂移）。
fn error_text(_theme: &iced::Theme) -> text::Style {
    text::Style {
        color: Some(crate::ui::ERROR),
    }
}

/// 琥珀态（等待重试 / 重试中尚无数据）的文字样式：固定琥珀（警告而非出错，见 [`tone_color`]）。
fn warning_text(_theme: &iced::Theme) -> text::Style {
    text::Style {
        color: Some(crate::ui::WARNING),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ResumeState, Transfer, TransferDirection};
    use rterm_core::Fingerprint;
    use std::path::PathBuf;
    use std::time::Instant;

    /// 造一条传输记录（字段取值与调度器无关，只求能渲染）。
    fn transfer(status: TransferStatus, transferred: u64, total: u64) -> Transfer {
        Transfer {
            id: 1,
            direction: TransferDirection::Download,
            name: "archive.tar.gz".to_string(),
            local: PathBuf::from("/tmp/archive.tar.gz"),
            remote: "/remote/archive.tar.gz".to_string(),
            transferred,
            total,
            status,
            attempts: 0,
            not_before: None,
            partial: None,
            error: None,
            speed: 0.0,
            client: None,
            resume: None,
            keep_staging: false,
        }
    }

    #[test]
    fn progress_fraction_is_empty_when_the_total_is_unknown() {
        // 总量未知时不得画满条（会被读成「已经下完了」）。
        assert_eq!(
            progress_fraction(&transfer(TransferStatus::Active, 0, 0)),
            0.0
        );
        assert_eq!(
            progress_fraction(&transfer(TransferStatus::WaitingRetry, 0, 0)),
            0.0
        );
    }

    #[test]
    fn progress_fraction_is_full_only_when_done() {
        assert_eq!(
            progress_fraction(&transfer(TransferStatus::Done, 0, 0)),
            1.0
        );
        assert_eq!(
            progress_fraction(&transfer(TransferStatus::Done, 100, 100)),
            1.0
        );
        assert_eq!(
            progress_fraction(&transfer(TransferStatus::Active, 250, 1000)),
            0.25
        );
    }

    #[test]
    fn detail_line_never_shows_zero_bytes_for_an_unknown_total() {
        // 刚启动 / 重试刚起来时不能显示「0 B」（会被读成「下完了 0 字节」）。
        let line = detail_line(&transfer(TransferStatus::Active, 0, 0), 2);
        assert!(!line.contains("0 B"), "不得显示 0 B，实际：{line}");
        assert!(!line.starts_with(' '), "不得以空格开头，实际：{line}");
        // 总量未知但有已传字节（元数据取不到）时，至少把已传量显示出来。
        let line = detail_line(&transfer(TransferStatus::Active, 4096, 0), 2);
        assert!(line.contains("4.0 KB"), "应显示已传字节，实际：{line}");
    }

    #[test]
    fn detail_line_shows_the_retry_counter_only_in_the_troubled_states() {
        // 重试次数必须在「等待重试 / 最终失败」两态能看到 n/N
        // （等待重试的首次退避可以很短，只藏在倒计时里等于看不见）。
        let mut waiting = transfer(TransferStatus::WaitingRetry, 0, 1000);
        waiting.attempts = 2;
        waiting.not_before = Some(Instant::now());
        // 只用「n/N」这个语言无关的片段断言，避免测试绑定到具体文案。
        assert!(
            detail_line(&waiting, 5).contains("2/5"),
            "等待重试应显示 2/5：{}",
            detail_line(&waiting, 5)
        );

        let mut failed = transfer(TransferStatus::Error, 500, 1000);
        failed.attempts = 5;
        assert!(
            detail_line(&failed, 5).contains("5/5"),
            "最终失败应显示 5/5：{}",
            detail_line(&failed, 5)
        );
    }

    #[test]
    fn detail_line_drops_the_counter_once_the_transfer_is_healthy_again() {
        // 重试成功、行重新跑起来后，计数必须消失，否则一次网络抖动会在那一行留下永久徽标。
        // 计数仍存在 `Transfer.attempts` 里（重试预算要用它），只是不显示。
        let mut active = transfer(TransferStatus::Active, 500, 1000);
        active.attempts = 1;
        let line = detail_line(&active, 5);
        assert!(!line.contains("1/5"), "重试成功后不该再挂计数：{line}");
        assert!(line.contains("50%"), "进度信息不得被计数挤掉：{line}");

        let mut done = transfer(TransferStatus::Done, 1000, 1000);
        done.attempts = 1;
        let line = detail_line(&done, 5);
        assert!(!line.contains("1/5"), "已完成的行不该挂计数：{line}");

        let mut queued = transfer(TransferStatus::Queued, 0, 1000);
        queued.attempts = 2;
        assert!(
            !detail_line(&queued, 5).contains("2/5"),
            "排队中也只显示「排队中」：{}",
            detail_line(&queued, 5)
        );
    }

    #[test]
    fn detail_line_omits_the_counter_when_never_retried() {
        let line = detail_line(&transfer(TransferStatus::Active, 500, 1000), 2);
        assert!(!line.contains("0/2"), "没重试过就不该出现计数：{line}");
    }

    #[test]
    fn waiting_retry_is_amber_and_failure_is_red() {
        // 等待重试必须拿到 WARNING（琥珀）。
        // 语义色分工刻意固定：琥珀 = 稍后自己再试，红 = 已失败到底，绿 = 完成。
        assert_eq!(
            tone_color(tone_of(&transfer(TransferStatus::WaitingRetry, 0, 1000))),
            Some(crate::ui::WARNING)
        );
        assert_eq!(
            tone_color(tone_of(&transfer(TransferStatus::Error, 0, 1000))),
            Some(crate::ui::ERROR)
        );
        assert_eq!(
            tone_color(tone_of(&transfer(TransferStatus::Done, 0, 1000))),
            Some(crate::ui::SUCCESS)
        );
        // 排队 / 首次传输中无语义色，走强调色（随主题）。
        assert_eq!(
            tone_color(tone_of(&transfer(TransferStatus::Queued, 0, 1000))),
            None
        );
        assert_eq!(
            tone_color(tone_of(&transfer(TransferStatus::Active, 500, 1000))),
            None
        );
        // 空轨也要能携带状态：透明度只改 a，色相不变。
        let tint = with_alpha(crate::ui::WARNING, 0.30);
        assert_eq!(
            (tint.r, tint.g, tint.b),
            (
                crate::ui::WARNING.r,
                crate::ui::WARNING.g,
                crate::ui::WARNING.b
            )
        );
        assert_eq!(tint.a, 0.30);
    }

    #[test]
    fn a_retry_that_has_not_delivered_anything_yet_is_still_amber() {
        // 琥珀不随「等待重试」结束，而是持续到第一个字节到达（见模块文档）。
        let mut retrying = transfer(TransferStatus::Active, 0, 4096);
        retrying.attempts = 1;
        assert!(is_retrying_without_data(&retrying));
        assert_eq!(tone_of(&retrying), Tone::Retry);

        // 第一个字节落地：琥珀立刻结束（同一个 `attempts`，只是 `transferred` 变了）。
        let mut moving = retrying.clone();
        moving.transferred = 65536;
        assert!(!is_retrying_without_data(&moving));
        assert_eq!(tone_of(&moving), Tone::Accent);

        // 首次尝试（没重试过）不适用：刚启动不该报成「重试中」，否则每次下载起手都闪琥珀。
        let fresh = transfer(TransferStatus::Active, 0, 4096);
        assert!(!is_retrying_without_data(&fresh));
        assert_eq!(tone_of(&fresh), Tone::Accent);

        // 排队中即便带重试计数也不是琥珀：那一段没在重试，是真在等并发额度。
        let mut queued = transfer(TransferStatus::Queued, 0, 4096);
        queued.attempts = 2;
        assert_eq!(tone_of(&queued), Tone::Accent);
    }

    #[test]
    fn detail_line_says_it_is_retrying_while_no_data_has_arrived() {
        // 这一态不能沿用进度段（总量已知时会显示「0 B / 4.0 KB 0%」，而「0 B」曾被读成
        // 「下完了 0 字节」），改为明说现状：为什么还琥珀 + 第几次。
        let mut retrying = transfer(TransferStatus::Active, 0, 4096);
        retrying.attempts = 1;
        let line = detail_line(&retrying, 5);
        assert!(line.contains("1/5"), "应给出第几次：{line}");
        assert!(!line.contains("0 B"), "不得显示 0 B：{line}");
        // 与「首次尝试、同样还没数据」的文案必须不同（那里走的是进度段），否则用户分不出
        // 「刚启动」与「正在重试」。断言用同一条文案链算出的两个值比较，故不绑定具体语言。
        let fresh = detail_line(&transfer(TransferStatus::Active, 0, 4096), 5);
        assert_ne!(line, fresh, "重试中尚无数据应与首次尝试的文案不同");

        // 数据到达后回到正常的进度文案（计数消失，见上一条用例）。
        let mut moving = retrying.clone();
        moving.transferred = 2048;
        let line = detail_line(&moving, 5);
        assert!(line.contains("50%"), "数据到了就显示进度：{line}");
        assert!(!line.contains("1/5"), "数据到了计数就该收走：{line}");
    }

    /// 造一份指纹（只有大小有意义时 `modified` 取 `None`）。
    fn fingerprint(len: u64) -> Fingerprint {
        Fingerprint {
            len,
            modified: None,
        }
    }

    #[test]
    fn only_a_row_with_a_real_breakpoint_offers_to_continue() {
        // 失败行的按钮文案由它决定：有断点说「继续下载」，否则说「重试」。
        // 两者发的是**同一条消息**（`RetryTransfer`），差别只在用户能否预期「不用从头再来」。
        let plain = transfer(TransferStatus::Error, 0, 1000);
        assert!(!can_continue(&plain), "没有续传记录 → 只能说「重试」");

        let mut from_zero = transfer(TransferStatus::Error, 0, 1000);
        from_zero.resume = Some(ResumeState {
            source: fingerprint(1000),
            offset: 0,
        });
        assert!(
            !can_continue(&from_zero),
            "上一轮本就是从 0 写的 → 「继续」与「重试」是同一件事，用朴素的那个"
        );

        let mut midway = transfer(TransferStatus::Error, 450, 1000);
        midway.resume = Some(ResumeState {
            source: fingerprint(1000),
            offset: 450,
        });
        assert!(can_continue(&midway), "断点在 45% → 按钮说「继续下载」");
    }

    #[test]
    fn a_resumed_retry_is_not_amber_because_data_is_already_there() {
        // 续传的重试天然不显琥珀：`AttemptStarted` 已把 `transferred` 置为断点（> 0），
        // 该行是「已经有数据」的正常样子——琥珀只属于「一个字节都还没拿到」的等待。
        let mut resumed = transfer(TransferStatus::Active, 450, 1000);
        resumed.attempts = 1;
        resumed.resume = Some(ResumeState {
            source: fingerprint(1000),
            offset: 450,
        });
        assert!(!is_retrying_without_data(&resumed));
        assert_eq!(tone_of(&resumed), Tone::Accent);

        // 对照：同样是重试，但从 0 起（不可续传）时仍是琥珀。
        let mut from_zero = transfer(TransferStatus::Active, 0, 1000);
        from_zero.attempts = 1;
        assert!(is_retrying_without_data(&from_zero));
        assert_eq!(tone_of(&from_zero), Tone::Retry);
    }
}
