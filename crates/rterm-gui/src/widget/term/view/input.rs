//! `TerminalView` 的事件处理：把鼠标 / 键盘 / 输入法事件解析为后端命令。

use super::scrollbar::{
    scrollbar_drag_step, scrollbar_drag_target, scrollbar_geometry, scrollbar_releases_drag,
};
use super::{TerminalView, TerminalViewState};
use crate::widget::term::backend::{Backend, Command, LinkAction, MouseButton, RenderableContent};
use crate::widget::term::bindings::{BindingAction, BindingsLayout, InputKind};
use crate::widget::term::terminal::Event;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use iced::mouse::{Cursor, ScrollDelta};
use iced::{Point, Rectangle, Size};
use iced_core::clipboard::Kind as ClipboardKind;
use iced_core::input_method::{self, InputMethod, Purpose};
use iced_core::keyboard::{Key, Modifiers, key::Named};
use iced_core::mouse::{self, Click};

impl<'a> TerminalView<'a> {
    /// 判断鼠标光标是否落在终端部件布局矩形范围内。
    pub(super) fn is_cursor_in_layout(
        &self,
        cursor: Cursor,
        layout: iced_graphics::core::Layout<'_>,
    ) -> bool {
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
    pub(super) fn is_cursor_hovered_hyperlink(&self, state: &TerminalViewState) -> bool {
        let content = self.term.backend.renderable_content();
        if let Some(hyperlink_range) = &content.hovered_hyperlink {
            return hyperlink_range.contains(&state.mouse_position_on_grid);
        }

        false
    }

    /// 比较布局尺寸与已记录尺寸，变化时发布终端重设大小命令。
    pub(super) fn handle_resize(
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
    pub(super) fn handle_mouse_event(
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
    pub(super) fn handle_left_button_pressed(
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
    pub(super) fn handle_cursor_moved(
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
    pub(super) fn handle_button_released(
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
    pub(super) fn handle_middle_button_pressed(
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
    pub(super) fn handle_middle_button_released(
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
    pub(super) fn handle_wheel_scrolled(
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
    pub(super) fn handle_scrollbar_event(
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
    pub(super) fn handle_keyboard_event(
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
    pub(super) fn handle_input_method_event(
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
    pub(super) fn caret_rect(
        bounds: Rectangle,
        padding: f32,
        content: &RenderableContent,
    ) -> Rectangle {
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
pub(super) fn paste_bytes(data: &str, bracketed: bool) -> Vec<u8> {
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
pub(super) fn read_middle_paste(clipboard: &dyn iced_graphics::core::Clipboard) -> Option<String> {
    clipboard
        .read(ClipboardKind::Primary)
        .filter(|data| !data.is_empty())
        .or_else(|| clipboard.read(ClipboardKind::Standard))
        .filter(|data| !data.is_empty())
}

/// 把 `Ctrl+字母/数字/符号` 转成对应的 ASCII 控制字符（如 `Ctrl+C` => `\x03`）。
///
/// 仅在系统未给出 `text` 时作为回退；普通字符键优先走 `text` 路径，不经过此处。
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
/// 优先采用系统给出的 `text`，缺失时查标准转义序列；`modifiers` 用于生成带修饰符的 CSI（如 `Ctrl+←` => `\x1b[1;5D`）。
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

/// 转发鼠标与键盘事件，收集命令并发布给后端。
pub(super) fn update(
    view: &mut TerminalView<'_>,
    state: &mut TerminalViewState,
    event: &iced_core::Event,
    layout: iced_graphics::core::Layout<'_>,
    cursor: Cursor,
    clipboard: &mut dyn iced_graphics::core::Clipboard,
    shell: &mut iced_graphics::core::Shell<'_, Event>,
) {
    view.handle_resize(state, layout, shell);

    // 指针离窗或窗口失焦后左键释放不再送达本部件；在此收起交互锁存，
    // 否则重新进入终端区域时滑过会被误当成仍在拖拽（滑块幻影跟随、选区幻影延伸）。
    if matches!(
        event,
        iced_core::Event::Mouse(iced_core::mouse::Event::CursorLeft)
            | iced_core::Event::Window(iced::window::Event::Unfocused)
    ) && state.end_pointer_interactions()
    {
        view.term.cache.clear();
        shell.request_redraw();
    }

    // 输入法策略逐帧续期：iced 每次事件分发都以 `InputMethod::Disabled` 起算、合并全树部件
    // 的申请，且只在重绘路径把结果落到窗口（交互路径会丢弃），故申请必须挂在重绘事件上。
    // 未聚焦则不再申请，窗口 IME 随之关闭，同时清掉残留的预编辑串。
    if matches!(
        event,
        iced_core::Event::Window(iced::window::Event::RedrawRequested(_))
    ) {
        if view.focused {
            // 预编辑由本部件就地绘制（见 `draw`），无需 iced 的 over-the-spot 覆盖层。
            let input_method: InputMethod<&str> = InputMethod::Enabled {
                cursor: TerminalView::caret_rect(
                    layout.bounds(),
                    view.padding,
                    view.term.backend.renderable_content(),
                ),
                purpose: Purpose::Terminal,
                preedit: None,
            };
            shell.request_input_method(&input_method);
        } else if state.preedit.take().is_some() {
            // 未聚焦：不再申请，窗口 IME 随之关闭；残留的预编辑串一并清掉。
            view.term.cache.clear();
        }
    }

    let is_cursor_in_layout = view.is_cursor_in_layout(cursor, layout);

    // 滚动条优先于选区 / 鼠标上报，且须早于下方 `!view.focused` 门控：否则未聚焦时首次按下
    // 会被转成 `FocusRequest` 而拖动失效。拖动中光标移出部件也继续接管，保证在别处释放也能收尾。
    let mut consumed = false;
    let mut scrollbar_commands = Vec::new();
    if let iced::Event::Mouse(mouse_event) = event
        && (is_cursor_in_layout || state.scrollbar_drag.is_some())
        && let Some(commands) =
            view.handle_scrollbar_event(state, layout, cursor, mouse_event, shell)
    {
        consumed = true;
        scrollbar_commands = commands;
    }

    let commands = if consumed {
        scrollbar_commands
    } else {
        match event {
            iced::Event::Mouse(mouse_event) if is_cursor_in_layout => {
                if !view.focused {
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
                        shell.publish(Event::FocusRequest(view.term.id));
                        shell.capture_event();
                    }
                    Vec::new()
                } else {
                    let was_dragged = state.is_dragged;
                    let commands = view.handle_mouse_event(
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
                            iced_core::mouse::Event::ButtonReleased(iced_core::mouse::Button::Left)
                        )
                        && !view
                            .term
                            .backend
                            .renderable_content()
                            .terminal_mode
                            .intersects(TermMode::MOUSE_MODE)
                    {
                        let selection = view.term.backend.selectable_content();
                        if !selection.is_empty() {
                            clipboard.write(ClipboardKind::Primary, selection);
                        }
                    }

                    commands
                }
            }
            iced::Event::Keyboard(keyboard_event) => {
                if !view.focused {
                    return;
                }

                view.handle_keyboard_event(state, clipboard, keyboard_event)
                    .into_iter()
                    .collect()
            }
            iced::Event::InputMethod(input_method_event) => {
                if !view.focused {
                    return;
                }

                let previous = state.preedit.clone();
                let command = TerminalView::handle_input_method_event(state, input_method_event);
                if state.preedit != previous {
                    // 预编辑串参与几何缓存，内容变化须清缓存并在本帧重绘。
                    view.term.cache.clear();
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
        shell.publish(Event::BackendCall(view.term.id, cmd));
    }
}
