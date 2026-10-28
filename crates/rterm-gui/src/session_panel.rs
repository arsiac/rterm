//! 中心“会话管理”面板。
//!
//! 未编辑时顶部为一排按钮栏（新建会话、全部展开、全部折叠、更多），其下为会话列表（双击列表项即可连接）；
//! 右键列表项弹出含新建会话 / 连接 / 编辑 / 复制 / 收藏 / 删除的菜单，行内星标切换收藏；
//! 编辑（新建 / 修改）以弹窗表单呈现（名称、主机、端口、用户名、认证方式及对应凭据、分组），
//! 列表在遮罩之下仍可见。

use crate::t;

use crate::App;
use crate::app::session::{EditorDraft, JumpDraft, Message, SessionField, field_id};
use crate::icons::{ICON_BUTTON_DEFAULT_PADDING, ICON_SIZE, Icon, icon_button};
use crate::ui::{
    DialogBtnStyle, DialogButton, SegmentOption, SegmentTone, dialog_panel, menu_entry,
    menu_separator,
};
use crate::widget::measured_column::MeasuredColumn;
use iced::widget::text::Wrapping;
use iced::widget::tooltip::Position;
use iced::widget::{button, column, container, mouse_area, row, text, text_input};
use iced::{Background, Border, Element, Length, Shadow, Theme};
use iced_autocomplete::auto_complete;
use iced_aw::widget::context_menu::ContextMenu;
use iced_widget::overlay::menu;
use rterm_config::SessionConfig;
use rterm_core::ConnectionStatus;
use std::collections::BTreeMap;
use std::fmt;

/// 操作按钮图标尺寸（像素），与列表图标保持一致。
const ACTION_ICON_SIZE: f32 = ICON_SIZE;

/// 会话编辑弹窗面板宽度占窗口宽的比例。
const EDITOR_W_RATIO: f32 = 0.40;
/// 会话编辑弹窗面板宽度下限（像素）：保证「主机 + 端口」同行。
const EDITOR_W_MIN: f32 = 440.0;
/// 会话编辑弹窗面板宽度上限（像素）。
const EDITOR_W_MAX: f32 = 560.0;
/// 会话编辑弹窗面板高度占窗口高的比例。
const EDITOR_H_RATIO: f32 = 0.82;
/// 会话编辑弹窗面板高度下限（像素）。
const EDITOR_H_MIN: f32 = 480.0;
/// 会话编辑弹窗面板高度上限（像素）。
const EDITOR_H_MAX: f32 = 760.0;
/// 面板窄于此宽度时，「两栏」排版退化为单栏。
const EDITOR_SINGLE_COL_W: f32 = 500.0;

/// 「主机 + 端口」同行时端口列的固定宽度（像素），端口值短，无需占满半行。
const PORT_FIELD_W: f32 = 96.0;

/// 折叠行箭头图标尺寸（像素）。
const FOLD_ICON_SIZE: f32 = 14.0;

/// 折叠态摘要行的最大字符数：超出截断，避免把卡片头撑成两行。
const SUMMARY_MAX_CHARS: usize = 48;

/// 编辑器中的认证方式选项；显示名随语言翻译，但写回配置时映射回内部值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthChoice {
    /// 密码认证。
    Password,
    /// 公钥文件认证。
    PublicKey,
    /// SSH agent 认证。
    Agent,
}

impl AuthChoice {
    /// 由内部认证值解析出选项（未知值回落为密码）。
    fn from_value(value: &str) -> Self {
        match value {
            "publickey" => AuthChoice::PublicKey,
            "agent" => AuthChoice::Agent,
            _ => AuthChoice::Password,
        }
    }

    /// 映射为持久化的内部认证方式值。
    fn value(self) -> &'static str {
        match self {
            AuthChoice::Password => "password",
            AuthChoice::PublicKey => "publickey",
            AuthChoice::Agent => "agent",
        }
    }
}

impl fmt::Display for AuthChoice {
    /// 按当前语言渲染认证方式的中文显示名。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&match self {
            AuthChoice::Password => t!("session.auth_password"),
            AuthChoice::PublicKey => t!("session.auth_publickey"),
            AuthChoice::Agent => t!("session.auth_agent"),
        })
    }
}

/// 始终为列表；编辑器以弹窗形式叠加，见 [`editor_overlay`]。
pub fn view(app: &App) -> Element<'_, Message> {
    list_view(app)
}

