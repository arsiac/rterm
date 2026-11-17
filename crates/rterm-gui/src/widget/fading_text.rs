//! 宽度不足时在右端淡出的单行文本部件。
//!
//! iced 的 `text` 搭配 `Wrapping::None` + 容器 `clip(true)` 只会在列宽边缘硬切，
//! 被切掉的文件名看起来像是原本就这么短，容易让用户误解。本部件在布局阶段用真实字体
//! 度量文本宽度：放得下就原样绘制；放不下则在文本末尾叠一段「透明 → 行背景色」的
//! 线性渐变，让文字自然淡入背景，一眼就能看出后面还有内容。
//!
//! 启用 [`FadingText::preserve_tail`] 后，溢出时改为「双段」显示：左段（名称前缀）
//! 在中段渐隐，尾段（扩展名或末尾若干字符）锚定名称列右端实显、左缘从背景渐显。
//! 文件名的区分度往往在尾部，这样同前缀的一批长文件名仍能一眼分辨，且全程无硬切。

use iced::advanced::text::paragraph;
use iced::advanced::text::{self, IntoFragment, Text, Wrapping};
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::{Layout, Widget, layout, renderer};
use iced::widget::text as text_widget;
use iced::{Color, Degrees, Element, Length, Pixels, Rectangle, Size, alignment, gradient};

/// 行背景色提供者：按当前主题解析淡出终点色。
type BackgroundFn<'a, Theme> = Box<dyn Fn(&Theme) -> Option<Color> + 'a>;

/// 尾段回退取末尾字符数（无扩展名 / 点开头隐藏文件 / 后缀过长时）。
const TAIL_FALLBACK_CHARS: usize = 4;
/// 扩展名允许的最大字符数（不含点），超过则不当作扩展名。
const TAIL_EXT_MAX_CHARS: usize = 8;
/// 尾段附带的前缀字符数，让 `报告.md` 不只显示 `.md`。
const TAIL_LEADING_CHARS: usize = 1;
/// 两段之间的空隙宽度（像素），避免渐隐与渐显叠在一起。
const TAIL_GAP: f32 = 8.0;
/// 左段最小可见宽度（像素）；低于此值退化为纯右端淡出。
const MIN_HEAD_WIDTH: f32 = 48.0;
/// 尾段允许占名称列宽度的最大比例，超过则退化为纯右端淡出。
const TAIL_MAX_RATIO: f32 = 0.45;
/// 淡出带占可用宽度的比例上限，避免极窄面板时整段名字都落在渐变里。
const FADE_RATIO: f32 = 0.35;
/// 淡出带的最小宽度（像素）。
const MIN_FADE_WIDTH: f32 = 16.0;
/// 尾段左缘渐显带的最大宽度（像素），避免吃掉太多尾部字符。
const TAIL_REVEAL_WIDTH: f32 = 12.0;

/// 单行文本：超出可用宽度时右端淡出，而不是在容器边缘被硬切。
///
/// 淡出只覆盖行内文本区域，终点色取所在行的背景色（由 `Theme` 闭包提供），
/// 因此悬停 / 选中行的底色变化也能正确衔接。调用方应传入**不透明**的行底色
/// （如 [`crate::theme::list_row_solid_bg`]）：半透明终点色遮不住文字，末端会残留半截字。
///
/// 设置 `fade_extend` 后，文本与淡出区会向右越出自身布局边界，借入相邻列的空闲宽度，
/// 让淡出带更宽而不至于多吞字符。此时父级不能裁剪本部件，且相邻内容需绘制在其后
/// （淡出终点色与行背景一致，越界部分只有叠在文字上时才可见）。
#[allow(missing_debug_implementations)]
pub struct FadingText<'a, Theme = iced::Theme, Renderer = iced::Renderer>
where
    Renderer: text::Renderer,
{
    content: text::Fragment<'a>,
    size: Option<Pixels>,
    line_height: text::LineHeight,
    font: Option<Renderer::Font>,
    width: Length,
    fade_width: f32,
    /// 淡出区向右越出部件边界的宽度（像素），用于借入相邻列的空白区域。
    fade_extend: f32,
    /// 溢出时是否保留文件名尾部并在右端实显。
    preserve_tail: bool,
    /// 淡出终点色（即文本所在行的背景色）；未设置时不绘制淡出。
    background: Option<BackgroundFn<'a, Theme>>,
}

