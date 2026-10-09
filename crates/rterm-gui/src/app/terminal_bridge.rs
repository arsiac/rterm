//! 终端 widget 与 PTY 桥接的生命周期及事件转发。

use crate::app::App;
use crate::app::contexts;
use crate::app::tabs;
use crate::app::tasks::open_terminal_task;
use crate::font;
use crate::i18n::localize_error;
use crate::message::{Message, ResizeSender};
use crate::t;
use crate::terminal_theme;
use crate::widget::term::actions::Action;
use crate::widget::term::settings::{
    BackendSettings, FontSettings, Settings as TermSettings, ThemeSettings,
};
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
    // 桥接结束（断线）时经 `disconnect_rx` 回发 `TerminalDisconnected(tab_id, reason)`，
    // 按标签（而非按会话）把状态置为 `Error` 并携带退出原因（会话结束 / 连接断开）。
    let (disconnect_tx, mut disconnect_rx) = mpsc::channel::<rterm_core::DisconnectReason>(1);
    // 取出本标签的 cwd 共享容器，随桥接传给核心层（pump 扫描 OSC 7 写入）。
    let cwd = app
        .tabs
        .list()
        .iter()
        .find(|t| t.id == tab_id)
        .map(|t| t.cwd.clone());
    let bridge = Task::perform(
        open_terminal_task(
            conn,
            super::DEFAULT_COLS,
            super::DEFAULT_ROWS,
            disconnect_tx,
            cwd,
            app.config.terminal.cwd_bootstrap,
            app.config.terminal.suppress_bootstrap_echo,
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
            // 桥接结束：`Some(reason)` 携带退出原因；发送端被丢弃（标签已清理）时
            // `recv()` 立即返回 `None`，以 `Unknown` 占位（该消息随后查无此标签，为无害空转）。
            disconnect_rx.recv().await.unwrap_or_default()
        },
        move |reason| Message::Tabs(tabs::Message::TerminalDisconnected(tab_id, reason)),
    );
    Task::batch([bridge, disconnect])
}

