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
use iced::{Border, Color, Element, Length, Theme};
use iced_aw::widget::context_menu::ContextMenu;
use rterm_config::SessionConfig;
use rterm_core::ConnectionStatus;
use std::collections::BTreeMap;
use std::fmt;

/// 操作按钮（新建会话、刷新、导入、导出）图标尺寸（像素），与列表图标保持一致。
const ACTION_ICON_SIZE: f32 = ICON_SIZE;

/// 会话编辑弹窗面板尺寸（像素）：两栏排版后无需过高，压扁以贴近桌面弹窗比例。
const EDITOR_W: f32 = 480.0;
/// 会话编辑弹窗面板高度（像素）。
const EDITOR_H: f32 = 560.0;

/// 「主机 + 端口」一行两栏时端口列的固定宽度（像素），端口值短，无需占满半行。
const PORT_FIELD_W: f32 = 96.0;

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
    if !ungrouped.is_empty() {
        items.push(group_header(app, "", ungrouped.len()));
        if !app.session.collapsed_groups.contains("") {
            for s in &ungrouped {
                items.push(session_row(app, s));
            }
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

    // 列表主体：有会话时铺可滚动列表；完全无会话时显示居中空状态提示。
    let body: Element<'_, Message> = if app.session.sessions.is_empty() {
        empty_state()
    } else {
        scrollable(column(items).spacing(4))
            .height(Length::Fill)
            .into()
    };

    column![toolbar, body].spacing(8).padding(10).into()
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
        .width(EDITOR_W)
        .height(EDITOR_H)
        .style(panel_style)
        .padding(0);
    Some(crate::sftp_dialogs::overlay_wrap(panel.into()))
}