/// 会话列表视图：按 `group` 聚合成可折叠分组区块，未分组会话置于末尾。
fn list_view(app: &App) -> Element<'_, Message> {
    // 先按过滤词收窄，再分组：分组名（Some）进 BTreeMap 自动按名称排序；None 归入未分组列表置后显示。
    let visible =
        crate::app::session::filtered_sessions(&app.session.sessions, &app.session.filter);
    let filtering = !app.session.filter.trim().is_empty();
    let mut grouped: BTreeMap<String, Vec<&SessionConfig>> = BTreeMap::new();
    let mut ungrouped: Vec<&SessionConfig> = Vec::new();
    for s in visible {
        match &s.group {
            Some(g) => grouped.entry(g.clone()).or_default().push(s),
            None => ungrouped.push(s),
        }
    }
    let by_name = |a: &&SessionConfig, b: &&SessionConfig| a.name.cmp(&b.name);
    for list in grouped.values_mut() {
        list.sort_by(by_name);
    }
    ungrouped.sort_by(by_name);

    // 依次铺分组头与组内会话行；键为空串代表「未分组」区块。
    // 过滤时忽略折叠态（自动展开命中行），空组因无可见项自然不出现。
    let mut items: Vec<Element<'_, Message>> = Vec::new();
    for (name, list) in &grouped {
        items.push(group_header(app, name, list.len()));
        if filtering || !app.session.collapsed_groups.contains(name) {
            for s in list {
                items.push(session_row(app, s));
            }
        }
    }
    if !ungrouped.is_empty() {
        items.push(group_header(app, "", ungrouped.len()));
        if filtering || !app.session.collapsed_groups.contains("") {
            for s in &ungrouped {
                items.push(session_row(app, s));
            }
        }
    }

    // 顶部按钮栏：与文件列表一致的一排右对齐图标按钮。
    let more_expanded = app.session.show_more_menu;
    let more_button = container(
        button(Icon::MoreVertical.svg(ACTION_ICON_SIZE))
            .on_press(Message::ToggleMoreMenu)
            .padding(ICON_BUTTON_DEFAULT_PADDING)
            .style(move |theme, st| crate::theme::icon_button_style(theme, st, more_expanded)),
    );
    // dropdown 面板与右键菜单样式相同。
    let more_menu = crate::ui::menu_container(
        MeasuredColumn::with_children(vec![
            menu_entry(t!("session.refresh"), Message::RefreshSessions),
            menu_entry(t!("session.import"), Message::ImportSessions),
            menu_entry(t!("session.export"), Message::ExportSessions),
        ])
        .spacing(2),
    );
    let more_dropdown = iced_aw::widget::drop_down::DropDown::new(
        more_button,
        more_menu,
        app.session.show_more_menu,
    )
    // 面板宽度自适应最长条目：DropDown 未指定宽度时退化为触发按钮宽度，故须显式给 Shrink。
    .width(Length::Shrink)
    .alignment(iced_aw::core::alignment::Alignment::BottomEnd)
    .on_dismiss(Message::ToggleMoreMenu);

    let toolbar_actions = row![
        icon_button(
            Icon::Add,
            ACTION_ICON_SIZE,
            t!("session.new"),
            Message::NewSession,
            Position::Bottom
        ),
        icon_button(
            Icon::ExpandAll,
            ACTION_ICON_SIZE,
            t!("session.expand_all"),
            Message::ExpandAllGroups,
            Position::Bottom
        ),
        icon_button(
            Icon::CollapseAll,
            ACTION_ICON_SIZE,
            t!("session.collapse_all"),
            Message::CollapseAllGroups,
            Position::Bottom
        ),
        Element::from(more_dropdown),
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);
    let toolbar = container(toolbar_actions)
        .width(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right);

    // 过滤输入框：随输入即时过滤（`Message::FilterChanged`），清空即恢复全量。
    let filter_input = text_input(&t!("session.filter_placeholder"), &app.session.filter)
        .on_input(Message::FilterChanged)
        .size(13)
        .padding([5u16, 8u16])
        .style(crate::ui::search_input_style);

    // 列表主体：有会话时铺可滚动列表；完全无会话显示空状态，有会话但无命中显示无匹配提示。
    let body: Element<'_, Message> = if app.session.sessions.is_empty() {
        empty_state()
    } else if items.is_empty() {
        no_match_state()
    } else {
        crate::ui::thin_vertical_scrollbar(column(items).spacing(4))
            .height(Length::Fill)
            .into()
    };

    // 无会话时不显示过滤框（无处可筛）。
    let mut content = column![toolbar].spacing(8).padding(10);
    if !app.session.sessions.is_empty() {
        content = content.push(filter_input);
    }
    content.push(body).into()
}

