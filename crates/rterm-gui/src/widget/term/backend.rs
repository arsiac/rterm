use crate::widget::term::actions::Action;
use crate::widget::term::russh_pty::RusshPty;
use crate::widget::term::settings::BackendSettings;
use alacritty_terminal::event::{Event, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{
    self, Term, TermMode, cell::Cell, test::TermSize, viewport_to_point,
};
use alacritty_terminal::tty::EventedPty;
use alacritty_terminal::{Grid, tty};
use iced::keyboard::Modifiers;
use iced_core::Size;
use std::borrow::Cow;
use std::cmp::min;
use std::io::Result;
use std::ops::{Index, RangeInclusive};
use std::sync::Arc;
use tokio::sync::mpsc;

/// 匹配可点击超链接的正则（覆盖 ipfs、magnet、http(s)、ssh 等协议）。
const URL_REGEX: &str = r#"(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file://|git://|ssh:|ftp://)[^\u{0000}-\u{001F}\u{007F}-\u{009F}<>"\s{-}\^⟨⟩`]+"#;

#[derive(Debug, Clone)]
/// 宿主向终端后端下发的命令。
pub enum Command {
    /// 向终端写入原始字节流（键盘输入等）。
    Write(Vec<u8>),
    /// 按行滚动视口：**正值向上**（回滚历史）、**负值向下**（回到实时输出）。
    ///
    /// 方向沿用 alacritty `Scroll::Delta` 的语义：`display_offset + n`，而 `display_offset`
    /// 为 0 表示贴住实时输出、越大越靠近历史。像素到行的折算由 `view` 完成，本枚举只认行数。
    Scroll(i32),
    /// 重设布局尺寸与字体测量结果（用于重算行列数）。
    Resize(Option<Size<f32>>, Option<Size<f32>>),
    /// 在给定像素坐标起点开始一次选区（指定选区类型）。
    SelectStart(SelectionType, (f32, f32)),
    /// 以给定像素坐标更新选区终点。
    SelectUpdate((f32, f32)),
    /// 在指定网格点执行超链接动作（悬停 / 清除 / 打开）。
    ProcessLink(LinkAction, Point),
    /// 向终端回送鼠标报告（按键、修饰键、坐标、是否按下）。
    MouseReport(MouseButton, Modifiers, Point, bool),
    /// 处理底层 alacritty 事件（标题变更、退出、PTY 写入等）。
    ProcessAlacrittyEvent(Event),
}

/// 鼠标协议模式，由 alacritty `TermMode` 推导而来。
#[derive(Debug, Clone)]
pub enum MouseMode {
    /// SGR（1006）扩展鼠标模式。
    Sgr,
    /// 普通鼠标模式，`bool` 表示是否使用 UTF-8 坐标编码（1005 模式）。
    Normal(bool),
}

impl From<TermMode> for MouseMode {
    /// 依据 alacritty `TermMode` 判断应使用的鼠标协议模式。
    fn from(term_mode: TermMode) -> Self {
        if term_mode.contains(TermMode::SGR_MOUSE) {
            MouseMode::Sgr
        } else if term_mode.contains(TermMode::UTF8_MOUSE) {
            MouseMode::Normal(true)
        } else {
            MouseMode::Normal(false)
        }
    }
}

#[derive(Debug, Clone)]
/// 鼠标报告使用的按键编码（对应 X10 / SGR 鼠标协议的 button 字段）。
pub enum MouseButton {
    /// 左键（编码 0）。
    LeftButton = 0,
    /// 中键（编码 1）。
    MiddleButton = 1,
    /// 右键（编码 2）。
    RightButton = 2,
    /// 左键拖动移动（编码 32）。
    LeftMove = 32,
    /// 中键拖动移动（编码 33）。
    MiddleMove = 33,
    /// 右键拖动移动（编码 34）。
    RightMove = 34,
    /// 无按键移动（编码 35）。
    NoneMove = 35,
    /// 向上滚动（编码 64）。
    ScrollUp = 64,
    /// 向下滚动（编码 65）。
    ScrollDown = 65,
    /// 其它按键（编码 99）。
    Other = 99,
}

#[derive(Debug, Clone)]
/// 超链接（OSC 8）相关动作。
pub enum LinkAction {
    /// 清除当前悬停的超链接。
    Clear,
    /// 标记某点处于超链接悬停态（用于高亮与点击判定）。
    Hover,
    /// 打开悬停的超链接。
    Open,
}

/// 终端的几何尺寸与字体度量，作为 alacritty `Dimensions` 的实现。
#[derive(Clone, Copy, Debug)]
pub struct TerminalSize {
    /// 单个单元格的像素宽度。
    pub cell_width: u16,
    /// 单个单元格的像素高度。
    pub cell_height: u16,
    /// 当前可见列数（由布局尺寸除以单元格宽度折算）。
    num_cols: u16,
    /// 当前可见行数（由布局尺寸除以单元格高度折算）。
    num_lines: u16,
    /// 布局区域的总宽度（像素）。
    layout_width: f32,
    /// 布局区域的总高度（像素）。
    layout_height: f32,
}

impl Default for TerminalSize {
    /// 返回 TerminalSize 的默认值（80 列、50 行、单元格 1×1 像素）。
    fn default() -> Self {
        Self {
            cell_width: 1,
            cell_height: 1,
            num_cols: 80,
            num_lines: 50,
            layout_width: 80.0,
            layout_height: 50.0,
        }
    }
}

impl Dimensions for TerminalSize {
    /// 返回视口总行数（此处等于可见行数）。
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    /// 返回可见列数。
    fn columns(&self) -> usize {
        self.num_cols as usize
    }

    /// 返回最后一列的索引。
    fn last_column(&self) -> Column {
        Column(self.num_cols as usize - 1)
    }

    /// 返回最底行的索引。
    fn bottommost_line(&self) -> Line {
        Line(self.num_lines as i32 - 1)
    }

    /// 返回可见行数。
    fn screen_lines(&self) -> usize {
        self.num_lines as usize
    }
}

impl From<TerminalSize> for WindowSize {
    /// 将 TerminalSize 转换为 alacritty `WindowSize`。
    fn from(size: TerminalSize) -> Self {
        Self {
            num_lines: size.num_lines,
            num_cols: size.num_cols,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        }
    }
}

/// 终端后端：桥接 russh shell 通道（或本地 PTY）与 alacritty event loop。
pub struct Backend {
    /// alacritty 终端状态（含网格与光标），由互斥锁保护供多线程访问。
    term: Arc<FairMutex<Term<EventProxy>>>,
    /// 当前终端几何尺寸与字体度量。
    size: TerminalSize,
    /// 向 alacritty event loop 推送消息的通知器。
    notifier: Notifier,
    /// 上一次同步后的可渲染内容快照。
    last_content: RenderableContent,
    /// 用于识别超链接的 URL 正则（crate 内可见）。
    pub(crate) url_regex: RegexSearch,
}

impl Backend {
    /// 走本地 PTY 子进程路径（拉起本机 shell，非 SSH 场景）。
    ///
    /// 本项目实际只走 [`Self::new_with_pty`]（远端 russh 通道）。本函数继承自上游 iced_term
    /// 的本地终端能力，当前唯一调用者是 `Terminal::new`，而后者本身也无调用方——即这条路径
    /// 目前是死的，保留以支撑将来的本地 shell 标签页。
    pub fn new(
        id: u64,
        pty_event_proxy_sender: mpsc::Sender<Event>,
        settings: BackendSettings,
    ) -> Result<Self> {
        let pty_config = tty::Options {
            shell: Some(tty::Shell::new(settings.program, settings.args)),
            working_directory: settings.working_directory,
            env: settings.env,
            ..tty::Options::default()
        };

        let pty = tty::new(&pty_config, TerminalSize::default().into(), id)?;
        Self::from_pty(id, pty_event_proxy_sender, pty)
    }

    /// SSH 场景：直接桥接 russh shell 通道，不经过本地 PTY 子进程。
    pub fn new_with_pty(
        id: u64,
        pty_event_proxy_sender: mpsc::Sender<Event>,
        pty: RusshPty,
    ) -> Result<Self> {
        Self::from_pty(id, pty_event_proxy_sender, pty)
    }

    /// 以给定 PTY 构造后端，初始化 alacritty 终端与 event loop。
    fn from_pty<Pty>(
        _id: u64,
        pty_event_proxy_sender: mpsc::Sender<Event>,
        pty: Pty,
    ) -> Result<Self>
    where
        Pty: EventedPty + OnResize + Send + 'static,
    {
        let config = term::Config::default();
        let terminal_size = TerminalSize::default();

        let event_proxy = EventProxy(pty_event_proxy_sender);

        let mut term = Term::new(config, &terminal_size, event_proxy.clone());

        let cursor = term.grid_mut().cursor_cell().clone();

        let initial_content = RenderableContent {
            grid: term.grid().clone(),
            selectable_range: None,
            terminal_mode: *term.mode(),
            terminal_size,
            cursor: cursor.clone(),
            hovered_hyperlink: None,
        };

        let term = Arc::new(FairMutex::new(term));

        let pty_event_loop = EventLoop::new(term.clone(), event_proxy, pty, false, false)?;

        let notifier = Notifier(pty_event_loop.channel());

        let _ = pty_event_loop.spawn();

        Ok(Self {
            term: term.clone(),
            size: terminal_size,
            notifier,
            last_content: initial_content,
            url_regex: RegexSearch::new(URL_REGEX).expect("invalid url regexp"),
        })
    }

    /// 处理一条宿主下发的 `Command`，返回需要宿主执行的 `Action`。
    pub fn handle(&mut self, cmd: Command) -> Action {
        let mut action = Action::default();
        let term = self.term.clone();
        let mut term = term.lock();
        match cmd {
            Command::ProcessAlacrittyEvent(event) => {
                match event {
                    Event::Exit => {
                        action = Action::Shutdown;
                    }
                    Event::Title(title) => {
                        action = Action::ChangeTitle(title);
                    }
                    Event::PtyWrite(pty) => self.notifier.notify(pty.into_bytes()),
                    _ => {}
                };
            }
            Command::Write(input) => {
                self.write(input);
                term.scroll_display(Scroll::Bottom);
            }
            Command::Scroll(delta) => {
                self.scroll(&mut term, delta);
            }
            Command::Resize(layout_size, font_measure) => {
                self.resize(&mut term, layout_size, font_measure);
            }
            Command::SelectStart(selection_type, (x, y)) => {
                self.start_selection(&mut term, selection_type, x, y);
            }
            Command::SelectUpdate((x, y)) => {
                self.update_selection(&mut term, x, y);
            }
            Command::ProcessLink(link_action, point) => {
                self.process_link_action(&term, link_action, point);
            }
            Command::MouseReport(button, modifiers, point, pressed) => {
                self.process_mouse_report(button, modifiers, point, pressed);
            }
        };

        action
    }

    /// 处理超链接动作：悬停时计算匹配范围、清除或打开链接。
    fn process_link_action(
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
                self.open_link();
            }
        };
    }

    /// 用系统默认程序打开当前悬停的超链接。
    fn open_link(&self) {
        if let Some(range) = &self.last_content.hovered_hyperlink {
            let start = range.start();
            let end = range.end();

            let mut url = String::from(self.last_content.grid.index(*start).c);
            for indexed in self.last_content.grid.iter_from(*start) {
                url.push(indexed.c);
                if indexed.point == *end {
                    break;
                }
            }

            open::that(url).unwrap_or_else(|_| {
                panic!("link opening is failed");
            })
        }
    }

    /// 依据当前鼠标模式，向终端回送 SGR 或普通鼠标报告。
    fn process_mouse_report(
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
    fn sgr_mouse_report(&self, point: Point, button: u8, pressed: bool) {
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
    fn normal_mouse_report(&self, point: Point, button: u8, is_utf8: bool) {
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
    fn start_selection(
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
    fn update_selection(&mut self, terminal: &mut Term<EventProxy>, x: f32, y: f32) {
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
    fn selection_side(&self, x: f32) -> Side {
        let cell_x = x as usize % self.size.cell_width as usize;
        let half_cell_width = (self.size.cell_width as f32 / 2.0) as usize;

        if cell_x > half_cell_width {
            Side::Right
        } else {
            Side::Left
        }
    }

    /// 依据布局尺寸与字体度量重算行列数，并通知 alacritty 重排。
    fn resize(
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
            self.notifier.on_resize(self.size.into());
            terminal.resize(TermSize::new(
                self.size.num_cols as usize,
                self.size.num_lines as usize,
            ));
        }
    }

    /// 将输入字节经 notifier 写入 PTY。
    fn write<I: Into<Cow<'static, [u8]>>>(&self, input: I) {
        self.notifier.notify(input.into());
    }

    /// 按行滚动视口；若终端处于 alt 屏或 alt-screen 滚动模式则改发方向键序列。
    fn scroll(&mut self, terminal: &mut Term<EventProxy>, delta_value: i32) {
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

    /// 返回当前选中范围内的纯文本（去除网格控制字符）。
    pub fn selectable_content(&self) -> String {
        let content = self.renderable_content();
        let mut result = String::new();
        if let Some(range) = content.selectable_range {
            for indexed in content.grid.display_iter() {
                if range.contains(indexed.point) {
                    result.push(indexed.c);
                }
            }
        }
        result
    }

    /// 将 alacritty 终端最新状态同步到可渲染内容快照。
    pub fn sync(&mut self) {
        let term = self.term.clone();
        let mut term = term.lock();
        self.internal_sync(&mut term);
    }

    /// 内部同步：刷新网格、选区、光标与终端模式快照。
    fn internal_sync(&mut self, terminal: &mut Term<EventProxy>) {
        let selectable_range = match &terminal.selection {
            Some(s) => s.to_range(terminal),
            None => None,
        };

        let cursor = terminal.grid_mut().cursor_cell().clone();
        self.last_content.grid = terminal.grid().clone();
        self.last_content.selectable_range = selectable_range;
        self.last_content.cursor = cursor.clone();
        self.last_content.terminal_mode = *terminal.mode();
        self.last_content.terminal_size = self.size;
    }

    /// 返回最近一次同步的可渲染内容快照。
    pub fn renderable_content(&self) -> &RenderableContent {
        &self.last_content
    }

    /// 取自 alacritty/src/display/hint.rs 的 regex_match_at 实现
    /// 若指定坐标落在正则匹配的文本范围内，则取回该匹配。
    fn regex_match_at(
        &self,
        terminal: &Term<EventProxy>,
        point: Point,
        regex: &mut RegexSearch,
    ) -> Option<Match> {
        visible_regex_match_iter(terminal, regex).find(|rm| rm.contains(&point))
    }
}

/// 复制自 alacritty/src/display/hint.rs：
/// 遍历所有可见的正则匹配。
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

/// 一次渲染所需的终端内容快照。
pub struct RenderableContent {
    /// 当前网格（含所有单元格）。
    pub grid: Grid<Cell>,
    /// 当前悬停命中、可点击的超链接坐标范围。
    pub hovered_hyperlink: Option<RangeInclusive<Point>>,
    /// 当前选区对应的可复制范围（无选区则为 `None`）。
    pub selectable_range: Option<SelectionRange>,
    /// 当前光标所在单元格。
    pub cursor: Cell,
    /// 当前 alacritty 终端模式。
    pub terminal_mode: TermMode,
    /// 当前终端几何尺寸。
    pub terminal_size: TerminalSize,
}

impl Default for RenderableContent {
    /// 返回全空的 RenderableContent 默认值。
    fn default() -> Self {
        Self {
            grid: Grid::new(0, 0, 0),
            hovered_hyperlink: None,
            selectable_range: None,
            cursor: Cell::default(),
            terminal_mode: TermMode::empty(),
            terminal_size: TerminalSize::default(),
        }
    }
}

impl Drop for Backend {
    /// 析构时向 event loop 发送关闭消息。
    fn drop(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

/// 实现 alacritty `EventListener`：将事件转发给宿主的消息通道。
#[derive(Clone)]
pub struct EventProxy(mpsc::Sender<Event>);

impl EventListener for EventProxy {
    /// 以阻塞方式将 alacritty 事件发送到宿主通道。
    fn send_event(&self, event: Event) {
        let _ = self.0.blocking_send(event);
    }
}
