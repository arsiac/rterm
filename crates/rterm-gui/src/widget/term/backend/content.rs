//! 终端可见区快照 [`RenderableContent`] 及其捕获、同步与超链接匹配。

use super::{Backend, EventProxy};
use crate::widget::term::backend::types::TerminalSize;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Direction, Line, Point};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::CursorShape;
use std::ops::RangeInclusive;

impl Backend {
    /// 捕获终端可见区到 `content`：只遍历视口行，并复用 `content.cells` 已有分配。
    ///
    /// 视口大小为「可见行数 × 列数」，与 scrollback 长度无关，故每条事件后调用也不随历史增长。
    pub(super) fn capture_viewport(
        content: &mut RenderableContent,
        terminal: &mut Term<EventProxy>,
        size: TerminalSize,
    ) {
        let cursor = terminal.grid_mut().cursor_cell().clone();
        let selectable_range = match &terminal.selection {
            Some(s) => s.to_range(terminal),
            None => None,
        };
        let cursor_shape = terminal.cursor_style().shape;
        let terminal_mode = *terminal.mode();
        let grid = terminal.grid();

        content.cells.clear();
        content
            .cells
            .extend(grid.display_iter().map(|indexed| indexed.cell.clone()));
        content.columns = grid.columns();
        content.display_offset = grid.display_offset();
        content.history_size = grid.history_size();
        content.cursor_point = grid.cursor.point;
        content.cursor = cursor;
        content.cursor_shape = cursor_shape;
        content.selectable_range = selectable_range;
        content.terminal_mode = terminal_mode;
        content.terminal_size = size;
    }

    /// 将 alacritty 终端最新状态同步到可渲染内容快照。
    pub fn sync(&mut self) {
        let term = self.term.clone();
        let mut term = term.lock();
        self.internal_sync(&mut term);
    }
    /// 内部同步：刷新视口单元格、选区、光标与终端模式快照。
    pub(super) fn internal_sync(&mut self, terminal: &mut Term<EventProxy>) {
        Self::capture_viewport(&mut self.last_content, terminal, self.size);
    }
    /// 取自 alacritty/src/display/hint.rs 的 regex_match_at 实现
    /// 若指定坐标落在正则匹配的文本范围内，则取回该匹配。
    pub(super) fn regex_match_at(
        &self,
        terminal: &Term<EventProxy>,
        point: Point,
        regex: &mut RegexSearch,
    ) -> Option<Match> {
        visible_regex_match_iter(terminal, regex).find(|rm| rm.contains(&point))
    }
}

/// 复制自 alacritty/src/display/hint.rs：遍历所有可见的正则匹配。
fn visible_regex_match_iter<'a>(
    term: &'a Term<EventProxy>,
    regex: &'a mut RegexSearch,
) -> impl Iterator<Item = Match> + 'a {
    let viewport_start = Line(-(term.grid().display_offset() as i32));
    let viewport_end = viewport_start + term.bottommost_line();
    let mut start = term.line_search_left(Point::new(viewport_start, Column(0)));
    let mut end = term.line_search_right(Point::new(viewport_end, Column(0)));
    start.line = start.line.max(viewport_start - 100);
    end.line = end.line.min(viewport_end + 100);

    RegexIter::new(start, end, Direction::Right, term, regex)
        .skip_while(move |rm| rm.end().line < viewport_start)
        .take_while(move |rm| rm.start().line <= viewport_end)
}
/// 一次渲染所需的终端可见区快照；只含视口（不含 scrollback），`cells` 按行主序排列，
/// `i` 号格子对应网格点 `(Line(i / columns - display_offset), Column(i % columns))`。
pub struct RenderableContent {
    /// 视口内单元格，按行主序。
    pub cells: Vec<Cell>,
    /// 视口列数，用于把 `cells` 下标还原为行列。
    pub columns: usize,
    /// 视口滚动偏移（0 表示贴住实时输出）。
    pub display_offset: usize,
    /// 视口之上可回滚的历史总行数（0 表示无历史）。
    pub history_size: usize,
    /// 当前光标所在的网格坐标。
    pub cursor_point: Point,
    /// 当前悬停命中、可点击的超链接坐标范围。
    pub hovered_hyperlink: Option<RangeInclusive<Point>>,
    /// 当前选区对应的可复制范围（无选区则为 `None`）。
    pub selectable_range: Option<SelectionRange>,
    /// 当前光标所在单元格。
    pub cursor: Cell,
    /// 当前光标形状（由 DECSCUSR 序列设定）。
    pub cursor_shape: CursorShape,
    /// 当前 alacritty 终端模式。
    pub terminal_mode: TermMode,
    /// 当前终端几何尺寸。
    pub terminal_size: TerminalSize,
}

impl RenderableContent {
    /// 视口行数
    pub fn rows(&self) -> usize {
        self.terminal_size.num_lines as usize
    }
}

impl Default for RenderableContent {
    /// 返回空视口的 RenderableContent 默认值。
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            columns: 1,
            display_offset: 0,
            history_size: 0,
            cursor_point: Point::default(),
            hovered_hyperlink: None,
            selectable_range: None,
            cursor: Cell::default(),
            cursor_shape: CursorShape::default(),
            terminal_mode: TermMode::empty(),
            terminal_size: TerminalSize::default(),
        }
    }
}
