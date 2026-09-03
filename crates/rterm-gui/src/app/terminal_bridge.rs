//! 终端 widget 与 PTY 桥接的生命周期及事件转发。

use crate::app::App;
use crate::app::tabs;
use crate::app::tasks::open_terminal_task;
use crate::font;
use crate::i18n::localize_error;
use crate::message::{Message, ResizeSender};
use crate::t;
use crate::terminal_theme;
use crate::widget::term::settings::{FontSettings, Settings as TermSettings, ThemeSettings};
use crate::widget::term::{
    BackendCommand, Command as TermCommand, Event as TermEvent, RusshPty, Terminal,
};
use iced::Task;
use rterm_core::ConnectionStatus;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

/// 把当前终端字体（族名 + 字号）热替换到所有已打开的终端标签。
///
/// 复用 `Terminal::handle(ChangeFont)` 路径，仅替换 `Terminal` 内部的 `TermFont`
/// 并触发重绘，不重建 widget；旧 `Font` 持有的 `Cow::Owned` 族名随丢弃释放，无泄漏。
pub(crate) fn apply_terminal_font(tabs: &mut tabs::State, font_name: &str, size: f32) {
    let font_type = font::resolve_terminal_font(font_name);
    for tab in tabs.list_mut().iter_mut() {
        if let Some(term) = tab.terminal.as_mut() {
            term.handle(TermCommand::ChangeFont(FontSettings {
                size,
                scale_factor: 1.3,
                font_type,
            }));
        }
    }
}

/// 把终端配色主题（预设名）热替换到所有已打开的终端标签。
///
/// 先按当前值解析调色板，再逐标签 `ChangeTheme`；不写配置（配置写回在父层 `apply_settings_event`）。
pub(crate) fn apply_terminal_theme(tabs: &mut tabs::State, theme: &str) {
    let palette = terminal_theme::resolve_terminal_theme(theme);
    for tab in tabs.list_mut().iter_mut() {
        if let Some(term) = tab.terminal.as_mut() {
            term.handle(TermCommand::ChangeTheme(Box::new(palette.clone())));
        }
    }
}

/// 为已存在的（连接中）标签挂载连接并发起桥接任务。
pub(crate) fn open_terminal_bridge(
    app: &mut App,
    tab_id: u64,
    conn: Arc<rterm_core::SshConnection>,
) -> Task<Message> {
    if let Some(tab) = app.tabs.tab_mut(tab_id) {
        tab.conn = Some(conn.clone());
    }
    // 桥接结束（断线）时经 `disconnect_rx` 回发 `TerminalDisconnected(tab_id)`，
    // 按标签（而非按会话）把状态置为 `Error`。
    let (disconnect_tx, mut disconnect_rx) = mpsc::channel::<()>(1);
    let bridge = Task::perform(
        open_terminal_task(
            conn,
            super::DEFAULT_COLS,
            super::DEFAULT_ROWS,
            disconnect_tx,
        ),
        move |res| {
            Message::Tabs(tabs::Message::TerminalOpened(
                tab_id,
                res.map_err(|e| localize_error(&e)),
            ))
        },
    );
    let disconnect = Task::perform(
        async move {
            let _ = disconnect_rx.recv().await;
        },
        move |_| Message::Tabs(tabs::Message::TerminalDisconnected(tab_id)),
    );
    Task::batch([bridge, disconnect])
}

