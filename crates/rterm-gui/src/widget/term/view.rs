use crate::widget::term::backend::{Backend, Command, LinkAction, MouseButton, RenderableContent};
use crate::widget::term::bindings::{BindingAction, BindingsLayout, InputKind};
use crate::widget::term::terminal::{Event, Terminal};
use crate::widget::term::theme::TerminalStyle;
use alacritty_terminal::index::Point as TerminalGridPoint;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::{TermMode, cell};
use alacritty_terminal::vte::ansi::{self as ansi, NamedColor};
use iced::alignment::Vertical;
use iced::font::{Style as FontStyle, Weight as FontWeight};
use iced::mouse::{Cursor, ScrollDelta};
use iced::widget::canvas::{Path, Text};
use iced::widget::container;
use iced::{Color, Element, Length, Point, Rectangle, Size, Theme};
use iced_core::clipboard::Kind as ClipboardKind;
use iced_core::keyboard::{Key, Modifiers, key::Named};
use iced_core::mouse::{self, Click};
use iced_core::text::{Alignment, LineHeight, Shaping};
use iced_core::widget::operation;
use iced_graphics::core::Widget;
use iced_graphics::core::widget::{Tree, tree};
use iced_graphics::geometry::Stroke;
use std::cell::Cell;

/// 终端画布部件：实现 iced `Widget`，把后端渲染内容绘成像素并转发鼠标 / 键盘事件。
pub struct TerminalView<'a> {
    /// 被渲染的终端实例，提供后端渲染内容、主题与字体等。
    term: &'a Terminal,
    /// 来自 app 的「终端是否持有键盘焦点」，用于绘制实心/空心光标与门控输入。
    focused: bool,
}

impl<'a> TerminalView<'a> {
    /// 以给定终端与焦点态构造可装箱的终端视图元素。
    pub fn show(term: &'a Terminal, focused: bool) -> Element<'a, Event> {
        container(Self { term, focused })
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| term.theme.container_style())
            .into()
    }

    /// 判断鼠标光标是否落在终端部件布局矩形范围内。
    fn is_cursor_in_layout(&self, cursor: Cursor, layout: iced_graphics::core::Layout<'_>) -> bool {
        if let Some(cursor_position) = cursor.position() {
            let layout_position = layout.position();
            let layout_size = layout.bounds();
            let is_triggered = cursor_position.x >= layout_position.x
                && cursor_position.y >= layout_position.y
                && cursor_position.x < (layout_position.x + layout_size.width)
                && cursor_position.y < (layout_position.y + layout_size.height);

            return is_triggered;
        }

        false
    }

    /// 判断当前鼠标位置是否悬停在某个超链接区域之上。
    fn is_cursor_hovered_hyperlink(&self, state: &TerminalViewState) -> bool {
        let content = self.term.backend.renderable_content();
        if let Some(hyperlink_range) = &content.hovered_hyperlink {
            return hyperlink_range.contains(&state.mouse_position_on_grid);
        }

        false
    }

    /// 比较布局尺寸与已记录尺寸，变化时发布终端重设大小命令。
    fn handle_resize(
        &mut self,
        state: &mut TerminalViewState,
        layout: iced_graphics::core::Layout<'_>,
        shell: &mut iced_graphics::core::Shell<'_, Event>,
    ) {
        let layout_size = layout.bounds().size();
        if state.size != layout_size {
            state.size = layout_size;
            let cmd = Command::Resize(Some(layout_size), Some(self.term.font.measure));
            shell.publish(Event::BackendCall(self.term.id, cmd));
        }
    }

    /// 分发鼠标事件为后端命令（鼠标报告、选区、滚轮滚动等）。
    fn handle_mouse_event(
        &self,
        state: &mut TerminalViewState,
        layout_position: Point,
        cursor_position: Point,
        event: &iced::mouse::Event,
    ) -> Vec<Command> {
        let mut commands = Vec::new();
        let terminal_content = self.term.backend.renderable_content();
        let terminal_mode = terminal_content.terminal_mode;

        match event {
            iced_core::mouse::Event::ButtonPressed(iced_core::mouse::Button::Left) => {
                if !self.focused {
                    return Vec::default();
                }

                Self::handle_left_button_pressed(
                    state,
                    &terminal_mode,
                    cursor_position,
                    layout_position,
                    &mut commands,
                );
            }
            iced_core::mouse::Event::CursorMoved { position } => {
                if !self.focused {
                    return Vec::default();
                }

                Self::handle_cursor_moved(
                    state,
                    self.term.backend.renderable_content(),
                    position,
                    layout_position,
                    &mut commands,
                );
            }
            iced_core::mouse::Event::ButtonReleased(iced_core::mouse::Button::Left) => {
                if !self.focused {
                    return Vec::default();
                }

                Self::handle_button_released(
                    state,
                    &terminal_mode,
                    &self.term.bindings,
                    &mut commands,
                );
            }
            iced::mouse::Event::WheelScrolled { delta } => {
                Self::handle_wheel_scrolled(state, *delta, &self.term.font.measure, &mut commands);
            }
            _ => {}
        }

        commands
    }