/// 会话列表为空时的占位提示
fn empty_state<'a>() -> Element<'a, Message> {
    let content = column![
        text(t!("session.empty_title"))
            .size(15)
            .wrapping(Wrapping::Word),
        text(t!("session.empty_hint"))
            .size(13)
            .wrapping(Wrapping::Word)
            .style(|theme: &Theme| text::Style {
                color: Some(crate::theme::custom_palette(theme).text_secondary),
            }),
    ]
    .spacing(4)
    .align_x(iced::alignment::Horizontal::Center);
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

/// 会话列表过滤后无命中时的占位提示。
fn no_match_state<'a>() -> Element<'a, Message> {
    container(crate::ui::hint_text(t!("session.filter_no_match")))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

/// 单条会话行：名称（主行）与 `user@host` 地址（次行），悬浮高亮，双击连接，右键菜单。
///
/// 背景按该会话的聚合连接状态着色，但只在**出错**时泛红提示——同一会话可开多个标签，
/// “已连接”并非唯一终态，故不对其特别上色（见 `theme::row_base_bg`）。
fn session_row<'a>(app: &'a App, s: &'a SessionConfig) -> Element<'a, Message> {
    // 会话行显示该会话全部标签的聚合状态（派生自各标签自身的连接状态）。
    let status = app.session_status(&s.id);

    // 地址另起一行：名称选填且保存时默认填 `user@host`，多行后即便两者重合也能区分主机。
    // 悬浮态由 App 记录的 `hovered_session` 决定，与选中态共同驱动行背景。
    let hovered = app.session.hovered_session.as_deref() == Some(s.id.as_str());
    let selected = app.session.selected_session.as_deref() == Some(s.id.as_str());
    // 星标：已收藏常亮（强调色实心星），未收藏仅在悬浮时以空心星出现；点击即切换收藏态。
    let favorited = s.favorite;
    let star: Element<'a, Message> = if favorited || hovered {
        let icon = if favorited {
            Icon::Star
        } else {
            Icon::StarOutline
        };
        let svg = icon
            .svg(12.0)
            .style(move |theme: &Theme, _| iced::widget::svg::Style {
                color: Some(if favorited {
                    crate::theme::accent_color(theme)
                } else {
                    crate::theme::custom_palette(theme).text_secondary
                }),
            });
        let tip = if favorited {
            t!("session.unfavorite")
        } else {
            t!("session.favorite")
        };
        iced::widget::tooltip(
            button(svg)
                .on_press(Message::ToggleFavorite(s.id.clone()))
                .style(|theme, st| crate::theme::icon_button_style(theme, st, false))
                .padding(2),
            text(tip),
            Position::Bottom,
        )
        .delay(iced::time::Duration::from_millis(
            crate::theme::TOOLTIP_DELAY_MS,
        ))
        .style(crate::theme::tooltip_style)
        .into()
    } else {
        // 未收藏且未悬浮时不显示星标，但留同宽占位，避免名称列随悬浮来回跳动。
        iced::widget::Space::new()
            .width(Length::Fixed(16.0))
            .height(Length::Fixed(16.0))
            .into()
    };
    let content = container(
        row![
            // 中栏可拖拽变窄，超长名称 / 地址一律单行截断，不撑宽面板。
            container(
                column![
                    text(&s.name).size(14).wrapping(Wrapping::None),
                    text(format!("{}@{}", s.username, s.host))
                        .size(12)
                        .wrapping(Wrapping::None)
                        .style(|theme: &Theme| text::Style {
                            color: Some(crate::theme::custom_palette(theme).text_secondary),
                        }),
                ]
                .spacing(2),
            )
            .width(Length::Fill)
            .clip(true),
            star,
        ]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fill);
    let item: Element<'a, Message> = mouse_area(
        container(content)
            .padding([6u16, 8u16])
            .width(Length::Fill)
            .style(move |theme| {
                let mut style = crate::theme::list_row_bg(theme, selected, hovered, status);
                style.border.radius = 6.0.into();
                style
            }),
    )
    .on_press(Message::SessionSelect(s.id.clone()))
    .on_enter(Message::SessionEnter(s.id.clone()))
    .on_exit(Message::SessionExit(s.id.clone()))
    .on_double_click(Message::ConnectSession(s.id.clone()))
    .interaction(iced::mouse::Interaction::Pointer)
    .into();

    // 右键菜单按列表项各自触发，空白处（无列表项）因此不弹菜单；
    // 按功能分四组：新建 / 连接（已连接追加“打开文件管理”）/ 编辑与收藏 / 删除。
    let id = s.id.clone();
    let favorited = s.favorite;
    let menu_overlay = move || {
        let mut actions = vec![
            menu_entry(t!("session.new"), Message::NewSession),
            menu_separator(),
            menu_entry(t!("session.connect"), Message::ConnectSession(id.clone())),
        ];
        if status == ConnectionStatus::Connected {
            actions.push(menu_entry(
                t!("session.open_files"),
                Message::OpenFiles(id.clone()),
            ));
        }
        actions.extend([
            menu_separator(),
            menu_entry(t!("session.edit"), Message::EditSession(id.clone())),
            menu_entry(t!("session.copy"), Message::DuplicateSession(id.clone())),
            menu_entry(
                if favorited {
                    t!("session.unfavorite")
                } else {
                    t!("session.favorite")
                },
                Message::ToggleFavorite(id.clone()),
            ),
            menu_separator(),
            menu_entry(
                t!("session.delete"),
                Message::RequestDeleteSession(id.clone()),
            ),
        ]);
        crate::ui::menu_container(MeasuredColumn::with_children(actions).spacing(2))
    };

    ContextMenu::new(item, menu_overlay).into()
}

