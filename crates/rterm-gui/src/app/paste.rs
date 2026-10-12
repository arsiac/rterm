//! 终端粘贴的量级判定与确认弹窗模块。
//!
//! 三条粘贴入口（键盘快捷键 / 中键 / 右键菜单）在此收口：按配置判定文本量级，达阈值者
//! 挂起为待确认项并弹窗询问，其余直接放行。放行经 [`Event::Paste`] 上行，由父层按标签当前
//! 括号粘贴模式编码后经终端写入通道（含断开态闸门）落笔。

use iced::Task;
use iced::widget::text_editor;
use std::collections::HashSet;

/// 待用户确认的粘贴。
pub struct PendingPaste {
    /// 发起粘贴的标签 id。
    pub tab_id: u64,
    /// 待粘贴的原始文本。
    pub text: String,
    /// 量级统计（供弹窗摘要与警告项渲染）。
    pub stats: PasteStats,
}

/// 粘贴文本的量级统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasteStats {
    /// 字节数。
    pub bytes: usize,
    /// 字符数。
    pub chars: usize,
    /// 逻辑行数（`\r\n` / `\r` / `\n` 均按换行计，末尾残行也算一行）。
    pub lines: usize,
    /// `\t` / `\n` / `\r` 之外的控制字符数。
    pub control_chars: usize,
}

impl PasteStats {
    /// 统计一段文本的量级。
    pub fn measure(text: &str) -> Self {
        Self {
            bytes: text.len(),
            chars: text.chars().count(),
            lines: count_lines(text),
            control_chars: text.chars().filter(|c| is_control_char(*c)).count(),
        }
    }
}

/// 粘贴确认模块状态。
#[derive(Default)]
pub struct State {
    /// 待用户确认的粘贴（`None` = 无弹窗）。
    pending: Option<PendingPaste>,
    /// 确认弹窗内「本次不再提醒」的勾选态。
    suppress_checked: bool,
    /// 已豁免提醒的标签集合：勾选后本次运行内该标签的粘贴一律放行，关标签时清理。
    suppressed: HashSet<u64>,
    /// 待确认项的内容预览缓冲：只读 `text_editor` 的绑定源，随待确认项建立 / 清空。
    preview: text_editor::Content,
}

impl State {
    /// 构造空状态。
    pub fn new() -> Self {
        Self::default()
    }

    /// 取待确认项（只读），供渲染层展示弹窗。
    pub fn pending(&self) -> Option<&PendingPaste> {
        self.pending.as_ref()
    }

    /// 当前「本次不再提醒」勾选态。
    pub fn suppress_checked(&self) -> bool {
        self.suppress_checked
    }

    /// 待确认项的内容预览缓冲（只读），供渲染层绑定 `text_editor`。
    pub fn preview(&self) -> &text_editor::Content {
        &self.preview
    }

    /// 关闭某标签时清理其待确认项与豁免记录。
    pub fn remove_for_tab(&mut self, tab_id: u64) {
        if self.pending.as_ref().is_some_and(|p| p.tab_id == tab_id) {
            self.pending = None;
            self.preview = text_editor::Content::new();
        }
        self.suppressed.remove(&tab_id);
    }

    /// 模块更新：按量级判定决定放行或挂起待确认；放行的写入经 [`Event::Paste`] 上行。
    pub fn update(&mut self, msg: Message, ctx: &Ctx) -> Task<Event> {
        match msg {
            Message::Request { tab_id, text } => self.request(tab_id, text, ctx),
            // 确认：放行本次（勾选「本次不再提醒」时顺带记下该标签），随后清空待确认。
            Message::Confirm => match self.pending.take() {
                Some(p) => {
                    if self.suppress_checked {
                        self.suppressed.insert(p.tab_id);
                    }
                    self.suppress_checked = false;
                    self.preview = text_editor::Content::new();
                    Task::done(Event::Paste {
                        tab_id: p.tab_id,
                        text: p.text,
                    })
                }
                None => Task::none(),
            },
            // 取消 / Esc：丢弃待确认，不写入。
            Message::Cancel => {
                self.pending = None;
                self.suppress_checked = false;
                self.preview = text_editor::Content::new();
                Task::none()
            }
            Message::SuppressToggled(v) => {
                self.suppress_checked = v;
                Task::none()
            }
            // 预览只读：放行滚动 / 光标 / 选区动作，屏蔽一切编辑动作。
            Message::PreviewAction(action) => {
                if !action.is_edit() {
                    self.preview.perform(action);
                }
                Task::none()
            }
        }
    }

    /// 处理一次粘贴请求：达阈值则挂起等待确认，否则直接放行。
    fn request(&mut self, tab_id: u64, text: String, ctx: &Ctx) -> Task<Event> {
        // 弹窗已挂起时忽略新请求（模态下终端已失焦，实际很难触发）。
        if self.pending.is_some() {
            return Task::none();
        }
        // 该标签已豁免（本次不再提醒）：直接放行。
        if self.suppressed.contains(&tab_id) {
            return Task::done(Event::Paste { tab_id, text });
        }
        match classify(&text, ctx) {
            Some(stats) => {
                self.suppress_checked = false;
                self.preview = text_editor::Content::with_text(&preview_text(&text));
                self.pending = Some(PendingPaste {
                    tab_id,
                    text,
                    stats,
                });
                Task::none()
            }
            None => Task::done(Event::Paste { tab_id, text }),
        }
    }
}