    /// 处理鼠标左键按下：鼠标模式发报告，否则发起选区。
    fn handle_left_button_pressed(
        state: &mut TerminalViewState,
        terminal_mode: &TermMode,
        cursor_position: Point,
        layout_position: Point,
        commands: &mut Vec<Command>,
    ) {
        let cmd = if terminal_mode.intersects(TermMode::MOUSE_MODE) {
            Command::MouseReport(
                MouseButton::LeftButton,
                state.keyboard_modifiers,
                state.mouse_position_on_grid,
                true,
            )
        } else {
            let current_click = Click::new(cursor_position, mouse::Button::Left, state.last_click);
            let selection_type = match current_click.kind() {
                mouse::click::Kind::Single => SelectionType::Simple,
                mouse::click::Kind::Double => SelectionType::Semantic,
                mouse::click::Kind::Triple => SelectionType::Lines,
            };
            state.last_click = Some(current_click);
            Command::SelectStart(
                selection_type,
                (
                    cursor_position.x - layout_position.x,
                    cursor_position.y - layout_position.y,
                ),
            )
        };
        commands.push(cmd);
        state.is_dragged = true;
    }

    /// 处理鼠标移动：更新网格坐标，拖拽时更新选区或悬浮链接。
    fn handle_cursor_moved(
        state: &mut TerminalViewState,
        terminal_content: &RenderableContent,
        position: &Point,
        layout_position: Point,
        commands: &mut Vec<Command>,
    ) {
        let cursor_x = position.x - layout_position.x;
        let cursor_y = position.y - layout_position.y;
        state.mouse_position_on_grid = Backend::selection_point(
            cursor_x,
            cursor_y,
            &terminal_content.terminal_size,
            terminal_content.grid.display_offset(),
        );

        // 根据终端模式与修饰键，分派命令或选区更新
        if state.is_dragged {
            let terminal_mode = terminal_content.terminal_mode;
            let cmd = if terminal_mode.intersects(TermMode::MOUSE_MOTION) {
                Command::MouseReport(
                    MouseButton::LeftMove,
                    state.keyboard_modifiers,
                    state.mouse_position_on_grid,
                    true,
                )
            } else {
                Command::SelectUpdate((cursor_x, cursor_y))
            };
            commands.push(cmd);
        }

        // 处理链接悬浮态（如适用）
        if state.keyboard_modifiers == Modifiers::COMMAND {
            commands.push(Command::ProcessLink(
                LinkAction::Hover,
                state.mouse_position_on_grid,
            ));
        }
    }

    /// 处理鼠标左键释放：结束拖拽，必要时上报鼠标或打开链接。
    fn handle_button_released(
        state: &mut TerminalViewState,
        terminal_mode: &TermMode,
        bindings: &BindingsLayout,
        commands: &mut Vec<Command>,
    ) {
        state.is_dragged = false;

        if terminal_mode.intersects(TermMode::MOUSE_MODE) {
            commands.push(Command::MouseReport(
                MouseButton::LeftButton,
                state.keyboard_modifiers,
                state.mouse_position_on_grid,
                false,
            ));
        }

        if bindings.get_action(
            InputKind::Mouse(iced_core::mouse::Button::Left),
            state.keyboard_modifiers,
            *terminal_mode,
        ) == BindingAction::LinkOpen
        {
            commands.push(Command::ProcessLink(
                LinkAction::Open,
                state.mouse_position_on_grid,
            ));
        }
    }

    /// 处理滚轮滚动：按行或像素折算为历史回滚命令。
    fn handle_wheel_scrolled(
        state: &mut TerminalViewState,
        delta: ScrollDelta,
        font_measure: &Size<f32>,
        commands: &mut Vec<Command>,
    ) {
        // winit 约定 `y` 为正表示内容向下移动（滚轮上滚 / 触控板下划），与 `Command::Scroll`
        // 的「正值向上回滚历史」同向，故两个分支都直接沿用 `y` 的符号，不做取反。
        match delta {
            ScrollDelta::Lines { y, .. } => {
                let lines = y.signum() * y.abs().round();
                commands.push(Command::Scroll(lines as i32));
            }
            ScrollDelta::Pixels { y, .. } => {
                // 单行像素高度：把不足一行的像素增量累积起来，满一行才折算，避免高分辨率
                // 触控板的小增量被 `trunc` 直接抹掉。
                let line_height = font_measure.height;
                state.scroll_pixels += y;
                let lines = (state.scroll_pixels / line_height).trunc();
                state.scroll_pixels %= line_height;
                if lines != 0.0 {
                    commands.push(Command::Scroll(lines as i32));
                }
            }
        }
    }

