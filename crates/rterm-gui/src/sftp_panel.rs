//! 中心“文件管理”面板（SFTP）。
//!
//! 在当前活动标签自己的 SFTP 通道上，提供目录浏览、导航、新建目录、删除、重命名、
//! 上传 / 下载等文件操作。列表项参考会话列表样式（单行：类型图标 + 名称 + 大小），
//! 右键菜单触发行内操作；所有写操作经异步任务执行，长耗时传输（上传 / 下载）的进度在
//! “传输”中心视图（`transfer_panel`）中汇总展示，完成后以 toast 通知反馈结果。

use crate::t;

use crate::App;
use crate::app::sftp::Message;
use crate::icons::{ICON_SIZE, Icon, icon_button};
use iced::widget::Id;
use iced::widget::text::Wrapping;
use iced::widget::tooltip::Position;
use iced::widget::{column, container, mouse_area, row, scrollable, text, text_input};
use iced::{Element, Length};
use iced_aw::widget::context_menu::ContextMenu;
use rterm_core::ConnectionStatus;

/// 文件条目与工具条图标尺寸（像素），与会话列表图标保持一致。
const ACTION_ICON_SIZE: f32 = ICON_SIZE;

/// 大小列固定宽度（像素）。目录虽不显示大小也保留该宽度，使各行的名称右边界对齐，
/// 避免因目录不显示大小而导致整体排版错位。
const SIZE_COL_W: f32 = 76.0;

/// 内联“新建文件夹”输入框的 [`Id`]，用于进入创建态时自动聚焦。
pub(crate) const NEW_DIR_INPUT_ID: Id = Id::new("sftp-new-dir");
/// 内联“重命名”输入框的 [`Id`]，用于进入重命名态时自动聚焦。
pub(crate) const RENAME_INPUT_ID: Id = Id::new("sftp-rename");

/// 内联输入态（新建目录 / 重命名）右侧的取消按钮图标尺寸（像素），略小于条目图标以保持克制。
const CANCEL_ICON_SIZE: f32 = 14.0;

/// 内联输入态（新建目录 / 重命名）右侧的取消按钮。
///
/// 采用 dismiss 图标（`Icon::Dismiss`），点击发送 [`crate::app::sftp::Message::SftpCancelDialog`] 退出输入态；
/// 图标与内边距均略小于工具条按钮，避免抢视觉焦点。
fn inline_cancel_button() -> Element<'static, Message> {
    icon_button(
        Icon::Dismiss,
        CANCEL_ICON_SIZE,
        t!("common.cancel"),
        crate::app::sftp::Message::SftpCancelDialog,
        Position::Bottom,
    )
}

/// 文件列表顶部的 “..” 合成项，用于返回上级目录。
///
/// 仅当当前目录存在上级（非根目录）时渲染。交互与普通目录一致：单击选中、双击进入上级
/// （[`crate::app::sftp::Message::SftpCd`] 跳转到上级目录），避免单击即触发导航导致双击时连跳多级。
/// 其名称固定为 ".."，并复用列表行的视觉样式与命中区域处理。
fn parent_entry(path: &str, selected: bool, hovered: bool) -> Element<'static, Message> {
    let target = crate::app::tasks::parent_path(path);
    let row_item = row![
        Icon::Folder.svg(ACTION_ICON_SIZE),
        container(
            text("..")
                .size(14)
                .width(Length::Fill)
                .wrapping(Wrapping::None)
        )
        .width(Length::Fill)
        .clip(true),
        text("")
            .size(12)
            .width(Length::Fixed(SIZE_COL_W))
            .align_x(iced::alignment::Horizontal::Right),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);
    mouse_area(
        container(row_item)
            .padding([6u16, 8u16])
            .width(Length::Fill)
            .style(move |theme| {
                let mut style = crate::theme::list_row_bg(
                    theme,
                    selected,
                    hovered,
                    ConnectionStatus::Disconnected,
                );
                style.border.radius = 6.0.into();
                style
            }),
    )
    .on_press(crate::app::sftp::Message::SftpSelect("..".to_string()))
    .on_double_click(crate::app::sftp::Message::SftpCd(target))
    .on_enter(crate::app::sftp::Message::SftpEntryEnter("..".to_string()))
    .on_exit(crate::app::sftp::Message::SftpEntryExit("..".to_string()))
    .interaction(iced::mouse::Interaction::Pointer)
    .into()
}