/// 分组头：显示分组名、会话数与折叠箭头，点击切换折叠态。
fn group_header<'a>(app: &'a App, key: &str, count: usize) -> Element<'a, Message> {
    let label = if key.is_empty() {
        t!("session.ungrouped")
    } else {
        key.to_string()
    };
    let collapsed = app.session.collapsed_groups.contains(key);
    let chevron = if collapsed {
        Icon::ChevronCircleRight
    } else {
        Icon::ChevronCircleDown
    };
    button(
        row![
            chevron.svg(14.0),
            text(label).size(13).width(Length::Fill),
            text(count.to_string()).size(11),
        ]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center),
    )
    .on_press(Message::ToggleGroup(key.to_string()))
    .width(Length::Fill)
    .padding([4, 8])
    .style(group_header_style)
    .into()
}

/// 分组头按钮样式：无背景、次要文字色，仅作可点击的分组分隔标识。
fn group_header_style(theme: &Theme, _st: button::Status) -> button::Style {
    let p = crate::theme::custom_palette(theme);
    button::Style {
        background: None,
        text_color: p.text_secondary,
        border: Border {
            color: p.border,
            width: 0.0,
            radius: 4.0.into(),
        },
        ..Default::default()
    }
}

/// 编辑器弹窗层：仅当存在编辑草稿时返回全屏遮罩层，否则返回 `None`。
///
/// 遮罩叠加在窗口最顶层（见 [`layout::view`](crate::layout::view)），列表在遮罩下仍可见。
/// 面板尺寸随窗口自适应（比例见 [`EDITOR_W_RATIO`] / [`EDITOR_H_RATIO`]），窄于
/// [`EDITOR_SINGLE_COL_W`] 时表单退化为单栏。
pub fn editor_overlay(app: &App) -> Option<Element<'_, Message>> {
    let draft = app.session.editor.as_ref()?;
    let panel = iced::widget::responsive(move |size| {
        let width = (size.width * EDITOR_W_RATIO).clamp(EDITOR_W_MIN, EDITOR_W_MAX);
        let height = (size.height * EDITOR_H_RATIO).clamp(EDITOR_H_MIN, EDITOR_H_MAX);
        container(
            container(editor_body(draft, width))
                .width(width)
                .height(height)
                .style(crate::ui::dialog_panel_style(None))
                .padding(0),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    });
    Some(crate::modal::overlay_wrap_dismissable(
        panel.into(),
        Message::CancelEdit,
    ))
}

/// 删除会话确认弹窗层：存在待确认删除 id 时返回遮罩层，否则 `None`。
/// 复用 `dialog_panel` + `overlay_wrap_dismissable`，与 SFTP 删除确认弹窗同源。
pub fn confirm_delete_overlay(app: &App) -> Option<Element<'_, Message>> {
    let id = app.session.pending_delete.as_ref()?;
    let name = app
        .session
        .sessions
        .iter()
        .find(|s| &s.id == id)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    let body: Element<'_, Message> =
        column![text(t!("session.delete_confirm", name => name)).size(14),]
            .spacing(8)
            .into();
    let panel = dialog_panel(
        t!("session.delete_title"),
        body,
        None,
        360.0,
        Some(DialogButton {
            label: t!("common.cancel").to_string(),
            on_press: Message::CancelDeleteSession,
            style: DialogBtnStyle::Neutral,
        }),
        DialogButton {
            label: t!("session.delete").to_string(),
            on_press: Message::ConfirmDeleteSession,
            style: DialogBtnStyle::Emphasis { danger: true },
        },
    );
    Some(crate::modal::overlay_wrap_dismissable(
        panel,
        Message::CancelDeleteSession,
    ))
}

/// 弹窗主体：标题栏（含关闭按钮）+ 可滚动表单（分节 + 折叠区）+ 固定底部按钮行。
///
/// `panel_w` 为面板实际宽度：窄于 [`EDITOR_SINGLE_COL_W`] 时两栏排版退化为单栏。
fn editor_body<'a>(draft: &'a EditorDraft, panel_w: f32) -> Element<'a, Message> {
    let single_col = panel_w < EDITOR_SINGLE_COL_W;

    // 标题栏随新建 / 编辑切换（强调条 + 标题 + 关闭按钮），关闭按钮复用「取消编辑」语义。
    let header = crate::ui::dialog_title_bar(
        if draft.is_new() {
            t!("session.editor_new")
        } else {
            t!("session.editor_edit")
        },
        Some(Message::CancelEdit),
    );

    // 表单分三节常显 + 一个折叠区：连接 / 认证 / 名称与分组常显，启动作业与跳板机收进「高级选项」。
    let form = column![
        connection_section(draft, single_col),
        auth_section(draft),
        identity_section(draft, single_col),
        advanced_section(draft, single_col),
    ]
    .spacing(18)
    .padding([16, 20]);

    // 页脚左侧只放一条消息：整表单级错误优先，否则只显示待修正项计数（错误详情就地显示）。
    let message = if let Some(e) = &draft.error {
        Some(e.clone())
    } else if draft.errors.is_empty() {
        None
    } else {
        Some(t!("session.error_count", count => draft.errors.len()))
    };
    let footer = crate::ui::dialog_footer(
        message,
        Some(crate::ui::DialogButton {
            label: t!("common.cancel"),
            on_press: Message::CancelEdit,
            style: crate::ui::DialogBtnStyle::Neutral,
        }),
        crate::ui::DialogButton {
            label: t!("session.save"),
            on_press: Message::SaveSession,
            style: crate::ui::DialogBtnStyle::Emphasis { danger: false },
        },
    );

    column![
        header,
        crate::ui::hairline(),
        crate::ui::thin_vertical_scrollbar(form).height(Length::Fill),
        crate::ui::hairline(),
        footer,
    ]
    .into()
}

