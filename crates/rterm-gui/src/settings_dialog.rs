//! 设置弹窗（应用级偏好配置）。
//!
//! 借鉴 s3dm 的弹窗规范：半透明黑遮罩 + 居中面板 + 圆角 + `opaque` 拦截穿透，
//! 面板内部采用「左侧分类导航 + 右侧滚动内容」双栏结构。弹窗由
//! [`App::settings`](crate::app::App::settings) 驱动，复用
//! [`sftp_dialogs::overlay_wrap`] 叠加到窗口最顶层。
//!
//! 本文件仅负责纯渲染：所有交互经 [`crate::message::Message::Settings`] 路由进
//! [`crate::app::settings`] 模块，自身不持有业务逻辑。

use crate::t;

use crate::app::App;
use crate::app::masterpw;
use crate::app::settings;
use crate::app::updates;
use crate::icons::{Icon, icon_button};
use crate::message::Message;
use crate::sftp_dialogs;
use crate::theme;
use iced::widget::{
    button, checkbox, column, combo_box, container, pick_list, row, scrollable, slider, text,
    text_input,
};
use iced::{Border, Element, Length, Theme};
use rterm_config::{Language, LogLevel};
use std::fmt;

/// 语言下拉框项的本地化展示名：随当前 UI 语言变化，避免英文界面仍显示「跟随系统」。
#[derive(Clone, Copy, PartialEq, Eq)]
struct LanguageOption(Language);

impl fmt::Display for LanguageOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self.0 {
            Language::System => t!("settings.language_system"),
            Language::ZhCn => t!("settings.language_zh_cn"),
            Language::En => t!("settings.language_en"),
        };
        f.write_str(&s)
    }
}

/// 语言下拉框的静态选项（`'static`，可被 iced 的 `pick_list` 借用而不受函数作用域限制）。
const LANGUAGE_OPTIONS: [LanguageOption; 3] = [
    LanguageOption(Language::System),
    LanguageOption(Language::ZhCn),
    LanguageOption(Language::En),
];

/// 设置弹窗面板宽度（像素）。
const SETTINGS_W: f32 = 720.0;
/// 设置弹窗面板高度（像素）。
const SETTINGS_H: f32 = 480.0;
/// 左侧分类导航栏宽度（像素）。
const NAV_W: f32 = 160.0;

/// 根据当前是否显示设置弹窗返回遮罩层元素；未显示时返回 `None`。
pub fn view(app: &App) -> Option<Element<'_, Message>> {
    if !app.settings.show_settings {
        return None;
    }
    let panel = container(body(app))
        .width(SETTINGS_W)
        .height(SETTINGS_H)
        .style(crate::ui::dialog_panel_style(None))
        .padding(0);
    Some(sftp_dialogs::overlay_wrap(panel.into()))
}

/// 弹窗主体：顶部标题栏（含强调条与关闭按钮）+ 左侧分类导航 + 右侧内容区。
fn body(app: &App) -> Element<'_, Message> {
    column![
        crate::ui::dialog_title_bar(
            t!("settings.title"),
            Some(Message::Settings(settings::Message::Toggle)),
        ),
        crate::ui::hairline(),
        row![nav_pane(app), content_pane(app)]
            .width(Length::Fill)
            .height(Length::Fill),
    ]
    .into()
}

/// 左侧分类导航栏（纯文字，选中项高亮）。
///
/// 底色取**窗口主背景**而非另派生一个灰：面板本身是 `surface_raised`（背景 +0.10），
/// 左栏下沉一档即构成「内嵌」观感，与输入框底同源。这样两侧无需再加竖分隔线——加线反而会
/// 与标题栏下的 [`crate::ui::hairline`] 撞出一个生硬的 T 形接点。
fn nav_pane(app: &App) -> Element<'_, Message> {
    let items = [
        nav_item(
            t!("settings.general"),
            settings::SettingsCategory::General,
            app.settings.category == settings::SettingsCategory::General,
        ),
        nav_item(
            t!("settings.masterpw"),
            settings::SettingsCategory::MasterPassword,
            app.settings.category == settings::SettingsCategory::MasterPassword,
        ),
        nav_item(
            t!("settings.appearance"),
            settings::SettingsCategory::Appearance,
            app.settings.category == settings::SettingsCategory::Appearance,
        ),
        nav_item(
            t!("settings.updates"),
            settings::SettingsCategory::Updates,
            app.settings.category == settings::SettingsCategory::Updates,
        ),
        nav_item(
            t!("settings.about"),
            settings::SettingsCategory::About,
            app.settings.category == settings::SettingsCategory::About,
        ),
    ];
    // `column!` 本身不支持 `.style()`，故外层包一个 container 承载底色与圆角。
    container(column(items).spacing(4).padding(8))
        .width(Length::Fixed(NAV_W))
        .height(Length::Fill)
        .style(nav_pane_style)
        .into()
}