    /// 处理键盘事件：解析键位绑定并生成写入、复制、粘贴等命令。
    fn handle_keyboard_event(
        &self,
        state: &mut TerminalViewState,
        clipboard: &mut dyn iced_graphics::core::Clipboard,
        event: &iced::keyboard::Event,
    ) -> Option<Command> {
        let mut binding_action = BindingAction::Ignore;
        let last_content = self.term.backend.renderable_content();
        match event {
            iced::keyboard::Event::ModifiersChanged(m) => {
                state.keyboard_modifiers = *m;
                let action = if state.keyboard_modifiers == Modifiers::COMMAND {
                    LinkAction::Hover
                } else {
                    LinkAction::Clear
                };
                return Some(Command::ProcessLink(action, state.mouse_position_on_grid));
            }
            iced::keyboard::Event::KeyPressed {
                key,
                modifiers,
                text,
                ..
            } => match &key {
                // 即使 text 为 None，键位绑定也使用物理字符键（如 Ctrl/Cmd 组合键）
                Key::Character(k) => {
                    let lower = k.to_ascii_lowercase();
                    binding_action = self.term.bindings.get_action(
                        InputKind::Char(lower),
                        state.keyboard_modifiers,
                        last_content.terminal_mode,
                    );

                    // 若无匹配绑定，则写入可打印文本（若有）；否则对 Ctrl+字母 / 数字
                    // 退回生成控制字符（如 Ctrl+C => \x03），避免这些组合键完全无输入。
                    if binding_action == BindingAction::Ignore {
                        if let Some(c) = text {
                            return Some(Command::Write(c.as_bytes().to_vec()));
                        } else if modifiers.control()
                            && k.chars().count() == 1
                            && let Some(ctrl_byte) = char_to_ctrl(k.chars().next().unwrap())
                        {
                            return Some(Command::Write(vec![ctrl_byte]));
                        }
                    }
                }
                Key::Named(code) => {
                    binding_action = self.term.bindings.get_action(
                        InputKind::KeyCode(*code),
                        *modifiers,
                        last_content.terminal_mode,
                    );

                    // 命名键（回车 / 退格 / 方向键等）若无匹配绑定，退回标准转义序列：
                    // 否则这些键在终端里完全无输入（如回车按了没反应）。已匹配绑定的键不受影响。
                    if binding_action == BindingAction::Ignore
                        && let Some(bytes) = named_key_bytes(*code, *modifiers, text.as_deref())
                    {
                        return Some(Command::Write(bytes));
                    }
                }
                _ => {}
            },
            _ => {}
        }

        match binding_action {
            BindingAction::Char(c) => {
                let mut buf = [0, 0, 0, 0];
                let str = c.encode_utf8(&mut buf);
                return Some(Command::Write(str.as_bytes().to_vec()));
            }
            BindingAction::Esc(seq) => {
                return Some(Command::Write(seq.as_bytes().to_vec()));
            }
            BindingAction::Paste => {
                if let Some(data) = clipboard.read(ClipboardKind::Standard) {
                    let input: Vec<u8> = data.bytes().collect();
                    return Some(Command::Write(input));
                }
            }
            BindingAction::Copy => {
                clipboard.write(
                    ClipboardKind::Standard,
                    self.term.backend.selectable_content(),
                );
            }
            _ => {}
        };

        None
    }
}

/// 把 `Ctrl+字母/数字/符号` 转成对应的 ASCII 控制字符（如 `Ctrl+C` => `\x03`）。
///
/// 仅当系统未给出 `text`（iced 在某些情况下对组合键不填充 `text`）时作为回退使用；
/// 普通字符键优先走 `text` 路径，不会经过此处。
fn char_to_ctrl(ch: char) -> Option<u8> {
    if ch.is_ascii() {
        // 'a'..='z' / 'A'..='Z' => 0x01..0x1a；'@' => 0x00；'[' => 0x1b（Ctrl+[ = ESC）等。
        Some(ch.to_ascii_lowercase() as u8 & 0x1f)
    } else {
        None
    }
}

/// 把「无键位绑定」的命名键转成终端字节序列。
///
/// 优先采用系统给出的 `text`（回车 `\r`、Tab `\t` 等已含），缺失时再查标准转义序列。
/// `modifiers` 用于为方向键 / Home / End 等生成带修饰符的 CSI 序列（如 `Ctrl+←` => `\x1b[1;5D`）。
fn named_key_bytes(name: Named, modifiers: Modifiers, text: Option<&str>) -> Option<Vec<u8>> {
    // 系统已给出文本（回车 / Tab / 空格等），直接采用。
    if let Some(t) = text
        && !t.is_empty()
    {
        return Some(t.as_bytes().to_vec());
    }

    // CSI 修饰符字节：1 + Shift(1) + Alt(2) + Ctrl(4)。无修饰符时为 1（不含修饰段）。
    let mut m = 1u8;
    if modifiers.contains(Modifiers::SHIFT) {
        m += 1;
    }
    if modifiers.contains(Modifiers::ALT) {
        m += 2;
    }
    if modifiers.contains(Modifiers::CTRL) {
        m += 4;
    }

    // 方向键 / Home / End：无修饰符为 `\x1b[A`，有修饰符为 `\x1b[1;{m}A`。
    let arrow = |dir: u8| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[1;{m}{}", dir as char).into_bytes()
        } else {
            format!("\x1b[{}", dir as char).into_bytes()
        }
    };
    // 其余转义键：无修饰符为 `\x1b[{n}~`，有修饰符为 `\x1b[{n};{m}~`。
    let tilde = |n: u8| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[{n};{m}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };

    Some(match name {
        Named::Enter => b"\r".to_vec(),
        Named::Backspace => b"\x7f".to_vec(),
        Named::Tab => b"\t".to_vec(),
        Named::Escape => b"\x1b".to_vec(),
        Named::ArrowUp => arrow(b'A'),
        Named::ArrowDown => arrow(b'B'),
        Named::ArrowRight => arrow(b'C'),
        Named::ArrowLeft => arrow(b'D'),
        Named::Home => arrow(b'H'),
        Named::End => arrow(b'F'),
        Named::Insert => tilde(2),
        Named::Delete => tilde(3),
        Named::PageUp => tilde(5),
        Named::PageDown => tilde(6),
        Named::F1 => b"\x1bOP".to_vec(),
        Named::F2 => b"\x1bOQ".to_vec(),
        Named::F3 => b"\x1bOR".to_vec(),
        Named::F4 => b"\x1bOS".to_vec(),
        Named::F5 => b"\x1b[15~".to_vec(),
        Named::F6 => b"\x1b[17~".to_vec(),
        Named::F7 => b"\x1b[18~".to_vec(),
        Named::F8 => b"\x1b[19~".to_vec(),
        Named::F9 => b"\x1b[20~".to_vec(),
        Named::F10 => b"\x1b[21~".to_vec(),
        Named::F11 => b"\x1b[23~".to_vec(),
        Named::F12 => b"\x1b[24~".to_vec(),
        _ => return None,
    })
}