/// 「连接」节：主机 + 端口（两栏 / 单栏随面板宽度切换）+ 用户名。
fn connection_section(draft: &EditorDraft, single_col: bool) -> Element<'_, Message> {
    let host = labeled_input(
        crate::ui::required_label(t!("session.host")),
        &draft.host,
        "",
        SessionField::Host,
        false,
        Length::Fill,
        error_for_field(draft, SessionField::Host),
    );
    let port = labeled_input(
        crate::ui::required_label(t!("session.port")),
        &draft.port,
        "",
        SessionField::Port,
        false,
        Length::Fixed(PORT_FIELD_W),
        error_for_field(draft, SessionField::Port),
    );
    let host_port: Element<'_, Message> = if single_col {
        column![host, port].spacing(12).into()
    } else {
        row![host, port].spacing(10).into()
    };
    crate::ui::form_section(
        t!("session.section_connection"),
        column![
            host_port,
            labeled_input(
                crate::ui::field_label(t!("session.username")),
                &draft.username,
                "",
                SessionField::Username,
                false,
                Length::Fill,
                None,
            ),
        ]
        .spacing(12),
    )
}

/// 「认证」节：分段控件（三种方式常驻）+ 形态说明条 + 凭据区。
fn auth_section(draft: &EditorDraft) -> Element<'_, Message> {
    let segmented = auth_segmented(&draft.auth, |value| {
        Message::EditorField(SessionField::Auth, value.to_string())
    });
    // 凭据字段随认证方式变化；密文信封留空表示「保持不变」，与目标主机同规。
    let cred: Element<'_, Message> = match draft.auth.as_str() {
        "publickey" => column![
            key_path_field(
                &draft.key_path,
                SessionField::KeyPath,
                Message::PickKeyFile,
                error_for_field(draft, SessionField::KeyPath),
            ),
            labeled_input(
                crate::ui::field_label(t!("session.passphrase")),
                &draft.passphrase,
                t!("session.keep_unchanged"),
                SessionField::Passphrase,
                true,
                Length::Fill,
                error_for_field(draft, SessionField::Passphrase),
            ),
        ]
        .spacing(12)
        .into(),
        "agent" => agent_hint(t!("session.agent_hint")),
        _ => labeled_input(
            crate::ui::field_label(t!("session.password")),
            &draft.password,
            t!("session.keep_unchanged"),
            SessionField::Password,
            true,
            Length::Fill,
            error_for_field(draft, SessionField::Password),
        ),
    };
    crate::ui::form_section(
        t!("session.section_auth"),
        column![
            column![
                segmented,
                crate::ui::hint_text(auth_shape_hint(&draft.auth)),
            ]
            .spacing(6),
            cred,
        ]
        .spacing(10),
    )
}

/// 「名称与分组」节：名称 + 分组自动补全（分组框不参与 Tab 链，故附提示气泡）。
fn identity_section(draft: &EditorDraft, single_col: bool) -> Element<'_, Message> {
    let name = labeled_input(
        crate::ui::field_label(t!("session.name")),
        &draft.name,
        "user@host",
        SessionField::Name,
        false,
        Length::Fill,
        None,
    );
    let group: Element<'_, Message> = column![
        crate::ui::field_label(t!("session.group")),
        crate::ui::hover_tooltip(
            auto_complete(
                draft.group_ac.as_ref(),
                "",
                |v| Message::EditorField(SessionField::Group, v),
                |v| Message::EditorField(SessionField::Group, v),
                Some(|opt: String| Message::EditorField(SessionField::Group, opt)),
            )
            .width(Length::Fill)
            .input_style(crate::ui::text_input_style)
            .menu_style(|theme: &Theme| {
                let p = crate::theme::custom_palette(theme);
                menu::Style {
                    background: Background::Color(p.surface_raised),
                    border: Border {
                        color: p.border,
                        width: 1.0,
                        radius: 6.0.into(),
                    },
                    text_color: theme.palette().text,
                    selected_text_color: theme.palette().text,
                    selected_background: Background::Color(p.hover_raised),
                    shadow: Shadow::default(),
                }
            }),
            t!("session.group_tab_hint"),
            Position::Bottom,
        ),
    ]
    .spacing(4)
    .width(Length::Fill)
    .into();
    let body: Element<'_, Message> = if single_col {
        column![name, group].spacing(12).into()
    } else {
        row![name, group].spacing(10).into()
    };
    crate::ui::form_section(t!("session.section_identity"), body)
}