/// 左栏底色：窗口主背景 + 跟随面板圆角的左下角。
///
/// 左下角必须显式给圆角：iced 的 `container` 默认不裁剪子元素（`clip = false`），
/// 左栏与面板下边缘齐平，方角会溢出面板的圆角轮廓。
fn nav_pane_style(theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(iced::Background::Color(theme.palette().background)),
        border: Border {
            radius: iced::border::Radius {
                bottom_left: crate::ui::DIALOG_PANEL_RADIUS,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

/// 单个分类导航项按钮（纯文字）。
fn nav_item(
    label: impl Into<String>,
    category: settings::SettingsCategory,
    selected: bool,
) -> Element<'static, Message> {
    button(text(label.into()).size(13).width(Length::Fill))
        .on_press(Message::Settings(settings::Message::CategorySelected(
            category,
        )))
        .width(Length::Fill)
        .style(move |theme, status| nav_item_style(theme, status, selected))
        .into()
}

/// 分类导航项样式：选中态以强调色高亮，悬停态浅色背景，文字颜色跟随主题。
///
/// 文字色区分选中与否：未选中取次级文字色、选中取主文本色。此前两者同为
/// `background.strong.text`（一个偏亮的灰），五项在视觉上等重、都在争抢注意力。
fn nav_item_style(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let background = if selected {
        Some(theme::accent_active_bg(theme).into())
    } else {
        match status {
            button::Status::Hovered | button::Status::Pressed => {
                Some(crate::theme::custom_palette(theme).hover.into())
            }
            _ => None,
        }
    };
    let text_color = if selected {
        theme.extended_palette().background.base.text
    } else {
        crate::theme::custom_palette(theme).text_secondary
    };
    button::Style {
        background,
        border: Border {
            radius: theme::ICON_BTN_RADIUS.into(),
            ..Default::default()
        },
        text_color,
        ..Default::default()
    }
}

/// 右侧内容区：固定在顶部的分类标题 + 其下的可滚动表单。
///
/// 标题移出滚动区，是为了滚动后仍能确认当前处在哪个分类（左栏高亮未必在视野内）；
/// 正文内边距也在这里统一给出，各分类页不再各自 `padding(20)`。
fn content_pane(app: &App) -> Element<'_, Message> {
    let (title, content) = match app.settings.category {
        settings::SettingsCategory::General => (t!("settings.general"), general_pane(app)),
        settings::SettingsCategory::MasterPassword => (t!("settings.masterpw"), masterpw_pane(app)),
        settings::SettingsCategory::Appearance => (t!("settings.appearance"), appearance_pane(app)),
        settings::SettingsCategory::Updates => (t!("settings.updates"), updates_pane(app)),
        settings::SettingsCategory::About => (t!("settings.about"), about_pane()),
    };
    let header = container(crate::ui::pane_title(title))
        .width(Length::Fill)
        .padding(crate::ui::DIALOG_BODY_PADDING);
    // 正文的上边距已由标题容器的下内边距给出，这里只补左右与底部。
    let body = container(content)
        .width(Length::Fill)
        .padding(iced::Padding {
            top: 0.0,
            right: crate::ui::DIALOG_BODY_PADDING[1],
            bottom: crate::ui::DIALOG_BODY_PADDING[0],
            left: crate::ui::DIALOG_BODY_PADDING[1],
        });
    column![
        header,
        scrollable(body).width(Length::Fill).height(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// “通用”分类：连接超时、日志级别、界面语言。
fn general_pane(app: &App) -> Element<'_, Message> {
    let timeout_input = text_input("30", &app.config.connection.timeout.to_string())
        .on_input(|s| Message::Settings(settings::Message::ConnectTimeout(s)))
        .style(crate::ui::text_input_style);
    let scrollback_input = text_input("10000", &app.config.terminal.scrollback.to_string())
        .on_input(|s| Message::Settings(settings::Message::Scrollback(s)))
        .style(crate::ui::text_input_style);
    let log_level_picker = pick_list(&LogLevel::ALL[..], Some(app.config.logging.level), |lv| {
        Message::Settings(settings::Message::LogLevel(lv))
    })
    .style(theme::pick_list_style)
    .width(Length::Fill);
    let language_picker = pick_list(
        &LANGUAGE_OPTIONS[..],
        Some(LanguageOption(app.config.appearance.language)),
        |lang| Message::Settings(settings::Message::Language(lang.0)),
    )
    .style(theme::pick_list_style)
    .width(Length::Fill);
    column![
        crate::ui::field_label(t!("settings.connect_timeout")),
        timeout_input,
        crate::ui::field_label(t!("settings.scrollback")),
        scrollback_input,
        crate::ui::hint_text(t!("settings.scrollback_note")),
        crate::ui::field_label(t!("settings.log_level")),
        row![
            log_level_picker,
            // 纯图标按钮：打开日志目录，避免长文字挤压 pick_list 宽度。
            icon_button(
                Icon::Folder,
                crate::icons::ICON_SIZE,
                t!("settings.open_log_folder"),
                Message::Settings(settings::Message::OpenLogFolder),
                iced::widget::tooltip::Position::Bottom,
            ),
        ]
        .align_y(iced::alignment::Vertical::Center)
        .spacing(8),
        crate::ui::hint_text(t!("settings.restart_note")),
        crate::ui::field_label(t!("settings.language")),
        language_picker,
        checkbox(app.config.terminal.cwd_bootstrap)
            .label(t!("settings.cwd_bootstrap"))
            .on_toggle(|v| Message::Settings(settings::Message::CwdBootstrap(v)))
            .spacing(8),
        crate::ui::hint_text(t!("settings.cwd_bootstrap_hint")),
        checkbox(app.config.terminal.suppress_bootstrap_echo)
            .label(t!("settings.suppress_bootstrap_echo"))
            .on_toggle(|v| Message::Settings(settings::Message::SuppressBootstrapEcho(v)))
            .spacing(8),
        crate::ui::hint_text(t!("settings.suppress_bootstrap_echo_hint")),
        checkbox(app.config.terminal.trim_trailing_whitespace)
            .label(t!("settings.trim_trailing_whitespace"))
            .on_toggle(|v| Message::Settings(settings::Message::TrimTrailingWhitespace(v)))
            .spacing(8),
        crate::ui::hint_text(t!("settings.trim_trailing_whitespace_hint")),
        checkbox(app.config.window.remember_size)
            .label(t!("settings.remember_window_size"))
            .on_toggle(|v| Message::Settings(settings::Message::RememberWindowSize(v)))
            .spacing(8),
        crate::ui::hint_text(t!("settings.remember_window_size_hint")),
    ]
    .spacing(10)
    .into()
}

/// “更新”分类：自动检查开关、立即检查、当前/最新版本与前往下载。
fn updates_pane(app: &App) -> Element<'_, Message> {
    // 动作按钮走 `ui::inline_action_button`：内联动作取比弹窗页脚按钮小一档的紧凑尺寸
    // （12px 字 + 竖向 5 内边距），与同级字段标签相称，不把分类页的视觉重心吸走。
    // 「立即检查」是主操作（强调色实底），「前往下载」是中性次要操作。
    let auto_check = checkbox(app.config.updates.auto_check)
        .label(t!("settings.auto_check_updates"))
        .on_toggle(|v| Message::Settings(settings::Message::AutoCheckUpdates(v)))
        .spacing(8);
    let check_button = crate::ui::inline_action_button(
        t!("settings.check_now"),
        Message::Updates(updates::Message::CheckNow),
        crate::ui::DialogBtnStyle::Emphasis { danger: false },
    );

    let current = text(env!("CARGO_PKG_VERSION")).size(14);
    // 横幅持有（版本, URL）即视为已发现更新；否则显示未知（尚未检查 / 检查失败）。
    let (latest, view_button) = match &app.updates.banner {
        Some((version, url)) => (
            text(version).size(14),
            Some(crate::ui::inline_action_button(
                t!("settings.view_release"),
                Message::Updates(updates::Message::OpenReleasePage(url.clone())),
                crate::ui::DialogBtnStyle::Neutral,
            )),
        ),
        None => (text(t!("settings.update_unknown")).size(14), None),
    };

    let mut actions: Vec<Element<'_, Message>> = vec![check_button.into()];
    if let Some(b) = view_button {
        actions.push(b.into());
    }

    // 「立即检查」的就地反馈：手动检查结果直接显示在弹窗内（弹窗打开时右下角 toast 不可见）。
    // 自动检查失败仅记日志、不在此呈现，故 `manual_status` 仅由手动检查维护。
    let status: Option<Element<'_, Message>> = match &app.updates.manual_status {
        Some(updates::CheckStatus::Checking) => Some(crate::ui::hint_text(t!("settings.checking"))),
        Some(updates::CheckStatus::UpToDate) => Some(
            text(t!("settings.up_to_date"))
                .size(13)
                .style(|theme: &Theme| text::Style {
                    // 用强调色本身而非 `*.base.text`（`*.base.text` 是配在彩色背景上的文字色，
                    // 作前景叠在面板中性底色上会在暗/亮主题下都看不清）。
                    color: Some(theme.extended_palette().success.strong.color),
                })
                .into(),
        ),
        Some(updates::CheckStatus::Found(v)) => Some(
            text(format!("{} v{v}", t!("settings.found_update")))
                .size(13)
                .style(|theme: &Theme| text::Style {
                    color: Some(theme.extended_palette().success.strong.color),
                })
                .into(),
        ),
        Some(updates::CheckStatus::Error(e)) => Some(
            text(format!("{}: {e}", t!("settings.check_failed")))
                .size(13)
                .style(|theme: &Theme| text::Style {
                    color: Some(theme.extended_palette().danger.base.color),
                })
                .into(),
        ),
        None => None,
    };

    let mut col = column![
        crate::ui::field_label(t!("settings.auto_check_updates")),
        auto_check,
        crate::ui::field_label(t!("settings.current_version")),
        current,
        crate::ui::field_label(t!("settings.latest_version")),
        latest,
        iced::widget::Space::new().height(6.0),
        row(actions)
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center),
    ]
    .spacing(10);
    // 仅在有手动检查结果时追加反馈行（弹窗打开时右下角 toast 不可见）。
    if let Some(s) = status {
        col = col.push(iced::widget::Space::new().height(6.0)).push(s);
    }
    col.into()
}

