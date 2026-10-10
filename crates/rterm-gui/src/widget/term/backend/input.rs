//! 终端后端的输入侧处理：选区、鼠标报告、超链接动作与尺寸重排。

use super::types::{LinkAction, MouseButton, MouseMode, TerminalSize};
use super::{Backend, EventProxy};
use alacritty_terminal::event::{Notify, OnResize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::viewport_to_point;
use alacritty_terminal::term::{Term, TermMode};
use iced::keyboard::Modifiers;
use iced_core::Size;
use log::warn;
use std::borrow::Cow;
use std::cmp::min;

impl Backend {
    /// 处理超链接动作：悬停时计算匹配范围、清除或打开链接。
    pub(super) fn process_link_action(
        &mut self,
        terminal: &Term<EventProxy>,
        link_action: LinkAction,
        point: Point,
    ) {
        match link_action {
            LinkAction::Hover => {
                self.last_content.hovered_hyperlink =
                    self.regex_match_at(terminal, point, &mut self.url_regex.clone());
            }
            LinkAction::Clear => {
                self.last_content.hovered_hyperlink = None;
            }
            LinkAction::Open => {
                self.open_link(terminal);
            }
        };
    }

    /// 用系统默认程序打开当前悬停的超链接。
    ///
    /// 直接在已持锁的 `terminal` 上取字符：链接范围可能落在视口外，快照不保证覆盖。
    pub(super) fn open_link(&self, terminal: &Term<EventProxy>) {
        if let Some(range) = &self.last_content.hovered_hyperlink {
            let start = range.start();
            let end = range.end();
            let grid = terminal.grid();

            let mut url = String::from(grid[*start].c);
            for indexed in grid.iter_from(*start) {
                url.push(indexed.c);
                if indexed.point == *end {
                    break;
                }
            }

            if let Err(e) = open::that(&url) {
                warn!("Failed to open hyperlink: {e}");
            }
        }
    }

    /// 依据当前鼠标模式，向终端回送 SGR 或普通鼠标报告。
    pub(super) fn process_mouse_report(
        &self,
        button: MouseButton,
        modifiers: Modifiers,
        point: Point,
        pressed: bool,
    ) {
        let mut mods = 0;
        if modifiers.contains(Modifiers::SHIFT) {
            mods += 4;
        }
        if modifiers.contains(Modifiers::ALT) {
            mods += 8;
        }
        if modifiers.contains(Modifiers::COMMAND) {
            mods += 16;
        }

        match MouseMode::from(self.last_content.terminal_mode) {
            MouseMode::Sgr => self.sgr_mouse_report(point, button as u8 + mods, pressed),
            MouseMode::Normal(is_utf8) => {
                if pressed {
                    self.normal_mouse_report(point, button as u8 + mods, is_utf8)
                } else {
                    self.normal_mouse_report(point, 3 + mods, is_utf8)
                }
            }
        }
    }

    /// 生成 SGR（1006）鼠标报告字节并写入 PTY。
    pub(super) fn sgr_mouse_report(&self, point: Point, button: u8, pressed: bool) {
        let c = if pressed { 'M' } else { 'm' };

        let msg = format!(
            "\x1b[<{};{};{}{}",
            button,
            point.column + 1,
            point.line + 1,
            c
        );

        self.notifier.notify(msg.as_bytes().to_vec());
    }

    /// 生成普通（含可选 UTF-8 编码）鼠标报告字节并写入 PTY。
    pub(super) fn normal_mouse_report(&self, point: Point, button: u8, is_utf8: bool) {
        let Point { line, column } = point;
        let max_point = if is_utf8 { 2015 } else { 223 };

        if line >= max_point || column >= max_point {
            return;
        }

        let mut msg = vec![b'\x1b', b'[', b'M', 32 + button];

        let mouse_pos_encode = |pos: usize| -> Vec<u8> {
            let pos = 32 + 1 + pos;
            let first = 0xC0 + pos / 64;
            let second = 0x80 + (pos & 63);
            vec![first as u8, second as u8]
        };

        if is_utf8 && column >= Column(95) {
            msg.append(&mut mouse_pos_encode(column.0));
        } else {
            msg.push(32 + 1 + column.0 as u8);
        }

        if is_utf8 && line >= 95 {
            msg.append(&mut mouse_pos_encode(line.0 as usize));
        } else {
            msg.push(32 + 1 + line.0 as u8);
        }

        self.notifier.notify(msg);
    }

    /// 在给定像素坐标处开始一次选区。
    pub(super) fn start_selection(
        &mut self,
        terminal: &mut Term<EventProxy>,
        selection_type: SelectionType,
        x: f32,
        y: f32,
    ) {
        let location = Self::selection_point(x, y, &self.size, terminal.grid().display_offset());
        terminal.selection = Some(Selection::new(
            selection_type,
            location,
            self.selection_side(x),
        ));
    }

    /// 以给定像素坐标更新当前选区的终点。
    pub(super) fn update_selection(&mut self, terminal: &mut Term<EventProxy>, x: f32, y: f32) {
        let display_offset = terminal.grid().display_offset();
        if let Some(ref mut selection) = terminal.selection {
            let location = Self::selection_point(x, y, &self.size, display_offset);
            selection.update(location, self.selection_side(x));
        }
    }

    /// 将像素坐标解析为选区锚点（按字符格宽高折算列行，供 `update_selection` 使用）。
    pub fn selection_point(
        x: f32,
        y: f32,
        terminal_size: &TerminalSize,
        display_offset: usize,
    ) -> Point {
        let col = (x as usize) / (terminal_size.cell_width as usize);
        let col = min(Column(col), Column(terminal_size.num_cols as usize - 1));

        let line = (y as usize) / (terminal_size.cell_height as usize);
        let line = min(line, terminal_size.num_lines as usize - 1);

        viewport_to_point(display_offset, Point::new(line, col))
    }

    /// 依据像素横坐标落在单元格的左半或右半，返回选区的命中侧。
    pub(super) fn selection_side(&self, x: f32) -> Side {
        let cell_x = x as usize % self.size.cell_width as usize;
        let half_cell_width = (self.size.cell_width as f32 / 2.0) as usize;

        if cell_x > half_cell_width {
            Side::Right
        } else {
            Side::Left
        }
    }

    /// 依据布局尺寸与字体度量重算行列数，并通知 alacritty 重排。
    pub(super) fn resize(
        &mut self,
        terminal: &mut Term<EventProxy>,
        layout_size: Option<Size<f32>>,
        font_measure: Option<Size<f32>>,
    ) {
        if let Some(size) = layout_size {
            self.size.layout_height = size.height;
            self.size.layout_width = size.width;
        };

        if let Some(size) = font_measure {
            self.size.cell_height = size.height as u16;
            self.size.cell_width = size.width as u16;
        }

        let lines = (self.size.layout_height / self.size.cell_height as f32).floor() as u16;
        let cols = (self.size.layout_width / self.size.cell_width as f32).floor() as u16;
        if lines > 0 && cols > 0 {
            self.size.num_lines = lines;
            self.size.num_cols = cols;
            // 宿主在每条命令后都会推送一次字体测量结果，同一尺寸会被反复下发；
            // 而每次下发都对应一个 SSH window-change 请求，故仅在尺寸真正变化时通知远端。
            if self.last_pty_size != Some((cols, lines)) {
                self.last_pty_size = Some((cols, lines));
                self.notifier.on_resize(self.size.into());
            }
            terminal.resize(TermSize::new(
                self.size.num_cols as usize,
                self.size.num_lines as usize,
            ));
        }
    }

    /// 将输入字节经 notifier 写入 PTY。
    pub(super) fn write<I: Into<Cow<'static, [u8]>>>(&self, input: I) {
        self.notifier.notify(input.into());
    }

    /// 按行滚动视口；若终端处于 alt 屏或 alt-screen 滚动模式则改发方向键序列。
    pub(super) fn scroll(&mut self, terminal: &mut Term<EventProxy>, delta_value: i32) {
        if delta_value != 0 {
            let scroll = Scroll::Delta(delta_value);
            if terminal
                .mode()
                .contains(TermMode::ALTERNATE_SCROLL | TermMode::ALT_SCREEN)
            {
                let line_cmd = if delta_value > 0 { b'A' } else { b'B' };
                let mut content = vec![];

                for _ in 0..delta_value.abs() {
                    content.push(0x1b);
                    content.push(b'O');
                    content.push(line_cmd);
                }

                self.notifier.notify(content);
            } else {
                terminal.grid_mut().scroll_display(scroll);
            }
        }
    }

    /// 返回当前选中范围内的纯文本（宽字符占位格不产出字符）。
    /// 若 `trim_trailing_whitespace` 开启，各行的尾部空格与制表符将被去除。
    pub fn selectable_content(&self) -> String {
        let term = self.term.clone();
        let term = term.lock();
        Self::selection_text(&term, self.trim_trailing_whitespace)
    }

    /// 由选区逐格提取文本（纯函数，供 [`Self::selectable_content`] 与测试使用）。
    ///
    /// 逐格取自网格本身而非视口快照：选区可能跨越视口之外的历史行。
    pub(super) fn selection_text(
        term: &Term<EventProxy>,
        trim_trailing_whitespace: bool,
    ) -> String {
        let grid = term.grid();
        let Some(mut range) = term.selection.as_ref().and_then(|s| s.to_range(term)) else {
            return String::new();
        };
        // 选区起点落在宽字符右半的占位格上时左移一格，把整字纳入（对齐上游 `line_to_string`）。
        if range.start.column > Column(0)
            && grid[range.start].flags.contains(Flags::WIDE_CHAR_SPACER)
        {
            range.start.column -= 1;
        }

        let mut result = String::new();
        let mut last_line: i32 = i32::MIN;
        for line in range.start.line.0..=range.end.line.0 {
            for column in 0..grid.columns() {
                let point = Point::new(Line(line), Column(column));
                if !range.contains(point) {
                    continue;
                }
                let cell = &grid[point];
                // 宽字符占位格不产出字符，否则「会话」会被复制成「会 话」。
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                // 换行时插入 \n。
                if line != last_line {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    last_line = line;
                }
                result.push(cell.c);
                // 组合字符（如 emoji ZWJ 序列）随主字符一并复制。
                if let Some(zerowidth) = cell.zerowidth() {
                    result.extend(zerowidth);
                }
            }
        }

        if trim_trailing_whitespace {
            result
                .lines()
                .map(|line| line.trim_end())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            result
        }
    }
}