/// SFTP 文件管理面板视图（模态对话框遮罩由 [`crate::layout`] 在窗口顶层叠加，
/// 以覆盖整个窗口，而非仅覆盖本面板）。
pub fn view(app: &App) -> Element<'_, Message> {
    panel(app)
}

/// 面板主体：导航栏 + 工具条 + 列表（反馈横幅已迁至 toast，进度也不在本面板内）。
fn panel(app: &App) -> Element<'_, Message> {
    // 渲染当前活动标签的 SFTP 视图；无活动标签或该标签尚未打开 SFTP 时显示占位。
    let Some(sftp) = app.active_sftp() else {
        return container(text(t!("sftp.open_hint")).size(14))
            .padding(20)
            .into();
    };

    // 文件操作反馈横幅已统一交由 `crate::widget::toast` 渲染，本面板只保留文件操作区。

    // 工具栏：一排操作按钮（返回上级、刷新、新建目录、上传），当前路径输入框独立显示在下方，
    // 全部复用 icon_button 的带延迟 tooltip。
    let actions = row![
        icon_button(
            Icon::ArrowReply,
            ACTION_ICON_SIZE,
            t!("sftp.up_dir"),
            crate::app::sftp::Message::SftpParent,
            Position::Bottom,
        ),
        icon_button(
            Icon::ArrowClockwise,
            ACTION_ICON_SIZE,
            t!("sftp.refresh"),
            crate::app::sftp::Message::SftpCd(sftp.path.clone()),
            Position::Bottom,
        ),
        icon_button(
            Icon::FolderAdd,
            ACTION_ICON_SIZE,
            t!("sftp.new_dir"),
            crate::app::sftp::Message::SftpNewDirConfirm,
            Position::Bottom
        ),
        icon_button(
            Icon::DocumentArrowUp,
            ACTION_ICON_SIZE,
            t!("sftp.upload"),
            crate::app::sftp::Message::SftpPickUpload,
            Position::Bottom,
        ),
        icon_button(
            Icon::FolderArrowUp,
            ACTION_ICON_SIZE,
            t!("sftp.upload_folder"),
            crate::app::sftp::Message::SftpPickUploadFolder,
            Position::Bottom,
        ),
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);

    // 工具栏：容器宽度填满并将内部按钮整体右对齐。
    let toolbar = container(actions)
        .width(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right);

    // 当前路径以 text_input 显示：圆角 6px、内边距与文件列表项一致（上下 6、左右 8），
    // 背景与列表项常态同色；编辑后回车（on_submit）即跳转到该路径。
    let path_input = text_input(&t!("sftp.go_path_placeholder"), &sftp.path_input)
        .on_input(crate::app::sftp::Message::SftpPathInput)
        .on_submit(crate::app::sftp::Message::SftpCd(
            if sftp.path_input.is_empty() {
                sftp.path.clone()
            } else {
                sftp.path_input.clone()
            },
        ))
        .size(14)
        .padding([6u16, 8u16])
        .width(Length::Fill)
        .style(crate::ui::text_input_style);

    // 当前路径输入框作为独立一行置于工具栏下方，宽度填满，便于输入较长绝对路径。
    let path_row = row![path_input].align_y(iced::alignment::Vertical::Center);

    // 文件条目列表：每行类型图标 + 名称 + 大小，右键弹出操作菜单。
    let mut entries: Vec<Element<'_, Message>> = sftp
        .entries
        .iter()
        .map(|e| {
            let type_icon = if e.is_dir {
                Icon::Folder
            } else {
                Icon::Document
            };
            let name = e.name.clone();
            let selected = sftp.selected.as_deref() == Some(e.name.as_str());
            // 悬浮态由 SFTP 视图记录的 `hovered` 决定，与选中态共同驱动行背景。
            let hovered = sftp.hovered.as_deref() == Some(e.name.as_str());

            // 内联“重命名”：命中原名时该位置渲染为输入框行（默认原名称），
            // 回车提交；输入为空或与原名相同都视为取消（由 update 处理）。
            if let Some((orig, cur)) = &sftp.renaming
                && orig == &name
            {
                let rename_input = text_input(&name, cur)
                    .id(RENAME_INPUT_ID)
                    .on_input(crate::app::sftp::Message::SftpRenameInput)
                    .on_submit(crate::app::sftp::Message::SftpRenameSubmit)
                    .size(14)
                    .width(Length::Fill)
                    .style(crate::ui::text_input_style);
                let rename_row = row![
                    type_icon.svg(ACTION_ICON_SIZE),
                    rename_input,
                    inline_cancel_button(),
                ]
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center);
                return container(rename_row)
                    .padding([6u16, 8u16])
                    .style(|theme| {
                        let mut style = crate::theme::plain_background(
                            crate::theme::custom_palette(theme).surface,
                        );
                        style.border.radius = 6.0.into();
                        style
                    })
                    .into();
            }

            // 大小列：目录不显示大小、也不以占位符填充，但保留固定宽度以保持对齐。
            let size_text = if e.is_dir {
                String::new()
            } else {
                format_size(e.size)
            };
            let name_text = text(name.clone())
                .size(14)
                .width(Length::Fill)
                .wrapping(Wrapping::None);
            let name_clipped = container(name_text).width(Length::Fill).clip(true);

            let row_item = row![
                type_icon.svg(ACTION_ICON_SIZE),
                name_clipped,
                text(size_text)
                    .size(12)
                    .width(Length::Fixed(SIZE_COL_W))
                    .align_x(iced::alignment::Horizontal::Right),
            ]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center);

            // 选中态与悬浮态共同驱动行背景样式。
            // padding 与背景样式放在 mouse_area 内部，使整行视觉区域（含内边距）都在命中范围内，
            // 否则边缘 6px/8px 处于交互区域之外，出现“看得见但点不到、悬浮不亮”的死区。
            let item: Element<'_, Message> = mouse_area(
                container(row_item)
                    .padding([6u16, 8u16])
                    .width(Length::Fill)
                    .style(move |theme| {
                        let mut style = crate::theme::list_row_bg(
                            theme,
                            selected,
                            hovered,
                            ConnectionStatus::Disconnected,
                        );
                        style.border.radius = 6.0.into();
                        style
                    }),
            )
            .on_press(crate::app::sftp::Message::SftpSelect(name.clone()))
            .on_double_click(if e.is_dir {
                crate::app::sftp::Message::SftpCd(crate::app::tasks::join_path(&sftp.path, &name))
            } else {
                crate::app::sftp::Message::SftpSelect(name.clone())
            })
            .on_enter(crate::app::sftp::Message::SftpEntryEnter(name.clone()))
            .on_exit(crate::app::sftp::Message::SftpEntryExit(name.clone()))
            .interaction(iced::mouse::Interaction::Pointer)
            .into();

            // 右键菜单提供行内操作（进入 / 下载、重命名、删除、复制路径、属性）。
            let path = sftp.path.clone();
            let is_dir = e.is_dir;
            let item: Element<'_, Message> = ContextMenu::new(item, move || {
                menu_actions(name.clone(), is_dir, path.clone())
            })
            .into();
            item
        })
        .collect();

    // 内联“新建文件夹”行：处于创建态时在列表顶部插入 folder 图标 + text_input，
    // 样式与列表项一致；回车（on_submit）提交，输入为空则取消（由 update 处理）。
    if let Some(name) = &sftp.creating_dir {
        let create_input = text_input(&t!("sftp.new_dir_name"), name)
            .id(NEW_DIR_INPUT_ID)
            .on_input(crate::app::sftp::Message::SftpNewDirInput)
            .on_submit(crate::app::sftp::Message::SftpNewDirSubmit)
            .size(14)
            .width(Length::Fill)
            .style(crate::ui::text_input_style);
        let create_row = row![
            Icon::Folder.svg(ACTION_ICON_SIZE),
            create_input,
            inline_cancel_button(),
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center);
        let create_item: Element<'_, Message> = container(create_row)
            .padding([6u16, 8u16])
            .style(|theme| {
                let mut style =
                    crate::theme::plain_background(crate::theme::custom_palette(theme).surface);
                style.border.radius = 6.0.into();
                style
            })
            .into();
        entries.insert(0, create_item);
    }

    // 列表顶部插入 “..” 返回上级目录项：仅在当前目录存在上级（非根目录）时显示，
    // 使根目录（如 "/")不出现无意义的 “..”。
    if crate::app::tasks::parent_path(&sftp.path) != sftp.path {
        let selected = sftp.selected.as_deref() == Some("..");
        let hovered = sftp.hovered.as_deref() == Some("..");
        entries.insert(0, parent_entry(&sftp.path, selected, hovered));
    }

    // 预留右侧空间给覆盖式滚动条，避免其出现时遮挡行尾内容。
    let list = scrollable(column(entries).spacing(4).padding(iced::Padding {
        right: 12.0,
        ..Default::default()
    }))
    .height(Length::Fill);

    // 传输进度统一由「传输」中心视图（transfer_panel，与会话 / 文件并列切换）汇总展示，
    // 本面板只保留文件操作区。
    column![toolbar, path_row, list]
        .spacing(8)
        .padding(10)
        .into()
}