/// “关于”分类：版本与简介（只读信息）。
fn about_pane() -> Element<'static, Message> {
    column![
        crate::ui::field_label(t!("settings.about_name")),
        text("rterm").size(14),
        crate::ui::field_label(t!("settings.about_version")),
        text(env!("CARGO_PKG_VERSION")).size(14),
        crate::ui::field_label(t!("settings.about_desc")),
        text(t!("settings.about_description")).size(14),
    ]
    .spacing(10)
    .into()
}

/// 「主密码」分类：按当前模式（随机密钥 / 已设主密码）展示状态与可用操作。
///
/// - 模式 0（未设主密码）：状态说明 + 「设置主密码」按钮（升级到模式 1，凭据重新加密）；
///   随机密钥必然在钥匙串，不显示「本机记住」开关。
/// - 模式 1（已设主密码）：状态说明 + 「更改主密码」/「关闭主密码」+ 「本机记住」开关。
fn masterpw_pane(app: &App) -> Element<'_, Message> {
    // 三个动作按钮统一走 `ui::inline_action_button`：内联动作的紧凑尺寸（12px 字 +
    // 竖向 5 内边距），左对齐成行。此前是 16px + `[10, 24]` 并各自套
    // `container(width=Fill).align_x(Center)` 做通栏居中——那正是页脚按钮已废除的形态；
    // 之后一度改为与页脚按钮同尺寸，但对分类页仍是过重的一排，故单列一档更小的尺寸。
    let action_btn = |label: String, on_press: masterpw::Message, danger: bool| {
        crate::ui::inline_action_button(
            label,
            Message::MasterPw(on_press),
            crate::ui::DialogBtnStyle::Emphasis { danger },
        )
    };

    let is_mode1 = app
        .vault
        .as_ref()
        .map(|v| v.header().master_password_set)
        .unwrap_or(false);

    let mut col = column![
        crate::ui::field_label(if is_mode1 {
            t!("masterpw.status_enabled")
        } else {
            t!("masterpw.status_random")
        }),
        text(if is_mode1 {
            t!("masterpw.status_desc")
        } else {
            t!("masterpw.status_random_desc")
        })
        .size(14),
    ];

    if is_mode1 {
        col = col
            .push(
                row![
                    action_btn(t!("masterpw.change"), masterpw::Message::ChangeOpen, false),
                    // 「关闭主密码」是重建随机密钥 + 全量重加密的不可逆降权，
                    // 故用危险红实底，且点击只打开二次确认（见 `masterpw_dialog`）。
                    action_btn(
                        t!("masterpw.disable"),
                        masterpw::Message::DisableRequest,
                        true
                    ),
                ]
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center),
            )
            // 「本机记住主密码」开关：开启后把 DEK 存入系统钥匙串，下次启动自动解锁。
            .push(
                checkbox(app.config.security.remember_master_key)
                    .label(t!("masterpw.remember"))
                    .on_toggle(|v| Message::MasterPw(masterpw::Message::RememberToggled(v))),
            )
            .push(crate::ui::hint_text(t!("masterpw.remember_desc")));
    } else {
        col = col
            .push(crate::ui::hint_text(t!("masterpw.setup_hint")))
            .push(action_btn(
                t!("masterpw.setup"),
                masterpw::Message::SetupOpen,
                false,
            ));
    }

    col.spacing(12).into()
}

