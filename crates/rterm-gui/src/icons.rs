//! 图标模块：从 `icons/` 目录内嵌的 SVG 资源构建 iced [`Svg`] 部件。
//!
//! 使用 Fluent UI System Icons（filled 16px）在编译期内嵌，运行时无文件依赖。图标为单色
//! 填充，默认按当前主题的次要文本色（`theme::custom_palette(theme).text_secondary`）着色，
//! 因而在深色 / 浅色及任意 iced 主题下均自动适配，调用点无需改动。

use iced::widget::button;
use iced::widget::svg::{Handle, Svg};
use iced::widget::text;
use iced::widget::tooltip as tooltip_widget;
use iced::widget::tooltip::Position;
use iced::{Color, Element, Length, Padding};

/// 图标默认像素尺寸。
pub const ICON_SIZE: f32 = 20.0;

/// 图标按钮的默认内边距。
pub const ICON_BUTTON_DEFAULT_PADDING: Padding = Padding::new(5f32);

/// 应用窗口图标（512×512 PNG），用于运行期窗口与任务栏显示。
///
/// 由 `icons/app/icon.svg` 派生（见 `icons/app/`），运行期无文件依赖。
pub const WINDOW_ICON: &[u8] = include_bytes!("../icons/app/icon-512.png");

/// 将图标文件名拼接为指向 `icons/` 目录的内嵌字节宏。
macro_rules! svg_bytes {
    ($name:expr) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/icons/",
            $name,
            ".svg"
        ))
    };
}

/// 全部可用图标。变体名对应 `icons/` 下文件名（去掉 `-16-filled` / `-24-filled` 后缀，转 PascalCase），
/// 集中枚举以避免散落的字符串文件名常量。
pub enum Icon {
    /// 活动栏“会话管理”。
    ListBar,
    /// 活动栏“文件管理”（已连接时的正常图标）。
    FolderMultiple,
    /// 活动栏“文件管理”（图标含禁止符号，用于表达未连接时不可用）。
    FolderJunk,
    /// 目录条目标记（与「折叠 / 展开」无关，后者用 `ChevronCircleRight` / `ChevronCircleDown`）；
    /// 也用作设置面板中「打开日志目录」等目录类按钮图标。
    Folder,
    /// 普通文件条目标记。
    Document,
    /// 新建 / 添加。
    Add,
    /// 上传文件。
    CloudArrowUp,
    /// 下载文件。
    CloudArrowDown,
    /// 新建目录。
    FolderAdd,
    /// 返回上级目录。
    ArrowReply,
    /// 刷新当前目录。
    ArrowClockwise,
    /// 取消操作。
    Dismiss,
    /// 应用设置（活动栏底部入口）。
    Settings,
    /// 分组收起态（指向右，点击展开）。
    ChevronCircleRight,
    /// 分组展开态（指向下，点击收起）。
    ChevronCircleDown,
    /// 标签列表 dropdown 收起态（指向下，点击展开）。
    ChevronDown,
    /// 标签列表 dropdown 展开态（指向上，点击收起）。
    ChevronUp,
    /// 导出会话到文件。
    ArrowExport,
    /// 从文件导入会话。
    ArrowImport,
    /// 传输面板（上传 + 下载，活动栏入口与面板标题）。
    ArrowSort,
    /// 传输完成。
    Checkmark,
    /// 传输失败。
    Warning,
}