/// 单条“更多 / 右键”菜单内容：目录给「进入」、文件给「下载」，另加重命名 / 删除 /
/// 复制路径 / 属性。
fn menu_actions<'a>(name: String, is_dir: bool, path: String) -> Element<'a, Message> {
    let mut actions = column![];
    if is_dir {
        actions = actions.push(crate::ui::menu_entry(
            t!("sftp.enter"),
            crate::app::sftp::Message::SftpCd(crate::app::tasks::join_path(&path, &name)),
        ));
    } else {
        actions = actions.push(crate::ui::menu_entry(
            t!("sftp.download"),
            crate::app::sftp::Message::SftpPickDownload(name.clone()),
        ));
    }
    actions = actions
        .push(crate::ui::menu_entry(
            t!("sftp.rename"),
            crate::app::sftp::Message::SftpRenameConfirm(name.clone()),
        ))
        .push(crate::ui::menu_entry(
            t!("sftp.delete"),
            crate::app::sftp::Message::SftpDeleteConfirm(name.clone()),
        ))
        .push(crate::ui::menu_entry(
            t!("sftp.copy_path"),
            crate::app::sftp::Message::SftpCopyPath(crate::app::tasks::join_path(&path, &name)),
        ))
        .push(crate::ui::menu_entry(
            t!("sftp.properties"),
            crate::app::sftp::Message::SftpShowProperties(name.clone()),
        ));
    crate::ui::menu_container(actions.into())
}

