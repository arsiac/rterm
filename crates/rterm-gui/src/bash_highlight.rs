//! bash 语法高亮：逐行分词并着色，供 `text_editor` 的 `highlight_with` 使用。
//!
//! 只区分 shell 单行命令里值得着色的词法成分（注释 / 字符串 / 变量 / 关键字 / 运算符 / 命令名），不追求完整语法；
//! 颜色全部取自当前主题，故换主题时编辑器配色随之变化。

use crate::ui::{SUCCESS, WARNING};
use iced::Font;
use iced_core::text::highlighter::{Format, Highlighter};
use std::ops::Range;

/// bash 词法类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashToken {
    /// `#` 起的行注释。
    Comment,
    /// 单引号或双引号字符串。
    String,
    /// `$NAME` / `${…}` / `$(…)` 等变量与命令替换。
    Variable,
    /// shell 关键字（`if` / `then` / `do` …）。
    Keyword,
    /// 运算符（管道 / 重定向 / `&&` / `;` …）。
    Operator,
    /// 命令名（各命令段的首个词）。
    Command,
}

/// 高亮器设置：着色只依赖主题，故无字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings;

/// 逐行 bash 高亮器。
///
/// 着色本身无跨行状态，但须回答渲染器的「从哪一行开始重新着色」——见 `next_line`。
#[derive(Debug, Clone, Copy)]
pub struct BashHighlighter {
    /// 下一个待着色的行号：渲染器从该行起逐行回调 [`Highlighter::highlight_line`]，
    /// 大于可见末行时整个缓冲区都已着色，渲染器据此跳过本轮。
    next_line: usize,
}

impl Highlighter for BashHighlighter {
    type Settings = Settings;
    type Highlight = BashToken;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, BashToken)>;

    fn new(_settings: &Self::Settings) -> Self {
        // 从头开始：新建时缓冲区里每一行都还没着色。
        Self { next_line: 0 }
    }

    fn update(&mut self, _settings: &Self::Settings) {}

    fn change_line(&mut self, line: usize) {
        // 只往回拉：把游标之后的改动并进本轮重着色。
        self.next_line = self.next_line.min(line);
    }

    fn current_line(&self) -> usize {
        self.next_line
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        // 回调按行号升序且自 `current_line()` 起，故每答一行即推进游标。
        self.next_line += 1;
        tokenize_line(line).into_iter()
    }
}

/// 把一个词法类别映射为当前主题下的文字格式。
pub fn bash_format(token: &BashToken, theme: &iced::Theme) -> Format<Font> {
    let p = crate::theme::custom_palette(theme);
    let color = Some(match token {
        BashToken::Comment => p.text_tertiary,
        BashToken::String => SUCCESS,
        BashToken::Variable => WARNING,
        BashToken::Keyword => crate::theme::accent_color(theme),
        BashToken::Operator => p.text_secondary,
        // 命令名与关键字同色，额外加粗以示区别。
        BashToken::Command => crate::theme::accent_color(theme),
    });
    Format {
        color,
        font: match token {
            BashToken::Command => Some(Font {
                weight: iced::font::Weight::Bold,
                ..Font::MONOSPACE
            }),
            _ => None,
        },
    }
}

/// 逐行分词：返回按字节偏移升序、互不重叠的高亮区间；不着色的词不出现。
///
/// 偏移均为**字节**偏移且落在字符边界上（iced 的高亮区间按字节消费）。
pub fn tokenize_line(line: &str) -> Vec<(Range<usize>, BashToken)> {
    let mut out: Vec<(Range<usize>, BashToken)> = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    // 当前位置是否处于「命令名」位置：行首、或刚经过管道 / 分号 / 逻辑连接符 / 关键字。
    let mut at_command = true;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\r' => i += 1,
            // `#` 只有在词首才是注释：词内的 `#` 由 `scan_word` 一并吃掉，不会走到这里。
            b'#' => {
                out.push((i..line.len(), BashToken::Comment));
                break;
            }
            b'\'' | b'"' => {
                let end = scan_quoted(line, i);
                out.push((i..end, BashToken::String));
                i = end;
                at_command = false;
            }
            b'$' => {
                let end = scan_variable(line, i);
                out.push((i..end, BashToken::Variable));
                i = end;
                at_command = false;
            }
            // 命令分隔符：其后又是一个命令段的首个词。
            b'|' | b'&' | b';' => {
                let end = scan_operator(bytes, i);
                out.push((i..end, BashToken::Operator));
                i = end;
                at_command = true;
            }
            // 重定向不切换命令段：目标文件名仍是同一命令的词。
            b'<' | b'>' => {
                let end = scan_operator(bytes, i);
                out.push((i..end, BashToken::Operator));
                i = end;
            }
            b'(' | b')' | b'{' | b'}' | b'!' => {
                out.push((i..i + 1, BashToken::Operator));
                i += 1;
                at_command = true;
            }
            _ => {
                let end = scan_word(line, i);
                if is_keyword(&line[i..end]) {
                    out.push((i..end, BashToken::Keyword));
                    // 关键字之后通常开启新命令段（`then` / `do` / `else` …）。
                    at_command = true;
                } else if at_command {
                    out.push((i..end, BashToken::Command));
                    at_command = false;
                }
                i = end;
            }
        }
    }
    out
}