impl Icon {
    /// 返回图标对应的内嵌 SVG 字节数据（编译期嵌入，生命周期为 `'static`）。
    pub fn bytes(&self) -> &'static [u8] {
        match self {
            Icon::ListBar => svg_bytes!("list-bar-16-filled"),
            Icon::FolderMultiple => svg_bytes!("folder-multiple-16-filled"),
            Icon::FolderJunk => svg_bytes!("folder-junk-24-filled"),
            Icon::Folder => svg_bytes!("folder-16-filled"),
            Icon::Document => svg_bytes!("document-16-filled"),
            Icon::Add => svg_bytes!("add-16-filled"),
            Icon::CloudArrowUp => svg_bytes!("cloud-arrow-up-16-filled"),
            Icon::CloudArrowDown => svg_bytes!("cloud-arrow-down-16-filled"),
            Icon::FolderAdd => svg_bytes!("folder-add-16-filled"),
            Icon::ArrowReply => svg_bytes!("arrow-reply-16-filled"),
            Icon::ArrowClockwise => svg_bytes!("arrow-clockwise-16-filled"),
            Icon::Dismiss => svg_bytes!("dismiss-16-filled"),
            Icon::Settings => svg_bytes!("settings-16-filled"),
            Icon::ChevronCircleRight => svg_bytes!("chevron-circle-right-16-filled"),
            Icon::ChevronCircleDown => svg_bytes!("chevron-circle-down-16-filled"),
            Icon::ChevronDown => svg_bytes!("chevron-down-16-filled"),
            Icon::ChevronUp => svg_bytes!("chevron-up-16-filled"),
            Icon::ArrowExport => svg_bytes!("arrow-export-16-filled"),
            Icon::ArrowImport => svg_bytes!("arrow-import-16-filled"),
            Icon::ArrowSort => svg_bytes!("arrow-sort-16-filled"),
            Icon::Checkmark => svg_bytes!("checkmark-16-filled"),
            Icon::Warning => svg_bytes!("warning-16-filled"),
        }
    }

    /// 构建指定像素大小的 [`Svg`] 部件，并按当前主题的次要文本色自动着色，
    /// 从而对任意 iced 主题（深色 / 浅色等）均保持可见。
    pub fn svg(&self, size: f32) -> Svg<'static> {
        Svg::new(Handle::from_memory(self.bytes()))
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .style(|theme, _status| iced::widget::svg::Style {
                color: Some(crate::theme::custom_palette(theme).text_secondary),
            })
    }

    /// 构建指定像素大小、以指定 `color` 重绘的 [`Svg`] 部件，用于按状态调整图标存在感
    /// （如禁用态灰显）。
    pub fn svg_with_color(&self, size: f32, color: Color) -> Svg<'static> {
        Svg::new(Handle::from_memory(self.bytes()))
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .style(move |_theme, _status| iced::widget::svg::Style { color: Some(color) })
    }
}

/// 构建一个图标按钮，并包裹悬停 `tooltip` 显示 `label`。
///
/// 用图标替代文字按钮，同时满足“无文字提示、悬停显示 tooltip”的需求。图标 SVG 为编译期
/// 内嵌的 `'static` 数据，`label` 接受任意字符串（含运行时翻译结果）。内边距固定为
/// [`ICON_BUTTON_DEFAULT_PADDING`]（四边 5px，比 iced 按钮默认的上 5 / 左右 10 更方），
/// 适用于一般场景。
pub fn icon_button<Message>(
    icon: Icon,
    size: f32,
    label: impl Into<String>,
    on_press: Message,
    position: Position,
) -> Element<'static, Message>
where
    Message: Clone + 'static,
{
    icon_button_with_padding(
        icon,
        size,
        label,
        on_press,
        position,
        ICON_BUTTON_DEFAULT_PADDING,
    )
}

/// 构建一个带自定义内边距的图标按钮，其余行为同 [`icon_button`]。
///
/// 需要压低占用高度的区域（如工具条）可传入更紧凑的内边距，避免默认的 5px 留白使整排过高。
pub fn icon_button_with_padding<Message>(
    icon: Icon,
    size: f32,
    label: impl Into<String>,
    on_press: Message,
    position: Position,
    padding: iced::Padding,
) -> Element<'static, Message>
where
    Message: Clone + 'static,
{
    let btn = button(icon.svg(size))
        .on_press(on_press)
        .padding(padding)
        .style(|theme, status| crate::theme::icon_button_style(theme, status, false));
    tooltip_widget(btn, text(label.into()), position)
        .delay(iced::time::Duration::from_millis(
            crate::theme::TOOLTIP_DELAY_MS,
        ))
        .style(crate::theme::tooltip_style)
        .into()
}