/// 「高级选项」折叠区：启动作业与跳板机默认收起。
///
/// 收起时其输入框**不进入控件树**——输入框可聚焦，留着会让 Tab 跳进看不见的输入框。
fn advanced_section(draft: &EditorDraft, single_col: bool) -> Element<'_, Message> {
    let toggle = fold_row(
        draft.advanced_open,
        t!("session.section_advanced"),
        advanced_badges(draft),
        Message::ToggleAdvanced,
    );
    let mut column = column![].spacing(10);
    if draft.advanced_open {
        // 分隔线画在折叠行上方，把折叠区与上方常显内容分开。
        column = column
            .push(crate::ui::hairline())
            .push(toggle)
            .push(
                column![
                    crate::ui::hint_text(t!("session.section_advanced_hint")),
                    labeled_input(
                        crate::ui::field_label(t!("session.startup_command")),
                        &draft.startup_command,
                        "",
                        SessionField::StartupCommand,
                        false,
                        Length::Fill,
                        None,
                    ),
                    crate::ui::hint_text(t!("session.startup_command_hint")),
                ]
                .spacing(4),
            )
            .push(sub_section_title(t!("session.section_jumps")))
            .push(jump_section(draft, single_col));
    } else {
        column = column.push(toggle);
    }
    column.into()
}

/// 折叠区内的次级节标题：12px 次级色。
fn sub_section_title(label: impl Into<String>) -> Element<'static, Message> {
    text(label.into())
        .size(12)
        .style(|theme: &Theme| iced::widget::text::Style {
            color: Some(crate::theme::custom_palette(theme).text_secondary),
        })
        .into()
}

/// 折叠态摘要徽标：跳板机台数 + 启动作业是否已设置。
fn advanced_badges(draft: &EditorDraft) -> Element<'static, Message> {
    let mut badges = row![].spacing(12);
    badges = badges.push(tertiary_text(
        t!("session.jump_count", count => draft.jumps.len()),
        Length::Shrink,
    ));
    if !draft.startup_command.trim().is_empty() {
        badges = badges.push(tertiary_text(t!("session.startup_set"), Length::Shrink));
    }
    badges.into()
}

/// 跳板链：逐个跳板机卡片（由外到内排序，与连接顺序一致）+ 追加按钮。
fn jump_section(draft: &EditorDraft, single_col: bool) -> Element<'_, Message> {
    let mut items: Vec<Element<'_, Message>> = Vec::new();
    if draft.jumps.is_empty() {
        items.push(crate::ui::hint_text(t!("session.jumps_empty")));
    }
    for (index, hop) in draft.jumps.iter().enumerate() {
        items.push(jump_card(draft, index, hop, single_col));
    }
    items.push(
        button(
            container(
                row![
                    Icon::Add.svg(ACTION_ICON_SIZE),
                    text(t!("session.jump_add")).size(13),
                ]
                .spacing(6)
                .align_y(iced::alignment::Vertical::Center),
            )
            .center_x(Length::Fill),
        )
        .padding([6, 10])
        .width(Length::Fill)
        .on_press(Message::AddJump)
        .style(crate::theme::card_button_style)
        .into(),
    );
    column(items).spacing(10).into()
}

