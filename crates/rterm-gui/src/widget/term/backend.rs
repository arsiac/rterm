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
    self, Term, TermMode,
    cell::{Cell, Flags},
    test::TermSize,
    viewport_to_point,
};
use alacritty_terminal::tty;
use alacritty_terminal::tty::EventedPty;
use alacritty_terminal::vte::ansi::CursorShape;
use iced::keyboard::Modifiers;
use iced_core::Size;
use log::warn;
use std::borrow::Cow;
use std::cmp::min;
use std::io::Result;
use std::ops::RangeInclusive;
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
    /// 事件代理（宿主事件通道的发送端包装）：`reattach` 重建 event loop 时复用同一
    /// 发送端，使宿主侧订阅（绑在通道接收端上）无需重建。
    event_proxy: EventProxy,
    /// 上一次同步后的可渲染内容快照。
    last_content: RenderableContent,
    /// 最近一次已下发 PTY 的行列数；用于抑制重复的 window-change 请求。
    last_pty_size: Option<(u16, u16)>,
    /// 用于识别超链接的 URL 正则（crate 内可见）。
    pub(crate) url_regex: RegexSearch,
    /// 复制时是否去除每行尾部空格。
    trim_trailing_whitespace: bool,
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
        Self::from_pty(
            id,
            pty_event_proxy_sender,
            pty,
            settings.scrollback,
            settings.trim_trailing_whitespace,
        )
    }

    /// SSH 场景：直接桥接 russh shell 通道，不经过本地 PTY 子进程。
    pub fn new_with_pty(
        id: u64,
        pty_event_proxy_sender: mpsc::Sender<Event>,
        pty: RusshPty,
        scrollback: usize,
        trim_trailing_whitespace: bool,
    ) -> Result<Self> {
        Self::from_pty(
            id,
            pty_event_proxy_sender,
            pty,
            scrollback,
            trim_trailing_whitespace,
        )
    }

    /// 以给定 PTY 构造后端，初始化 alacritty 终端与 event loop。
    fn from_pty<Pty>(
        _id: u64,
        pty_event_proxy_sender: mpsc::Sender<Event>,
        pty: Pty,
        scrollback: usize,
        trim_trailing_whitespace: bool,
    ) -> Result<Self>
    where
        Pty: EventedPty + OnResize + Send + 'static,
    {
        let config = term::Config {
            scrolling_history: scrollback,
            ..Default::default()
        };
        let terminal_size = TerminalSize::default();

        let event_proxy = EventProxy(pty_event_proxy_sender);

        let mut term = Term::new(config, &terminal_size, event_proxy.clone());

        let mut initial_content = RenderableContent::default();
        Self::capture_viewport(&mut initial_content, &mut term, terminal_size);

        let term = Arc::new(FairMutex::new(term));

        let pty_event_loop = EventLoop::new(term.clone(), event_proxy.clone(), pty, false, false)?;

        let notifier = Notifier(pty_event_loop.channel());

        let _ = pty_event_loop.spawn();

        Ok(Self {
            term: term.clone(),
            size: terminal_size,
            notifier,
            event_proxy,
            last_content: initial_content,
            last_pty_size: None,
            url_regex: RegexSearch::new(URL_REGEX).expect("invalid url regexp"),
            trim_trailing_whitespace,
        })
    }

    /// 就地换接一条新的 PTY（断线重连）：终端网格、滚动历史与选区原样保留，
    /// 事件订阅（绑在宿主通道接收端与 id 上）不变，仅重建 alacritty event loop。
    ///
    /// 旧 event loop 会被 `Msg::Shutdown` 确定性停掉（新循环建成后、切换 notifier 前）：
    /// 正常断开时它已因 pty 的子进程事件（见 `russh_pty` 模块文档）自行收尾，但若桥接
    /// 尚未收场（异常路径），不显式停旧线程则它会一直持有已死的管道空转。
    pub fn reattach(&mut self, pty: RusshPty) -> Result<()> {
        // 先建新循环再停旧循环：`EventLoop::new` 失败（如 poller 创建失败）时
        // 保留旧 notifier 不动，避免后端停在半死状态（旧循环已停、新循环没有）。
        let pty_event_loop = EventLoop::new(
            self.term.clone(),
            self.event_proxy.clone(),
            pty,
            false,
            false,
        )?;
        let _ = self.notifier.0.send(Msg::Shutdown);
        self.notifier = Notifier(pty_event_loop.channel());
        let _ = pty_event_loop.spawn();
        // 重置尺寸记忆：新通道在建桥时按自己的 cols/rows 开局，若旧值恰与新尺寸相同，
        // 不重置则后续 Resize 会被去重吞掉，新 shell 将按错尺寸一直活着。
        self.last_pty_size = None;
        Ok(())
    }

    /// 捕获终端可见区到 `content`：只遍历视口行，并复用 `content.cells` 已有分配。
    ///
    /// 视口大小为「可见行数 × 列数」（约数千格），与 scrollback 长度无关，
    /// 故本函数可在每条事件后调用而不引入随历史增长的开销。
    fn capture_viewport(
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
        content.cursor_point = grid.cursor.point;
        content.cursor = cursor;
        content.cursor_shape = cursor_shape;
        content.selectable_range = selectable_range;
        content.terminal_mode = terminal_mode;
        content.terminal_size = size;
    }

    /// 处理一条宿主下发的 `Command`，返回需要宿主执行的 `Action`。
    ///
    /// 终端锁按需获取：仅 `Wakeup` 这类纯重绘信号的事件不取锁，避免与解析线程争抢。
    pub fn handle(&mut self, cmd: Command) -> Action {
        match cmd {
            Command::ProcessAlacrittyEvent(event) => match event {
                // PtyWrite 需要访问 notifier，单独分支；其余事件走无状态的纯映射。
                Event::PtyWrite(pty) => {
                    self.notifier.notify(pty.into_bytes());
                    Action::default()
                }
                event => action_for_event(event),
            },
            Command::Write(input) => {
                self.write(input);
                let term = self.term.clone();
                term.lock().scroll_display(Scroll::Bottom);
                Action::default()
            }
            Command::Scroll(delta) => {
                let term = self.term.clone();
                let mut term = term.lock();
                self.scroll(&mut term, delta);
                Action::default()
            }
            Command::Resize(layout_size, font_measure) => {
                let term = self.term.clone();
                let mut term = term.lock();
                self.resize(&mut term, layout_size, font_measure);
                Action::default()
            }
            Command::SelectStart(selection_type, (x, y)) => {
                let term = self.term.clone();
                let mut term = term.lock();
                self.start_selection(&mut term, selection_type, x, y);
                Action::default()
            }
            Command::SelectUpdate((x, y)) => {
                let term = self.term.clone();
                let mut term = term.lock();
                self.update_selection(&mut term, x, y);
                Action::default()
            }
            Command::ProcessLink(link_action, point) => {
                let term = self.term.clone();
                let term = term.lock();
                self.process_link_action(&term, link_action, point);
                Action::default()
            }
            Command::MouseReport(button, modifiers, point, pressed) => {
                self.process_mouse_report(button, modifiers, point, pressed);
                Action::default()
            }
        }
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
                self.open_link(terminal);
            }
        };
    }

    /// 用系统默认程序打开当前悬停的超链接。
    ///
    /// 直接在已持锁的 `terminal` 上取字符：超链接范围可能落在视口之外，
    /// 视口快照不保证覆盖，故不能改从 `last_content` 读取。
    fn open_link(&self, terminal: &Term<EventProxy>) {
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
    fn selection_text(term: &Term<EventProxy>, trim_trailing_whitespace: bool) -> String {
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

    /// 将 alacritty 终端最新状态同步到可渲染内容快照。
    pub fn sync(&mut self) {
        let term = self.term.clone();
        let mut term = term.lock();
        self.internal_sync(&mut term);
    }

    /// 当前网格的行列数（列, 行），供重连给新 PTY 定开局尺寸——重连不会再触发 resize，
    /// 尺寸只能在建桥时给定。
    pub fn grid_size(&self) -> (u16, u16) {
        (self.size.num_cols, self.size.num_lines)
    }

    /// 把视口拉回底部（贴住实时输出）。
    ///
    /// 直接滚网格而不用 [`Command::Scroll`]：后者在备用屏（含滚动模式）会改发方向键
    /// 字节到 pty，对新 shell 是误输入。
    pub fn scroll_to_bottom(&mut self) {
        let term = self.term.clone();
        term.lock().scroll_display(Scroll::Bottom);
    }

    /// 内部同步：刷新视口单元格、选区、光标与终端模式快照。
    fn internal_sync(&mut self, terminal: &mut Term<EventProxy>) {
        Self::capture_viewport(&mut self.last_content, terminal, self.size);
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

/// 把 alacritty 事件归约为宿主动作：退出 / 标题 / 响铃需要宿主响应，其余静默忽略。
fn action_for_event(event: Event) -> Action {
    match event {
        Event::Exit => Action::Shutdown,
        Event::Title(title) => Action::ChangeTitle(title),
        Event::Bell => Action::Bell,
        _ => Action::default(),
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

/// 一次渲染所需的终端可见区快照。
///
/// 只含视口内容（可见行数 × 列数），不含 scrollback：捕获开销与历史长度无关。
/// `cells` 按行主序排列，`i` 号格子对应网格点
/// `(Line(i / columns - display_offset), Column(i % columns))`。
pub struct RenderableContent {
    /// 视口内单元格，按行主序。
    pub cells: Vec<Cell>,
    /// 视口列数，用于把 `cells` 下标还原为行列。
    pub columns: usize,
    /// 视口滚动偏移（0 表示贴住实时输出）。
    pub display_offset: usize,
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

impl Default for RenderableContent {
    /// 返回空视口的 RenderableContent 默认值。
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            columns: 1,
            display_offset: 0,
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

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::Processor;

    /// 构造无 PTY 的终端：历史缓冲 10 行，视口取 [`TerminalSize::default`]（80×50）。
    fn test_term() -> Term<EventProxy> {
        let (tx, _rx) = mpsc::channel(16);
        let config = term::Config {
            scrolling_history: 10,
            ..Default::default()
        };
        Term::new(config, &TerminalSize::default(), EventProxy(tx))
    }

    /// 把字节流喂给终端解析（不涉及 PTY，仅驱动 vt 状态机）。
    fn feed(term: &mut Term<EventProxy>, input: &[u8]) {
        let mut parser: Processor = Processor::new();
        parser.advance(term, input);
    }

    /// 写入字节并捕获一次视口快照。
    fn snapshot_after(term: &mut Term<EventProxy>, input: &[u8]) -> RenderableContent {
        feed(term, input);
        let mut content = RenderableContent::default();
        Backend::capture_viewport(&mut content, term, TerminalSize::default());
        content
    }

    #[test]
    fn viewport_snapshot_covers_visible_area_only() {
        let mut term = test_term();
        let content = snapshot_after(&mut term, b"abc\r\ndefg");
        let size = TerminalSize::default();

        assert_eq!(content.columns, size.columns());
        assert_eq!(content.cells.len(), size.screen_lines() * size.columns());
        assert_eq!(content.display_offset, 0);
    }

    #[test]
    fn viewport_snapshot_maps_row_major_from_top_visible_line() {
        let mut term = test_term();
        let content = snapshot_after(&mut term, b"abc\r\ndefg");
        let columns = content.columns;

        assert_eq!(content.cells[0].c, 'a');
        assert_eq!(content.cells[2].c, 'c');
        assert_eq!(content.cells[columns].c, 'd');
        assert_eq!(content.cells[columns + 3].c, 'g');
        assert_eq!(content.cursor_point, Point::new(Line(1), Column(4)));
    }

    #[test]
    fn viewport_snapshot_matches_alacritty_viewport_mapping() {
        let mut term = test_term();
        // 写满一屏并溢出一部分，再回滚 5 行，覆盖 display_offset != 0 的历史行。
        let input: String = (0..60).map(|i| format!("L{i}\r\n")).collect();
        feed(&mut term, input.as_bytes());
        term.grid_mut().scroll_display(Scroll::Delta(5));

        let mut content = RenderableContent::default();
        Backend::capture_viewport(&mut content, &mut term, TerminalSize::default());

        assert_eq!(content.display_offset, 5);
        let size = TerminalSize::default();
        let columns = content.columns;
        assert_eq!(columns, size.columns());
        assert_eq!(content.cells.len(), size.screen_lines() * columns);

        // 以 alacritty 自身的「屏幕点 → 网格点」换算为对照，逐格核对下标映射与滚动偏移。
        let grid = term.grid();
        for row in 0..size.screen_lines() {
            for col in 0..columns {
                let point = viewport_to_point(5, Point::new(row, Column(col)));
                assert_eq!(
                    content.cells[row * columns + col],
                    grid[point],
                    "row {row} col {col}"
                );
            }
        }
    }

    /// 响铃必须显式映射为 `Action::Bell`，不能落进兜底分支被静默吞掉。
    #[test]
    fn bell_event_maps_to_a_bell_action() {
        assert_eq!(action_for_event(Event::Bell), Action::Bell);
        assert_eq!(
            action_for_event(Event::Title("t".into())),
            Action::ChangeTitle("t".to_string())
        );
        // 无宿主响应义务的事件仍须静默。
        assert_eq!(action_for_event(Event::Wakeup), Action::Ignore);
    }

    /// 端到端钉住「BEL 字节到达解析层就会产生 `Event::Bell`」这一前提：
    /// 喂真实字节流（而非直接构造事件），上游 alacritty 若改了行为这里有红灯。
    #[test]
    fn a_bel_byte_in_the_stream_emits_a_bell_event() {
        let (tx, mut rx) = mpsc::channel(16);
        let mut term = Term::new(
            term::Config::default(),
            &TerminalSize::default(),
            EventProxy(tx),
        );
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x07");

        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            events.iter().any(|e| matches!(e, Event::Bell)),
            "\\x07 应产生 Bell 事件，实际：{events:?}"
        );
    }

    /// `Event::Exit`（pty 子进程事件使 `Term::exit()` 发出）必须映射为 `Action::Shutdown`：
    /// GUI 侧据此识别「终端自身收尾」并明确忽略，绝不当作关标签信号
    /// （见 `app::terminal_bridge::handle_terminal_event` 的分流注释）。
    #[test]
    fn exit_event_maps_to_a_shutdown_action() {
        assert_eq!(action_for_event(Event::Exit), Action::Shutdown);
    }

    // ===============================================================
    // 断线重连（reattach）用例
    //
    // 把一个真实 `RusshPty` 挂在一对 socketpair 上模拟桥接（Windows 的命名管道不便在
    // 测试里搭，故 `cfg(unix)`）：测试线程从远端写端代替 pump 喂输出，alacritty
    // event loop 从近端读端解析；断言直接落在锁内的 `Term`（含历史缓冲）上。
    // ===============================================================

    /// 一条模拟桥接的测试装具。
    #[cfg(unix)]
    struct Fixture {
        /// 挂着事件循环的后端（其 `term` 供断言直接查看）。
        backend: Backend,
        /// 模拟桥接异步端的写端：测试写进去的内容即「远端 shell 输出」。
        remote: std::fs::File,
        /// 桥接结束状态（`request_stop` 驱动结束观察线程，见 `russh_pty`）。
        bridge: Arc<rterm_core::BridgeState>,
        /// 尺寸变更接收端（pty 转发 window-change 的去向）。
        resizes: mpsc::Receiver<(u32, u32)>,
        /// 后端事件通道的接收端（宿主订阅的原型）。
        events: mpsc::Receiver<Event>,
    }

    #[cfg(unix)]
    impl Fixture {
        /// 建装具；`scrollback` 为历史缓冲行数。
        fn new(scrollback: usize) -> Self {
            let (local, remote) = socket_pair();
            let bridge = Arc::new(rterm_core::BridgeState::new());
            let (resize_tx, resizes) = mpsc::channel(8);
            let pty = RusshPty::new(
                local.try_clone().expect("clone the sync end"),
                local,
                bridge.clone(),
                resize_tx,
            )
            .expect("wrap the sync end as a pty");
            let (event_tx, events) = mpsc::channel(64);
            let backend = Backend::new_with_pty(1, event_tx, pty, scrollback, true)
                .expect("create the backend");
            Self {
                backend,
                remote,
                bridge,
                resizes,
                events,
            }
        }
    }

    /// 建一对非阻塞 socketpair 并转成 `File`，返回 `(近端, 远端)`。
    /// 近端交给 pty（事件循环侧），远端由测试持作桥接异步端的替身。
    #[cfg(unix)]
    fn socket_pair() -> (std::fs::File, std::fs::File) {
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixStream;
        let (near, far) = UnixStream::pair().expect("create a socketpair");
        near.set_nonblocking(true)
            .expect("set the near end non-blocking");
        far.set_nonblocking(true)
            .expect("set the far end non-blocking");
        (
            std::fs::File::from(OwnedFd::from(near)),
            std::fs::File::from(OwnedFd::from(far)),
        )
    }

    /// 再造一条模拟桥接供 reattach 换接，返回 `(pty, 远端写端, 桥接状态, 尺寸接收端)`。
    #[cfg(unix)]
    fn extra_bridge() -> (
        RusshPty,
        std::fs::File,
        Arc<rterm_core::BridgeState>,
        mpsc::Receiver<(u32, u32)>,
    ) {
        let (local, remote) = socket_pair();
        let bridge = Arc::new(rterm_core::BridgeState::new());
        let (resize_tx, resizes) = mpsc::channel(8);
        let pty = RusshPty::new(
            local.try_clone().expect("clone the sync end"),
            local,
            bridge.clone(),
            resize_tx,
        )
        .expect("wrap the sync end as a pty");
        (pty, remote, bridge, resizes)
    }

    /// 代替 pump 向「远端」写一段输出。
    #[cfg(unix)]
    fn feed_remote(remote: &mut std::fs::File, data: &[u8]) {
        use std::io::Write as _;
        remote.write_all(data).expect("write into the fake bridge");
    }

    /// 取整格文本（历史缓冲 + 视口），行间以换行分隔。
    #[cfg(unix)]
    fn grid_text(term: &Arc<FairMutex<Term<EventProxy>>>) -> String {
        let term = term.lock();
        let grid = term.grid();
        // `GridIterator` 先推进游标再产出（起点本身被跳过），故从「顶行上一行」的末列
        // 起步，使第一个产出的格子恰为历史缓冲首行首列。
        let start = Point::new(grid.topmost_line() - 1, grid.last_column());
        let mut text = String::new();
        let mut last_line = i32::MIN;
        for indexed in grid.iter_from(start) {
            if indexed.point.line.0 != last_line {
                if !text.is_empty() {
                    text.push('\n');
                }
                last_line = indexed.point.line.0;
            }
            text.push(indexed.c);
        }
        text
    }

    /// 取视口文本（仅可见区，不含历史缓冲）。
    #[cfg(unix)]
    fn viewport_text(term: &Arc<FairMutex<Term<EventProxy>>>) -> String {
        term.lock()
            .grid()
            .display_iter()
            .map(|indexed| indexed.c)
            .collect()
    }

    /// 轮询等整格文本出现 `needle`（事件循环异步解析，需等待）；超时打印网格判失败。
    #[cfg(unix)]
    fn wait_grid_contains(term: &Arc<FairMutex<Term<EventProxy>>>, needle: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if grid_text(term).contains(needle) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "timed out waiting for {needle:?} in the terminal grid; current grid:\n{}",
            grid_text(term)
        );
    }

    /// 等一条满足条件的事件（其余事件跳过）；超时或通道关闭判失败。
    #[cfg(unix)]
    async fn wait_event(rx: &mut mpsc::Receiver<Event>, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let event = match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => panic!("event channel closed while waiting"),
                Err(_) => panic!("timed out waiting for a matching event"),
            };
            if pred(&event) {
                return event;
            }
        }
    }

    /// 等一条 resize 通知；超时判失败。
    #[cfg(unix)]
    async fn wait_resize(rx: &mut mpsc::Receiver<(u32, u32)>) -> (u32, u32) {
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for a resize notification")
            .expect("resize channel closed")
    }

    /// 喂一段输出 → 断开 → reattach → 再喂——
    /// 旧行留在历史缓冲、新输出被同一终端解析、旧循环在断开时自行收尾（发出 `Event::Exit`
    /// 即证明它没有对着已死的管道空转挂死）。
    #[cfg(unix)]
    #[tokio::test]
    async fn reattach_keeps_scrollback_and_parses_new_output() {
        let mut fx = Fixture::new(100);

        // 1) 喂出把标记行顶出视口、只留在历史缓冲的输出。
        let mut old = String::from("old-marker\r\n");
        old.push_str(&"filler-line\r\n".repeat(60));
        feed_remote(&mut fx.remote, old.as_bytes());
        wait_grid_contains(&fx.backend.term, "old-marker");
        assert!(
            !viewport_text(&fx.backend.term).contains("old-marker"),
            "前提不成立：标记行应已被顶出视口，仅存于历史缓冲"
        );

        // 2) 断开：远端管道关端（pump 退出后的形态）+ 停止请求（关标签路径）。
        //    停止请求唤醒 pty 的结束观察线程 → 事件循环走子进程事件分支收尾，
        //    并向宿主发送 `Event::Exit`。
        drop(fx.remote);
        fx.bridge.request_stop();
        let exit = wait_event(&mut fx.events, |e| matches!(e, Event::Exit)).await;
        assert!(matches!(exit, Event::Exit));

        // 3) reattach 到一条新桥接。
        let (pty, mut remote, bridge, _resizes) = extra_bridge();
        fx.backend.reattach(pty).expect("reattach must succeed");

        // 4) 新输出由同一个终端解析；旧行仍在（reattach 未重建 Term）。
        feed_remote(&mut remote, b"new-marker\r\n");
        wait_grid_contains(&fx.backend.term, "new-marker");
        assert!(
            grid_text(&fx.backend.term).contains("old-marker"),
            "reattach 后历史缓冲必须原样保留"
        );

        // 收尾：让新循环也走子进程事件退出，测试进程不残留观察线程。
        bridge.request_stop();
    }

    /// reattach 必须重置尺寸记忆：否则新通道的实际尺寸恰与旧值相同时，
    /// 后续 Resize 会被去重吞掉，新 shell 将按错尺寸一直活着。
    #[cfg(unix)]
    #[tokio::test]
    async fn reattach_resets_pty_size_bookkeeping() {
        let mut fx = Fixture::new(10);
        let layout = Size::new(800.0, 600.0);
        let font = Size::new(10.0, 20.0);

        // 首次下发：新尺寸（80 列 × 30 行）被转发到旧通道。
        fx.backend.handle(Command::Resize(Some(layout), Some(font)));
        assert_eq!(wait_resize(&mut fx.resizes).await, (80, 30));

        // 同尺寸重复下发：被记忆去重，不再转发。
        fx.backend.handle(Command::Resize(Some(layout), Some(font)));
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            fx.resizes.try_recv().is_err(),
            "尺寸未变化时不应重复通知远端"
        );

        // 换接新 pty 后，同尺寸必须重新通知（记忆已重置）。
        let (pty, _remote, bridge, mut resizes) = extra_bridge();
        fx.backend.reattach(pty).expect("reattach must succeed");
        fx.backend.handle(Command::Resize(Some(layout), Some(font)));
        assert_eq!(wait_resize(&mut resizes).await, (80, 30));

        bridge.request_stop();
    }

    /// `grid_size` 反映最后一次布局折算出的行列数——重连按它给定新 PTY 的开局尺寸。
    #[cfg(unix)]
    #[tokio::test]
    async fn grid_size_reflects_the_last_layout() {
        let mut fx = Fixture::new(10);
        // 尚未收到布局：取默认尺寸（80 列 × 50 行）。
        assert_eq!(fx.backend.grid_size(), (80, 50));

        // 布局 800×600 像素、单元格 10×20 像素 ⇒ 80 列 × 30 行。
        fx.backend.handle(Command::Resize(
            Some(Size::new(800.0, 600.0)),
            Some(Size::new(10.0, 20.0)),
        ));
        assert_eq!(fx.backend.grid_size(), (80, 30));

        // 收尾：让事件循环走子进程事件退出（同其它用例）。
        fx.bridge.request_stop();
    }

    /// 「视口回到底部」：翻回历史后回到底部、贴住实时输出。
    ///
    /// 用普通 `#[test]` 而非 `#[tokio::test]`：`Term::scroll_display` 经 `EventProxy`
    /// 阻塞发事件，`blocking_send` 不允许在 tokio 运行时线程上调用。
    #[cfg(unix)]
    #[test]
    fn scroll_to_bottom_resets_a_scrolled_viewport() {
        let mut fx = Fixture::new(100);
        let mut output = String::new();
        for i in 0..80 {
            output.push_str(&format!("line-{i:02}\r\n"));
        }
        feed_remote(&mut fx.remote, output.as_bytes());
        wait_grid_contains(&fx.backend.term, "line-79");

        fx.backend
            .term
            .lock()
            .grid_mut()
            .scroll_display(Scroll::Delta(10));
        fx.backend.sync();
        assert_eq!(
            fx.backend.renderable_content().display_offset,
            10,
            "前提：视口已翻回历史"
        );

        fx.backend.scroll_to_bottom();
        fx.backend.sync();
        assert_eq!(
            fx.backend.renderable_content().display_offset,
            0,
            "视口应回到底部、贴住实时输出"
        );

        fx.bridge.request_stop();
    }

    // ===============================================================
    // 复制选区（`selectable_content`）用例
    // ===============================================================

    /// 建一个简单拖拽选区：起点 → 终点（含两端所在格）。
    fn select_simple(term: &mut Term<EventProxy>, start: Point, end: Point) {
        term.selection = Some(Selection::new(SelectionType::Simple, start, Side::Left));
        term.selection
            .as_mut()
            .expect("selection just set")
            .update(end, Side::Right);
    }

    /// 复制含 CJK 的行：宽字符占位格不得产出空格（「会话」不能被复制成「会 话」）。
    #[test]
    fn selection_text_skips_wide_char_spacers() {
        let mut term = test_term();
        feed(&mut term, "会话 abc".as_bytes());
        select_simple(
            &mut term,
            Point::new(Line(0), Column(0)),
            Point::new(Line(0), Column(7)),
        );

        assert_eq!(Backend::selection_text(&term, false), "会话 abc");
    }

    /// 选区起点落在宽字符右半的占位格上时，整字一并纳入（对齐上游起始列修正）。
    #[test]
    fn selection_text_backs_up_when_starting_on_a_spacer() {
        let mut term = test_term();
        feed(&mut term, "会话 abc".as_bytes());
        // Column(1) 是「会」的占位格。
        select_simple(
            &mut term,
            Point::new(Line(0), Column(1)),
            Point::new(Line(0), Column(7)),
        );

        assert_eq!(Backend::selection_text(&term, false), "会话 abc");
    }

    /// 组合字符（zerowidth）随主字符一并复制，不丢附加码点。
    #[test]
    fn selection_text_keeps_zerowidth_characters() {
        let mut term = test_term();
        feed(&mut term, "e\u{301}x".as_bytes());
        select_simple(
            &mut term,
            Point::new(Line(0), Column(0)),
            Point::new(Line(0), Column(1)),
        );

        assert_eq!(Backend::selection_text(&term, false), "e\u{301}x");
    }

    /// 选区落在视口之外的历史行同样完整复制（逐格取自网格，不受滚动位置影响）。
    #[test]
    fn selection_text_covers_history_outside_the_viewport() {
        let mut term = test_term();
        // 视口 50 行：写 51 行后上滚 2 行，L0/L1 进入历史缓冲。
        let input: String = (0..51).map(|i| format!("L{i}\r\n")).collect();
        feed(&mut term, input.as_bytes());
        assert_eq!(
            term.grid().topmost_line(),
            Line(-2),
            "前提：L0/L1 已被顶入历史缓冲，当前视口自 L2 起"
        );

        select_simple(
            &mut term,
            Point::new(Line(-2), Column(0)),
            Point::new(Line(-1), Column(1)),
        );

        assert_eq!(Backend::selection_text(&term, true), "L0\nL1");
    }
}