/// 模块内部消息：粘贴请求与弹窗决策。
#[derive(Clone)]
pub enum Message {
    /// 收到一次粘贴请求（携带标签 id 与待粘贴文本）。
    Request {
        /// 发起粘贴的标签 id。
        tab_id: u64,
        /// 待粘贴的文本。
        text: String,
    },
    /// 确认弹窗：放行本次粘贴。
    Confirm,
    /// 确认弹窗：取消本次粘贴（Esc 等价）。
    Cancel,
    /// 切换「本次不再提醒」勾选态。
    SuppressToggled(bool),
    /// 预览框的编辑动作（只读：放行滚动 / 光标 / 选区，屏蔽编辑）。
    PreviewAction(text_editor::Action),
}

/// 模块上行事件：把放行的粘贴交父层编码写入。
#[derive(Clone)]
pub enum Event {
    /// 放行粘贴：父层按标签当前括号粘贴模式编码后经终端写入通道落笔。
    Paste {
        /// 目标标签 id。
        tab_id: u64,
        /// 待粘贴文本。
        text: String,
    },
}

/// 父层只读上下文：粘贴拦截判据的阈值与开关。
pub struct Ctx {
    /// 总开关：关闭时一切粘贴直接放行。
    pub paste_warn_enabled: bool,
    /// 多行告警阈值（逻辑行数 ≥ 此值即告警）；0 表示不按行数告警。
    pub paste_warn_lines: usize,
    /// 巨量告警阈值（字节数 ≥ 此值即告警）；0 表示不按字节数告警。
    pub paste_warn_bytes: usize,
    /// 是否因「含控制字符」告警。
    pub paste_warn_control_chars: bool,
}

/// 判定一次粘贴是否需要确认；返回 `None` 表示可直接放行。
pub(crate) fn classify(text: &str, ctx: &Ctx) -> Option<PasteStats> {
    if !ctx.paste_warn_enabled {
        return None;
    }
    let stats = PasteStats::measure(text);
    let hit = (ctx.paste_warn_lines > 0 && stats.lines >= ctx.paste_warn_lines)
        || (ctx.paste_warn_bytes > 0 && stats.bytes >= ctx.paste_warn_bytes)
        || (ctx.paste_warn_control_chars && stats.control_chars > 0);
    hit.then_some(stats)
}

/// 逻辑行数：`\r\n` / `\r` / `\n` 均按换行计；以换行收尾时末行不计（`"a\n"` 为 1 行）。
pub(crate) fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let mut lines = 1usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines += 1;
            }
            '\n' => lines += 1,
            _ => {}
        }
    }
    if text.ends_with('\n') || text.ends_with('\r') {
        lines -= 1;
    }
    lines
}

/// 控制字符判定：`\t` / `\n` / `\r` 之外的控制字符（Unicode Cc）。
pub(crate) fn is_control_char(c: char) -> bool {
    c.is_control() && !matches!(c, '\t' | '\n' | '\r')
}

/// 生成预览文本：控制字符转义为可见记法（不原样输出，以便看清不可见的控制字符），
/// `\t` 展开为空格、`\r` 丢弃；不截断，交由 `text_editor` 滚动查看。
pub(crate) fn preview_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\r' => {}
            '\t' => out.push_str("    "),
            _ if is_control_char(c) => out.push_str(&escape_control(c)),
            _ => out.push(c),
        }
    }
    out
}