/// 单跳跳板机卡片：折叠态为单行摘要（箭头 + 名称 + 摘要 + 删除），展开态补全表单。
fn jump_card<'a>(
    draft: &'a EditorDraft,
    index: usize,
    hop: &'a JumpDraft,
    single_col: bool,
) -> Element<'a, Message> {
    let open = draft.expanded_jumps.contains(&index);
    // 箭头与名称同属一个按钮，点击即展开 / 收起；删除按钮独立在外。
    let head = button(
        row![
            fold_chevron(open),
            text(t!("session.jump_label", index => index + 1)).size(13),
            jump_summary_text(hop, single_col),
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .padding([2, 0])
    .style(jump_head_style())
    .on_press(Message::ToggleJump(index));
    let head_row = row![
        head,
        icon_button(
            Icon::Delete,
            ACTION_ICON_SIZE,
            t!("session.jump_remove"),
            Message::RemoveJump(index),
            Position::Bottom,
        ),
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);

    let mut card = column![head_row].spacing(10);
    if open {
        card = card.push(jump_body(draft, index, hop, single_col));
    }
    container(card)
        .width(Length::Fill)
        .padding([8, 10])
        .style(|theme: &Theme| {
            let p = crate::theme::custom_palette(theme);
            iced::widget::container::Style {
                background: Some(Background::Color(p.surface)),
                border: Border {
                    color: p.border,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

/// 展开态的跳板机表单：主机 + 端口、用户名、认证分段与凭据。
fn jump_body<'a>(
    draft: &'a EditorDraft,
    index: usize,
    hop: &'a JumpDraft,
    single_col: bool,
) -> Element<'a, Message> {
    let host = labeled_input(
        crate::ui::required_label(t!("session.host")),
        &hop.host,
        "",
        SessionField::JumpHost(index),
        false,
        Length::Fill,
        error_for_field(draft, SessionField::JumpHost(index)),
    );
    let port = labeled_input(
        crate::ui::required_label(t!("session.port")),
        &hop.port,
        "",
        SessionField::JumpPort(index),
        false,
        Length::Fixed(PORT_FIELD_W),
        error_for_field(draft, SessionField::JumpPort(index)),
    );
    let host_port: Element<'_, Message> = if single_col {
        column![host, port].spacing(12).into()
    } else {
        row![host, port].spacing(10).into()
    };
    let cred: Element<'_, Message> = match hop.auth.as_str() {
        "publickey" => column![
            key_path_field(
                &hop.key_path,
                SessionField::JumpKeyPath(index),
                Message::PickJumpKeyFile(index),
                error_for_field(draft, SessionField::JumpKeyPath(index)),
            ),
            labeled_input(
                crate::ui::field_label(t!("session.passphrase")),
                &hop.passphrase,
                t!("session.keep_unchanged"),
                SessionField::JumpPassphrase(index),
                true,
                Length::Fill,
                error_for_field(draft, SessionField::JumpPassphrase(index)),
            ),
        ]
        .spacing(12)
        .into(),
        "agent" => agent_hint(t!("session.agent_hint")),
        _ => labeled_input(
            crate::ui::field_label(t!("session.password")),
            &hop.password,
            t!("session.keep_unchanged"),
            SessionField::JumpPassword(index),
            true,
            Length::Fill,
            error_for_field(draft, SessionField::JumpPassword(index)),
        ),
    };
    column![
        host_port,
        labeled_input(
            crate::ui::field_label(t!("session.username")),
            &hop.username,
            "",
            SessionField::JumpUsername(index),
            false,
            Length::Fill,
            None,
        ),
        auth_segmented(&hop.auth, move |value| {
            Message::EditorField(SessionField::JumpAuth(index), value.to_string())
        }),
        cred,
    ]
    .spacing(10)
    .into()
}

/// 认证方式分段控件：三段常驻、单击即切换。
///
/// 不用下拉框：弹层会被弹窗内的滚动区裁切。
fn auth_segmented<'a>(
    value: &str,
    on_pick: impl Fn(&'static str) -> Message + 'a,
) -> Element<'a, Message> {
    let current = AuthChoice::from_value(value);
    let options = [
        SegmentOption {
            label: AuthChoice::Password.to_string(),
            selected: current == AuthChoice::Password,
            tone: SegmentTone::Accent,
            on_press: on_pick(AuthChoice::Password.value()),
        },
        SegmentOption {
            label: AuthChoice::PublicKey.to_string(),
            selected: current == AuthChoice::PublicKey,
            tone: SegmentTone::Accent,
            on_press: on_pick(AuthChoice::PublicKey.value()),
        },
        SegmentOption {
            label: AuthChoice::Agent.to_string(),
            selected: current == AuthChoice::Agent,
            tone: SegmentTone::Accent,
            on_press: on_pick(AuthChoice::Agent.value()),
        },
    ];
    crate::ui::segmented(options)
}

/// 认证形态说明条：当前认证方式需要哪些凭据。
fn auth_shape_hint(auth: &str) -> String {
    match auth {
        "publickey" => t!("session.auth_shape_publickey"),
        "agent" => t!("session.auth_shape_agent"),
        _ => t!("session.auth_shape_password"),
    }
}

/// 折叠行：箭头 + 标题 + 右侧摘要徽标，整行可点。
fn fold_row(
    open: bool,
    title: impl Into<String>,
    badges: Element<'static, Message>,
    on_toggle: Message,
) -> Element<'static, Message> {
    button(
        row![
            fold_chevron(open),
            text(title.into()).size(13).width(Length::Fill),
            badges
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .padding([8, 10])
    .style(fold_row_style(open))
    .on_press(on_toggle)
    .into()
}

/// 折叠箭头：收起指向右、展开指向下。
fn fold_chevron(open: bool) -> Element<'static, Message> {
    let icon = if open {
        Icon::ChevronCircleDown
    } else {
        Icon::ChevronCircleRight
    };
    icon.svg(FOLD_ICON_SIZE).into()
}

/// 折叠行样式：常态无底色，悬停铺一层弱底；标题色随展开态由次级升为主色。
fn fold_row_style(open: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme: &Theme, status| {
        let p = crate::theme::custom_palette(theme);
        let background = match status {
            button::Status::Hovered | button::Status::Pressed => {
                Some(Background::Color(p.hover_raised))
            }
            _ => None,
        };
        button::Style {
            background,
            border: Border::default().rounded(6.0),
            text_color: if open {
                theme.extended_palette().background.base.text
            } else {
                p.text_secondary
            },
            ..Default::default()
        }
    }
}

/// 跳板机卡片头样式：与折叠行同族，常态即为主色。
fn jump_head_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    |theme: &Theme, status| {
        let p = crate::theme::custom_palette(theme);
        let background = match status {
            button::Status::Hovered | button::Status::Pressed => {
                Some(Background::Color(p.hover_raised))
            }
            _ => None,
        };
        button::Style {
            background,
            border: Border::default().rounded(6.0),
            text_color: theme.extended_palette().background.base.text,
            ..Default::default()
        }
    }
}

