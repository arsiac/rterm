//! 中心“会话管理”面板。
//!
//! 未编辑时顶部为一排按钮栏（新建会话、刷新、导入、导出），其下为会话列表（双击列表项即可连接）；
//! 右键列表项弹出含新建会话 / 连接 / 编辑 / 删除的菜单；
//! 编辑（新建 / 修改）以弹窗表单呈现（名称、主机、端口、用户名、认证方式及对应凭据、分组），
//! 列表在遮罩之下仍可见。

use crate::t;

use crate::App;
use crate::app::session::{EditorDraft, Message, SessionField};
use crate::icons::{ICON_SIZE, Icon, icon_button};
use crate::ui::menu_entry;
use iced::widget::text::Wrapping;
use iced::widget::tooltip::Position;
use iced::widget::{
    button, column, container, mouse_area, pick_list, row, rule, scrollable, text, text_input,
};
use iced::{Border, Element, Length, Theme};
use iced_aw::widget::context_menu::ContextMenu;
use rterm_config::SessionConfig;
use rterm_core::ConnectionStatus;
use std::collections::BTreeMap;
use std::fmt;

/// 操作按钮（新建会话、刷新、导入、导出）图标尺寸（像素），与列表图标保持一致。
const ACTION_ICON_SIZE: f32 = ICON_SIZE;

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
    /// 全部认证方式选项，供 `pick_list` 展示。
    const ALL: [AuthChoice; 3] = [
        AuthChoice::Password,
        AuthChoice::PublicKey,
        AuthChoice::Agent,
    ];

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
    // 分组名（Some）进 BTreeMap 自动按名称排序；None 归入未分组列表置后显示。
    let mut grouped: BTreeMap<String, Vec<&SessionConfig>> = BTreeMap::new();
    let mut ungrouped: Vec<&SessionConfig> = Vec::new();
    for s in &app.session.sessions {
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

    // 依次铺分组头与（未折叠时的）组内会话行；键为空串代表「未分组」区块。
    let mut items: Vec<Element<'_, Message>> = Vec::new();
    for (name, list) in &grouped {
        items.push(group_header(app, name, list.len()));
        if !app.session.collapsed_groups.contains(name) {
            for s in list {
                items.push(session_row(app, s));
            }
        }
    }
    items.push(group_header(app, "", ungrouped.len()));
    if !app.session.collapsed_groups.contains("") {
        for s in &ungrouped {
            items.push(session_row(app, s));
        }
    }

    // 顶部按钮栏：与文件列表一致的一排右对齐图标按钮（新建会话、刷新、导入、导出），
    // 置于右键菜单触发区之外，避免右键工具栏时误弹会话菜单。
    let toolbar_actions = row![
        icon_button(
            Icon::Add,
            ACTION_ICON_SIZE,
            t!("session.new"),
            Message::NewSession,
            Position::Bottom
        ),
        icon_button(
            Icon::ArrowClockwise,
            ACTION_ICON_SIZE,
            t!("session.refresh"),
            Message::RefreshSessions,
            Position::Bottom
        ),
        icon_button(
            Icon::ArrowImport,
            ACTION_ICON_SIZE,
            t!("session.import"),
            Message::ImportSessions,
            Position::Bottom
        ),
        icon_button(
            Icon::ArrowExport,
            ACTION_ICON_SIZE,
            t!("session.export"),
            Message::ExportSessions,
            Position::Bottom
        ),
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);
    let toolbar = container(toolbar_actions)
        .width(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right);

    column![
        toolbar,
        column![scrollable(column(items).spacing(4)).height(Length::Fill),],
    ]
    .spacing(8)
    .padding(10)
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
    let content = container(
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
    // 中栏可拖拽变窄，超长名称 / 地址一律单行截断，不撑宽面板。
    .width(Length::Fill)
    .clip(true);
    let item: Element<'a, Message> = mouse_area(
        container(content)
            .padding([6u16, 8u16])
            .width(Length::Fill)
            .style(move |theme| {
                let mut style = crate::theme::plain_background(crate::theme::list_row_bg(
                    theme, selected, hovered, status,
                ));
                style.border.radius = 6.0.into();
                style
            }),
    )
    .on_enter(Message::SessionEnter(s.id.clone()))
    .on_exit(Message::SessionExit(s.id.clone()))
    .on_double_click(Message::ConnectSession(s.id.clone()))
    .interaction(iced::mouse::Interaction::Pointer)
    .into();

    // 右键菜单按列表项各自触发，空白处（无列表项）因此不弹菜单；
    // 含“新建会话”与连接 / 编辑 / 删除，已连接再追加“打开文件管理”。
    let id = s.id.clone();
    let menu_overlay = move || {
        let mut actions = column![
            menu_entry(t!("session.new"), Message::NewSession),
            menu_entry(t!("session.connect"), Message::ConnectSession(id.clone())),
            menu_entry(t!("session.edit"), Message::EditSession(id.clone())),
            menu_entry(t!("session.delete"), Message::DeleteSession(id.clone())),
        ]
        .spacing(2);
        if status == ConnectionStatus::Connected {
            actions = actions.push(menu_entry(
                t!("session.open_files"),
                Message::OpenFiles(id.clone()),
            ));
        }
        crate::ui::menu_container(actions.into())
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
pub fn editor_overlay(app: &App) -> Option<Element<'_, Message>> {
    let draft = app.session.editor.as_ref()?;
    let panel = container(editor_body(draft))
        .width(440.0)
        .height(640.0)
        .style(panel_style)
        .padding(0);
    Some(crate::sftp_dialogs::overlay_wrap(panel.into()))
}

/// 弹窗主体：标题栏（含关闭按钮）+ 分隔线 + 可滚动表单。
fn editor_body<'a>(draft: &'a EditorDraft) -> Element<'a, Message> {
    // 标题栏随新建 / 编辑切换，右侧关闭按钮复用「取消编辑」语义。
    let is_new = draft.id.is_empty();
    let header = row![
        text(if is_new {
            t!("session.editor_new")
        } else {
            t!("session.editor_edit")
        })
        .size(18)
        .width(Length::Fill),
        icon_button(
            Icon::Dismiss,
            ACTION_ICON_SIZE,
            t!("common.close"),
            Message::CancelEdit,
            Position::Bottom
        ),
    ]
    .align_y(iced::alignment::Vertical::Center)
    .spacing(8)
    .padding([10, 12]);

    let divider = rule::horizontal(1);

    let auth_choice = match draft.auth.as_str() {
        "publickey" => AuthChoice::PublicKey,
        "agent" => AuthChoice::Agent,
        _ => AuthChoice::Password,
    };

    // 凭据字段随认证方式变化（字符串均取自 draft，生命周期与 &app 一致）。
    // 凭据为密文信封，编辑器内留空表示「保持不变」，故占位提示强调这一点。
    let cred_field: Element<'a, Message> = match draft.auth.as_str() {
        "password" => labeled_input(
            t!("session.password"),
            &draft.password,
            t!("session.keep_unchanged"),
            SessionField::Password,
            true,
        ),
        "publickey" => column![
            // 私钥路径：输入框 + 文件系统选择按钮（document 图标）。
            column![
                text(t!("session.key_path")).size(14),
                row![
                    text_input("", &draft.key_path)
                        .on_input(move |v| Message::EditorField(SessionField::KeyPath, v))
                        .style(crate::ui::text_input_style),
                    icon_button(
                        Icon::Document,
                        ACTION_ICON_SIZE,
                        t!("session.pick_key"),
                        Message::PickKeyFile,
                        Position::Bottom
                    ),
                ]
                .spacing(6)
                .align_y(iced::alignment::Vertical::Center),
            ]
            .spacing(2),
            labeled_input(
                t!("session.passphrase"),
                &draft.passphrase,
                t!("session.keep_unchanged"),
                SessionField::Passphrase,
                true,
            ),
        ]
        .spacing(6)
        .into(),
        _ => text(t!("session.agent_hint")).into(),
    };

    // 字段区可滚动；保存按钮置于滚动区之外的固定底部，不随表单滚动。
    let fields = column![
        labeled_input(
            t!("session.name"),
            &draft.name,
            "user@host",
            SessionField::Name,
            false,
        ),
        labeled_input(
            t!("session.group"),
            &draft.group,
            "",
            SessionField::Group,
            false
        ),
        labeled_input(
            t!("session.host"),
            &draft.host,
            "",
            SessionField::Host,
            false
        ),
        labeled_input(
            t!("session.port"),
            &draft.port,
            "",
            SessionField::Port,
            false
        ),
        labeled_input(
            t!("session.username"),
            &draft.username,
            "",
            SessionField::Username,
            false,
        ),
        column![
            text(t!("session.auth")).size(14),
            pick_list(&AuthChoice::ALL[..], Some(auth_choice), |c| {
                Message::EditorField(SessionField::Auth, c.value().to_string())
            },)
            .width(Length::Fill)
            .style(crate::theme::pick_list_style),
        ]
        .spacing(2),
        cred_field,
    ]
    .spacing(10)
    .padding(16);

    // 校验错误显示在弹窗底部（按钮上方），随字段修改自动清除。
    let mut footer = column![].spacing(8);
    if let Some(e) = &draft.error {
        footer = footer.push(text(e.clone()).size(13).color(crate::ui::DANGER));
    }
    footer = footer.push(
        container(
            button(text(t!("session.save")).size(16))
                .on_press(Message::SaveSession)
                .style(save_btn_style)
                .padding([10, 24]),
        )
        .width(Length::Fill)
        .align_x(iced::alignment::Horizontal::Center),
    );
    let footer = container(footer).width(Length::Fill).padding([12, 16]);

    column![
        header,
        divider,
        scrollable(fields).height(Length::Fill),
        footer,
    ]
    .into()
}

