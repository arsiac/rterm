//! SFTP 文件管理面板的模态对话框。
//!
//! 面板内统一为“标题 / 分隔线 / 正文 / 确认·取消按钮”结构，经
//! [`crate::modal::overlay_wrap_dismissable`] 叠加为窗口级模态。所有对话框经 [`view`] 分发，
//! 由 [`App`] 的 `sftp.dialog` 状态驱动。

use crate::app::tasks::apply_upload_decisions;
use crate::icons::Icon;
use crate::modal::overlay_wrap_dismissable;
use crate::t;
use crate::theme;

use crate::App;
use crate::app::sftp::Message;
use crate::sftp_panel;
use crate::state::{ConflictDecision, ConflictRow, SftpDialog};
use crate::ui::{SegmentOption, SegmentTone};
use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Background, Border, Element, Length, Theme};
use rterm_core::FileEntry;

/// 根据当前打开的对话框返回遮罩层元素；无对话框时返回 `None`。
///
/// 该函数仅在当前活动标签的 SFTP 视图存在对话框时返回内容（`App::active_sftp`）；
/// 实际由 `crate::layout` 用 `iced::widget::stack` 叠加，覆盖**整个窗口**而非仅文件列表。
pub fn view(app: &App) -> Option<Element<'_, Message>> {
    let view = app.active_sftp()?;
    let dialog = view.dialog.as_ref()?;
    let content = match dialog {
        SftpDialog::Delete { name, is_dir } => delete_dialog(name, *is_dir),
        SftpDialog::OverwriteDownload {
            name, is_folder, ..
        } => overwrite_dialog(name, *is_folder),
        SftpDialog::UploadConflicts { items, rows } => {
            upload_conflicts_dialog(items, rows, 1 + view.queued_conflicts.len())
        }
        SftpDialog::Properties { entry, path } => properties_dialog(entry, path),
    };
    // 属性框无取消动作，遮罩单击对齐其「关闭」按钮；确认框对齐「取消」。
    let close = match dialog {
        SftpDialog::Properties { .. } => Message::SftpDialogConfirm,
        _ => Message::SftpCancelDialog,
    };
    Some(overlay_wrap_dismissable(content, close))
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

/// 下载同名确认弹窗：文件问覆盖；目录问整树合并（框形同一套，只换措辞与按钮文案）。
fn overwrite_dialog(name: &str, is_folder: bool) -> Element<'_, Message> {
    if is_folder {
        dialog_panel(
            t!("sftp.download_folder"),
            column![text(t!("sftp.download_folder_merge", name => name)).size(14),],
            t!("sftp.download"),
            true,
        )
    } else {
        dialog_panel(
            t!("sftp.overwrite_title"),
            column![text(t!("sftp.overwrite_confirm", name => name)).size(14),],
            t!("sftp.overwrite"),
            true,
        )
    }
}

