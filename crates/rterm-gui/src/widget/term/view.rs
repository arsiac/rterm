use crate::widget::term::backend::{Backend, Command, LinkAction, MouseButton, RenderableContent};
use crate::widget::term::bindings::{BindingAction, BindingsLayout, InputKind};
use crate::widget::term::terminal::{Event, Terminal};
use alacritty_terminal::index::{Column, Line, Point as TerminalGridPoint};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::{TermMode, cell};
use alacritty_terminal::vte::ansi::{self as ansi, CursorShape, NamedColor};
use iced::alignment::Vertical;
use iced::font::{Style as FontStyle, Weight as FontWeight};
use iced::mouse::{Cursor, ScrollDelta};
use iced::widget::canvas::{Path, Text};
use iced::{Color, Element, Length, Point, Rectangle, Size, Theme};
use iced_core::clipboard::Kind as ClipboardKind;
use iced_core::input_method::{self, InputMethod, Purpose};
use iced_core::keyboard::{Key, Modifiers, key::Named};
use iced_core::mouse::{self, Click};
use iced_core::text::{Alignment, LineHeight, Shaping};
use iced_core::widget::operation;
use iced_graphics::core::Widget;
use iced_graphics::core::widget::{Tree, tree};
use iced_graphics::geometry::Stroke;
use std::cell::Cell;

/// 滚动条滑块宽度（像素）：叠加在终端内容最右侧一列之上。
const SCROLLBAR_WIDTH: f32 = 6.0;
/// 滚动条轨道相对内容上下边缘的留白（像素）。
const SCROLLBAR_INSET: f32 = 2.0;
/// 滚动条滑块最小高度（像素）：历史极长时仍可抓取。
const SCROLLBAR_MIN_THUMB: f32 = 24.0;
/// 滚动条滑块圆角半径（像素）。
const SCROLLBAR_RADIUS: f32 = 3.0;
/// 滚动条滑块透明度：常态 / 悬停 / 拖动。
const SCROLLBAR_ALPHA: f32 = 0.28;
const SCROLLBAR_ALPHA_HOVER: f32 = 0.45;
const SCROLLBAR_ALPHA_DRAG: f32 = 0.60;

/// 终端画布部件：实现 iced `Widget`，把后端渲染内容绘成像素并转发鼠标 / 键盘事件。
pub struct TerminalView<'a> {
    /// 被渲染的终端实例，提供后端渲染内容、主题与字体等。
    term: &'a Terminal,
    /// 来自 app 的「终端是否持有键盘焦点」，用于绘制实心/空心光标与门控输入。
    focused: bool,
    /// 终端内容与外层容器边框之间的内边距（像素）。
    /// 由 widget 自行在绘制与事件坐标中偏移，外层容器不再设 padding。
    padding: f32,
    /// 是否显示并接管右侧滚动条。
    scrollbar: bool,
}

impl<'a> TerminalView<'a> {
    /// 以给定终端与焦点态构造可装箱的终端视图元素。
    pub fn show(term: &'a Terminal, focused: bool) -> Self {
        Self {
            term,
            focused,
            padding: 4.0,
            scrollbar: true,
        }
    }

    /// 设置终端内容的内边距（像素）。
    pub fn padding(mut self, value: f32) -> Self {
        self.padding = value;
        self
    }

