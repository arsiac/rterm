//! 终端 widget 与 PTY 桥接的生命周期及事件转发。

use crate::app::App;
use crate::app::contexts;
use crate::app::tabs;
use crate::app::tasks::{BridgeOptions, open_terminal_task};
use crate::font;
use crate::i18n::localize_error;
use crate::message::{Message, ResizeSender};
use crate::state::TerminalTab;
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
///
/// 尚无终端（首次连接）用默认行列数、不注入字节；已有终端（原地重连）按当前网格尺寸
/// 开局并注入复位 + 清屏 + 提示行——重连不会再触发 resize，尺寸只能在建桥时给定。
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
    let opts = match app
        .tabs
        .list()
        .iter()
        .find(|t| t.id == tab_id)
        .and_then(|t| t.terminal.as_ref().map(|term| (term, t)))
    {
        Some((term, tab)) => {
            // 已有终端 ⇒ 原地重连：按当前网格尺寸开局（不会再触发 resize），注入提示行。
            let (cols, rows) = term.grid_size();
            let old_cwd = tab.cwd.lock().ok().and_then(|g| g.clone());
            BridgeOptions {
                cols,
                rows,
                inject_out: Some(reconnect_inject_bytes(old_cwd.as_deref())),
            }
        }
        // 尚无终端 ⇒ 首次连接：沿用默认尺寸（终端就绪后由布局 resize 校正），不注入。
        None => BridgeOptions {
            cols: super::DEFAULT_COLS,
            rows: super::DEFAULT_ROWS,
            inject_out: None,
        },
    };
    let bridge = Task::perform(
        open_terminal_task(
            conn,
            opts,
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

/// 重连时注入的整段字节：复位序列 + 清屏 + 一行「会话已重新开始」提示（带旧 cwd）。
///
/// 复位序列为手写的 xterm soft reset（alacritty 未实现 DECSTR）；不退备用屏则清屏与
/// 提示行落进备用屏、随全屏程序退出一起消失，旧模式位（鼠标上报、括号粘贴）也会污染
/// 新 shell 的首行键入。
/// 清屏取 xterm 系 `clear` 语义（`\x1b[H\x1b[2J\x1b[3J`：归位 + 清视口 + 清 scrollback），
/// 避免旧残帧（半截 MOTD、旧提示符）与新输出按格混排。
/// 提示行带旧 cwd，只读、不执行任何命令。
fn reconnect_inject_bytes(old_cwd: Option<&str>) -> Vec<u8> {
    /// 渲染层复位：退备用屏 → 清 SGR → 滚动区 → 光标 / 自动换行 / 方向键 →
    /// 鼠标上报与括号粘贴 / 焦点上报全关。
    const RESET: &[u8] = b"\x1b[?1049l\x1b[0m\x1b[r\x1b[?25h\x1b[?7h\x1b[?1l\
        \x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\x1b[?2004l\x1b[?1004l";
    /// 清屏：光标归位 → 清视口（ED 2）→ 清 scrollback（ED 3）。
    const CLEAR: &[u8] = b"\x1b[H\x1b[2J\x1b[3J";
    let body = match old_cwd {
        Some(dir) => t!("terminal.reconnected_notice_cwd", cwd => dir.to_string()),
        None => t!("terminal.reconnected_notice"),
    };
    let mut bytes = Vec::with_capacity(RESET.len() + CLEAR.len() + body.len() + 16);
    bytes.extend_from_slice(RESET);
    bytes.extend_from_slice(CLEAR);
    bytes.extend_from_slice(b"[rterm] ");
    bytes.extend_from_slice(body.as_bytes());
    bytes.extend_from_slice(b"\r\n");
    bytes
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
    // 已有终端 ⇒ 原地重连：换接新 pty，不重建 widget（网格、选中态、字体 / 主题与
    // 事件订阅均复用）。
    let reattaching = app
        .tabs
        .list()
        .iter()
        .find(|t| t.id == tab_id)
        .is_some_and(|t| t.terminal.is_some());
    if reattaching {
        return reattach_terminal_widget(app, tab_id, russh_pty, bridge, resize_tx);
    }
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
                apply_bridge_mounted_state(tab, &bridge);
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

/// 桥接挂载到标签后的连接态收尾（新建与原地重连共用）。
///
/// 按桥接实况置状态：仍在运行 → 已连接并清旧归因；已结束 → 失败态 + 归因。必须查实况
/// 而非无条件置已连接——远端 shell 秒退时 `TerminalDisconnected` 可能先于
/// `TerminalOpened` 到达，照常置位会留下看似正常、实际已死的终端。
fn apply_bridge_mounted_state(tab: &mut TerminalTab, bridge: &Arc<rterm_core::BridgeState>) {
    // 记录桥接状态：关标签 / 关窗口时请求停止、下次断开时读取归因都指向它。
    tab.bridge = Some(bridge.clone());
    tab.error = None;
    if bridge.is_finished() {
        tab.status = ConnectionStatus::Error;
        tab.disconnect_reason = Some(bridge.reason());
    } else {
        tab.status = ConnectionStatus::Connected;
        tab.disconnect_reason = None;
    }
}

/// 原地重连落地：把新桥接的 pty 换接到既有终端上（[`Terminal::reattach`]），
/// 恢复该标签的连接态字段，并作废绑在旧连接上的 SFTP 通道。
///
/// 换接失败（极罕见）时标签保持断开态、原因写入 `tab.error` 由横幅展示，可再按 Enter
/// 重试（旧桥接状态仍挂在标签上）。
fn reattach_terminal_widget(
    app: &mut App,
    tab_id: u64,
    russh_pty: RusshPty,
    bridge: Arc<rterm_core::BridgeState>,
    resize_tx: ResizeSender,
) -> Task<Message> {
    let Some(tab) = app.tabs.tab_mut(tab_id) else {
        return Task::none();
    };
    let Some(terminal) = tab.terminal.as_mut() else {
        // 调用前已判定终端存在；走到这里说明状态在异步间隙被外部改动，按无操作处理。
        return Task::none();
    };
    if let Err(e) = terminal.reattach(russh_pty) {
        tab.status = ConnectionStatus::Error;
        tab.error = Some(t!("app.terminal_reattach_failed", err => e));
        return Task::none();
    }
    tab.resize_tx = Some(resize_tx);
    apply_bridge_mounted_state(tab, &bridge);
    tab.banner_flash = None;
    // 旧 SFTP 客户端绑在已死的连接上（任何调用秒失败），直接作废：视图回到未打开态，
    // 由用户重新打开（不自动重建通道 / 续跑队列）。
    app.sftp.invalidate(tab_id);
    app.status = Some(t!("app.terminal_ready"));
    // 焦点与新建挂载一致：重连成功即视为终端持有键盘焦点。
    app.terminal_focused = true;
    // 延时强制重绘一次（同新建挂载的说明）：TerminalReady 经 refresh_terminal 复核尺寸——
    // `Backend::reattach` 已重置尺寸记忆，同尺寸也会重新下发 window-change。
    Task::batch([Task::perform(sleep(Duration::from_millis(80)), move |_| {
        Message::Tabs(tabs::Message::TerminalReady(tab_id))
    })])
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
/// 拦截 `Error` / `Disconnected`（桥接已死）与 `Connecting`（重连在途）——后者依据：
/// 此刻还能收到键入，说明终端仍挂在已死的后端上（新建标签该状态下没有终端组件）。
/// 整批字节均为 `\r` / `\n`（Enter，含小键盘）转成重连请求（在途时被防重复挡下）；
/// 其余按键（含带换行的多行粘贴、空写入）只闪横幅，不触发重连。
fn disconnected_write_action(
    status: ConnectionStatus,
    tab_id: u64,
    bytes: &[u8],
) -> Option<tabs::Message> {
    if !matches!(
        status,
        ConnectionStatus::Error | ConnectionStatus::Disconnected | ConnectionStatus::Connecting
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

    /// 连接正常（`Connected`）的写入一律原样放行（含回车，绝不误触发重连）。
    #[test]
    fn writes_pass_through_while_connected() {
        assert!(disconnected_write_action(ConnectionStatus::Connected, 1, b"\r").is_none());
        assert!(disconnected_write_action(ConnectionStatus::Connected, 1, b"ls").is_none());
    }

    /// 拦截的三种状态下纯回车（`\r` / `\n` 的组合）都转成重连请求。
    #[test]
    fn enter_writes_request_reconnect_while_gated() {
        for status in [
            ConnectionStatus::Error,
            ConnectionStatus::Disconnected,
            ConnectionStatus::Connecting,
        ] {
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

    /// 拦截期间其余按键只触发横幅闪烁，不触发重连。
    #[test]
    fn other_writes_only_flash_the_banner_while_gated() {
        let samples: [&[u8]; 5] = [b"a", b"ls\r", b"\x1b[A", b"line1\nline2", b""];
        for status in [
            ConnectionStatus::Error,
            ConnectionStatus::Disconnected,
            ConnectionStatus::Connecting,
        ] {
            for bytes in samples {
                assert!(matches!(
                    disconnected_write_action(status, 7, bytes),
                    Some(tabs::Message::DisconnectedKeyPressed(7))
                ));
            }
        }
    }

    /// 重连注入字节的组装：复位前缀在最前、清屏紧随其后、旧 cwd 只在提供时出现、
    /// 以换行收尾（正文文案随 locale，故只钉结构、不断言文案本身）。
    #[test]
    fn reconnect_inject_bytes_assembles_the_reset_and_clear_prefix_and_optional_cwd() {
        let bytes = reconnect_inject_bytes(Some("/var/log"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            bytes.starts_with(b"\x1b[?1049l"),
            "退备用屏在最前，清屏与提示行必须落在主屏上"
        );
        assert!(
            text.contains("\x1b[H\x1b[2J\x1b[3J"),
            "清屏（归位 + 清视口 + 清 scrollback）应紧随复位序列"
        );
        let clear_at = text.find("\x1b[2J").expect("清屏序列应存在");
        let notice_at = text.find("[rterm] ").expect("提示行应存在");
        assert!(clear_at < notice_at, "提示行应在清屏之后写入");
        assert!(text.contains("/var/log"), "旧 cwd 应出现在提示行里");
        assert!(bytes.ends_with(b"\r\n"));

        let plain = reconnect_inject_bytes(None);
        assert!(String::from_utf8_lossy(&plain).contains("\x1b[3J"));
        assert!(String::from_utf8_lossy(&plain).contains("[rterm] "));
        assert!(!String::from_utf8_lossy(&plain).contains("/var/log"));
    }
}
