use crate::widget::term::backend::RenderableContent;
use alacritty_terminal::index::{Column, Line, Point as GridPoint};
use alacritty_terminal::term::cell;

pub(super) const TEST_PADDING: f32 = 4.0;

/// 构造光标位于指定网格点的渲染内容；默认单元格为 1×1 像素，故断言可直接按格数书写。
pub(super) fn content_at(line: i32, column: usize) -> RenderableContent {
    RenderableContent {
        cursor_point: GridPoint {
            line: Line(line),
            column: Column(column),
        },
        ..Default::default()
    }
}

/// 测试用快照：`rows` 行视口、`columns` 列、`history` 行历史、`display_offset` 回滚偏移。
pub(super) fn content_with(
    rows: usize,
    columns: usize,
    history: usize,
    display_offset: usize,
) -> RenderableContent {
    RenderableContent {
        cells: vec![cell::Cell::default(); rows * columns],
        columns,
        display_offset,
        history_size: history,
        ..Default::default()
    }
}