    /// 设置是否显示右侧滚动条。
    pub fn scrollbar(mut self, enabled: bool) -> Self {
        self.scrollbar = enabled;
        self
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
            let content_size = Size::new(
                layout_size.width - self.padding * 2.0,
                layout_size.height - self.padding * 2.0,
            );
            let cmd = Command::Resize(Some(content_size), Some(self.term.font.measure));
            shell.publish(Event::BackendCall(self.term.id, cmd));
        }
    }

    /// 分发鼠标事件为后端命令（鼠标报告、选区、滚轮滚动、中键粘贴等）。
    fn handle_mouse_event(
        &self,
        state: &mut TerminalViewState,
        layout_position: Point,
        cursor_position: Point,
        event: &iced::mouse::Event,
        clipboard: &dyn iced_graphics::core::Clipboard,
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
                    self.padding,
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
                    self.padding,
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
            iced_core::mouse::Event::ButtonPressed(iced_core::mouse::Button::Middle) => {
                if !self.focused {
                    return Vec::default();
                }

                Self::handle_middle_button_pressed(state, &terminal_mode, clipboard, &mut commands);
            }
            iced_core::mouse::Event::ButtonReleased(iced_core::mouse::Button::Middle) => {
                if !self.focused {
                    return Vec::default();
                }

                Self::handle_middle_button_released(state, &terminal_mode, &mut commands);
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
        padding: f32,
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
                    cursor_position.x - layout_position.x - padding,
                    cursor_position.y - layout_position.y - padding,
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
        padding: f32,
        commands: &mut Vec<Command>,
    ) {
        let cursor_x = position.x - layout_position.x - padding;
        let cursor_y = position.y - layout_position.y - padding;
        state.mouse_position_on_grid = Backend::selection_point(
            cursor_x,
            cursor_y,
            &terminal_content.terminal_size,
            terminal_content.display_offset,
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

    /// 处理鼠标中键按下：鼠标模式下作为 button 2 上报给远端应用，否则粘贴
    /// （PRIMARY 优先，Linux 惯例；见 `read_middle_paste`）。
    fn handle_middle_button_pressed(
        state: &TerminalViewState,
        terminal_mode: &TermMode,
        clipboard: &dyn iced_graphics::core::Clipboard,
        commands: &mut Vec<Command>,
    ) {
        if terminal_mode.intersects(TermMode::MOUSE_MODE) {
            commands.push(Command::MouseReport(
                MouseButton::MiddleButton,
                state.keyboard_modifiers,
                state.mouse_position_on_grid,
                true,
            ));
        } else if let Some(data) = read_middle_paste(clipboard) {
            let bracketed = terminal_mode.contains(TermMode::BRACKETED_PASTE);
            commands.push(Command::Write(paste_bytes(&data, bracketed)));
        }
    }

    /// 处理鼠标中键释放：仅鼠标模式下把释放（button 2）上报给远端应用。
    fn handle_middle_button_released(
        state: &TerminalViewState,
        terminal_mode: &TermMode,
        commands: &mut Vec<Command>,
    ) {
        if terminal_mode.intersects(TermMode::MOUSE_MODE) {
            commands.push(Command::MouseReport(
                MouseButton::MiddleButton,
                state.keyboard_modifiers,
                state.mouse_position_on_grid,
                false,
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

    /// 处理滚动条条带内的鼠标事件；返回待发命令，未命中且未拖拽时返回 `None` 交回选区 / 鼠标上报。
    ///
    /// 拖动期间即使光标移出条带也持续接管，直到左键释放。
    fn handle_scrollbar_event(
        &self,
        state: &mut TerminalViewState,
        layout: iced_graphics::core::Layout<'_>,
        cursor: Cursor,
        event: &iced::mouse::Event,
        shell: &mut iced_graphics::core::Shell<'_, Event>,
    ) -> Option<Vec<Command>> {
        // 指针在窗外释放时 `cursor.position()` 不可得会提前返回，须先于位置判断解锁存，免残留成幻影拖拽。
        if scrollbar_releases_drag(event, state.scrollbar_drag.is_some()) {
            state.scrollbar_drag = None;
            return Some(Vec::new());
        }

        let content = self.term.backend.renderable_content();
        let geometry = if self.scrollbar {
            scrollbar_geometry(layout.bounds(), self.padding, content)
        } else {
            None
        };
        let position = cursor.position()?;
        let over_track = geometry
            .as_ref()
            .is_some_and(|(track, _)| track.contains(position));

        // 悬停高亮：几何被 `Cache` 缓存，跨边界时须清缓存并请求重绘，否则透明度变化不重绘。
        if state.scrollbar_hovered != over_track {
            state.scrollbar_hovered = over_track;
            self.term.cache.clear();
            shell.request_redraw();
        }

        let Some((track, thumb)) = geometry else {
            // 条带消失（关闭开关 / 备用屏 / 无历史）时收尾拖拽态，避免状态卡住。
            state.scrollbar_drag = None;
            return None;
        };

        match event {
            iced_core::mouse::Event::ButtonPressed(iced_core::mouse::Button::Left) => {
                if !track.contains(position) {
                    return None;
                }
                if thumb.contains(position) {
                    // 记下抓取点相对滑块顶部的偏移（拖动才跟手），并以当前偏移播种增量基准。
                    state.scrollbar_drag = Some(position.y - thumb.y);
                    state.scrollbar_target = content.display_offset;
                    Some(Vec::new())
                } else {
                    // 点击空轨翻一页：页大小取当前视口行数，方向由点击落在滑块的哪一侧决定。
                    let rows = (content.cells.len() / content.columns.max(1)) as i32;
                    let delta = if position.y < thumb.y { rows } else { -rows };
                    Some(vec![Command::Scroll(delta)])
                }
            }
            iced_core::mouse::Event::CursorMoved { .. } => {
                let grab = state.scrollbar_drag?;
                let target = scrollbar_drag_target(
                    track,
                    thumb.height,
                    content.history_size,
                    position.y,
                    grab,
                );
                let (delta, tracked) = scrollbar_drag_step(target, state.scrollbar_target);
                state.scrollbar_target = tracked;
                // 偏移未变时不发空命令（`Scroll(0)` 只会白跑一趟后端）。
                Some(if delta == 0 {
                    Vec::new()
                } else {
                    vec![Command::Scroll(delta)]
                })
            }
            _ => None,
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
                    let bracketed = self
                        .term
                        .backend
                        .renderable_content()
                        .terminal_mode
                        .contains(TermMode::BRACKETED_PASTE);
                    return Some(Command::Write(paste_bytes(&data, bracketed)));
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

    /// 处理输入法事件：提交文本写入 PTY，预编辑串暂存待绘制。
    fn handle_input_method_event(
        state: &mut TerminalViewState,
        event: &input_method::Event,
    ) -> Option<Command> {
        match event {
            // 上屏：候选文本写入 PTY，本次组合随之结束。
            input_method::Event::Commit(text) => {
                state.preedit = None;
                if text.is_empty() {
                    return None;
                }

                Some(Command::Write(text.as_bytes().to_vec()))
            }
            // 组合中的文本（如拼音串）：空串表示组合被清空，否则暂存待逐帧绘制。
            input_method::Event::Preedit(content, _selection) => {
                state.preedit = (!content.is_empty()).then(|| content.clone());
                None
            }
            // 输入法开启 / 关闭不携带文本，按组合结束处理。
            input_method::Event::Opened | input_method::Event::Closed => {
                state.preedit = None;
                None
            }
        }
    }

    /// 计算输入法候选框锚定的字符格矩形：由终端光标格换算到窗口坐标并裁剪进内容区。
    fn caret_rect(bounds: Rectangle, padding: f32, content: &RenderableContent) -> Rectangle {
        let cell_size = Size::new(
            content.terminal_size.cell_width as f32,
            content.terminal_size.cell_height as f32,
        );
        let cursor = content.cursor_point;
        let display_offset = content.display_offset as f32;
        let x = bounds.x + padding + cursor.column.0 as f32 * cell_size.width;
        let y = bounds.y + padding + ((cursor.line.0 as f32) + display_offset) * cell_size.height;

        // 光标滚出可视区（回滚历史）时把锚点收回内容区，避免候选框落到窗口外。
        let inner = bounds.shrink(padding);
        let max_x = (inner.x + inner.width - cell_size.width).max(inner.x);
        let max_y = (inner.y + inner.height - cell_size.height).max(inner.y);

        Rectangle::new(
            Point::new(x.clamp(inner.x, max_x), y.clamp(inner.y, max_y)),
            cell_size,
        )
    }
}

/// 按 bracketed-paste 模式包裹粘贴内容：远端应用发过 `\e[?2004h` 时用
/// `\e[200~ … \e[201~` 包裹，使其识别为「粘贴」而非逐字符输入（避免 vim 等自动缩进叠加）。
fn paste_bytes(data: &str, bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return data.as_bytes().to_vec();
    }
    let bytes = data.as_bytes();
    let mut wrapped = Vec::with_capacity(bytes.len() + 12);
    wrapped.extend_from_slice(b"\x1b[200~");
    wrapped.extend_from_slice(bytes);
    wrapped.extend_from_slice(b"\x1b[201~");
    wrapped
}

/// 中键粘贴的数据源：PRIMARY 优先（Linux 惯例），空或不可用（Windows / macOS 无 PRIMARY）
/// 时回退标准剪贴板。
fn read_middle_paste(clipboard: &dyn iced_graphics::core::Clipboard) -> Option<String> {
    clipboard
        .read(ClipboardKind::Primary)
        .filter(|data| !data.is_empty())
        .or_else(|| clipboard.read(ClipboardKind::Standard))
        .filter(|data| !data.is_empty())
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
        let layout_size = layout.bounds().size();

        // 焦点 / 滚动条开关变化但布局尺寸不变时，几何缓存直接复用旧绘制结果，导致光标
        // （实心/空心）与滚动条不随状态切换刷新。此处检测变化并清缓存，强制本帧重绘。
        if self.focused != state.last_focus.get() || self.scrollbar != state.last_scrollbar.get() {
            self.term.cache.clear();
            state.last_focus.set(self.focused);
            state.last_scrollbar.set(self.scrollbar);
        }

        let geom = self.term.cache.draw(renderer, viewport.size(), |frame| {
            // 裁剪矩形：四边内缩 padding，使右侧和底部内容不溢出到容器边缘。
            let clip_rect = Rectangle::new(
                Point::new(
                    layout_offset_x + self.padding,
                    layout_offset_y + self.padding,
                ),
                Size::new(
                    layout_size.width - self.padding * 2.0,
                    layout_size.height - self.padding * 2.0,
                ),
            );

            frame.with_clip(clip_rect, |frame| {
                // 预计算内循环使用的常量
                let display_offset = content.display_offset as f32;
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
                // 视口快照首行对应的网格行号；第 i 行即网格行 `first_line + i`。
                let first_line = -(display_offset as i32);
                let columns = content.columns.max(1);

                for (index, cell) in content.cells.iter().enumerate() {
                    // 低成本计算每格几何信息
                    let line = first_line + (index / columns) as i32;
                    let col = (index % columns) as f32;
                    let point = TerminalGridPoint::new(Line(line), Column(index % columns));

                    // 解析该格的位置点（含内边距偏移）
                    let x = layout_offset_x + self.padding + (col * cell_width);
                    let y = layout_offset_y
                        + self.padding
                        + (((line as f32) + display_offset) * cell_height);
                    let cell_center_y = y + half_h;
                    let cell_center_x = x + half_w;

                    // 解析该格的颜色
                    let mut fg = self.term.theme.get_color(cell.fg);
                    let mut bg = self.term.theme.get_color(cell.bg);

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
                            .with_layout_offset_y(layout_offset_y)
                            .with_padding(self.padding);
                    }

                    // 处理暗淡、反显与选中文本
                    if cell
                        .flags
                        .intersects(cell::Flags::DIM | cell::Flags::DIM_BOLD)
                    {
                        fg.a *= 0.7;
                    }
                    if cell.flags.contains(cell::Flags::INVERSE)
                        || content.selectable_range.is_some_and(|r| r.contains(point))
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
                                .with_padding(self.padding)
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
                            .with_layout_offset_y(layout_offset_y)
                            .with_padding(self.padding);
                    }

                    // 绘制悬浮超链接下划线（较少见，逐格绘制以保证正确）
                    if content.hovered_hyperlink.as_ref().is_some_and(|range| {
                        range.contains(&point) && range.contains(&state.mouse_position_on_grid)
                    }) || cell.flags.contains(cell::Flags::UNDERLINE)
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
                    let cursor_on_cell = content.cursor_point == point
                        && content.terminal_mode.contains(TermMode::SHOW_CURSOR);
                    // 实心块会铺满整格，此时字形须改用单元格背景色，否则与块同色不可见。
                    let mut cursor_inverts_text = false;

                    if cursor_on_cell && content.cursor_shape != CursorShape::Hidden {
                        let cursor_color = self.term.theme.get_color(content.cursor.fg);
                        let block_width = if cell.flags.contains(cell::Flags::WIDE_CHAR) {
                            cell_width * 2.0
                        } else {
                            cell_width
                        };
                        let cursor_rect =
                            Path::rectangle(Point::new(x, y), Size::new(block_width, cell_height));
                        let outline = Stroke::default()
                            .with_width(font_size * 0.1)
                            .with_color(cursor_color);
                        let bar_width = font_size * 0.15;

                        // 焦点在其它组件时一律画空心框，只提示光标位置。
                        if !self.focused {
                            frame.stroke(&cursor_rect, outline);
                        } else {
                            match content.cursor_shape {
                                CursorShape::Block => {
                                    frame.fill(&cursor_rect, cursor_color);
                                    cursor_inverts_text = true;
                                }
                                CursorShape::HollowBlock => {
                                    frame.stroke(&cursor_rect, outline);
                                }
                                CursorShape::Beam => {
                                    let beam = Path::rectangle(
                                        Point::new(x, y),
                                        Size::new(bar_width, cell_height),
                                    );
                                    frame.fill(&beam, cursor_color);
                                }
                                CursorShape::Underline => {
                                    let underline = Path::rectangle(
                                        Point::new(x, y + cell_height - bar_width),
                                        Size::new(block_width, bar_width),
                                    );
                                    frame.fill(&underline, cursor_color);
                                }
                                CursorShape::Hidden => {}
                            }
                        }
                    }

                    // 绘制文本
                    if cell.c != ' ' && cell.c != '\t' {
                        if cursor_inverts_text {
                            fg = bg;
                        }
                        // 由格子标志解析字体样式（粗体/斜体）
                        let mut font = self.term.font.font_type;
                        if cell
                            .flags
                            .intersects(cell::Flags::BOLD | cell::Flags::DIM_BOLD)
                        {
                            font.weight = FontWeight::Bold;
                        }
                        if cell.flags.contains(cell::Flags::ITALIC) {
                            font.style = FontStyle::Italic;
                        }
                        let text = Text {
                            content: cell.c.to_string(),
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

                // 输入法预编辑串叠加在网格之上：先用终端底色盖住光标格（否则实心光标块会
                // 吃掉首字），再按终端字体左对齐、垂直居中绘制，字形间距由字体自行推进。
                if let Some(preedit) = state.preedit.as_ref() {
                    let cursor = content.cursor_point;
                    let cell_x =
                        layout_offset_x + self.padding + cursor.column.0 as f32 * cell_width;
                    let cell_y = layout_offset_y
                        + self.padding
                        + ((cursor.line.0 as f32) + display_offset) * cell_height;

                    frame.fill(
                        &Path::rectangle(
                            Point::new(cell_x, cell_y),
                            Size::new(cell_width, cell_height),
                        ),
                        default_bg,
                    );
                    frame.fill_text(Text {
                        content: preedit.clone(),
                        position: Point::new(cell_x, cell_y + cell_height * 0.5),
                        font: self.term.font.font_type,
                        size: iced_core::Pixels(font_size),
                        color: self
                            .term
                            .theme
                            .get_color(ansi::Color::Named(NamedColor::Foreground)),
                        align_x: Alignment::Left,
                        align_y: Vertical::Center,
                        shaping: Shaping::Advanced,
                        line_height: LineHeight::Relative(font_scale_factor),
                        ..Default::default()
                    });
                }
            }); // with_clip

            // 滚动条叠加在内容最右侧，画在 `with_clip` 外故不被裁剪；配色取终端前景色以兼顾深浅底色。
            if self.scrollbar
                && let Some((_track, thumb)) =
                    scrollbar_geometry(layout.bounds(), self.padding, content)
            {
                let base = self
                    .term
                    .theme
                    .get_color(ansi::Color::Named(NamedColor::Foreground));
                let alpha = if state.scrollbar_drag.is_some() {
                    SCROLLBAR_ALPHA_DRAG
                } else if state.scrollbar_hovered {
                    SCROLLBAR_ALPHA_HOVER
                } else {
                    SCROLLBAR_ALPHA
                };
                frame.fill(
                    &Path::rounded_rectangle(
                        thumb.position(),
                        thumb.size(),
                        SCROLLBAR_RADIUS.into(),
                    ),
                    Color { a: alpha, ..base },
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

        // 指针离窗或窗口失焦后左键释放不再送达本部件；在此收起交互锁存，
        // 否则重新进入终端区域时滑过会被误当成仍在拖拽（滑块幻影跟随、选区幻影延伸）。
        if matches!(
            event,
            iced_core::Event::Mouse(iced_core::mouse::Event::CursorLeft)
                | iced_core::Event::Window(iced::window::Event::Unfocused)
        ) && state.end_pointer_interactions()
        {
            self.term.cache.clear();
            shell.request_redraw();
        }

        // 输入法策略逐帧续期：iced 每次事件分发都以 `InputMethod::Disabled` 起算、合并全树部件
        // 的申请，且只在重绘路径把结果落到窗口（交互路径会丢弃），故申请必须挂在重绘事件上。
        // 未聚焦则不再申请，窗口 IME 随之关闭，同时清掉残留的预编辑串。
        if matches!(
            event,
            iced_core::Event::Window(iced::window::Event::RedrawRequested(_))
        ) {
            if self.focused {
                // 预编辑由本部件就地绘制（见 `draw`），无需 iced 的 over-the-spot 覆盖层。
                let input_method: InputMethod<&str> = InputMethod::Enabled {
                    cursor: Self::caret_rect(
                        layout.bounds(),
                        self.padding,
                        self.term.backend.renderable_content(),
                    ),
                    purpose: Purpose::Terminal,
                    preedit: None,
                };
                shell.request_input_method(&input_method);
            } else if state.preedit.take().is_some() {
                // 未聚焦：不再申请，窗口 IME 随之关闭；残留的预编辑串一并清掉。
                self.term.cache.clear();
            }
        }

        let is_cursor_in_layout = self.is_cursor_in_layout(cursor, layout);

        // 滚动条优先于选区 / 鼠标上报，且须早于下方 `!self.focused` 门控：否则未聚焦时首次按下
        // 会被转成 `FocusRequest` 而拖动失效。拖动中光标移出部件也继续接管，保证在别处释放也能收尾。
        let mut consumed = false;
        let mut scrollbar_commands = Vec::new();
        if let iced::Event::Mouse(mouse_event) = event
            && (is_cursor_in_layout || state.scrollbar_drag.is_some())
            && let Some(commands) =
                self.handle_scrollbar_event(state, layout, cursor, mouse_event, shell)
        {
            consumed = true;
            scrollbar_commands = commands;
        }

        let commands = if consumed {
            scrollbar_commands
        } else {
            match event {
                iced::Event::Mouse(mouse_event) if is_cursor_in_layout => {
                    if !self.focused {
                        // 终端未持键盘焦点时，在终端区域按下鼠标即请求交还焦点；否则点击会被完全忽略，
                        // 失去焦点后只能切标签页才能找回。
                        if matches!(
                            mouse_event,
                            iced_core::mouse::Event::ButtonPressed(iced_core::mouse::Button::Left)
                                | iced_core::mouse::Event::ButtonPressed(
                                    iced_core::mouse::Button::Right
                                )
                                | iced_core::mouse::Event::ButtonPressed(
                                    iced_core::mouse::Button::Middle
                                )
                        ) {
                            shell.publish(Event::FocusRequest(self.term.id));
                            shell.capture_event();
                        }
                        Vec::new()
                    } else {
                        let was_dragged = state.is_dragged;
                        let commands = self.handle_mouse_event(
                            state,
                            layout.position(),
                            cursor.position().unwrap(),
                            mouse_event,
                            clipboard,
                        );

                        // 左键拖选结束即把选区写入 PRIMARY（Linux 中键粘贴惯例）；鼠标模式下的拖拽是上报而非选区，不写。
                        if was_dragged
                            && matches!(
                                mouse_event,
                                iced_core::mouse::Event::ButtonReleased(
                                    iced_core::mouse::Button::Left
                                )
                            )
                            && !self
                                .term
                                .backend
                                .renderable_content()
                                .terminal_mode
                                .intersects(TermMode::MOUSE_MODE)
                        {
                            let selection = self.term.backend.selectable_content();
                            if !selection.is_empty() {
                                clipboard.write(ClipboardKind::Primary, selection);
                            }
                        }

                        commands
                    }
                }
                iced::Event::Keyboard(keyboard_event) => {
                    if !self.focused {
                        return;
                    }

                    self.handle_keyboard_event(state, clipboard, keyboard_event)
                        .into_iter()
                        .collect()
                }
                iced::Event::InputMethod(input_method_event) => {
                    if !self.focused {
                        return;
                    }

                    let previous = state.preedit.clone();
                    let command = Self::handle_input_method_event(state, input_method_event);
                    if state.preedit != previous {
                        // 预编辑串参与几何缓存，内容变化须清缓存并在本帧重绘。
                        self.term.cache.clear();
                        shell.request_redraw();
                    }
                    command.into_iter().collect()
                }
                _ => Vec::new(),
            }
        };

        // 滚动条按下 / 拖动可能不产生命令（原地按下），但事件已被接管，仍须捕获以免下层重复处理左键。
        if consumed || !commands.is_empty() {
            shell.capture_event();
        }

        for cmd in commands {
            shell.publish(Event::BackendCall(self.term.id, cmd));
        }
    }

    /// 返回鼠标交互样式：滚动条上竖直调整，超链接上显示手型，其余文本光标。
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

        // 滚动条条带优先于文本 / 手型：拖动中或悬停在轨道上都用竖直调整光标。
        if let Some(position) = cursor.position() {
            let over_track = self.scrollbar
                && scrollbar_geometry(
                    layout.bounds(),
                    self.padding,
                    self.term.backend.renderable_content(),
                )
                .is_some_and(|(track, _)| track.contains(position));
            if state.scrollbar_drag.is_some() || over_track {
                cursor_mode = iced_core::mouse::Interaction::ResizingVertically;
            }
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
    /// 上次绘制时记录的滚动条开关：同理，开关切换需清缓存才能让滚动条消失 / 出现。
    last_scrollbar: Cell<bool>,
    /// 输入法预编辑串（组合中、尚未上屏的文本），由本部件绘制在光标格处。
    preedit: Option<String>,
    /// 滚动条拖拽态：`Some` 为抓取点相对滑块顶部的偏移，`None` 表示未拖拽。
    scrollbar_drag: Option<f32>,
    /// 拖拽中上一次已下达的回滚偏移，作为相对增量的基准。
    ///
    /// 后端 `display_offset` 晚一轮才更新，同批事件读到同一旧值；拿它当基准会让增量叠加过冲。
    scrollbar_target: usize,
    /// 指针是否悬停在滚动条上（仅用于加深透明度）。
    scrollbar_hovered: bool,
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
            last_scrollbar: Cell::new(true),
            preedit: None,
            scrollbar_drag: None,
            scrollbar_target: 0,
            scrollbar_hovered: false,
        }
    }

    /// 收起指针离窗 / 窗口失焦后不再有释放事件送达的交互锁存；返回是否需要重绘。
    fn end_pointer_interactions(&mut self) -> bool {
        // 两个 take 都必须求值，不能并入 `||` 链——否则前项为真时短路，后续锁存清不掉。
        let hovered = std::mem::take(&mut self.scrollbar_hovered);
        let dragging = self.scrollbar_drag.take().is_some();
        let selecting = std::mem::take(&mut self.is_dragged);
        hovered || dragging || selecting
    }
}

impl Default for TerminalViewState {
    /// 经由 `new` 生成终端视图状态的默认值。
    fn default() -> Self {
        Self::new()
    }
}

/// 滚动条几何（纯函数）：返回 `(轨道矩形, 滑块矩形)`，无历史 / 视口为空 / 备用屏下返回 `None`。
///
/// 备用屏判据只取 [`TermMode::ALT_SCREEN`]：[`TermMode::ALTERNATE_SCROLL`] 是默认就置位的修饰位。
fn scrollbar_geometry(
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
fn scrollbar_drag_target(
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
fn scrollbar_drag_step(target: usize, tracked: usize) -> (i32, usize) {
    (target as i32 - tracked as i32, target)
}

/// 该事件是否应解除滚动条拖拽锁存（纯函数）：左键释放一律解除，未拖拽时不解除。
///
/// 不看光标是否可得——指针在窗外释放时 iced 报告光标不可得，走不到常规释放分支而残留幻影拖拽。
fn scrollbar_releases_drag(event: &iced::mouse::Event, dragging: bool) -> bool {
    dragging
        && matches!(
            event,
            iced_core::mouse::Event::ButtonReleased(iced_core::mouse::Button::Left)
        )
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
    /// 终端内容的内边距（像素）。
    padding: f32,
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

    /// 设置内边距并返回自身。
    fn with_padding(mut self, value: f32) -> Self {
        self.padding = value;
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
        let flush_y = self.layout_offset_y
            + self.padding
            + ((line as f32 + self.display_offset) * self.cell_height);
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

    const TEST_PADDING: f32 = 4.0;

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
                TEST_PADDING,
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
                    TEST_PADDING,
                    &mut commands,
                );

                assert_eq!(commands.len(), 1);
                assert!(matches!(
                    commands[0],
                    Command::SelectStart(_selection_type, (146.0, 96.0))
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
                        line: Line(0),
                        column: Column(0),
                    },
                ),
                (
                    Point { x: 0.0, y: 0.0 },
                    Point { x: 2.0, y: 2.0 },
                    TerminalGridPoint {
                        line: Line(0),
                        column: Column(0),
                    },
                ),
                (
                    Point { x: 0.0, y: 0.0 },
                    Point { x: 30.0, y: 2.0 },
                    TerminalGridPoint {
                        line: Line(0),
                        column: Column(26),
                    },
                ),
                (
                    Point { x: 10.0, y: 0.0 },
                    Point { x: 30.0, y: 2.0 },
                    TerminalGridPoint {
                        line: Line(0),
                        column: Column(16),
                    },
                ),
                (
                    Point { x: 10.0, y: 10.0 },
                    Point { x: 30.0, y: 2.0 },
                    TerminalGridPoint {
                        line: Line(0),
                        column: Column(16),
                    },
                ),
            ];

            for (layout_position, cursor_position, expected) in cases {
                TerminalView::handle_cursor_moved(
                    &mut state,
                    &terminal_content,
                    &cursor_position,
                    layout_position,
                    TEST_PADDING,
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
                TEST_PADDING,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(commands[0], Command::SelectUpdate((91.0, 141.0))));
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
                TEST_PADDING,
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
                TEST_PADDING,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(commands[0], Command::SelectUpdate((91.0, 141.0))));
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
                TEST_PADDING,
                &mut commands,
            );

            assert_eq!(commands.len(), 2);
            assert!(matches!(commands[0], Command::SelectUpdate((91.0, 141.0))));
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

    mod middle_paste_tests {
        use super::*;
        use alacritty_terminal::index::{Column, Line};

        /// 剪贴板桩：分别预置 PRIMARY / 标准剪贴板的内容；写入忽略（本组用例只验证读取与决策）。
        #[derive(Default)]
        struct StubClipboard {
            primary: Option<String>,
            standard: Option<String>,
        }

        impl iced_graphics::core::Clipboard for StubClipboard {
            fn read(&self, kind: ClipboardKind) -> Option<String> {
                match kind {
                    ClipboardKind::Primary => self.primary.clone(),
                    ClipboardKind::Standard => self.standard.clone(),
                }
            }

            fn write(&mut self, _kind: ClipboardKind, _contents: String) {}
        }

        #[test]
        fn paste_bytes_keeps_data_without_bracketed_paste() {
            assert_eq!(paste_bytes("ls\n", false), b"ls\n".to_vec());
        }

        #[test]
        fn paste_bytes_wraps_data_in_bracketed_paste() {
            assert_eq!(
                paste_bytes("ls\n", true),
                b"\x1b[200~ls\n\x1b[201~".to_vec()
            );
        }

        #[test]
        fn middle_paste_prefers_primary_over_standard() {
            let clipboard = StubClipboard {
                primary: Some("primary".into()),
                standard: Some("standard".into()),
            };

            assert_eq!(read_middle_paste(&clipboard).as_deref(), Some("primary"));
        }

        #[test]
        fn middle_paste_falls_back_to_standard_when_primary_empty_or_unavailable() {
            let empty_primary = StubClipboard {
                primary: Some(String::new()),
                standard: Some("standard".into()),
            };
            assert_eq!(
                read_middle_paste(&empty_primary).as_deref(),
                Some("standard")
            );

            let missing_primary = StubClipboard {
                primary: None,
                standard: Some("standard".into()),
            };
            assert_eq!(
                read_middle_paste(&missing_primary).as_deref(),
                Some("standard")
            );
        }

        #[test]
        fn middle_paste_yields_nothing_when_both_empty() {
            let empty_both = StubClipboard {
                primary: Some(String::new()),
                standard: Some(String::new()),
            };
            assert!(read_middle_paste(&empty_both).is_none());

            let missing_both = StubClipboard::default();
            assert!(read_middle_paste(&missing_both).is_none());
        }

        #[test]
        fn middle_press_reports_in_mouse_mode() {
            let state = TerminalViewState::new();
            let clipboard = StubClipboard {
                primary: Some("ignored".into()),
                standard: None,
            };
            let mut commands = Vec::new();
            let _modifiers = Modifiers::empty();

            TerminalView::handle_middle_button_pressed(
                &state,
                &TermMode::MOUSE_MODE,
                &clipboard,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(
                commands[0],
                Command::MouseReport(
                    MouseButton::MiddleButton,
                    _modifiers,
                    TerminalGridPoint {
                        line: Line(0),
                        column: Column(0),
                    },
                    true,
                )
            ));
        }

        #[test]
        fn middle_press_pastes_outside_mouse_mode() {
            let state = TerminalViewState::new();
            let clipboard = StubClipboard {
                primary: Some("primary".into()),
                standard: None,
            };
            let mut commands = Vec::new();

            TerminalView::handle_middle_button_pressed(
                &state,
                &TermMode::empty(),
                &clipboard,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(
                &commands[0],
                Command::Write(bytes) if bytes == b"primary"
            ));
        }

        #[test]
        fn middle_press_wraps_paste_when_bracketed() {
            let state = TerminalViewState::new();
            let clipboard = StubClipboard {
                primary: Some("primary".into()),
                standard: None,
            };
            let mut commands = Vec::new();

            TerminalView::handle_middle_button_pressed(
                &state,
                &TermMode::BRACKETED_PASTE,
                &clipboard,
                &mut commands,
            );

            assert_eq!(commands.len(), 1);
            assert!(matches!(
                &commands[0],
                Command::Write(bytes) if bytes == b"\x1b[200~primary\x1b[201~"
            ));
        }

        #[test]
        fn middle_press_with_empty_clipboard_writes_nothing() {
            let state = TerminalViewState::new();
            let mut commands = Vec::new();

            TerminalView::handle_middle_button_pressed(
                &state,
                &TermMode::empty(),
                &StubClipboard::default(),
                &mut commands,
            );

            assert!(commands.is_empty());
        }

        #[test]
        fn middle_release_reports_only_in_mouse_mode() {
            let state = TerminalViewState::new();

            let mut commands = Vec::new();
            TerminalView::handle_middle_button_released(
                &state,
                &TermMode::MOUSE_MODE,
                &mut commands,
            );
            assert_eq!(commands.len(), 1);
            assert!(matches!(
                commands[0],
                Command::MouseReport(MouseButton::MiddleButton, _, _, false)
            ));

            let mut commands = Vec::new();
            TerminalView::handle_middle_button_released(&state, &TermMode::empty(), &mut commands);
            assert!(commands.is_empty());
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

    mod handle_input_method_event_tests {
        use super::*;

        #[test]
        fn commit_writes_utf8_and_ends_composition() {
            let mut state = TerminalViewState::new();
            state.preedit = Some("nihao".to_owned());

            let command = TerminalView::handle_input_method_event(
                &mut state,
                &input_method::Event::Commit("你好".to_owned()),
            );

            assert!(matches!(
                command,
                Some(Command::Write(bytes)) if bytes == "你好".as_bytes()
            ));
            assert_eq!(state.preedit, None);
        }

        #[test]
        fn commit_without_text_writes_nothing() {
            let mut state = TerminalViewState::new();

            let command = TerminalView::handle_input_method_event(
                &mut state,
                &input_method::Event::Commit(String::new()),
            );

            assert!(command.is_none());
        }

        #[test]
        fn preedit_is_stored_until_emptied() {
            let mut state = TerminalViewState::new();

            TerminalView::handle_input_method_event(
                &mut state,
                &input_method::Event::Preedit("nihao".to_owned(), None),
            );
            assert_eq!(state.preedit.as_deref(), Some("nihao"));

            // 空串表示组合被清空（提交前输入法会先发一次空预编辑串）。
            let command = TerminalView::handle_input_method_event(
                &mut state,
                &input_method::Event::Preedit(String::new(), None),
            );
            assert!(command.is_none());
            assert_eq!(state.preedit, None);
        }

        #[test]
        fn closed_clears_preedit() {
            let mut state = TerminalViewState::new();
            state.preedit = Some("nihao".to_owned());

            let command =
                TerminalView::handle_input_method_event(&mut state, &input_method::Event::Closed);

            assert!(command.is_none());
            assert_eq!(state.preedit, None);
        }
    }

    mod caret_rect_tests {
        use alacritty_terminal::index::{Column, Line};

        use super::*;

        /// 构造光标位于指定网格点的渲染内容；默认单元格为 1×1 像素，故断言可直接按格数书写。
        fn content_at(line: i32, column: usize) -> RenderableContent {
            RenderableContent {
                cursor_point: TerminalGridPoint {
                    line: Line(line),
                    column: Column(column),
                },
                ..Default::default()
            }
        }

        #[test]
        fn anchors_to_cursor_cell() {
            let bounds = Rectangle::new(Point::new(100.0, 50.0), Size::new(400.0, 300.0));

            let rect = TerminalView::caret_rect(bounds, TEST_PADDING, &content_at(2, 3));

            assert_eq!(rect.x, 100.0 + TEST_PADDING + 3.0);
            assert_eq!(rect.y, 50.0 + TEST_PADDING + 2.0);
        }

        #[test]
        fn clamps_into_content_area_when_cursor_is_out_of_view() {
            let bounds = Rectangle::new(Point::ORIGIN, Size::new(200.0, 100.0));

            // 光标滚出可视区（回滚历史）时把锚点收回内容区底边，避免候选框落到窗口外。
            let rect = TerminalView::caret_rect(bounds, TEST_PADDING, &content_at(1000, 0));

            assert_eq!(rect.y, 100.0 - TEST_PADDING - 1.0);
        }
    }

    mod scrollbar_tests {
        use super::*;

        /// 测试用快照：`rows` 行视口、`columns` 列、`history` 行历史。
        fn content_with(
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

        /// 轨道贴住内容右缘、上下各内缩 `padding + SCROLLBAR_INSET`。
        #[test]
        fn track_hugs_the_right_content_edge() {
            let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));
            let (track, _thumb) =
                scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 50, 0)).unwrap();

            assert_eq!(track.x, 10.0 + 400.0 - TEST_PADDING - SCROLLBAR_WIDTH);
            assert_eq!(track.y, 20.0 + TEST_PADDING + SCROLLBAR_INSET);
            assert_eq!(track.width, SCROLLBAR_WIDTH);
            assert_eq!(track.height, 300.0 - (TEST_PADDING + SCROLLBAR_INSET) * 2.0);
        }

        /// 贴住实时输出（偏移 0）时滑块在轨道最下；滚到历史顶端时在最上。
        #[test]
        fn thumb_travels_between_track_ends() {
            let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));

            let (_track, bottom) =
                scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 50, 0)).unwrap();
            assert_eq!(bottom.y + bottom.height, 26.0 + 288.0);

            let (_track, top) =
                scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 50, 50)).unwrap();
            assert_eq!(top.y, 26.0);
        }

        /// 历史越长滑块越短，但不低于 [`SCROLLBAR_MIN_THUMB`]。
        #[test]
        fn thumb_shrinks_with_history_but_keeps_a_grabable_minimum() {
            let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));

            // 视口 50 行、历史 50 行 ⇒ 占总行数一半。
            let (_track, half) =
                scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 50, 0)).unwrap();
            assert_eq!(half.height, 288.0 / 2.0);

            // 历史极长时钳到最小高度，否则滑块细到抓不住。
            let (_track, tiny) =
                scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 100_000, 0))
                    .unwrap();
            assert_eq!(tiny.height, SCROLLBAR_MIN_THUMB);
        }

        /// 无历史 / 备用屏 / 视口为空时不画滚动条。
        #[test]
        fn no_geometry_without_scrollback_or_on_the_alternate_screen() {
            let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));

            assert!(
                scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 0, 0)).is_none()
            );
            assert!(
                scrollbar_geometry(bounds, TEST_PADDING, &content_with(0, 80, 50, 0)).is_none()
            );

            let alt_screen = RenderableContent {
                terminal_mode: TermMode::ALT_SCREEN,
                ..content_with(50, 80, 50, 0)
            };
            assert!(scrollbar_geometry(bounds, TEST_PADDING, &alt_screen).is_none());
        }

        /// 默认终端模式（含 [`TermMode::ALTERNATE_SCROLL`]）仍须显示滚动条。
        ///
        /// 该位是 alacritty 默认就置位的修饰位而非「备用屏」信号，曾据它排除致普通会话无滚动条。
        #[test]
        fn default_term_mode_still_shows_scrollbar() {
            let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));
            assert!(TermMode::default().contains(TermMode::ALTERNATE_SCROLL));

            let content = RenderableContent {
                terminal_mode: TermMode::default(),
                ..content_with(50, 80, 50, 0)
            };
            assert!(scrollbar_geometry(bounds, TEST_PADDING, &content).is_some());
        }

        /// 拖动映射：抓取点跟手（端点与中点），越界钳到两端。
        #[test]
        fn drag_target_maps_cursor_to_scroll_offset() {
            let track = Rectangle::new(Point::new(400.0, 26.0), Size::new(6.0, 288.0));
            let thumb_height = 144.0;
            let span = 288.0 - thumb_height;

            // 抓在滑块顶部拖到轨道顶 ⇒ 完全回滚（偏移 = 历史长度）。
            assert_eq!(
                scrollbar_drag_target(track, thumb_height, 50, track.y, 0.0),
                50
            );
            // 拖到轨道底 ⇒ 贴住实时输出。
            assert_eq!(
                scrollbar_drag_target(track, thumb_height, 50, track.y + span, 0.0),
                0
            );
            // 中点 ⇒ 一半。
            assert_eq!(
                scrollbar_drag_target(track, thumb_height, 50, track.y + span / 2.0, 0.0),
                25
            );
            // 抓取偏移抵消光标位移：光标相对滑块顶部的落点未变则偏移不变。
            assert_eq!(
                scrollbar_drag_target(track, thumb_height, 50, track.y + 72.0, 72.0),
                50
            );
            // 拖出轨道两端钳到极限。
            assert_eq!(
                scrollbar_drag_target(track, thumb_height, 50, track.y - 500.0, 0.0),
                50
            );
            assert_eq!(
                scrollbar_drag_target(track, thumb_height, 50, track.y + 5000.0, 0.0),
                0
            );
        }

        /// 一步增量以「上次已下达的偏移」为基准，故一批多次移动的增量之和恰为最终目标。
        ///
        /// 若改用晚一轮才更新的后端 `display_offset` 作基准，同批增量叠加会冲过目标再回弹——即抖动成因。
        #[test]
        fn drag_step_telescopes_across_a_batch() {
            let seed = 12usize;
            let mut tracked = seed;
            let mut applied = 0i32;

            for target in [30usize, 8, 45, 45] {
                let (delta, next) = scrollbar_drag_step(target, tracked);
                applied += delta;
                tracked = next;
            }

            // 总和即最终目标相对种子的位移，中途的来回都被抵消。
            assert_eq!(applied, 45 - seed as i32);
            assert_eq!(tracked, 45);
            // 目标与基准相同则不发命令。
            assert_eq!(scrollbar_drag_step(45, 45), (0, 45));
        }

        /// 仅「拖拽中 + 左键释放」解除锁存；未拖拽的释放留给选区 / 鼠标上报。
        #[test]
        fn only_left_release_while_dragging_ends_the_drag() {
            let release = iced_core::mouse::Event::ButtonReleased(iced_core::mouse::Button::Left);
            assert!(scrollbar_releases_drag(&release, true));
            assert!(!scrollbar_releases_drag(&release, false));

            let moved = iced_core::mouse::Event::CursorMoved {
                position: Point::ORIGIN,
            };
            assert!(!scrollbar_releases_drag(&moved, true));
        }

        /// 离窗 / 失焦收尾：清空拖拽与悬停锁存并报告需要重绘；全空时不动重绘。
        #[test]
        fn ending_pointer_interactions_clears_latches() {
            let mut state = TerminalViewState {
                scrollbar_drag: Some(3.0),
                is_dragged: true,
                scrollbar_hovered: true,
                ..TerminalViewState::new()
            };
            assert!(state.end_pointer_interactions());
            assert!(state.scrollbar_drag.is_none());
            assert!(!state.is_dragged);
            assert!(!state.scrollbar_hovered);

            // 已无任何锁存 / 悬停：不要求重绘。
            assert!(!state.end_pointer_interactions());
        }
    }
}