/// 将字节数格式化为可读大小（B / KB / MB / GB / TB），供列表与属性框复用。
pub(crate) fn format_size(bytes: u64) -> String {
    /// 可读文件大小的单位档位（字节→TB），`format_size` 逐档相除取首位。
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// 将 SFTP 返回的原始权限位格式化为 `drwxr-xr-x` 形式，供属性框展示。
///
/// 首位为文件类型（目录 / 符号链接 / 常规文件等），其后三组分别为属主、属组、其他的
/// 读 / 写 / 执行位；未置位以 `-` 填充。
pub(crate) fn format_permissions(mode: u32) -> String {
    /// 文件类型位对应的展示字符（取 `mode & 0o170000`）。
    const TYPE_CHARS: [(u32, char); 6] = [
        (0o040_000, 'd'), // 目录
        (0o120_000, 'l'), // 符号链接
        (0o010_000, 'p'), // 命名管道
        (0o020_000, 'c'), // 字符设备
        (0o060_000, 'b'), // 块设备
        (0o140_000, 's'), // 套接字
    ];
    let file_type = mode & 0o170_000;
    let type_char = TYPE_CHARS
        .iter()
        .find(|(bits, _)| *bits == file_type)
        .map_or('-', |(_, c)| *c);

    let mut out = String::with_capacity(10);
    out.push(type_char);
    // 属主 / 属组 / 其他各占 3 位，从高位到低位依次是读 / 写 / 执行。
    for shift in [6u32, 3, 0] {
        for (bit, ch) in [(0o4, 'r'), (0o2, 'w'), (0o1, 'x')] {
            out.push(if (mode >> shift) & bit != 0 { ch } else { '-' });
        }
    }
    out
}