impl Widget<Event, Theme, iced::Renderer> for TerminalView<'_> {
    /// 返回部件建议尺寸（宽高均填满父容器）。
    fn size(&self) -> Size<Length> {
        Size {
            width: Length::Fill,
            height: Length::Fill,
        }
    }

    /// 返回部件状态类型标签，指向 `TerminalViewState`。
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<TerminalViewState>()
    }

    /// 构造并返回部件的初始内部状态 `TerminalViewState`。
    fn state(&self) -> tree::State {
        tree::State::new(TerminalViewState::new())
    }

    /// 计算部件布局节点，使其填满可用的宽高限制。
    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &iced_core::layout::Limits,
    ) -> iced_core::layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fill, Size::ZERO);
        iced::advanced::layout::Node::new(size)
    }

    /// 应用部件操作（此处无需额外处理，留空）。
    fn operate(
        &mut self,
        _tree: &mut Tree,
        _layout: iced_core::Layout<'_>,
        _renderer: &iced::Renderer,
        _operation: &mut dyn operation::Operation,
    ) {
    }

    /// 将后端渲染内容绘制为几何图元（背景、文本、光标、下划线）。
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout,
        _cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<TerminalViewState>();
        let content = self.term.backend.renderable_content();
        let term_size = content.terminal_size;
        let cell_width = term_size.cell_width as f32;
        let cell_height = term_size.cell_height as f32;
        let font_size = self.term.font.size;
        let font_scale_factor = self.term.font.scale_factor;
        let layout_offset_x = layout.position().x;
        let layout_offset_y = layout.position().y;

        // 焦点变化但布局尺寸不变时，几何缓存直接复用旧绘制结果，导致光标（实心/空心）
        // 不随键盘焦点切换刷新。此处检测焦点变化并清缓存，强制本帧重绘光标状态。
        if self.focused != state.last_focus.get() {
            self.term.cache.clear();
            state.last_focus.set(self.focused);
        }

        let geom = self.term.cache.draw(renderer, viewport.size(), |frame| {
            // 预计算内循环使用的常量
            let display_offset = content.grid.display_offset() as f32;
            let cell_size = Size::new(cell_width, cell_height);
            let half_w = cell_width * 0.5;
            let half_h = cell_height * 0.5;
            // 默认使用背景调色板颜色
            // 因为部件全局背景色必须保持一致
            let default_bg = self
                .term
                .theme
                .get_color(ansi::Color::Named(NamedColor::Background));

            let mut last_line: Option<i32> = None;
            let mut bg_batch_rect = BackgroundRect::default();

            for indexed in content.grid.display_iter() {
                // 低成本计算每格几何信息
                let line = indexed.point.line.0;
                let col = indexed.point.column.0 as f32;

                // 解析该格的位置点
                let x = layout_offset_x + (col * cell_width);
                let y = layout_offset_y + (((line as f32) + display_offset) * cell_height);
                let cell_center_y = y + half_h;
                let cell_center_x = x + half_w;

                // 解析该格的颜色
                let mut fg = self.term.theme.get_color(indexed.fg);
                let mut bg = self.term.theme.get_color(indexed.bg);

                // 若检测到换行，
                // 需要刷新待绘背景矩形并初始化新矩形
                if last_line != Some(line) {
                    if bg_batch_rect.can_flush() {
                        let line = last_line.unwrap_or(line);
                        frame.fill(&bg_batch_rect.build(line), bg_batch_rect.color);
                    }

                    last_line = Some(line);
                    bg_batch_rect = BackgroundRect::default()
                        .with_cell_height(cell_height)
                        .with_display_offset(display_offset)
                        .with_layout_offset_y(layout_offset_y);
                }

                // 处理暗淡、反显与选中文本
                if indexed
                    .cell
                    .flags
                    .intersects(cell::Flags::DIM | cell::Flags::DIM_BOLD)
                {
                    fg.a *= 0.7;
                }
                if indexed.cell.flags.contains(cell::Flags::INVERSE)
                    || content
                        .selectable_range
                        .is_some_and(|r| r.contains(indexed.point))
                {
                    std::mem::swap(&mut fg, &mut bg);
                }

                // 批量绘制背景：跳过默认背景（容器已绘制）
                if bg != default_bg {
                    if bg_batch_rect.can_extend(bg, x) {
                        // 同色且连续：扩展当前段
                        bg_batch_rect.extend(cell_width);
                    } else {
                        // 新的着色段（或不连续）：若已有则先刷新上一段
                        if bg_batch_rect.can_flush() {
                            frame.fill(&bg_batch_rect.build(line), bg_batch_rect.color);
                        }

                        // 开启新段但暂不绘制，等待可能的延伸
                        bg_batch_rect = BackgroundRect::default()
                            .with_cell_height(cell_height)
                            .with_display_offset(display_offset)
                            .with_layout_offset_y(layout_offset_y)
                            .activate()
                            .with_color(bg)
                            .with_start_x(x)
                            .with_width(cell_width);
                    }
                } else if bg_batch_rect.can_flush() {
                    // 背景回到默认，刷新当前背景矩形并初始化新矩形
                    frame.fill(&bg_batch_rect.build(line), bg_batch_rect.color);

                    bg_batch_rect = BackgroundRect::default()
                        .with_cell_height(cell_height)
                        .with_display_offset(display_offset)
                        .with_layout_offset_y(layout_offset_y);
                }

                // 绘制悬浮超链接下划线（较少见，逐格绘制以保证正确）
                if content.hovered_hyperlink.as_ref().is_some_and(|range| {
                    range.contains(&indexed.point) && range.contains(&state.mouse_position_on_grid)
                }) || indexed.cell.flags.contains(cell::Flags::UNDERLINE)
                {
                    let underline_height = y + cell_size.height;
                    let underline = Path::line(
                        Point::new(x, underline_height),
                        Point::new(x + cell_size.width, underline_height),
                    );
                    frame.stroke(
                        &underline,
                        Stroke::default()
                            .with_width(font_size * 0.15)
                            .with_color(fg),
                    );
                }

                // 处理光标渲染
                if content.grid.cursor.point == indexed.point
                    && content.terminal_mode.contains(TermMode::SHOW_CURSOR)
                {
                    let cursor_color = self.term.theme.get_color(content.cursor.fg);
                    let cursor_rect = Path::rectangle(Point::new(x, y), cell_size);
                    // 聚焦（可接收键盘输入）画实心块；焦点在其它组件时画空心块轮廓。
                    if self.focused {
                        frame.fill(&cursor_rect, cursor_color);
                    } else {
                        frame.stroke(
                            &cursor_rect,
                            Stroke::default()
                                .with_width(font_size * 0.1)
                                .with_color(cursor_color),
                        );
                    }
                }

                // 绘制文本
                if indexed.c != ' ' && indexed.c != '\t' {
                    if content.grid.cursor.point == indexed.point
                        && content.terminal_mode.contains(TermMode::APP_CURSOR)
                    {
                        fg = bg;
                    }
                    // 由格子标志解析字体样式（粗体/斜体）
                    let mut font = self.term.font.font_type;
                    if indexed
                        .cell
                        .flags
                        .intersects(cell::Flags::BOLD | cell::Flags::DIM_BOLD)
                    {
                        font.weight = FontWeight::Bold;
                    }
                    if indexed.cell.flags.contains(cell::Flags::ITALIC) {
                        font.style = FontStyle::Italic;
                    }
                    let text = Text {
                        content: indexed.cell.c.to_string(),
                        position: Point::new(cell_center_x, cell_center_y),
                        font,
                        size: iced_core::Pixels(font_size),
                        color: fg,
                        align_x: Alignment::Center,
                        align_y: Vertical::Center,
                        shaping: Shaping::Advanced,
                        line_height: LineHeight::Relative(font_scale_factor),
                        ..Default::default()
                    };
                    frame.fill_text(text);
                }
            }

            // 结束时刷新剩余的背景段
            if bg_batch_rect.can_flush() {
                frame.fill(
                    &bg_batch_rect.build(last_line.unwrap_or(0)),
                    bg_batch_rect.color,
                );
            }
        });

        use iced::advanced::graphics::geometry::Renderer as _;
        renderer.draw_geometry(geom);
    }

    /// 转发鼠标与键盘事件，收集命令并发布给后端。
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced_core::Event,
        layout: iced_graphics::core::Layout<'_>,
        cursor: Cursor,
        _renderer: &iced::Renderer,
        clipboard: &mut dyn iced_graphics::core::Clipboard,
        shell: &mut iced_graphics::core::Shell<'_, Event>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<TerminalViewState>();
        self.handle_resize(state, layout, shell);

        let is_cursor_in_layout = self.is_cursor_in_layout(cursor, layout);

        let commands = match event {
            iced::Event::Mouse(mouse_event) if is_cursor_in_layout => self.handle_mouse_event(
                state,
                layout.position(),
                cursor.position().unwrap(),
                mouse_event,
            ),
            iced::Event::Keyboard(keyboard_event) => {
                if !self.focused {
                    return;
                }

                self.handle_keyboard_event(state, clipboard, keyboard_event)
                    .into_iter()
                    .collect()
            }
            _ => Vec::new(),
        };

        if !commands.is_empty() {
            shell.capture_event();
        }

        for cmd in commands {
            shell.publish(Event::BackendCall(self.term.id, cmd));
        }
    }

    /// 返回鼠标交互样式：超链接上显示手型，其余文本光标。
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> iced_core::mouse::Interaction {
        let state = tree.state.downcast_ref::<TerminalViewState>();
        let mut cursor_mode = iced_core::mouse::Interaction::Idle;
        let terminal_mode = self.term.backend.renderable_content().terminal_mode;
        if self.is_cursor_in_layout(cursor, layout) && !terminal_mode.contains(TermMode::SGR_MOUSE)
        {
            cursor_mode = iced_core::mouse::Interaction::Text;
        }

        if self.is_cursor_hovered_hyperlink(state) {
            cursor_mode = iced_core::mouse::Interaction::Pointer;
        }

        cursor_mode
    }
}

