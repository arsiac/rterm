use crate::widget::term::settings::FontSettings;
use iced::{Font, Size};
use iced_core::{
    Text,
    alignment::Vertical,
    text::{Alignment, LineHeight, Paragraph, Shaping as TextShaping},
};
use iced_graphics::text::paragraph;

/// 终端渲染所用的字体状态：字号、字体族、缩放系数与单字测量尺寸。
#[derive(Debug, Clone)]
pub struct TermFont {
    /// 字号（像素）。
    pub(crate) size: f32,
    /// 字体族（`iced::Font`，可为内置或已缓存的族名）。
    pub(crate) font_type: Font,
    /// 额外缩放系数（影响行高与字形网格）。
    pub(crate) scale_factor: f32,
    /// 单个字形「m」的测量尺寸（宽、高）。
    pub(crate) measure: Size<f32>,
}

impl TermFont {
    /// 由 [`FontSettings`] 构造 `TermFont`，并初次测量单字尺寸。
    pub fn new(settings: FontSettings) -> Self {
        Self {
            size: settings.size,
            font_type: settings.font_type,
            scale_factor: settings.scale_factor,
            measure: font_measure(settings.size, settings.scale_factor, settings.font_type),
        }
    }

    /// 在字号或字体族变化后重新测量单字尺寸。
    pub fn sync(&mut self) {
        self.measure = font_measure(self.size, self.scale_factor, self.font_type)
    }
}

/// 以单字「m」渲染测量其最小包围盒尺寸，作为字符网格基准。
fn font_measure(font_size: f32, scale_factor: f32, font_type: Font) -> Size<f32> {
    let paragraph = paragraph::Paragraph::with_text(Text {
        content: "m",
        font: font_type,
        size: iced_core::Pixels(font_size),
        align_y: Vertical::Center,
        align_x: Alignment::Center,
        shaping: TextShaping::Advanced,
        line_height: LineHeight::Relative(scale_factor),
        bounds: Size::INFINITE,
        wrapping: iced_core::text::Wrapping::Glyph,
    });

    paragraph.min_bounds()
}