impl<'a, Theme, Renderer> FadingText<'a, Theme, Renderer>
where
    Renderer: text::Renderer,
{
    /// 以给定文本构造部件。
    pub fn new(content: impl IntoFragment<'a>) -> Self {
        Self {
            content: content.into_fragment(),
            size: None,
            line_height: text::LineHeight::default(),
            font: None,
            width: Length::Shrink,
            fade_width: 0.0,
            fade_extend: 0.0,
            preserve_tail: false,
            background: None,
        }
    }

    /// 设置字号。
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// 设置宽度策略；通常用 `Length::Fill` 占满所在列，由布局给出的宽度触发淡出。
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// 启用右端淡出：`width` 为期望的淡出区宽度（像素），`background` 给出淡出终点的
    /// 行背景色；返回 `None`（如底色为渐变）时退化为硬切。
    ///
    /// 实际淡出带会按可用宽度自适应收敛（见 [`effective_fade`]），避免面板极窄时
    /// 整段文字都落进渐变里。
    pub fn fade_out(
        mut self,
        width: impl Into<Pixels>,
        background: impl Fn(&Theme) -> Option<Color> + 'a,
    ) -> Self {
        self.fade_width = width.into().0.max(0.0);
        self.background = Some(Box::new(background));
        self
    }

    /// 让文本与淡出区向右越出部件边界 `extend` 像素（默认 0），借入相邻列的空闲宽度。
    ///
    /// 调用方需保证父级不裁剪本部件，且相邻内容在本部件之后绘制。仅作用于
    /// 单段（纯右端淡出）模式；双段模式的尾段锚定在名称列内，不越界。
    pub fn fade_extend(mut self, extend: impl Into<Pixels>) -> Self {
        self.fade_extend = extend.into().0.max(0.0);
        self
    }

    /// 溢出时保留文件名尾部（扩展名或末尾若干字符）并在名称列右端实显。
    ///
    /// 左段在中段渐隐、尾段左缘从行底色渐显，两段之间留一小段空隙，全程无硬切；
    /// 尾段过宽、名称列过窄或名称本身太短时自动退化为纯右端淡出。
    pub fn preserve_tail(mut self) -> Self {
        self.preserve_tail = true;
        self
    }
}

/// 部件状态：复用的段落缓存 + 上一次布局的判定依据。
struct State<P: paragraph::Paragraph> {
    /// 当前缓存的段落，内容为完整文本（文字的裁切交给绘制阶段）。
    paragraph: paragraph::Plain<P>,
    /// 双段模式下缓存的尾段段落，内容为 [`tail_fragment`] 的返回值。
    tail_paragraph: paragraph::Plain<P>,
    /// 上一次布局的原始文本，用于判断缓存是否仍然有效。
    last_content: String,
    /// 上一次布局的可用宽度（像素）。
    last_width: f32,
    /// 上一次布局的字号（像素）；字号变化会改变度量结果，必须让缓存失效。
    last_size: f32,
    /// 上一次布局的字体；字体变化同样必须让缓存失效。
    last_font: Option<P::Font>,
    /// 上一次布局的淡出带期望宽度（像素）。
    last_fade_width: f32,
    /// 上一次布局是否启用了保留尾段。
    last_preserve_tail: bool,
    /// 上一次布局时文本是否溢出（决定是否绘制淡出）。
    overflowed: bool,
    /// 本帧是否走双段显示（保留尾段）。
    split: bool,
    /// 双段模式下左段的可见宽度（像素）。
    head_width: f32,
    /// 双段模式下尾段起点相对名称列左端的偏移（像素）。
    tail_x: f32,
    /// 双段模式下尾段的宽度（像素）。
    tail_width: f32,
    /// 本帧实际使用的淡出带宽度（像素）。
    fade: f32,
}

impl<P: paragraph::Paragraph> Default for State<P> {
    fn default() -> Self {
        Self {
            paragraph: paragraph::Plain::default(),
            tail_paragraph: paragraph::Plain::default(),
            last_content: String::new(),
            last_width: -1.0,
            last_size: -1.0,
            last_font: None,
            last_fade_width: -1.0,
            last_preserve_tail: false,
            overflowed: false,
            split: false,
            head_width: 0.0,
            tail_x: 0.0,
            tail_width: 0.0,
            fade: 0.0,
        }
    }
}

/// 构造用于度量 / 绘制的文本定义：强制 `Wrapping::None`，度量结果即单行固有宽度。
fn definition<Font: Copy>(
    content: &str,
    bounds: Size,
    size: Pixels,
    line_height: text::LineHeight,
    font: Font,
) -> Text<&str, Font> {
    Text {
        content,
        bounds,
        size,
        line_height,
        font,
        align_x: alignment::Horizontal::Left.into(),
        align_y: alignment::Vertical::Top,
        shaping: text::Shaping::default(),
        wrapping: Wrapping::None,
    }
}