impl<'a> From<TerminalView<'a>> for Element<'a, Event, Theme, iced::Renderer> {
    /// 将终端视图包装为可加入 iced 元素树的 `Element`。
    fn from(widget: TerminalView<'a>) -> Self {
        Self::new(widget)
    }
}

/// 终端视图的部件内部状态，跨帧保留交互与输入上下文。
#[derive(Debug, Clone)]
struct TerminalViewState {
    /// 当前是否处于鼠标拖拽（选区）进行中。
    is_dragged: bool,
    /// 上一次鼠标点击信息，用于判定单击、双击、三击。
    last_click: Option<mouse::Click>,
    /// 滚轮像素滚动的累积余量，满一行才折算为行滚动。
    scroll_pixels: f32,
    /// 当前键盘修饰键状态（如 Ctrl、Cmd）。
    keyboard_modifiers: Modifiers,
    /// 部件最近一次布局得到的尺寸。
    size: Size<f32>,
    /// 鼠标当前对应的网格坐标点。
    mouse_position_on_grid: TerminalGridPoint,
    /// 上次绘制时记录的焦点状态：焦点变化但布局尺寸不变时，几何缓存不会重跑，故在 `draw` 中据此判断是否需清缓存以重绘光标（实心/空心）。
    last_focus: Cell<bool>,
}