/// “外观”分类：程序主题、界面字体、终端配色与字号。
fn appearance_pane(app: &App) -> Element<'_, Message> {
    let theme_choice = pick_list(
        crate::theme::theme_names(),
        Some(current_theme_label(&app.config.appearance.theme)),
        |label| Message::Settings(settings::Message::Theme(label.to_string())),
    )
    .style(theme::pick_list_style);
    // 下拉框显示「系统默认」而非空串；选中默认标签或键入该标签均映射为空字符串，
    // 从而回退到 iced 默认字体（见 `lib.rs` 启动期应用逻辑）。
    let font_selection = if app.config.appearance.ui_font.trim().is_empty() {
        crate::font::default_font_label()
    } else {
        app.config.appearance.ui_font.clone()
    };
    let font_picker = combo_box(
        &app.settings.ui_font_combo,
        &t!("settings.ui_font_placeholder"),
        Some(&font_selection),
        |name: String| map_default_font(name, |s| Message::Settings(settings::Message::UiFont(s))),
    )
    .on_input(|s: String| map_default_font(s, |s| Message::Settings(settings::Message::UiFont(s))))
    .input_style(crate::ui::text_input_style)
    .width(Length::Fill);
    // 终端配色主题下拉框：选项为预设名，选中即热切换到所有已打开的终端标签。
    let terminal_theme_picker = pick_list(
        crate::terminal_theme::TERMINAL_THEME_NAMES,
        Some(app.config.terminal.theme.as_str()),
        |name: &str| Message::Settings(settings::Message::TerminalTheme(name.to_string())),
    )
    .style(theme::pick_list_style)
    .width(Length::Fill);
    // 终端字体下拉框：仅列出等宽字体（非等宽会破坏字符网格），选中即热切换所有终端。
    let terminal_font_selection = if app.config.terminal.font.trim().is_empty() {
        crate::font::default_font_label()
    } else {
        app.config.terminal.font.clone()
    };
    let terminal_font_picker = combo_box(
        &app.settings.terminal_font_combo,
        &t!("settings.terminal_font_placeholder"),
        Some(&terminal_font_selection),
        |name: String| {
            map_default_font(name, |s| {
                Message::Settings(settings::Message::TerminalFont(s))
            })
        },
    )
    .on_input(|s: String| {
        map_default_font(s, |s| Message::Settings(settings::Message::TerminalFont(s)))
    })
    .input_style(crate::ui::text_input_style)
    .width(Length::Fill);
    let size_slider = slider(8.0..=32.0, app.config.terminal.font_size, |size| {
        Message::Settings(settings::Message::FontSize(size))
    })
    .step(1.0_f32);
    let size_value = text(format!("{:.0} px", app.config.terminal.font_size)).size(13);
    column![
        crate::ui::field_label(t!("settings.theme")),
        theme_choice,
        crate::ui::field_label(t!("settings.ui_font")),
        font_picker,
        crate::ui::field_label(t!("settings.ui_font_preview")),
        font_preview(app),
        crate::ui::field_label(t!("settings.terminal_theme")),
        terminal_theme_picker,
        crate::ui::field_label(t!("settings.terminal_font")),
        terminal_font_picker,
        crate::ui::field_label(t!("settings.terminal_font_preview")),
        terminal_font_preview(app),
        crate::ui::field_label(t!("settings.terminal_font_size")),
        row![size_slider, size_value]
            .spacing(12)
            .align_y(iced::alignment::Vertical::Center),
        crate::ui::hint_text(t!("settings.appearance_note")),
    ]
    .spacing(10)
    .into()
}

