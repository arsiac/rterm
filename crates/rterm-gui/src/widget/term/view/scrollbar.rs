//! 终端右侧叠加式滚动条的几何计算（纯函数）：轨道 / 滑块矩形与拖拽映射。

use crate::widget::term::backend::RenderableContent;
use alacritty_terminal::term::TermMode;
use iced::{Point, Rectangle, Size};

/// 滚动条滑块宽度（像素）：叠加在终端内容最右侧一列之上。
pub(super) const SCROLLBAR_WIDTH: f32 = 6.0;
/// 滚动条轨道相对内容上下边缘的留白（像素）。
pub(super) const SCROLLBAR_INSET: f32 = 2.0;
/// 滚动条滑块最小高度（像素）：历史极长时仍可抓取。
pub(super) const SCROLLBAR_MIN_THUMB: f32 = 24.0;
/// 滚动条滑块圆角半径（像素）。
pub(super) const SCROLLBAR_RADIUS: f32 = 3.0;
/// 滚动条滑块透明度：常态 / 悬停 / 拖动。
pub(super) const SCROLLBAR_ALPHA: f32 = 0.28;
pub(super) const SCROLLBAR_ALPHA_HOVER: f32 = 0.45;
pub(super) const SCROLLBAR_ALPHA_DRAG: f32 = 0.60;

/// 滚动条几何（纯函数）：返回 `(轨道矩形, 滑块矩形)`，无历史 / 视口为空 / 备用屏下返回 `None`。
///
/// 备用屏判据只取 [`TermMode::ALT_SCREEN`]：[`TermMode::ALTERNATE_SCROLL`] 是默认就置位的修饰位。
pub(super) fn scrollbar_geometry(
    bounds: Rectangle,
    padding: f32,
    content: &RenderableContent,
) -> Option<(Rectangle, Rectangle)> {
    let rows = content.cells.len() / content.columns.max(1);
    let history = content.history_size;
    if history == 0 || rows == 0 || content.terminal_mode.contains(TermMode::ALT_SCREEN) {
        return None;
    }

    let track = Rectangle::new(
        Point::new(
            bounds.x + bounds.width - padding - SCROLLBAR_WIDTH,
            bounds.y + padding + SCROLLBAR_INSET,
        ),
        Size::new(
            SCROLLBAR_WIDTH,
            bounds.height - (padding + SCROLLBAR_INSET) * 2.0,
        ),
    );
    if track.height <= 0.0 || track.width <= 0.0 {
        return None;
    }

    let total = (history + rows) as f32;
    let thumb_height = (track.height * rows as f32 / total)
        .max(SCROLLBAR_MIN_THUMB)
        .min(track.height);
    // 偏移 0 表示贴住实时输出，对应滑块在轨道最下。
    let progress = (history - content.display_offset) as f32 / history as f32;
    let thumb = Rectangle::new(
        Point::new(track.x, track.y + progress * (track.height - thumb_height)),
        Size::new(track.width, thumb_height),
    );

    Some((track, thumb))
}

/// 由拖拽光标位置反算目标回滚偏移（纯函数）；`grab` 为抓取点相对滑块顶部的偏移。
pub(super) fn scrollbar_drag_target(
    track: Rectangle,
    thumb_height: f32,
    history: usize,
    cursor_y: f32,
    grab: f32,
) -> usize {
    let span = track.height - thumb_height;
    if span <= 0.0 {
        return 0;
    }
    let progress = ((cursor_y - track.y - grab) / span).clamp(0.0, 1.0);
    (((1.0 - progress) * history as f32).round() as usize).min(history)
}

/// 由拖拽目标与上一次已下达的偏移算出相对增量（纯函数），返回 `(增量, 新的基准)`。
///
/// 基准取自上次已下达值而非后端 `display_offset`，同批多次移动的增量依次相消、合计恰为目标。
pub(super) fn scrollbar_drag_step(target: usize, tracked: usize) -> (i32, usize) {
    (target as i32 - tracked as i32, target)
}

/// 该事件是否应解除滚动条拖拽锁存（纯函数）：左键释放一律解除，未拖拽时不解除。
///
/// 不看光标是否可得——指针在窗外释放时 iced 报告光标不可得，走不到常规释放分支而残留幻影拖拽。
pub(super) fn scrollbar_releases_drag(event: &iced::mouse::Event, dragging: bool) -> bool {
    dragging
        && matches!(
            event,
            iced_core::mouse::Event::ButtonReleased(iced_core::mouse::Button::Left)
        )
}