/// 编辑器弹窗面板样式：提亮背景 + 圆角 + 细边框。
///
/// 背景取 `extended_palette().background.strong`；设置弹窗的 `panel_style` 用的是
/// `custom_palette(theme).surface_raised`，两者并非同一取值（历史遗留，未统一）。
pub(crate) fn panel_style(theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(iced::Background::Color(
            theme.extended_palette().background.strong.color,
        )),
        border: Border {
            color: crate::theme::custom_palette(theme).border,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

/// 生成带标签的输入框（标签在输入框顶部，`placeholder` 为空值占位提示）。
///
/// `secure` 为 `true` 时掩码输入（用于密码 / 私钥口令等敏感字段）。
fn labeled_input<'a>(
    label: impl Into<String>,
    value: &'a str,
    placeholder: impl Into<String>,
    field: SessionField,
    secure: bool,
) -> Element<'a, Message> {
    let placeholder = placeholder.into();
    column![
        text(label.into()).size(14),
        text_input(&placeholder, value)
            .secure(secure)
            .on_input(move |v| Message::EditorField(field, v))
            .style(crate::ui::text_input_style),
    ]
    .spacing(2)
    .into()
}

/// 保存按钮样式：作为弹窗主操作按钮，悬浮 / 按下切换为主题强调色。
///
/// 不能用背景微调做反馈：自定义调色板的 `hover` 与 `surface_raised` 恒等
/// （均为背景 ±0.08/0.10），切换后无任何可见变化。
pub(crate) fn save_btn_style(theme: &Theme, status: button::Status) -> button::Style {
    let p = crate::theme::custom_palette(theme);
    let pal = theme.extended_palette();
    let (bg, text_color) = match status {
        button::Status::Pressed => (pal.primary.strong.color, pal.primary.strong.text),
        button::Status::Hovered => (pal.primary.base.color, pal.primary.base.text),
        _ => (p.surface_raised, pal.background.base.text),
    };
    button::Style {
        background: Some(iced::Background::Color(bg)),
        text_color,
        border: Border {
            color: p.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

/// 危险操作按钮样式：用于「关闭主密码」等不可逆 / 降权操作，用 [`crate::ui::DANGER`] 红描边以警示。
///
/// 与 [`save_btn_style`] 同样的反馈思路：常态为中性底色，悬浮 / 按下转为危险红。
pub(crate) fn danger_btn_style(theme: &Theme, status: button::Status) -> button::Style {
    let p = crate::theme::custom_palette(theme);
    let pal = theme.extended_palette();
    let danger = crate::ui::DANGER;
    let (bg, text_color, border) = match status {
        button::Status::Pressed => (danger, pal.background.base.text, danger),
        button::Status::Hovered => (danger, pal.background.base.text, danger),
        _ => (p.surface_raised, pal.background.base.text, danger),
    };
    button::Style {
        background: Some(iced::Background::Color(bg)),
        text_color,
        border: Border {
            color: border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}