/// 上传冲突逐条决策弹窗：标题+批次徽标 / 副标题 / 应用到全部工具条 / 逐条目行卡片 / 页脚汇总。
///
/// 配色一律取应用主题（`custom_palette` / `accent_color` / `DANGER`）；`batch_total` > 1 时标题旁出徽标。
fn upload_conflicts_dialog<'a>(
    items: &[(std::path::PathBuf, String)],
    rows: &[ConflictRow],
    batch_total: usize,
) -> Element<'a, Message> {
    let kept = apply_upload_decisions(items, rows).len();
    let skipped = items.len() - kept;
    let summary = if skipped == 0 {
        t!("sftp.foot_sum_all", total => items.len())
    } else {
        t!("sftp.foot_sum_mixed", total => items.len(), up => kept, skipped => skipped)
    };
    let primary_label = if kept == 0 {
        t!("sftp.cancel_upload")
    } else {
        t!("sftp.upload_count", count => kept)
    };

    let mut title_line = row![text(t!("sftp.conflict_title")).size(15)].spacing(10);
    if batch_total > 1 {
        title_line = title_line.push(
            container(
                text(t!("sftp.batch_badge", current => 1usize, total => batch_total)).size(11.5),
            )
            .padding([1, 10])
            .style(|theme: &Theme| {
                let accent = theme::accent_color(theme);
                container::Style {
                    background: Some(Background::Color(iced::Color::from_rgba(
                        accent.r, accent.g, accent.b, 0.20,
                    ))),
                    text_color: Some(accent),
                    border: Border {
                        radius: 999.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }),
        );
    }

    let toolbar = row![
        container(text(t!("sftp.apply_all")).size(12)).style(weak_text),
        tool_button(
            t!("sftp.apply_overwrite"),
            Message::SftpConflictApplyAll(ConflictDecision::Overwrite),
        ),
        tool_button(
            t!("sftp.apply_skip"),
            Message::SftpConflictApplyAll(ConflictDecision::Skip),
        ),
        tool_button(
            t!("sftp.apply_rename"),
            Message::SftpConflictApplyAll(ConflictDecision::Rename),
        ),
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);

    let mut list = column![].spacing(8);
    for (index, row_state) in rows.iter().enumerate() {
        list = list.push(conflict_row(row_state, index));
    }

    let foot = row![
        container(text(summary).size(12))
            .style(weak_text)
            .width(Length::Fill),
        button(text(t!("common.cancel")).size(13))
            .padding([6, 18])
            .style(crate::ui::dialog_btn_style_neutral)
            .on_press(Message::SftpCancelDialog),
        button(text(primary_label).size(13))
            .padding([6, 18])
            .style(|theme, status| crate::ui::dialog_btn_style(theme, status, false))
            .on_press(Message::SftpDialogConfirm),
    ]
    .spacing(10)
    .align_y(iced::alignment::Vertical::Center);

    container(
        column![
            container(
                column![
                    title_line,
                    container(text(t!("sftp.conflict_hint", count => rows.len())).size(12.5))
                        .style(secondary_text),
                ]
                .spacing(6),
            )
            .width(Length::Fill)
            .padding(iced::Padding {
                top: 18.0,
                right: 20.0,
                bottom: 10.0,
                left: 20.0
            }),
            container(toolbar)
                .width(Length::Fill)
                .padding(iced::Padding {
                    top: 2.0,
                    right: 20.0,
                    bottom: 10.0,
                    left: 20.0
                }),
            // 内滚 + 外层封顶（≈6 行），沿用 known_hosts 列表的做法与细滚动条基座。
            container(
                container(
                    scrollable(list)
                        .width(Length::Fill)
                        .height(Length::Shrink)
                        .direction(crate::ui::thin_scrollbar_direction()),
                )
                .width(Length::Fill)
                .max_height(336.0),
            )
            .width(Length::Fill)
            .padding(iced::Padding {
                top: 0.0,
                right: 14.0,
                bottom: 10.0,
                left: 14.0
            }),
            crate::ui::hairline(),
            container(foot).width(Length::Fill).padding([13, 20]),
        ]
        .width(Length::Fill),
    )
    .width(520.0)
    .style(crate::ui::dialog_panel_style(None))
    .into()
}

/// 冲突单行：图标 + 名称（裁剪 + 悬浮全名）+ 可选目录风险提示 / 重命名预览 + 分段三档钮。
fn conflict_row<'a>(row_state: &ConflictRow, index: usize) -> Element<'a, Message> {
    let icon = if row_state.is_dir {
        Icon::Folder
    } else {
        Icon::Document
    };
    let name_clip = crate::ui::hover_tooltip(
        container(
            text(row_state.name.clone())
                .size(13)
                .width(Length::Fill)
                .wrapping(text::Wrapping::None),
        )
        .clip(true),
        row_state.name.clone(),
        iced::widget::tooltip::Position::FollowCursor,
    );
    let mut meta = column![name_clip];
    if row_state.is_dir {
        meta = meta.push(container(text(t!("sftp.dir_merge_hint")).size(11.5)).style(danger_text));
    }
    if row_state.decision == ConflictDecision::Rename {
        meta = meta.push(
            container(
                text(t!("sftp.rename_preview", name => row_state.new_name.clone())).size(11.5),
            )
            .style(accent_text),
        );
    }
    let seg = crate::ui::segmented([
        SegmentOption {
            label: t!("sftp.overwrite"),
            selected: row_state.decision == ConflictDecision::Overwrite,
            tone: SegmentTone::Danger,
            on_press: Message::SftpConflictDecide(index, ConflictDecision::Overwrite),
        },
        SegmentOption {
            label: t!("sftp.skip"),
            selected: row_state.decision == ConflictDecision::Skip,
            tone: SegmentTone::Neutral,
            on_press: Message::SftpConflictDecide(index, ConflictDecision::Skip),
        },
        SegmentOption {
            label: t!("sftp.rename"),
            selected: row_state.decision == ConflictDecision::Rename,
            tone: SegmentTone::Accent,
            on_press: Message::SftpConflictDecide(index, ConflictDecision::Rename),
        },
    ]);
    container(
        row![icon.svg(16.0), container(meta).width(Length::Fill), seg]
            .spacing(10)
            .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .padding([10, 12])
    .style(|theme: &Theme| {
        let p = theme::custom_palette(theme);
        container::Style {
            background: Some(Background::Color(p.surface)),
            text_color: Some(p.text_secondary),
            border: Border::default().rounded(10.0),
            ..Default::default()
        }
    })
    .into()
}

/// 工具条小描边按钮（应用到全部三钮）。
fn tool_button<'a>(label: String, on_press: Message) -> Element<'a, Message> {
    button(text(label).size(12))
        .padding([3, 10])
        .style(crate::ui::dialog_btn_style_neutral)
        .on_press(on_press)
        .into()
}

/// 弱文本色（页脚汇总 / 工具条标签），经容器 `text_color` 下发给子文本。
fn weak_text(theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(theme::custom_palette(theme).text_tertiary),
        ..Default::default()
    }
}

/// 次文本色（副标题）。
fn secondary_text(theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(theme::custom_palette(theme).text_secondary),
        ..Default::default()
    }
}

/// 危险语义文字（目录整树合并提示）。
fn danger_text(_theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(crate::ui::DANGER),
        ..Default::default()
    }
}

/// 强调色文字（重命名预览、批次徽标）。
fn accent_text(theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(theme::accent_color(theme)),
        ..Default::default()
    }
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
