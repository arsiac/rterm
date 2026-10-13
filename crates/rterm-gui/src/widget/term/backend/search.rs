//! 终端内搜索：锁内搜索状态机与纯函数判定。
//!
//! 查询语义为「字面子串 + 忽略大小写」的默认档，正则与大小写开关经 [`SearchFlags`]
//! 注入 [`build_pattern`]。命中列表不缓存：导航按需 `search_next` 实时求解，
//! 视口高亮在每次同步时对当前视口重算，保证流式输出下无陈旧状态。

use super::types::{SearchFlags, SearchOutcome};
use super::{Backend, EventProxy};
use crate::widget::term::actions::Action;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};

/// 命中计数封顶（超出显示「999+」）：防大 scrollback 下逐键全网格扫描失控。
pub(super) const SEARCH_COUNT_CAP: usize = 999;

/// 需要转义的 `regex` 语法元字符（其余字符按字面处理）。
const REGEX_METACHARS: &[char] = &[
    '\\', '.', '+', '*', '?', '(', ')', '|', '[', ']', '{', '}', '^', '$',
];

/// 锁内搜索态：编译好的正则与当前命中（绝对网格坐标）。
#[derive(Debug)]
pub(super) struct SearchState {
    /// 当前查询编译出的双向 DFA。
    pub(super) regex: RegexSearch,
    /// 当前命中范围；`None` 表示尚无跳转目标（无命中或刚清除）。
    pub(super) active: Option<Match>,
}

/// 把查询串组装为匹配模式（纯函数）。
///
/// 字面模式逐字符转义元字符；正则模式原样采用。大小写开关统一用显式内联标志表达
/// （`(?i)` / `(?-i)`），覆盖 `RegexSearch` 自带的 smart-case（按查询是否含大写选档），
/// 使行为只取决于开关而非查询内容。
pub(super) fn build_pattern(query: &str, flags: &SearchFlags) -> String {
    let body = if flags.regex {
        query.to_string()
    } else {
        escape_literal(query)
    };
    let case = if flags.case_sensitive {
        "(?-i)"
    } else {
        "(?i)"
    };
    format!("{case}{body}")
}

/// 转义正则元字符（纯函数）。
fn escape_literal(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for c in query.chars() {
        if REGEX_METACHARS.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// 命中行入视口的 `display_offset` 增量（纯函数）：正值向上回滚历史、负值向下贴回
/// 实时输出（同 `Scroll::Delta` 语义）。命中已在视口内返回 0，不扰动用户滚动位置；
/// 在上方贴视口顶边、在下方贴视口底边，均为最小位移。
pub(super) fn scroll_delta_for_match(
    line: i32,
    display_offset: usize,
    rows: usize,
    history: usize,
) -> i32 {
    let offset = display_offset.clamp(0, history) as i32;
    let top = -offset;
    let bottom = rows.saturating_sub(1) as i32 - offset;
    if line < top {
        -line - offset
    } else if line > bottom {
        rows.saturating_sub(1) as i32 - line - offset
    } else {
        0
    }
}

/// 当前视口的顶行（绝对网格行号，负数 = 历史）。
fn viewport_top_line(terminal: &Term<EventProxy>) -> Line {
    Line(-(terminal.grid().display_offset() as i32))
}

impl Backend {
    /// 见 [`run_set_query`]：在锁内终端上执行一次查询更新。
    pub(super) fn search_set_query(
        &mut self,
        terminal: &mut Term<EventProxy>,
        query: &str,
        flags: SearchFlags,
    ) -> Action {
        run_set_query(terminal, &mut self.search, query, flags)
    }

    /// 见 [`run_step`]：在锁内终端上跳转到下一个 / 上一个命中。
    pub(super) fn search_step(&mut self, terminal: &mut Term<EventProxy>, direction: Direction) {
        run_step(terminal, &mut self.search, direction);
    }

    /// 关闭搜索：清空搜索态，下一次同步的高亮快照随之为空。
    pub(super) fn search_close(&mut self) {
        self.search = None;
    }
}

/// 更新查询：编译正则、全网格计数（封顶），有命中则跳到首个命中并滚动到位。
///
/// 空查询直接清除搜索态；编译失败（仅正则模式可达）返回 `Invalid` 并保留旧状态，
/// 高亮不动、仅文案提示。
pub(super) fn run_set_query(
    terminal: &mut Term<EventProxy>,
    current: &mut Option<SearchState>,
    query: &str,
    flags: SearchFlags,
) -> Action {
    if query.is_empty() {
        *current = None;
        return Action::default();
    }
    let pattern = build_pattern(query, &flags);
    let Ok(mut regex) = RegexSearch::new(&pattern) else {
        return Action::SearchStats(SearchOutcome::Invalid);
    };

    // 全网格计数：从最老历史行扫到视口底，命中达封顶即停。
    let top = Line(-(terminal.grid().history_size() as i32));
    let bottom = viewport_top_line(terminal) + terminal.bottommost_line();
    let start = Point::new(top, Column(0));
    let end = Point::new(bottom, terminal.last_column());
    let found = RegexIter::new(start, end, Direction::Right, terminal, &mut regex)
        .take(SEARCH_COUNT_CAP + 1)
        .count();
    let capped = found > SEARCH_COUNT_CAP;
    let count = found.min(SEARCH_COUNT_CAP);

    let active = if count > 0 {
        let origin = Point::new(viewport_top_line(terminal), Column(0));
        terminal.search_next(&mut regex, origin, Direction::Right, Side::Left, None)
    } else {
        None
    };
    if let Some(m) = &active {
        scroll_to_match(terminal, m);
    }
    *current = Some(SearchState { regex, active });
    let outcome = if count == 0 {
        SearchOutcome::NoMatches
    } else {
        SearchOutcome::Matches { count, capped }
    };
    Action::SearchStats(outcome)
}

/// 取下一个 / 上一个命中并滚动到位。
///
/// origin 在当前命中之外再让开一格（`Boundary::Grid` 在网格尽头夹紧），避免
/// `search_next` 的「起点即命中」判定原地返回同一个匹配；整网格回绕由
/// `search_next`（`max_lines = None`）内置完成，无命中时保持原状。
pub(super) fn run_step(
    terminal: &mut Term<EventProxy>,
    current: &mut Option<SearchState>,
    direction: Direction,
) {
    let Some(state) = current.as_mut() else {
        return;
    };
    let origin = match &state.active {
        Some(m) => match direction {
            Direction::Right => m.end().add(terminal, Boundary::Grid, 1),
            Direction::Left => m.start().sub(terminal, Boundary::Grid, 1),
        },
        None => Point::new(viewport_top_line(terminal), Column(0)),
    };
    let side = match direction {
        Direction::Right => Side::Left,
        Direction::Left => Side::Right,
    };
    let next = terminal.search_next(&mut state.regex, origin, direction, side, None);
    if let Some(m) = &next {
        scroll_to_match(terminal, m);
        state.active = next;
    }
}

/// 把命中行以最小位移滚入视口。
fn scroll_to_match(terminal: &mut Term<EventProxy>, m: &Match) {
    let delta = scroll_delta_for_match(
        m.start().line.0,
        terminal.grid().display_offset(),
        terminal.screen_lines(),
        terminal.grid().history_size(),
    );
    if delta != 0 {
        terminal.scroll_display(Scroll::Delta(delta));
    }
}