/// 词的终止字节：ASCII 空白与以下会成为独立成分的符号（不含 `#`，使 `a#b` 保持一个词）。
fn is_word_break(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t'
            | b'\r'
            | b'\''
            | b'"'
            | b'$'
            | b'|'
            | b'&'
            | b';'
            | b'<'
            | b'>'
            | b'('
            | b')'
            | b'{'
            | b'}'
            | b'!'
    )
}

/// 扫一个普通词：从 `start` 到下一个终止字节。
///
/// 终止字节全为 ASCII，故返回的偏移必落在字符边界上（多字节字符整体留在词内）。
fn scan_word(line: &str, start: usize) -> usize {
    let bytes = line.as_bytes();
    let mut i = start;
    while i < bytes.len() && !is_word_break(bytes[i]) {
        i += 1;
    }
    i
}

/// 扫一个引号字符串：返回收盘引号之后一位；未收盘则到行尾。
///
/// 双引号内识别 `\` 转义（跳过被转义的字符），单引号内不识别（bash 语义）。
fn scan_quoted(line: &str, start: usize) -> usize {
    let bytes = line.as_bytes();
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if quote == b'"' => {
                i += 1 + line[i + 1..].chars().next().map_or(0, char::len_utf8);
            }
            b if b == quote => return i + 1,
            _ => i += 1,
        }
    }
    line.len()
}

/// 扫一个变量 / 命令替换：返回其末尾之后一位（裸 `$` 只占一个字节）。
fn scan_variable(line: &str, start: usize) -> usize {
    let bytes = line.as_bytes();
    let i = start + 1;
    match bytes.get(i) {
        Some(b'{') => scan_balanced(line, i, b'{', b'}'),
        Some(b'(') => scan_balanced(line, i, b'(', b')'),
        Some(b) if b.is_ascii_alphabetic() || *b == b'_' => {
            let mut j = i;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            j
        }
        // `$?` `$1` `$@` 这类单字符特殊变量。
        Some(b)
            if b.is_ascii_digit()
                || matches!(b, b'?' | b'!' | b'#' | b'@' | b'*' | b'$' | b'-') =>
        {
            i + 1
        }
        _ => start + 1,
    }
}