/// 界面字体预览样本：以当前选中的字体族实时展示示例文字。
///
/// 预览为逐控件指定 `Font`，不依赖全局默认字体的启动期设置，故选中即可即时反映。
/// 族名由 `crate::font` 缓存并 `Box::leak` 为 `&'static str`（iced 的 `Family::Name` 只收
/// `&'static str`），故每帧重建也只是查表、不会持续泄漏。
fn font_preview(app: &App) -> Element<'_, Message> {
    let font = crate::font::resolve_font(&app.config.appearance.ui_font);
    container(
        column![
            text("The quick brown fox jumps over the lazy dog.")
                .font(font)
                .size(15),
            text("rterm 界面字体预览 0123456789").font(font).size(14),
        ]
        .spacing(6),
    )
    .padding(12)
    .style(preview_style)
    .into()
}

/// 终端字体预览样本：使用等宽字体族展示含制表符与边框字形的示例，
/// 直观验证字符网格是否对齐。族名同样由 `crate::font` 缓存为 `&'static str`。
fn terminal_font_preview(app: &App) -> Element<'_, Message> {
    let font = crate::font::resolve_terminal_font(&app.config.terminal.font);
    container(
        column![
            text("The quick brown fox jumps over the lazy dog.")
                .font(font)
                .size(15),
            text("rterm 终端字体预览 0123456789 ├──┐")
                .font(font)
                .size(14),
        ]
        .spacing(6),
    )
    .padding(12)
    .style(preview_style)
    .into()
}

