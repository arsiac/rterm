//! SFTP 文件管理面板的模态对话框与统一遮罩层。
//!
//! 面板内统一为“标题 / 分隔线 / 正文 / 确认·取消按钮”结构。所有对话框经
//! [`view`] 分发，由 [`App`] 的 `sftp.dialog` 状态驱动。

use crate::t;

use crate::App;
use crate::app::sftp::Message;
use crate::sftp_panel;
use crate::state::SftpDialog;
use iced::widget::{column, container, row, text};
use iced::{Color, Element, Length, Theme};
use iced_core::layout::{Layout, Limits, Node};
use iced_core::mouse::{self, Cursor, Interaction};
use iced_core::widget::operation;
use iced_core::widget::tree::{self, Tree};
use iced_core::{Clipboard, Event, Rectangle, Shell, Size, Vector};
use iced_core::{overlay, renderer};
use rterm_core::FileEntry;

/// 将子元素包装为全屏模态遮罩：半透明黑底 + 居中 + 拦截鼠标 / 键盘事件穿透。
///
/// 与消息类型无关（遮罩层只做布局 / 事件拦截），故对消息类型 `M` 泛型，
/// 既可用于 SFTP 模块消息，也可用于主密码 / 主机密钥等顶层消息对话框。
pub fn overlay_wrap<'a, M: Clone + 'a>(child: Element<'a, M>) -> Element<'a, M> {
    let overlay = container(child)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_: &Theme| container::Style {
            background: Some(iced::Background::Color(Color::from_rgba(
                0.0, 0.0, 0.0, 0.30,
            ))),
            ..Default::default()
        })
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    modal(overlay.into())
}

/// 包装模态遮罩内容：在子元素处理事件之后捕获鼠标按下与键盘事件，阻止穿透到下层。
///
/// iced 内建 `opaque` 只捕获鼠标按下，键盘事件仍会下探到下层组件（如终端持有键盘焦点时
/// 照常把按键写入 PTY），故此处对键盘一并捕获。捕获发生在内层处理之后，故弹窗自身控件
/// 仍能收到按键，仅阻断向窗口其余部分的传播。
fn modal<'a, M, T, R>(content: Element<'a, M, T, R>) -> Element<'a, M, T, R>
where
    M: 'a,
    T: 'a,
    R: iced_core::Renderer + 'a,
{
    Element::new(Modal { content })
}

/// [`modal`] 的包装组件；仅拦截穿透事件，布局与绘制全部转发给子元素。
struct Modal<'a, M, T, R> {
    content: Element<'a, M, T, R>,
}

impl<M, T, R> iced_core::Widget<M, T, R> for Modal<'_, M, T, R>
where
    R: iced_core::Renderer,
{
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &R, limits: &Limits) -> Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut R,
        theme: &T,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &R,
        operation: &mut dyn operation::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &R,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        // 鼠标按下仅在命中遮罩范围内拦截；键盘与位置无关，一律拦截。
        let block = matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(_)) | Event::Keyboard(_)
        );

        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );

        if block && (matches!(event, Event::Keyboard(_)) || cursor.is_over(layout.bounds())) {
            shell.capture_event();
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &R,
    ) -> Interaction {
        let interaction = self
            .content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer);

        if interaction == Interaction::None && cursor.is_over(layout.bounds()) {
            Interaction::Idle
        } else {
            interaction
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &R,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, T, R>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

/// 根据当前打开的对话框返回遮罩层元素；无对话框时返回 `None`。
///
/// 该函数仅在当前活动标签的 SFTP 视图存在对话框时返回内容（`App::active_sftp`）；
/// 实际由 `crate::layout` 用 `iced::widget::stack` 叠加，覆盖**整个窗口**而非仅文件列表。
pub fn view(app: &App) -> Option<Element<'_, Message>> {
    let dialog = app.active_sftp().and_then(|s| s.dialog.as_ref())?;
    let content = match dialog {
        SftpDialog::Delete { name, is_dir } => delete_dialog(name, *is_dir),
        SftpDialog::OverwriteDownload { name, .. } => overwrite_dialog(name),
        SftpDialog::Properties { entry, path } => properties_dialog(entry, path),
    };
    Some(overlay_wrap(content))
}