impl TerminalViewState {
    /// 构造全部字段取默认初值的终端视图状态。
    fn new() -> Self {
        Self {
            is_dragged: false,
            last_click: None,
            scroll_pixels: 0.0,
            keyboard_modifiers: Modifiers::empty(),
            size: Size::from([0.0, 0.0]),
            mouse_position_on_grid: TerminalGridPoint::default(),
            last_focus: Cell::new(false),
        }
    }
}

impl Default for TerminalViewState {
    /// 经由 `new` 生成终端视图状态的默认值。
    fn default() -> Self {
        Self::new()
    }
}

/// 用于批量合并并绘制连续同色背景矩形的辅助结构。
#[derive(Default)]
struct BackgroundRect {
    /// 网格显示偏移（滚动历史产生的行偏移）。
    display_offset: f32,
    /// 单个单元格的高度。
    cell_height: f32,
    /// 部件布局在画布中的纵向偏移。
    layout_offset_y: f32,
    /// 本背景矩形是否已激活（开始了一段着色）。
    is_active: bool,
    /// 本段背景矩形使用的颜色。
    color: Color,
    /// 本段背景矩形起始的横向坐标。
    start_x: f32,
    /// 本段背景矩形的宽度（可随连续同色格扩展）。
    width: f32,
}

impl BackgroundRect {
    /// 设置显示偏移并返回自身，用于链式构造。
    fn with_display_offset(mut self, value: f32) -> Self {
        self.display_offset = value;
        self
    }

    /// 设置单元格高度并返回自身。
    fn with_cell_height(mut self, value: f32) -> Self {
        self.cell_height = value;
        self
    }

    /// 设置布局纵向偏移并返回自身。
    fn with_layout_offset_y(mut self, value: f32) -> Self {
        self.layout_offset_y = value;
        self
    }

    /// 设置背景矩形宽度并返回自身。
    fn with_width(mut self, value: f32) -> Self {
        self.width = value;
        self
    }

    /// 设置起始横向坐标并返回自身。
    fn with_start_x(mut self, value: f32) -> Self {
        self.start_x = value;
        self
    }

    /// 设置背景颜色并返回自身。
    fn with_color(mut self, value: Color) -> Self {
        self.color = value;
        self
    }

    /// 标记本背景矩形为已激活状态并返回自身。
    fn activate(mut self) -> Self {
        self.is_active = true;
        self
    }

    /// 依据行号与偏移计算并生成矩形路径。
    fn build(&self, line: i32) -> Path {
        let flush_y =
            self.layout_offset_y + ((line as f32 + self.display_offset) * self.cell_height);
        Path::rectangle(
            Point::new(self.start_x, flush_y),
            Size::new(self.width, self.cell_height),
        )
    }

    /// 判断当前段是否已就绪、可提交绘制。
    fn can_flush(&self) -> bool {
        self.is_active && self.width > 0.0
    }

    /// 判断给定颜色与位置能否续接到当前段。
    fn can_extend(&self, bg: Color, x: f32) -> bool {
        self.is_active && bg == self.color && (self.start_x + self.width - x).abs() < f32::EPSILON
    }