/// 从完整名称中挑出要保留的尾部片段。
///
/// 最后一个 `.` 位于首字符之后、且其后 1..=[`TAIL_EXT_MAX_CHARS`] 个字符时，视为扩展名
/// 并连带前一个字符一起保留（`报告.md` → `告.md`）；点开头隐藏文件（`.gitignore`）不套用
/// 扩展名规则；其余情况取末尾 [`TAIL_FALLBACK_CHARS`] 个字符。按 `char` 边界切分，
/// CJK、空格和中英混排都不会切到半个字符。
fn tail_fragment(content: &str) -> &str {
    let chars: Vec<(usize, char)> = content.char_indices().collect();
    if chars.is_empty() {
        return content;
    }
    if let Some(dot) = chars.iter().rposition(|(_, c)| *c == '.')
        && dot > 0
    {
        let ext_len = chars.len() - dot - 1;
        if (1..=TAIL_EXT_MAX_CHARS).contains(&ext_len) {
            let start = chars[dot.saturating_sub(TAIL_LEADING_CHARS)].0;
            return &content[start..];
        }
    }
    let start = chars[chars.len().saturating_sub(TAIL_FALLBACK_CHARS)].0;
    &content[start..]
}

/// 把期望的淡出带宽度收敛到可用宽度内：上限为可用宽度的 [`FADE_RATIO`]，下限
/// [`MIN_FADE_WIDTH`]，且绝不超过可用宽度。
fn effective_fade(requested: f32, visible_width: f32) -> f32 {
    if requested <= 0.0 || visible_width <= 0.0 {
        return 0.0;
    }
    requested
        .min(visible_width * FADE_RATIO)
        .max(MIN_FADE_WIDTH)
        .min(visible_width)
}