/// 弱色小字（三级文本色，12px）：`width` 为 `Fill` 时占满剩余空间。
fn tertiary_text(label: impl Into<String>, width: Length) -> Element<'static, Message> {
    text(label.into())
        .size(12)
        .width(width)
        .style(|theme: &Theme| iced::widget::text::Style {
            color: Some(crate::theme::custom_palette(theme).text_tertiary),
        })
        .into()
}

/// 跳板机折叠态摘要文本：窄屏只留 `host:port`，完整摘要（含用户名）进 tooltip。
fn jump_summary_text(hop: &JumpDraft, single_col: bool) -> Element<'static, Message> {
    let full = jump_summary(hop, true);
    if !single_col {
        return summary_text(truncate(full, SUMMARY_MAX_CHARS));
    }
    let short = jump_summary(hop, false);
    crate::ui::hover_tooltip(
        summary_text(truncate(short, SUMMARY_MAX_CHARS)),
        full,
        Position::Bottom,
    )
}

/// 跳板机折叠态摘要：`host:port　user`（主机未填时给出占位说明）。
///
/// `with_user` 为 `false` 时略去用户名，仅用于窄屏形态。
fn jump_summary(hop: &JumpDraft, with_user: bool) -> String {
    let user = if with_user { hop.username.trim() } else { "" };
    let raw = if hop.host.trim().is_empty() {
        t!("session.jump_summary_empty", user => user)
    } else {
        t!(
            "session.jump_summary",
            host => hop.host.trim(),
            port => hop.port.trim(),
            user => user
        )
    };
    raw.trim_end().to_string()
}

/// 摘要文字：占满卡片头剩余宽度并裁切，长主机名不会把删除按钮挤出卡片。
fn summary_text(label: impl Into<String>) -> Element<'static, Message> {
    container(tertiary_text(label, Length::Fill))
        .width(Length::Fill)
        .clip(true)
        .into()
}

/// 截断过长文本并加省略号：摘要行不换行，避免把卡片头撑成两行。
fn truncate(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let mut cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// 取某字段的校验错误文案（若有）。
fn error_for_field(draft: &EditorDraft, field: SessionField) -> Option<&str> {
    draft
        .errors
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, msg)| msg.as_str())
}

/// SSH agent 提示：以「内嵌说明块」呈现（弱文字 + 表面底色 + 细边框），
/// 与输入框同处一列时高度、圆角一致，避免此处出现一块突兀的裸文字。
fn agent_hint<'a>(label: impl Into<String>) -> Element<'a, Message> {
    container(crate::ui::hint_text(label))
        .width(Length::Fill)
        .padding([8, 10])
        .style(|theme: &Theme| {
            let p = crate::theme::custom_palette(theme);
            iced::widget::container::Style {
                background: Some(iced::Background::Color(p.surface)),
                border: Border {
                    color: p.border,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

/// 生成带标签的输入框（标签在输入框顶部，`placeholder` 为空值占位提示）。
///
/// `label` 直接传入标签元素；`secure` 为 `true` 时掩码输入；`width` 控制整列宽度；
/// `error` 非空时描边转危险色并在框下补错误文案。输入框带固定 `id`（iced 0.14 需显式 `id`）。
fn labeled_input<'a>(
    label: Element<'a, Message>,
    value: &'a str,
    placeholder: impl Into<String>,
    field: SessionField,
    secure: bool,
    width: Length,
    error: Option<&'a str>,
) -> Element<'a, Message> {
    let placeholder = placeholder.into();
    let input = text_input(&placeholder, value)
        .id(field_id(field))
        .secure(secure)
        .on_input(move |v| Message::EditorField(field, v))
        .on_submit(Message::SaveSession)
        .style(input_style(error.is_some()));
    let mut column = column![label, input].spacing(4).width(width);
    if let Some(msg) = error {
        column = column.push(crate::ui::field_error(msg));
    }
    column.into()
}

/// 私钥路径输入：文本框 + 文件选择按钮（选择结果经 `pick` 消息回填 `field`）。
fn key_path_field<'a>(
    key_path: &'a str,
    field: SessionField,
    pick: Message,
    error: Option<&'a str>,
) -> Element<'a, Message> {
    let input = text_input("", key_path)
        .id(field_id(field))
        .on_input(move |v| Message::EditorField(field, v))
        .on_submit(Message::SaveSession)
        .style(input_style(error.is_some()));
    let mut column = column![
        crate::ui::field_label(t!("session.key_path")),
        row![
            input,
            icon_button(
                Icon::Document,
                ACTION_ICON_SIZE,
                t!("session.pick_key"),
                pick,
                Position::Bottom,
            ),
        ]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center),
    ]
    .spacing(4);
    if let Some(msg) = error {
        column = column.push(crate::ui::field_error(msg));
    }
    column.into()
}

/// 按是否有校验错误选择输入框样式。
fn input_style(error: bool) -> fn(&Theme, text_input::Status) -> text_input::Style {
    if error {
        crate::ui::text_input_error_style
    } else {
        crate::ui::text_input_style
    }
}