/// 删除确认弹窗：按条目类型展示「删除文件 / 目录」警告与确认按钮。
fn delete_dialog(name: &str, is_dir: bool) -> Element<'_, Message> {
    let kind = if is_dir {
        t!("sftp.dir")
    } else {
        t!("sftp.file")
    };
    dialog_panel(
        t!("sftp.delete_title"),
        column![text(t!("sftp.delete_confirm", kind => kind, name => name)).size(14),],
        t!("sftp.delete"),
        true,
    )
}

/// 下载覆盖确认弹窗：提示本地已存在同名文件，确认后将覆盖。
fn overwrite_dialog(name: &str) -> Element<'_, Message> {
    dialog_panel(
        t!("sftp.overwrite_title"),
        column![text(t!("sftp.overwrite_confirm", name => name)).size(14),],
        t!("sftp.overwrite"),
        true,
    )
}

/// 只读信息框，展示名称 / 类型 / 大小 / 修改时间 / 完整路径。
fn properties_dialog<'a>(entry: &'a FileEntry, path: &'a str) -> Element<'a, Message> {
    let kind = if entry.is_dir {
        t!("sftp.dir")
    } else {
        t!("sftp.file")
    };
    let size = if entry.is_dir {
        "—".to_string()
    } else {
        sftp_panel::format_size(entry.size)
    };
    let modified = entry.modified.clone().unwrap_or_else(|| t!("sftp.unknown"));
    // 服务端未返回权限 / 属主 / 属组时退化为「未知」。
    let unknown = t!("sftp.unknown");
    let permissions = entry
        .permissions
        .map_or_else(|| unknown.clone(), sftp_panel::format_permissions);
    let user = entry.user.clone().unwrap_or_else(|| unknown.clone());
    let group = entry.group.clone().unwrap_or(unknown);
    // 只读信息框：没有可取消的操作，只需居中的单个「关闭」按钮，故左按钮传 None。
    crate::ui::dialog_panel(
        t!("sftp.properties"),
        column![
            prop_row(t!("sftp.prop_name"), entry.name.clone(), 72.0),
            prop_row(t!("sftp.prop_type"), kind.to_string(), 72.0),
            prop_row(t!("sftp.prop_size"), size, 72.0),
            prop_row(t!("sftp.prop_modified"), modified, 72.0),
            prop_row(t!("sftp.prop_permissions"), permissions, 72.0),
            prop_row(t!("sftp.prop_owner"), user, 72.0),
            prop_row(t!("sftp.prop_group"), group, 72.0),
            prop_row(t!("sftp.prop_path"), path.to_string(), 72.0),
        ]
        .spacing(10),
        None,
        360.0,
        None,
        crate::ui::DialogButton {
            label: t!("common.close"),
            on_press: Message::SftpDialogConfirm,
            style: crate::ui::DialogBtnStyle::Emphasis { danger: false },
        },
    )
}

/// 属性框中的单行“标签 : 值”；`label_width` 为标签列固定宽度（按最长标签调整）。
///
/// 标签复用共享的 [`crate::ui::field_label`]（13px 次级色），与表单弹窗的字段标签同一层级；
/// 与消息类型无关，故对消息类型 `M` 泛型（SFTP 属性框用 `sftp::Message`、主机密钥框用顶层
/// `Message`，均可复用）。
pub(crate) fn prop_row<'a, M: Clone + 'a>(
    label: impl Into<String>,
    value: String,
    label_width: f32,
) -> Element<'a, M> {
    row![
        container(crate::ui::field_label(label)).width(Length::Fixed(label_width)),
        // WordOrGlyph：指纹这类无空格长 token 按词换行放不下时退化为字符级断行，
        // 否则整个 token 溢出面板。
        text(value)
            .size(14)
            .width(Length::Fill)
            .wrapping(text::Wrapping::WordOrGlyph),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center)
    .into()
}

/// SFTP 文件操作确认 / 取消弹窗
fn dialog_panel<'a>(
    title: impl Into<String>,
    body: impl Into<Element<'a, Message>>,
    confirm_label: impl Into<String>,
    danger: bool,
) -> Element<'a, Message> {
    crate::ui::dialog_panel(
        title,
        body,
        None,
        360.0,
        Some(crate::ui::DialogButton {
            label: t!("common.cancel"),
            on_press: crate::app::sftp::Message::SftpCancelDialog,
            style: crate::ui::DialogBtnStyle::Neutral,
        }),
        crate::ui::DialogButton {
            label: confirm_label.into(),
            on_press: crate::app::sftp::Message::SftpDialogConfirm,
            style: crate::ui::DialogBtnStyle::Emphasis { danger },
        },
    )
}