/// 弹窗主体：标题栏（含关闭按钮）+ 可滚动表单（分节）+ 固定底部按钮行。
fn editor_body<'a>(draft: &'a EditorDraft) -> Element<'a, Message> {
    // 标题栏随新建 / 编辑切换，右侧关闭按钮复用「取消编辑」语义；
    // 标题左侧的强调条取统一取色点 accent_color，随当前主题即时适配。
    let is_new = draft.is_new();
    let accent_bar = container(text("").size(16))
        .width(3.0)
        .height(16.0)
        .style(|theme: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(crate::theme::accent_color(theme))),
            border: Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    let header = row![
        accent_bar,
        text(if is_new {
            t!("session.editor_new")
        } else {
            t!("session.editor_edit")
        })
        .size(16)
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
    .padding([12, 16]);

    let auth_choice = match draft.auth.as_str() {
        "publickey" => AuthChoice::PublicKey,
        "agent" => AuthChoice::Agent,
        _ => AuthChoice::Password,
    };

    // 凭据字段随认证方式变化（字符串均取自 draft，生命周期与 &app 一致）。
    // 凭据为密文信封，编辑器内留空表示「保持不变」，故占位提示强调这一点。
    let cred_field: Element<'a, Message> = match draft.auth.as_str() {
        "password" => labeled_input(
            field_label(t!("session.password")),
            &draft.password,
            t!("session.keep_unchanged"),
            SessionField::Password,
            true,
            Length::Fill,
        ),
        "publickey" => column![
            // 私钥路径：输入框 + 文件系统选择按钮（document 图标）。
            column![
                field_label(t!("session.key_path")),
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
            .spacing(4),
            labeled_input(
                field_label(t!("session.passphrase")),
                &draft.passphrase,
                t!("session.keep_unchanged"),
                SessionField::Passphrase,
                true,
                Length::Fill,
            ),
        ]
        .spacing(12)
        .into(),
        _ => agent_hint(t!("session.agent_hint")),
    };

    // 表单分三节：连接（主机 + 端口两栏 / 用户名）、认证（方式 + 凭据）、名称与分组（两栏）。
    // 节标题用主文本色、字段标签用次级色，形成两级层次；节间距 18px、节内 12px 拉开呼吸感。
    let form = column![
        form_section(
            t!("session.section_connection"),
            column![
                row![
                    labeled_input(
                        required_label(t!("session.host")),
                        &draft.host,
                        "",
                        SessionField::Host,
                        false,
                        Length::Fill,
                    ),
                    labeled_input(
                        required_label(t!("session.port")),
                        &draft.port,
                        "",
                        SessionField::Port,
                        false,
                        Length::Fixed(PORT_FIELD_W),
                    ),
                ]
                .spacing(10),
                labeled_input(
                    field_label(t!("session.username")),
                    &draft.username,
                    "",
                    SessionField::Username,
                    false,
                    Length::Fill,
                ),
            ]
            .spacing(12),
        ),
        form_section(
            t!("session.section_auth"),
            column![
                column![
                    field_label(t!("session.auth")),
                    pick_list(&AuthChoice::ALL[..], Some(auth_choice), |c| {
                        Message::EditorField(SessionField::Auth, c.value().to_string())
                    },)
                    .width(Length::Fill)
                    .style(crate::theme::pick_list_style),
                ]
                .spacing(4),
                cred_field,
            ]
            .spacing(12),
        ),
        form_section(
            t!("session.section_identity"),
            row![
                labeled_input(
                    field_label(t!("session.name")),
                    &draft.name,
                    "user@host",
                    SessionField::Name,
                    false,
                    Length::Fill,
                ),
                labeled_input(
                    field_label(t!("session.group")),
                    &draft.group,
                    "",
                    SessionField::Group,
                    false,
                    Length::Fill,
                ),
            ]
            .spacing(10),
        ),
    ]
    .spacing(18)
    .padding([16, 20]);

    // 底部固定行：左侧校验错误（随字段修改自动清除，不撑走按钮），右侧「取消」次要按钮
    // +「保存」强调色主按钮（主次分明，符合桌面弹窗的按钮位置习惯）。
    let footer = container(
        row![
            container(
                text(draft.error.clone().unwrap_or_default())
                    .size(13)
                    .wrapping(Wrapping::Word)
                    .color(crate::ui::DANGER),
            )
            .width(Length::Fill)
            .align_y(iced::alignment::Vertical::Center),
            button(text(t!("common.cancel")).size(13))
                .on_press(Message::CancelEdit)
                .style(crate::sftp_dialogs::dialog_btn_style_neutral)
                .padding([8, 16]),
            button(text(t!("session.save")).size(13))
                .on_press(Message::SaveSession)
                .style(save_btn_style)
                .padding([8, 20]),
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .padding([12, 16]);

    column![
        header,
        hairline(),
        scrollable(form).height(Length::Fill),
        hairline(),
        footer,
    ]
    .into()
}

/// 编辑器弹窗面板样式：抬升表面底色 + 圆角 + 细边框 + 柔和投影。
///
/// 背景统一取 [`crate::theme::custom_palette`] 的 `surface_raised`（由主题背景感知亮度派生，
/// 随任意 iced 主题即时适配），与设置弹窗、tooltip 等抬升表面同源；投影让弹窗在遮罩上
/// 呈现悬浮层次，深浅主题下均用半透明黑（浅色主题遮罩本身已压暗背景，视觉一致）。
pub(crate) fn panel_style(theme: &Theme) -> iced::widget::container::Style {
    let p = crate::theme::custom_palette(theme);
    iced::widget::container::Style {
        background: Some(iced::Background::Color(p.surface_raised)),
        border: Border {
            color: p.border,
            width: 1.0,
            radius: 10.0.into(),
        },
        shadow: iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
            offset: iced::Vector::new(0.0, 8.0),
            blur_radius: 24.0,
        },
        ..Default::default()
    }
}

/// 弹窗内低对比度细分割线（表头下方 / 底部按钮区上方），颜色随主题 border 派生。
fn hairline() -> Element<'static, Message> {
    rule::horizontal(1)
        .style(|theme: &Theme| iced::widget::rule::Style {
            color: crate::theme::custom_palette(theme).border,
            radius: 0.0.into(),
            fill_mode: iced::widget::rule::FillMode::Full,
            snap: true,
        })
        .into()
}