/// 预览样本块的底色与描边：与输入框同源——取窗口主背景（比面板 `surface_raised` 低一档，
/// 构成「内嵌」观感）+ [`crate::theme::input_border_color`]。
///
/// 此前取 `extended_palette().background.weak`：那档颜色在浅色主题下比主背景更亮、深色主题下
/// 反而更暗，且边框取的是 `custom_palette().border`——与输入框不是同一个值，同一页里两种
/// 「嵌块」看起来不像一家。
fn preview_style(theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(iced::Background::Color(theme.palette().background)),
        border: Border {
            color: crate::theme::input_border_color(theme),
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

/// 将下拉框 / 输入框的字体选择归一化：选中「系统默认」标签映射为空字符串，
/// 其余原样作为字体族名称经 `into` 写入对应配置消息。
///
/// UI 字体与终端字体仅目标消息变体不同，故以 `into` 构造器去重，避免两份对称实现。
fn map_default_font(name: String, into: impl FnOnce(String) -> Message) -> Message {
    if name.trim() == crate::font::default_font_label() {
        into(String::new())
    } else {
        into(name)
    }
}

/// 将配置中的主题标识映射为下拉框展示名。
///
/// 兼容旧版配置值 `"dark"` / `"light"`（分别映射 `Dark` / `Light`）；
/// 否则在 [`theme::theme_names`] 中按显示名查找，未匹配时回退 `Dark`。
fn current_theme_label(theme: &str) -> &'static str {
    match theme {
        "dark" => "Dark",
        "light" => "Light",
        other => crate::theme::theme_names()
            .iter()
            .find(|n| **n == other)
            .copied()
            .unwrap_or("Dark"),
    }
}