    /// 按给定宽度向右扩展当前背景段。
    fn extend(&mut self, value: f32) {
        self.width += value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod handle_left_button_pressed_tests {
        use super::*;
        use alacritty_terminal::index::{Column, Line};

        #[test]
        fn handles_mouse_mode_with_left_click() {
            let mut state = TerminalViewState::new();
            let terminal_mode = TermMode::MOUSE_MODE;
            let layout_position = Point { x: 5.0, y: 5.0 };
            let cursor_position = Point { x: 100.0, y: 150.0 };
            let mut commands = Vec::new();
            let _modifiers = Modifiers::empty();

            TerminalView::handle_left_button_pressed(
                &mut state,
                &terminal_mode,
                cursor_position,
                layout_position,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(
                commands[0],
                Command::MouseReport(
                    MouseButton::LeftButton,
                    _modifiers,
                    TerminalGridPoint {
                        line: Line(0),
                        column: Column(0),
                    },
                    true,
                )
            ));
            assert!(state.is_dragged);
        }

        #[test]
        fn starts_simple_selection_with_left_click() {
            let terminal_mode = TermMode::SGR_MOUSE;
            let cursor_position = Point { x: 200.0, y: 150.0 };
            let layout_position = Point { x: 50.0, y: 50.0 };

            let cases = vec![
                SelectionType::Simple,
                SelectionType::Semantic,
                SelectionType::Lines,
            ];

            for _selection_type in cases {
                let mut state = TerminalViewState::new();
                state.keyboard_modifiers = Modifiers::SHIFT;
                let mut commands = Vec::new();

                TerminalView::handle_left_button_pressed(
                    &mut state,
                    &terminal_mode,
                    cursor_position,
                    layout_position,
                    &mut commands,
                );

                assert_eq!(commands.len(), 1);
                assert!(matches!(
                    commands[0],
                    Command::SelectStart(_selection_type, (150.0, 100.0))
                ),);
                assert!(state.is_dragged);
            }
        }
    }

    mod handle_cursor_moved_tests {
        use alacritty_terminal::index::{Column, Line};

        use super::*;

        #[test]
        fn updates_mouse_position_on_grid() {
            let mut state = TerminalViewState::new();
            let terminal_content = RenderableContent::default();
            let mut commands = Vec::new();
            let cases = vec![
                (
                    Point { x: 0.0, y: 0.0 },
                    Point { x: 1.0, y: 1.0 },
                    TerminalGridPoint {
                        line: Line(1),
                        column: Column(1),
                    },
                ),
                (
                    Point { x: 0.0, y: 0.0 },
                    Point { x: 2.0, y: 2.0 },
                    TerminalGridPoint {
                        line: Line(2),
                        column: Column(2),
                    },
                ),
                (
                    Point { x: 0.0, y: 0.0 },
                    Point { x: 30.0, y: 2.0 },
                    TerminalGridPoint {
                        line: Line(2),
                        column: Column(30),
                    },
                ),
                (
                    Point { x: 10.0, y: 0.0 },
                    Point { x: 30.0, y: 2.0 },
                    TerminalGridPoint {
                        line: Line(2),
                        column: Column(20),
                    },
                ),
                (
                    Point { x: 10.0, y: 10.0 },
                    Point { x: 30.0, y: 2.0 },
                    TerminalGridPoint {
                        line: Line(0),
                        column: Column(20),
                    },
                ),
            ];

            for (layout_position, cursor_position, expected) in cases {
                TerminalView::handle_cursor_moved(
                    &mut state,
                    &terminal_content,
                    &cursor_position,
                    layout_position,
                    &mut commands,
                );

                assert_eq!(state.mouse_position_on_grid, expected);
            }
        }

        #[test]
        fn generates_drag_update_command_when_dragged() {
            let mut state = TerminalViewState::new();
            state.is_dragged = true; // 模拟进行中的拖拽操作
            let terminal_content = RenderableContent::default();
            let layout_position = Point { x: 5.0, y: 5.0 };
            let cursor_position = Point { x: 100.0, y: 150.0 };
            let mut commands = Vec::new();

            TerminalView::handle_cursor_moved(
                &mut state,
                &terminal_content,
                &cursor_position,
                layout_position,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(commands[0], Command::SelectUpdate((95.0, 145.0))));
        }

        #[test]
        fn generates_drag_update_command_when_dragged_in_mouse_motion_mode() {
            let mut state = TerminalViewState::new();
            state.is_dragged = true; // 模拟进行中的拖拽操作
            let terminal_content = RenderableContent {
                terminal_mode: TermMode::MOUSE_MOTION,
                ..Default::default()
            };
            let layout_position = Point { x: 5.0, y: 5.0 };
            let cursor_position = Point { x: 100.0, y: 150.0 };
            let mut commands = Vec::new();
            let _modifiers = Modifiers::empty();

            TerminalView::handle_cursor_moved(
                &mut state,
                &terminal_content,
                &cursor_position,
                layout_position,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(
                commands[0],
                Command::MouseReport(
                    MouseButton::LeftMove,
                    _modifiers,
                    TerminalGridPoint {
                        line: Line(49),
                        column: Column(79),
                    },
                    true,
                )
            ));
        }

        #[test]
        fn generates_drag_update_command_when_dragged_in_srg_mode_with_key_mods() {
            let mut state = TerminalViewState::new();
            state.keyboard_modifiers = Modifiers::SHIFT;
            state.is_dragged = true; // 模拟进行中的拖拽操作
            let terminal_content = RenderableContent {
                terminal_mode: TermMode::SGR_MOUSE,
                ..Default::default()
            };
            let layout_position = Point { x: 5.0, y: 5.0 };
            let cursor_position = Point { x: 100.0, y: 150.0 };
            let mut commands = Vec::new();

            TerminalView::handle_cursor_moved(
                &mut state,
                &terminal_content,
                &cursor_position,
                layout_position,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(commands[0], Command::SelectUpdate((95.0, 145.0))));
        }

        #[test]
        fn generates_drag_update_and_link_open() {
            let mut state = TerminalViewState::new();
            state.keyboard_modifiers = Modifiers::COMMAND;
            state.is_dragged = true; // 模拟进行中的拖拽操作
            let terminal_content = RenderableContent {
                terminal_mode: TermMode::SGR_MOUSE,
                ..Default::default()
            };
            let layout_position = Point { x: 5.0, y: 5.0 };
            let cursor_position = Point { x: 100.0, y: 150.0 };
            let mut commands = Vec::new();

            TerminalView::handle_cursor_moved(
                &mut state,
                &terminal_content,
                &cursor_position,
                layout_position,
                &mut commands,
            );

            assert_eq!(commands.len(), 2);
            assert!(matches!(commands[0], Command::SelectUpdate((95.0, 145.0))));
            assert!(matches!(
                commands[1],
                Command::ProcessLink(
                    LinkAction::Hover,
                    TerminalGridPoint {
                        line: Line(49),
                        column: Column(79),
                    },
                )
            ));
        }
    }

    mod handle_button_released_tests {
        use super::*;
        use alacritty_terminal::index::{Column, Line};

        #[test]
        fn mouse_mode_activated() {
            let mut state = TerminalViewState::new();
            let terminal_mode = TermMode::MOUSE_MODE;
            let bindings = BindingsLayout::new();
            let mut commands = Vec::new();
            let _modifiers = Modifiers::empty();

            TerminalView::handle_button_released(
                &mut state,
                &terminal_mode,
                &bindings,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(
                commands[0],
                Command::MouseReport(
                    MouseButton::LeftButton,
                    _modifiers,
                    TerminalGridPoint {
                        line: Line(0),
                        column: Column(0)
                    },
                    false
                )
            ));
        }

        #[test]
        fn link_open_on_button_release() {
            let mut state = TerminalViewState::new();
            state.keyboard_modifiers = Modifiers::COMMAND;
            let terminal_mode = TermMode::MOUSE_MODE;
            let bindings = BindingsLayout::new();
            let mut commands = Vec::new();
            let _modifiers = Modifiers::empty();

            TerminalView::handle_button_released(
                &mut state,
                &terminal_mode,
                &bindings,
                &mut commands,
            );

            assert_eq!(commands.len(), 2);
            assert!(matches!(
                commands[0],
                Command::MouseReport(
                    MouseButton::LeftButton,
                    _modifiers,
                    TerminalGridPoint {
                        line: Line(0),
                        column: Column(0)
                    },
                    false
                )
            ));
            assert!(matches!(
                commands[1],
                Command::ProcessLink(
                    LinkAction::Open,
                    TerminalGridPoint {
                        line: Line(0),
                        column: Column(0)
                    }
                ),
            ));
        }

        #[test]
        fn link_open_on_button_release_in_non_mouse_mode() {
            let mut state = TerminalViewState::new();
            state.keyboard_modifiers = Modifiers::COMMAND;
            state.mouse_position_on_grid = TerminalGridPoint {
                line: Line(4),
                column: Column(10),
            };
            let terminal_mode = TermMode::empty(); // 假定 SGR_MOUSE 模式不影响链接打开
            let bindings = BindingsLayout::new();
            let mut commands = Vec::new();

            TerminalView::handle_button_released(
                &mut state,
                &terminal_mode,
                &bindings,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(
                commands[0],
                Command::ProcessLink(
                    LinkAction::Open,
                    TerminalGridPoint {
                        line: Line(4),
                        column: Column(10)
                    }
                ),
            ));
        }
    }

    mod handle_wheel_scrolled_tests {
        use super::*;
        use crate::widget::term::font::TermFont;
        use crate::widget::term::settings::FontSettings;

        #[test]
        fn scroll_wheel_up_by_lines() {
            let mut state = TerminalViewState::new();
            let font = TermFont::new(FontSettings::default());
            let mut commands = Vec::new();

            TerminalView::handle_wheel_scrolled(
                &mut state,
                ScrollDelta::Lines { y: 3.0, x: 0.0 }, // 滚轮上滚 3 行（y 为正 = 回滚历史）
                &font.measure,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(commands[0], Command::Scroll(3)));
        }

        #[test]
        fn scroll_wheel_down_by_lines() {
            let mut state = TerminalViewState::new();
            let font = TermFont::new(FontSettings::default());
            let mut commands = Vec::new();

            TerminalView::handle_wheel_scrolled(
                &mut state,
                ScrollDelta::Lines { y: -2.0, x: 0.0 },
                &font.measure,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(commands[0], Command::Scroll(-2)));
        }

        #[test]
        fn scroll_wheel_up_by_pixels() {
            let mut state = TerminalViewState::new();
            let font = TermFont::new(FontSettings::default());
            let mut commands = Vec::new();

            TerminalView::handle_wheel_scrolled(
                &mut state,
                ScrollDelta::Pixels { y: 45.0, x: 0.0 },
                &font.measure,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(commands[0], Command::Scroll(2)));
            assert_eq!(state.scroll_pixels, 8.600002);
        }

        #[test]
        fn scroll_wheel_down_by_pixels() {
            let mut state = TerminalViewState::new();
            let font = TermFont::new(FontSettings::default());
            let mut commands = Vec::new();

            TerminalView::handle_wheel_scrolled(
                &mut state,
                ScrollDelta::Pixels { y: -60.0, x: 0.0 },
                &font.measure,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(commands[0], Command::Scroll(-3)));
            assert_eq!(state.scroll_pixels, -5.4000034);
        }
    }
}