/// 表单字段标签：次要文字色、略小字号，与输入值形成视觉层级，颜色随主题派生。
fn field_label<'a>(label: impl Into<String>) -> Element<'a, Message> {
    text(label.into())
        .size(13)
        .style(|theme: &Theme| iced::widget::text::Style {
            color: Some(crate::theme::custom_palette(theme).text_secondary),
        })
        .into()
}

/// 必填字段标签：在标签右侧追加强调色星号（强调色取统一取色点，随主题即时适配）。
fn required_label<'a>(label: impl Into<String>) -> Element<'a, Message> {
    row![
        field_label(label),
        text("*")
            .size(13)
            .style(|theme: &Theme| iced::widget::text::Style {
                color: Some(crate::theme::accent_color(theme)),
            }),
    ]
    .spacing(3)
    .into()
}

/// 小节标题：主文本色 13px，与次级色的字段标签构成两级层次。
fn section_title<'a>(label: impl Into<String>) -> Element<'a, Message> {
    text(label.into())
        .size(13)
        .style(|theme: &Theme| iced::widget::text::Style {
            color: Some(theme.extended_palette().background.base.text),
        })
        .into()
}

/// 表单小节：小节标题 + 字段组（节内 12px 间距；节间间距由外层列表给出）。
fn form_section<'a>(
    title: impl Into<String>,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![section_title(title), content.into()]
        .spacing(12)
        .into()
}

/// 说明 / 提示文字：12px 弱文字色，随主题派生（与设置弹窗的提示文字同级）。
fn hint_text<'a>(label: impl Into<String>) -> Element<'a, Message> {
    text(label.into())
        .size(12)
        .wrapping(Wrapping::Word)
        .style(|theme: &Theme| iced::widget::text::Style {
            color: Some(theme.extended_palette().background.weak.text),
        })
        .into()
}

/// SSH agent 提示：以「内嵌说明块」呈现（弱文字 + 表面底色 + 细边框），
/// 与输入框同处一列时高度、圆角一致，避免此处出现一块突兀的裸文字。
fn agent_hint<'a>(label: impl Into<String>) -> Element<'a, Message> {
    container(hint_text(label))
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
/// `label` 直接传入标签元素（便于 [`field_label`] / [`required_label`] 复用同一布局）；
/// `secure` 为 `true` 时掩码输入（用于密码 / 私钥口令等敏感字段）；`width` 控制整列宽度，
/// 使「主机 + 端口」这类一行两栏的排版得以成立。
fn labeled_input<'a>(
    label: Element<'a, Message>,
    value: &'a str,
    placeholder: impl Into<String>,
    field: SessionField,
    secure: bool,
    width: Length,
) -> Element<'a, Message> {
    let placeholder = placeholder.into();
    column![
        label,
        text_input(&placeholder, value)
            .secure(secure)
            .on_input(move |v| Message::EditorField(field, v))
            .style(crate::ui::text_input_style),
    ]
    .spacing(4)
    .width(width)
    .into()
}

/// 保存按钮样式：弹窗主操作按钮，常态即为强调色实底（统一取色点，随主题即时适配）。
///
/// 直接复用 [`crate::sftp_dialogs::dialog_btn_style`] 的强调按钮样式：悬浮 / 按下降至 85%
/// 不透明度作反馈（背景微调在中性底色上不可见，实底强调色上则清晰可辨），文字恒为白色。
/// 设置弹窗 / 主密码弹窗的保存按钮共用此样式，保证各弹窗主操作观感一致。
pub(crate) fn save_btn_style(theme: &Theme, status: button::Status) -> button::Style {
    crate::sftp_dialogs::dialog_btn_style(theme, status, false)
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