/// 控制字符的可见转义：DEL 显示为 `^?`，C0 码显示为 `^X`，其余按 `\u{..}`。
fn escape_control(c: char) -> String {
    let code = c as u32;
    match code {
        0x7f => "^?".to_string(),
        0x00..=0x1f => format!("^{}", char::from_u32(code + 0x40).unwrap_or('?')),
        _ => format!("\\u{{{code:04x}}}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认阈值上下文（行 4 / 字节 1024 / 控制字符开）。
    fn ctx() -> Ctx {
        Ctx {
            paste_warn_enabled: true,
            paste_warn_lines: 4,
            paste_warn_bytes: 1024,
            paste_warn_control_chars: true,
        }
    }

    #[test]
    fn count_lines_handles_crlf_and_trailing_newline() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("ls"), 1);
        assert_eq!(count_lines("ls\n"), 1);
        assert_eq!(count_lines("a\nb"), 2);
        assert_eq!(count_lines("a\nb\n"), 2);
        assert_eq!(count_lines("a\r\nb\r\n"), 2);
        assert_eq!(count_lines("a\rb"), 2);
        assert_eq!(count_lines("a\n\nb\n"), 3);
    }

    #[test]
    fn measure_counts_bytes_chars_lines_and_control_chars() {
        let s = PasteStats::measure("ab\ncd");
        assert_eq!(s.bytes, 5);
        assert_eq!(s.chars, 5);
        assert_eq!(s.lines, 2);
        assert_eq!(s.control_chars, 0);

        // ESC / 退格计入控制字符；\t \n \r 不计。
        let s = PasteStats::measure("a\x1b\tb\r\n");
        assert_eq!(s.control_chars, 1);
    }

    #[test]
    fn classify_allows_short_single_line_text() {
        assert!(classify("ls -la", &ctx()).is_none());
    }

    #[test]
    fn classify_warns_on_line_and_byte_thresholds() {
        assert!(classify("a\nb\nc\nd\n", &ctx()).is_some(), "4 行应告警");
        assert!(classify("a\nb\nc\n", &ctx()).is_none(), "3 行不告警");
        let big = "x".repeat(1024);
        assert!(classify(&big, &ctx()).is_some(), "1024 字节应告警");
        assert!(classify(&"x".repeat(1023), &ctx()).is_none());
    }

    #[test]
    fn classify_warns_on_control_chars_when_enabled() {
        assert!(classify("a\x1bb", &ctx()).is_some());
        let off = Ctx {
            paste_warn_control_chars: false,
            ..ctx()
        };
        assert!(classify("a\x1bb", &off).is_none());
    }

    #[test]
    fn disabled_switch_allows_everything() {
        let off = Ctx {
            paste_warn_enabled: false,
            ..ctx()
        };
        assert!(classify("a\nb\nc\nd\ne\n", &off).is_none());
    }

    #[test]
    fn zero_threshold_disables_that_criterion() {
        let c = Ctx {
            paste_warn_lines: 0,
            paste_warn_bytes: 0,
            paste_warn_control_chars: false,
            ..ctx()
        };
        assert!(classify("a\nb\nc\nd\ne\n", &c).is_none());
    }

    #[test]
    fn small_paste_is_released_directly() {
        let mut s = State::new();
        let _ = s.update(
            Message::Request {
                tab_id: 1,
                text: "ls".into(),
            },
            &ctx(),
        );
        // `Task` 在单测里无法执行，故只校验状态：未挂起待确认即为放行。
        assert!(s.pending().is_none());
        assert!(!s.suppress_checked());
    }

    #[test]
    fn large_paste_is_held_until_confirmed() {
        let mut s = State::new();
        let _ = s.update(
            Message::Request {
                tab_id: 3,
                text: "a\nb\nc\nd\n".into(),
            },
            &ctx(),
        );
        let pending = s.pending().expect("应挂起待确认");
        assert_eq!(pending.tab_id, 3);
        assert_eq!(pending.stats.lines, 4);

        // 确认后清空待确认。
        let _ = s.update(Message::Confirm, &ctx());
        assert!(s.pending().is_none());
    }

    #[test]
    fn cancel_discards_without_release() {
        let mut s = State::new();
        let _ = s.update(
            Message::Request {
                tab_id: 3,
                text: "a\nb\nc\nd\n".into(),
            },
            &ctx(),
        );
        assert!(s.pending().is_some());
        let _ = s.update(Message::Cancel, &ctx());
        assert!(s.pending().is_none());
    }

    #[test]
    fn suppression_skips_warning_for_that_tab_only() {
        let mut s = State::new();
        let _ = s.update(
            Message::Request {
                tab_id: 3,
                text: "a\nb\nc\nd\n".into(),
            },
            &ctx(),
        );
        let _ = s.update(Message::SuppressToggled(true), &ctx());
        let _ = s.update(Message::Confirm, &ctx());
        assert!(!s.suppress_checked(), "确认后勾选态应复位");

        // 同一标签的后续大量粘贴不再挂起（直接放行）。
        let _ = s.update(
            Message::Request {
                tab_id: 3,
                text: "a\nb\nc\nd\n".into(),
            },
            &ctx(),
        );
        assert!(s.pending().is_none(), "已豁免标签应直接放行");

        // 另一标签仍会告警。
        let _ = s.update(
            Message::Request {
                tab_id: 4,
                text: "a\nb\nc\nd\n".into(),
            },
            &ctx(),
        );
        assert!(s.pending().is_some(), "未豁免标签仍应告警");
    }

    #[test]
    fn preview_text_escapes_control_chars_and_drops_cr() {
        assert_eq!(preview_text("a\x1bb\x07c\r\n"), "a^[b^Gc\n");
    }

    #[test]
    fn preview_text_expands_tabs() {
        assert_eq!(preview_text("a\tb"), "a    b");
    }

    #[test]
    fn preview_text_does_not_truncate() {
        let s = "x".repeat(10_000);
        assert_eq!(preview_text(&s).chars().count(), 10_000);
    }

    #[test]
    fn remove_for_tab_clears_pending_and_suppression() {
        let mut s = State::new();
        let _ = s.update(
            Message::Request {
                tab_id: 9,
                text: "a\nb\nc\nd\n".into(),
            },
            &ctx(),
        );
        s.remove_for_tab(9);
        assert!(s.pending().is_none());

        // 关标签后再开同 id 的新标签（id 单调递增，此处仅验证豁免记录已随标签清理）。
        s.suppressed.insert(9);
        s.remove_for_tab(9);
        assert!(!s.suppressed.contains(&9));
    }
}
