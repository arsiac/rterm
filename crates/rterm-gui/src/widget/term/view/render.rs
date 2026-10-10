//! `TerminalView` 的绘制：把后端渲染内容绘成背景、文本、光标、超链接下划线与滚动条。

use super::scrollbar::{
    SCROLLBAR_ALPHA, SCROLLBAR_ALPHA_DRAG, SCROLLBAR_ALPHA_HOVER, SCROLLBAR_RADIUS,
    scrollbar_geometry,
};
use super::{TerminalView, TerminalViewState};
use alacritty_terminal::index::{Column, Line, Point as TerminalGridPoint};
use alacritty_terminal::term::{TermMode, cell};
use alacritty_terminal::vte::ansi::{self as ansi, CursorShape, NamedColor};
use iced::alignment::Vertical;
use iced::font::{Style as FontStyle, Weight as FontWeight};
use iced::widget::canvas::{Path, Text};
use iced::{Color, Point, Rectangle, Size};
use iced_core::text::{Alignment, LineHeight, Shaping};
use iced_graphics::geometry::Stroke;

/// 把后端快照绘成几何图元：背景、文本、光标、下划线、滚动条。
pub(super) fn draw(
    view: &TerminalView<'_>,
    state: &TerminalViewState,
    renderer: &mut iced::Renderer,
    layout: iced::advanced::Layout<'_>,
    viewport: &Rectangle,
) {
    let content = view.term.backend.renderable_content();
    let term_size = content.terminal_size;
    let cell_width = term_size.cell_width as f32;
    let cell_height = term_size.cell_height as f32;
    let font_size = view.term.font.size;
    let font_scale_factor = view.term.font.scale_factor;
    let layout_offset_x = layout.position().x;
    let layout_offset_y = layout.position().y;
    let layout_size = layout.bounds().size();

    // 焦点 / 滚动条开关变化但布局尺寸不变时，几何缓存直接复用旧绘制结果，导致光标
    // （实心/空心）与滚动条不随状态切换刷新。此处检测变化并清缓存，强制本帧重绘。
    if view.focused != state.last_focus.get() || view.scrollbar != state.last_scrollbar.get() {
        view.term.cache.clear();
        state.last_focus.set(view.focused);
        state.last_scrollbar.set(view.scrollbar);
    }

    let geom = view.term.cache.draw(renderer, viewport.size(), |frame| {
        // 裁剪矩形：四边内缩 padding，使右侧和底部内容不溢出到容器边缘。
        let clip_rect = Rectangle::new(
            Point::new(
                layout_offset_x + view.padding,
                layout_offset_y + view.padding,
            ),
            Size::new(
                layout_size.width - view.padding * 2.0,
                layout_size.height - view.padding * 2.0,
            ),
        );

        frame.with_clip(clip_rect, |frame| {
            // 预计算内循环使用的常量
            let display_offset = content.display_offset as f32;
            let cell_size = Size::new(cell_width, cell_height);
            let half_w = cell_width * 0.5;
            let half_h = cell_height * 0.5;
            // 部件全局背景色必须一致，故默认背景取调色板 Background 色。
            let default_bg = view
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
                let x = layout_offset_x + view.padding + (col * cell_width);
                let y = layout_offset_y
                    + view.padding
                    + (((line as f32) + display_offset) * cell_height);
                let cell_center_y = y + half_h;
                let cell_center_x = x + half_w;

                // 解析该格的颜色
                let mut fg = view.term.theme.get_color(cell.fg);
                let mut bg = view.term.theme.get_color(cell.bg);

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
                        .with_padding(view.padding);
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
                            .with_padding(view.padding)
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
                        .with_padding(view.padding);
                }

                // 绘制下划线（悬浮命中的链接格，或带 UNDERLINE 标志的格）。
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
                    let cursor_color = view.term.theme.get_color(content.cursor.fg);
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
                    if !view.focused {
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
                    let mut font = view.term.font.font_type;
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
                let cell_x = layout_offset_x + view.padding + cursor.column.0 as f32 * cell_width;
                let cell_y = layout_offset_y
                    + view.padding
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
                    font: view.term.font.font_type,
                    size: iced_core::Pixels(font_size),
                    color: view
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
        if view.scrollbar
            && let Some((_track, thumb)) =
                scrollbar_geometry(layout.bounds(), view.padding, content)
        {
            let base = view
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
                &Path::rounded_rectangle(thumb.position(), thumb.size(), SCROLLBAR_RADIUS.into()),
                Color { a: alpha, ..base },
            );
        }
    });

    use iced::advanced::graphics::geometry::Renderer as _;
    renderer.draw_geometry(geom);
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