/// 桥接就绪后用返回的本地 OUT/IN 双管道端创建终端组件，并自动聚焦。
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_terminal_widget(
    app: &mut App,
    tab_id: u64,
    conout: Arc<std::fs::File>,
    conin: Arc<std::fs::File>,
    disconnect: Arc<std::sync::atomic::AtomicBool>,
    resize_tx: ResizeSender,
) -> Task<Message> {
    // 把本地管道同步端包成 russh 自定义 pty，直接桥接远端 shell 通道。
    let conout = conout
        .try_clone()
        .expect("failed to clone local pipe output handle");
    let conin = conin
        .try_clone()
        .expect("failed to clone local pipe input handle");
    let russh_pty = RusshPty::new(conout, conin, disconnect.clone(), resize_tx.clone());
    let settings = TermSettings {
        backend: Default::default(),
        font: FontSettings {
            size: app.config.font_size,
            scale_factor: 1.3,
            font_type: font::resolve_terminal_font(&app.config.terminal_font),
        },
        theme: ThemeSettings::new(Box::new(terminal_theme::resolve_terminal_theme(
            &app.config.terminal_theme,
        ))),
    };
    match Terminal::new_with_pty(tab_id, settings, russh_pty) {
        Ok(terminal) => {
            if let Some(tab) = app.tabs.tab_mut(tab_id) {
                tab.terminal = Some(terminal);
                tab.resize_tx = Some(resize_tx);
                // 记录桥接断开标志，关标签 / 关窗口时置位以通知 pump 退出。
                tab.disconnect = Some(disconnect.clone());
                // 终端组件就绪即代表连接可用，此时才把本标签标记为已连接，
                // 使文件管理等依赖 Connected 的逻辑与终端实际可用状态一致。
                tab.status = ConnectionStatus::Connected;
                tab.error = None;
            }
            app.status = Some(t!("app.terminal_ready"));
            // 焦点由 app 级 `terminal_focused` 驱动并传入 widget，此处置 true
            // 即代表新建终端持有键盘焦点（光标实心、可输入）。
            app.terminal_focused = true;
            // 延迟一小段时间后强制重绘一次，
            // 以覆盖订阅激活与首屏远端数据到达的时序差（避免首屏空白或显示不全）。
            Task::batch([Task::perform(sleep(Duration::from_millis(80)), move |_| {
                Message::Tabs(tabs::Message::TerminalReady(tab_id))
            })])
        }
        Err(e) => {
            app.status = Some(t!("app.terminal_create_failed", err => e));
            app.tabs.list_mut().retain(|t| t.id != tab_id);
            Task::none()
        }
    }
}

/// 处理终端部件后端回调（键盘 / 鼠标 / resize 等）。
///
/// 当前版本的 `crate::widget::term::Event` 仅含 `BackendCall` 一个变体，故直接解构。
pub(crate) fn handle_terminal_event(app: &mut App, event: TermEvent) -> Task<Message> {
    let TermEvent::BackendCall(id, backend_cmd) = event;
    // 点击 / 选择 / 滚轮 / 键入等「用户与终端的交互」都以非 Resize 的 BackendCall 形式到达；
    // 一旦出现即说明键盘焦点已落回终端，恢复聚焦态（修正「点回终端边框仍显未聚焦」）。
    // 必须排除 `ProcessAlacrittyEvent`：它是 PTY 输出经订阅推送的事件，不代表用户交互——
    // 否则后台终端一有输出（日志、运行命令）就会误把焦点判为聚焦，而用户其实在文件管理器输入。
    let is_user_interaction = matches!(
        &backend_cmd,
        BackendCommand::SelectStart(..)
            | BackendCommand::SelectUpdate(..)
            | BackendCommand::MouseReport(..)
            | BackendCommand::Scroll(..)
            | BackendCommand::ProcessLink(..)
            | BackendCommand::Write(..)
    );
    if is_user_interaction {
        app.terminal_focused = true;
    }
    if let Some(tab) = app.tabs.tab_mut(id) {
        // 本地终端尺寸变化时，转发到远端（window-change）。
        if let BackendCommand::Resize(Some(layout), Some(font)) = &backend_cmd {
            let cols = (layout.width / font.width).floor().max(1.0) as u32;
            let rows = (layout.height / font.height).floor().max(1.0) as u32;
            if let Some(tx) = &tab.resize_tx {
                let _ = tx.try_send((cols, rows));
            }
        }
        // 将命令转交终端组件（写入 PTY / 调整布局等）。
        if let Some(term) = tab.terminal.as_mut() {
            term.handle(TermCommand::ProxyToBackend(backend_cmd));
        }
    }
    Task::none()
}