/// 从 `from`（开括号处）扫到配对闭括号之后一位；不配对则到行尾。
fn scan_balanced(line: &str, from: usize, open: u8, close: u8) -> usize {
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == open {
            depth += 1;
        } else if bytes[i] == close {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    line.len()
}

/// 扫一个运算符：双写（`||` `&&` `;;` `<<` `>>`）算一个整体。
fn scan_operator(bytes: &[u8], start: usize) -> usize {
    let doubled = matches!(
        (bytes[start], bytes.get(start + 1).copied()),
        (b'|', Some(b'|'))
            | (b'&', Some(b'&'))
            | (b';', Some(b';'))
            | (b'<', Some(b'<'))
            | (b'>', Some(b'>'))
    );
    if doubled { start + 2 } else { start + 1 }
}

/// 是否为 shell 关键字（含 `true` / `false`，避免它们被当作命令名着色加粗）。
fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "if" | "then"
            | "else"
            | "elif"
            | "fi"
            | "for"
            | "while"
            | "until"
            | "do"
            | "done"
            | "case"
            | "esac"
            | "in"
            | "function"
            | "select"
            | "time"
            | "coproc"
            | "return"
            | "break"
            | "continue"
            | "true"
            | "false"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把一行分词结果还原成（词面, 类别）序列，便于表格断言。
    fn spans(line: &str) -> Vec<(&str, BashToken)> {
        tokenize_line(line)
            .into_iter()
            .map(|(range, token)| (&line[range], token))
            .collect()
    }

    #[test]
    fn highlights_the_command_word() {
        assert_eq!(spans("echo hello"), [("echo", BashToken::Command)]);
        // 行首即命令，且参数不着色。
        assert_eq!(spans("ls -la /tmp"), [("ls", BashToken::Command)]);
    }

    #[test]
    fn comment_spans_to_end_of_line_but_not_inside_a_word() {
        assert_eq!(
            spans("# just a note"),
            [("# just a note", BashToken::Comment)]
        );
        assert_eq!(
            spans("echo a#b"),
            [("echo", BashToken::Command)],
            "词内的 # 是普通字符"
        );
        assert_eq!(
            spans("ls; # tail"),
            [
                ("ls", BashToken::Command),
                (";", BashToken::Operator),
                ("# tail", BashToken::Comment)
            ]
        );
    }

    #[test]
    fn highlights_strings_with_escapes() {
        assert_eq!(
            spans(r#"echo 'a b' "c\"d""#),
            [
                ("echo", BashToken::Command),
                ("'a b'", BashToken::String),
                (r#""c\"d""#, BashToken::String)
            ]
        );
        // 未收盘的字符串延续到行尾，不越界。
        assert_eq!(
            spans("echo 'abc"),
            [("echo", BashToken::Command), ("'abc", BashToken::String)]
        );
    }

    #[test]
    fn highlights_variables_and_command_substitution() {
        assert_eq!(
            spans("cp $HOME/${x}/$(date +%s) $1"),
            [
                ("cp", BashToken::Command),
                ("$HOME", BashToken::Variable),
                ("${x}", BashToken::Variable),
                ("$(date +%s)", BashToken::Variable),
                ("$1", BashToken::Variable),
            ],
            "路径分隔符与参数不着色，三种变量写法各自成段"
        );
    }

    #[test]
    fn pipeline_restarts_the_command_position() {
        assert_eq!(
            spans("ls | grep -i x && cat y"),
            [
                ("ls", BashToken::Command),
                ("|", BashToken::Operator),
                ("grep", BashToken::Command),
                ("&&", BashToken::Operator),
                ("cat", BashToken::Command),
            ]
        );
    }

    #[test]
    fn highlights_keywords() {
        assert_eq!(
            spans("if true; then echo hi; fi"),
            [
                ("if", BashToken::Keyword),
                ("true", BashToken::Keyword),
                (";", BashToken::Operator),
                ("then", BashToken::Keyword),
                ("echo", BashToken::Command),
                (";", BashToken::Operator),
                ("fi", BashToken::Keyword),
            ]
        );
    }

    #[test]
    fn redirection_keeps_the_same_command_section() {
        assert_eq!(
            spans("ls > out 2>> err"),
            [
                ("ls", BashToken::Command),
                (">", BashToken::Operator),
                (">>", BashToken::Operator),
            ],
            "重定向目标与文件描述符数字都不着色"
        );
    }

    #[test]
    fn ranges_are_sorted_disjoint_and_on_char_boundaries() {
        let corpus = [
            "echo hello",
            r#"awk '{print $1}' file | sort -u"#,
            "for i in $(seq 1 10); do echo ${i}; done",
            "echo 中文 # 注释",
            "echo $'a\\nb' ${x:-默认值}",
            "   ",
            "",
            "echo \"unterminated",
            "cat <<'EOF'",
        ];
        for line in corpus {
            let mut prev_end = 0;
            for (range, _) in tokenize_line(line) {
                assert!(
                    line.is_char_boundary(range.start) && line.is_char_boundary(range.end),
                    "{line:?} → {range:?} 落在字符边界上"
                );
                assert!(range.start >= prev_end, "{line:?} 区间升序且不重叠");
                assert!(range.end <= line.len(), "{line:?} 区间不出界");
                prev_end = range.end;
            }
        }
    }

    #[test]
    fn format_colors_come_from_the_theme_and_command_is_bold() {
        let theme = iced::Theme::Dark;
        for token in [
            BashToken::Comment,
            BashToken::String,
            BashToken::Variable,
            BashToken::Keyword,
            BashToken::Operator,
            BashToken::Command,
        ] {
            assert!(
                bash_format(&token, &theme).color.is_some(),
                "{token:?} 必须有颜色，否则着色等于没做"
            );
        }
        let command = bash_format(&BashToken::Command, &theme);
        assert!(command.font.is_some(), "命令名额外加粗");
        assert!(
            bash_format(&BashToken::Operator, &theme).font.is_none(),
            "只有命令名加粗"
        );
    }

    /// 渲染器先问 `current_line()` 再从该行起逐行回调；返回 `usize::MAX` 会让它整轮跳过。
    #[test]
    fn walks_the_line_cursor_the_renderer_asks_for() {
        let mut highlighter = BashHighlighter::new(&Settings);
        assert_eq!(highlighter.current_line(), 0, "首次整缓冲区都待着色");
        for line in ["echo a", "echo b"] {
            let _ = highlighter.highlight_line(line);
        }
        assert_eq!(highlighter.current_line(), 2, "着色过的行不再重来");
        highlighter.change_line(1);
        assert_eq!(highlighter.current_line(), 1, "改动行往回拉游标");
        highlighter.change_line(9);
        assert_eq!(highlighter.current_line(), 1, "更靠后的改动不动游标");
    }
}