/// 在给定矩形上叠一段水平渐变：`reveal` 为 `true` 时从左端底色渐变到透明
/// （尾段左缘渐显），否则从透明渐变到右端底色（文字向右淡出）。
fn fill_fade<R>(renderer: &mut R, bounds: Rectangle, color: Color, reveal: bool)
where
    R: text::Renderer,
{
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }
    let transparent = Color { a: 0.0, ..color };
    let (first, last) = if reveal {
        (color, transparent)
    } else {
        (transparent, color)
    };
    renderer.fill_quad(
        renderer::Quad {
            bounds,
            ..renderer::Quad::default()
        },
        gradient::Linear::new(Degrees(90.0))
            .add_stop(0.0, first)
            .add_stop(1.0, last),
    );
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for FadingText<'_, Theme, Renderer>
where
    Renderer: text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Renderer::Paragraph>::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
        let size = self.size.unwrap_or_else(|| renderer.default_size());
        let font = self.font.unwrap_or_else(|| renderer.default_font());
        let line_height = self.line_height;

        layout::sized(limits, self.width, Length::Shrink, |limits| {
            let bounds = limits.max();
            let available = bounds.width;

            // 文本、宽度、字号、字体或淡出配置都没变时直接复用上一帧的段落与判定结果，
            // 每帧只做几次标量比较，绝不重新度量或分配。
            let changed = state.last_content != *self.content
                || state.last_width != available
                || state.last_size != size.0
                || state.last_font != Some(font)
                || state.last_fade_width != self.fade_width
                || state.last_preserve_tail != self.preserve_tail;
            if changed {
                let _ = state.paragraph.update(definition(
                    &self.content,
                    bounds,
                    size,
                    line_height,
                    font,
                ));
                state.last_content.clear();
                state.last_content.push_str(&self.content);
                state.last_width = available;
                state.last_size = size.0;
                state.last_font = Some(font);
                state.last_fade_width = self.fade_width;
                state.last_preserve_tail = self.preserve_tail;

                state.overflowed = state.paragraph.min_bounds().width > available;
                state.fade = effective_fade(self.fade_width, available + self.fade_extend);
                state.split = false;

                // 双段显示只在溢出且显式开启时启用；所有退化条件都在这里一次性收敛，
                // 绘制阶段只读结果，不做任何度量。
                if state.overflowed && self.preserve_tail && self.fade_width > 0.0 {
                    let tail = tail_fragment(&self.content);
                    if !tail.is_empty() && tail.len() < self.content.len() {
                        let _ = state.tail_paragraph.update(definition(
                            tail,
                            bounds,
                            size,
                            line_height,
                            font,
                        ));
                        let tail_width = state.tail_paragraph.min_bounds().width;
                        if tail_width > 0.0 && tail_width <= available * TAIL_MAX_RATIO {
                            let head_width = available - tail_width - TAIL_GAP;
                            if head_width >= MIN_HEAD_WIDTH {
                                state.split = true;
                                state.head_width = head_width;
                                state.tail_width = tail_width;
                                state.tail_x = available - tail_width;
                                state.fade = state.fade.min(head_width * 0.5);
                            }
                        }
                    }
                }
            }

            state.paragraph.min_bounds()
        })
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: iced::advanced::mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
        let bounds = layout.bounds();
        // 可视右边界：允许向右越界时把裁剪线一并右移，文本才能在相邻列的空白处淡出。
        let visible = Rectangle {
            width: bounds.width + self.fade_extend,
            ..bounds
        };
        let Some(clip) = visible.intersection(viewport) else {
            return;
        };
        let color = self
            .background
            .as_ref()
            .and_then(|background| background(theme));

        if state.split {
            // 左段：从名称列左端起绘制，只裁到左段右边界；尾段：把段落起点平移到
            // 名称列右端减去尾段宽度处，只裁到名称列右边界。两段都不越入大小列。
            let head = Rectangle {
                width: state.head_width,
                ..bounds
            };
            if let Some(head_clip) = head.intersection(viewport) {
                text_widget::draw(
                    renderer,
                    style,
                    bounds,
                    state.paragraph.raw(),
                    text_widget::Style { color: None },
                    &head_clip,
                );
            }
            let tail_bounds = Rectangle {
                x: bounds.x + state.tail_x,
                width: state.tail_width,
                ..bounds
            };
            if let Some(tail_clip) = tail_bounds.intersection(viewport) {
                text_widget::draw(
                    renderer,
                    style,
                    tail_bounds,
                    state.tail_paragraph.raw(),
                    text_widget::Style { color: None },
                    &tail_clip,
                );
            }
            if let Some(color) = color {
                fill_fade(
                    renderer,
                    Rectangle {
                        x: bounds.x + state.head_width - state.fade,
                        width: state.fade,
                        ..bounds
                    },
                    color,
                    false,
                );
                let reveal = state
                    .fade
                    .min(state.tail_width * 0.5)
                    .min(TAIL_REVEAL_WIDTH);
                fill_fade(
                    renderer,
                    Rectangle {
                        x: bounds.x + state.tail_x,
                        width: reveal,
                        ..bounds
                    },
                    color,
                    true,
                );
            }
            return;
        }

        // 单段模式：文本按可视宽度裁切，越界部分只落在相邻列的空闲区域。
        text_widget::draw(
            renderer,
            style,
            bounds,
            state.paragraph.raw(),
            text_widget::Style { color: None },
            &clip,
        );

        if !state.overflowed || self.fade_width <= 0.0 {
            return;
        }
        let Some(color) = color else {
            return;
        };

        // 把文本末段叠进行背景色：左端全透明，右端不透明，形成自然的淡出。
        let width = state.fade.min(visible.width);
        fill_fade(
            renderer,
            Rectangle {
                x: visible.x + visible.width - width,
                width,
                ..bounds
            },
            color,
            false,
        );
    }
}

impl<'a, Message, Theme, Renderer> From<FadingText<'a, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: text::Renderer + 'a,
{
    fn from(text: FadingText<'a, Theme, Renderer>) -> Self {
        Self::new(text)
    }
}

#[cfg(test)]
mod tests {
    use super::{effective_fade, tail_fragment};

    #[test]
    fn extension_keeps_leading_character() {
        assert_eq!(tail_fragment("客户交付_最终版_v2_已确认.md"), "认.md");
        assert_eq!(tail_fragment("archive.tar.gz"), "r.gz");
    }

    #[test]
    fn dotfile_does_not_treat_first_dot_as_extension() {
        assert_eq!(tail_fragment(".gitignore"), "nore");
    }

    #[test]
    fn cjk_and_spaces_fall_back_to_last_chars() {
        assert_eq!(tail_fragment("中文 文件名 没有扩展名"), "有扩展名");
        assert_eq!(tail_fragment("report.verylongextension"), "sion");
    }

    #[test]
    fn short_name_is_returned_whole() {
        assert_eq!(tail_fragment("a.md"), "a.md");
        assert_eq!(tail_fragment(""), "");
    }

    #[test]
    fn fade_width_is_capped_to_available_width() {
        assert!((effective_fade(56.0, 100.0) - 35.0).abs() < 0.01);
        assert!((effective_fade(56.0, 30.0) - 16.0).abs() < 0.01);
        assert!((effective_fade(56.0, 10.0) - 10.0).abs() < 0.01);
        assert_eq!(effective_fade(0.0, 100.0), 0.0);
    }
}
