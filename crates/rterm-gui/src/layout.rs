//! 三栏布局装配。
//!
//! 整体为水平 [`row!`]：固定宽度活动栏 + `pane_grid` 双栏（中心面板 / 右侧终端区）。
//! 两栏之间的分隔条由 `pane_grid` 原生提供拖拽与悬停高亮；左侧栏像素宽度通过
//! [`Message::Panes`] 在窗口缩放时按固定值
//! 重算比例，从而保持恒定，仅右侧跟随窗口变化。

use crate::App;
use crate::activity_bar;
use crate::app::panes;
use crate::app::updates;
use crate::icons::Icon;
use crate::message::Message;
use crate::session_panel;
use crate::sftp_panel;
use crate::state::CenterView;
use crate::t;
use crate::terminal_pane;
use crate::theme;
use crate::transfer_panel;
use iced::widget::{Space, button, column, container, pane_grid, row, stack, text};
use iced::{Color, Element, Length};

/// 渲染整个三栏主布局：固定宽度活动栏 + 中心面板 / 右侧终端区的 `pane_grid`。
pub fn view(app: &App) -> Element<'_, Message> {
    let activity = activity_bar::view(app);

    // `pane_grid` 双栏：依据 pane 标识映射到中心面板或右侧终端区。
    let grid = pane_grid::PaneGrid::new(&app.panes.pane_grid_state, |pane, _state, _max| {
        let body: Element<'_, Message> = if pane == app.panes.center_pane {
            match app.center {
                CenterView::Sessions => session_panel::view(app).map(Message::Session),
                CenterView::Files => sftp_panel::view(app).map(Message::Sftp),
                CenterView::Transfers => transfer_panel::view(app).map(Message::Transfer),
            }
        } else {
            terminal_pane::view(app)
        };
        // 中心 pane 不描边；右侧（终端）pane 仅在其左侧保留一条 1px 分隔线，
        // 不画上 / 右 / 下边框（iced 的 Border 为统一宽度，无法单边，故用独立分隔元素实现）。
        // 活动栏（surface 底色）与中心 pane（基色）的色差已足以区分两者。
        let bordered = if pane == app.panes.center_pane {
            container(body).height(Length::Fill)
        } else {
            let divider = container("")
                .width(Length::Fixed(1.0))
                .height(Length::Fill)
                .style(|theme| container::Style {
                    background: Some(crate::theme::pane_divider_color(theme).into()),
                    ..container::Style::default()
                });
            container(row![divider, body].spacing(0)).height(Length::Fill)
        };
        pane_grid::Content::new(bordered)
    })
    .spacing(0)
    .on_resize(4.0, |e| Message::Panes(panes::Message::Resized(e)))
    .style(theme::pane_grid_style);

    let main: Element<'_, Message> = row![
        container(activity)
            .width(Length::Fixed(theme::ACTIVITY_BAR_WIDTH))
            .height(Length::Fill),
        grid,
    ]
    .height(Length::Fill)
    .into();

    // 顶部横幅栈：目前只会装入更新提示（蓝），非阻塞，用户可手动关闭。
    let mut top: Vec<Element<'_, Message>> = Vec::new();

    // 更新提示横幅：发现新版本时显示，含「查看」按钮（打开发布页）与关闭。
    // 横幅属更新检查模块，故本函数产出 `updates::Message`，由下方 `.map(Message::Updates)` 提升。
    if let Some(banner) = update_banner(app) {
        top.push(banner.map(Message::Updates));
    }

    // 全局提示横幅（如导入 / 导出结果）已统一交由 `crate::widget::toast` 渲染（见函数末尾的
    // `toaster.view`），此处不再单独绘制。

    let body: Element<'_, Message> = if top.is_empty() {
        main
    } else {
        column(top).push(main).into()
    };

    // 将模态遮罩层叠加在窗口最顶层：覆盖整个窗口（活动栏 + 终端区 + 文件面板），
    // 而非仅覆盖文件管理面板，确保属性 / 确认框弹出时全窗口交互被拦截。
    let mut layers: Vec<Element<'_, Message>> = vec![body];
    if let Some(dialog) = crate::sftp_dialogs::view(app) {
        layers.push(dialog.map(Message::Sftp));
    }
    if let Some(dialog) = crate::settings_dialog::view(app) {
        layers.push(dialog);
    }
    // 会话编辑器弹窗叠加在最上层（列表在遮罩下仍可见）。
    if let Some(dialog) = crate::session_panel::editor_overlay(app) {
        layers.push(dialog.map(Message::Session));
    }
    // 主密码设置 / 解锁弹窗：未就绪时强制拦截，置于编辑器之上。
    // 视图生产 `masterpw::Message`，经 `.map(Message::MasterPw)` 接入顶层路由。
    if let Some(dialog) = crate::masterpw_dialog::view(app) {
        layers.push(dialog.map(Message::MasterPw));
    }
    // 「更改主密码」弹窗：从设置面板进入，置于主密码弹窗之上、主机密钥弹窗之下。
    if let Some(dialog) = crate::masterpw_change_dialog::view(app) {
        layers.push(dialog.map(Message::MasterPw));
    }
    // 主机密钥确认弹窗永远最顶层：安全决策必须压过其他一切弹窗。
    if let Some(dialog) = crate::host_key_dialog::view(app) {
        layers.push(dialog.map(Message::HostKey));
    }
    let root: Element<'_, Message> = stack(layers).into();

    // 将 toast 通知层叠加在窗口最顶层：覆盖主界面与所有模态弹窗之上，
    // 由 `crate::widget::toast` 在屏幕角落渲染并自动超时消失。
    app.toaster
        .view(root, Message::ToastDismissed, Message::ToastHovered)
}

/// 顶部更新提示横幅（发现新版本时）：产出 [`updates::Message`]，由调用方
/// `.map(Message::Updates)` 提升为顶层消息。
fn update_banner(app: &App) -> Option<Element<'_, updates::Message>> {
    let (version, url) = app.updates.banner.as_ref()?;
    let banner = container(
        row![
            text(t!("settings.update_banner", version => version))
                .size(13)
                .color(Color::WHITE),
            Space::new().width(Length::Fill),
            button(text(t!("settings.view_release")).size(13))
                .on_press(updates::Message::OpenReleasePage(url.clone()))
                .padding([2, 8])
                .style(|_t, _s| button::Style {
                    text_color: Color::WHITE,
                    border: iced::Border {
                        color: Color::WHITE,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..button::Style::default()
                }),
            button(Icon::Dismiss.svg_with_color(16.0, Color::WHITE))
                .on_press(updates::Message::DismissBanner)
                .padding(2)
                .style(|_t, _s| button::Style::default()),
        ]
        .align_y(iced::alignment::Vertical::Center)
        .spacing(6),
    )
    .width(Length::Fill)
    .padding(6)
    .style(|_t| container::Style {
        background: Some(iced::Background::Color(crate::theme::ACCENT)),
        ..container::Style::default()
    });
    Some(banner.into())
}