/// 桥接就绪后用返回的本地 OUT/IN 双管道端创建终端组件，并自动聚焦。
pub(crate) fn spawn_terminal_widget(
    app: &mut App,
    tab_id: u64,
    conout: Arc<std::fs::File>,
    conin: Arc<std::fs::File>,
    bridge: Arc<rterm_core::BridgeState>,
    resize_tx: ResizeSender,
) -> Task<Message> {
    // 把本地管道同步端包成 russh 自定义 pty，直接桥接远端 shell 通道。
    let conout = match conout.try_clone() {
        Ok(f) => f,
        Err(e) => {
            app.status = Some(t!("app.pipe_clone_failed", err => e));
            return Task::none();
        }
    };
    let conin = match conin.try_clone() {
        Ok(f) => f,
        Err(e) => {
            app.status = Some(t!("app.pipe_clone_failed", err => e));
            return Task::none();
        }
    };
    let russh_pty = match RusshPty::new(conout, conin, bridge.clone(), resize_tx.clone()) {
        Ok(p) => p,
        Err(e) => {
            app.status = Some(t!("app.pipe_clone_failed", err => e));
            return Task::none();
        }
    };
    let settings = TermSettings {
        backend: BackendSettings {
            scrollback: app.config.terminal.scrollback,
            trim_trailing_whitespace: app.config.terminal.trim_trailing_whitespace,
            ..Default::default()
        },
        font: FontSettings {
            size: app.config.terminal.font_size,
            scale_factor: 1.3,
            font_type: font::resolve_terminal_font(&app.config.terminal.font),
        },
        theme: ThemeSettings::new(Box::new(terminal_theme::resolve_terminal_theme(
            &app.config.terminal.theme,
        ))),
    };
    match Terminal::new_with_pty(tab_id, settings, russh_pty) {
        Ok(terminal) => {
            if let Some(tab) = app.tabs.tab_mut(tab_id) {
                tab.terminal = Some(terminal);
                tab.resize_tx = Some(resize_tx);
                // 记录桥接状态，供关标签 / 关窗口时请求停止，断开时读取退出原因。
                tab.bridge = Some(bridge);
                // 新桥接已挂载：清掉上一次的断开归因，避免旧原因残留
                // 让「已重新连上」的标签仍被当成断开态。
                tab.disconnect_reason = None;
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
pub(crate) fn handle_terminal_event(app: &mut App, event: TermEvent) -> Task<Message> {
    match event {
        // 用户在终端区域按下鼠标以取回键盘焦点：直接置聚焦态（光标转实心、恢复输入门控）。
        // 仅展示中的活动标签终端会收到此事件，故无需再校验标签 id。
        TermEvent::FocusRequest(_id) => {
            app.terminal_focused = true;
            Task::none()
        }
        TermEvent::BackendCall(id, backend_cmd) => {
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
            // 断开态闸门：桥接已死的终端会把键入静默丢进无人消费的后端通道，
            // 故在转发之前拦下——回车转成重连请求，其余按键让横幅闪烁提示。
            if let BackendCommand::Write(bytes) = &backend_cmd {
                let gated = app
                    .tabs
                    .list()
                    .iter()
                    .find(|t| t.id == id)
                    .and_then(|tab| disconnected_write_action(tab.status, id, bytes));
                if let Some(msg) = gated {
                    let ctx = contexts::tabs_ctx(app);
                    return app
                        .tabs
                        .update(msg, &ctx, &app.sftp)
                        .map(Message::TabsEvent);
                }
            }
            let action = if let Some(tab) = app.tabs.tab_mut(id) {
                // 本地终端尺寸变化时，转发到远端（window-change）。
                if let BackendCommand::Resize(Some(layout), Some(font)) = &backend_cmd {
                    let cols = (layout.width / font.width).floor().max(1.0) as u32;
                    let rows = (layout.height / font.height).floor().max(1.0) as u32;
                    if let Some(tx) = &tab.resize_tx {
                        let _ = tx.try_send((cols, rows));
                    }
                }
                // 将命令转交终端组件（写入 PTY / 调整布局等），并取回宿主动作。
                tab.terminal
                    .as_mut()
                    .map(|term| term.handle(TermCommand::ProxyToBackend(backend_cmd)))
                    .unwrap_or_default()
            } else {
                Action::default()
            };
            // 宿主动作分流：响铃走视觉提示；`Shutdown` 与 `ChangeTitle` 暂无消费方。
            //
            // `Action::Shutdown` 由 alacritty 在「PTY 已结束」时产出。它表示终端自身收尾，
            // **不代表**用户要关标签：断开归因由 `TerminalDisconnected` 负责、
            // 关标签只走显式关闭动作，故必须显式忽略。
            match action {
                Action::Bell if app.config.terminal.bell => {
                    // 响铃视觉提示。
                    let ctx = contexts::tabs_ctx(app);
                    return app
                        .tabs
                        .update(tabs::Message::Bell(id), &ctx, &app.sftp)
                        .map(Message::TabsEvent);
                }
                _ => {}
            }
            Task::none()
        }
    }
}

/// 断开态下键入的处置决定：返回 `Some` 时该次写入被拦下并转成标签消息。
///
/// 仅 `Error` / `Disconnected`（桥接已死）拦截；`Connected` / `Connecting` 的写入原样放行。
/// 其中整批字节均为 `\r` / `\n`（即 Enter，含小键盘）判为回车，转成重连请求；
/// 其余按键（含带换行的多行粘贴、空写入）只让横幅闪烁提示，不触发重连。
fn disconnected_write_action(
    status: ConnectionStatus,
    tab_id: u64,
    bytes: &[u8],
) -> Option<tabs::Message> {
    if !matches!(
        status,
        ConnectionStatus::Error | ConnectionStatus::Disconnected
    ) {
        return None;
    }
    let enter = !bytes.is_empty() && bytes.iter().all(|b| matches!(b, b'\r' | b'\n'));
    Some(if enter {
        tabs::Message::ReconnectRequested(tab_id)
    } else {
        tabs::Message::DisconnectedKeyPressed(tab_id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 连接态的写入原样放行（含回车，绝不误触发重连）。
    #[test]
    fn writes_pass_through_while_connected() {
        assert!(disconnected_write_action(ConnectionStatus::Connected, 1, b"\r").is_none());
        assert!(disconnected_write_action(ConnectionStatus::Connecting, 1, b"a").is_none());
    }

    /// 断开态下纯回车（`\r` / `\n` 的组合）转成重连请求。
    #[test]
    fn enter_only_writes_request_reconnect_while_disconnected() {
        for status in [ConnectionStatus::Error, ConnectionStatus::Disconnected] {
            assert!(matches!(
                disconnected_write_action(status, 7, b"\r"),
                Some(tabs::Message::ReconnectRequested(7))
            ));
            assert!(matches!(
                disconnected_write_action(status, 7, b"\r\n"),
                Some(tabs::Message::ReconnectRequested(7))
            ));
        }
    }

    /// 断开态下其余按键只触发横幅闪烁，不触发重连。
    #[test]
    fn other_writes_only_flash_the_banner_while_disconnected() {
        let samples: [&[u8]; 5] = [b"a", b"ls\r", b"\x1b[A", b"line1\nline2", b""];
        for bytes in samples {
            assert!(matches!(
                disconnected_write_action(ConnectionStatus::Error, 7, bytes),
                Some(tabs::Message::DisconnectedKeyPressed(7))
            ));
        }
    }
}
