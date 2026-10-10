//! 终端后端：桥接 russh shell 通道（或本地 PTY）与 alacritty event loop。
//!
//! 宿主经 [`Command`] 下发操作、经 [`Action`] 响应；`types` 定类型、`input` 处理输入、`content` 管快照。

mod content;
mod input;
mod types;

pub use self::content::RenderableContent;
pub use self::types::{Command, LinkAction, MouseButton};

use self::types::{TerminalSize, URL_REGEX, action_for_event};
use crate::widget::term::actions::Action;
use crate::widget::term::russh_pty::RusshPty;
use crate::widget::term::settings::BackendSettings;
use alacritty_terminal::event::{Event, EventListener, Notify, OnResize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::search::RegexSearch;
use alacritty_terminal::term::{self, Term};
use alacritty_terminal::tty;
use alacritty_terminal::tty::EventedPty;
use std::io::Result;
use std::sync::Arc;
use tokio::sync::mpsc;

/// 终端后端：持有 alacritty `Term`、event loop 通知器与最近一次渲染快照。
pub struct Backend {
    /// alacritty 终端状态（含网格与光标），由互斥锁保护供多线程访问。
    term: Arc<FairMutex<Term<EventProxy>>>,
    /// 当前终端几何尺寸与字体度量。
    size: TerminalSize,
    /// 向 alacritty event loop 推送消息的通知器。
    notifier: Notifier,
    /// 事件代理（宿主事件通道的发送端包装）：`reattach` 重建 event loop 时复用同一
    /// 发送端，使宿主侧订阅（绑在通道接收端上）无需重建。
    event_proxy: EventProxy,
    /// 上一次同步后的可渲染内容快照。
    last_content: RenderableContent,
    /// 最近一次已下发 PTY 的行列数；用于抑制重复的 window-change 请求。
    last_pty_size: Option<(u16, u16)>,
    /// 用于识别超链接的 URL 正则（crate 内可见）。
    pub(crate) url_regex: RegexSearch,
    /// 复制时是否去除每行尾部空格。
    trim_trailing_whitespace: bool,
}

impl Backend {
    /// 走本地 PTY 子进程路径（继承自上游 iced_term，拉起本机 shell，非 SSH 场景）。
    ///
    /// 上层入口 `Terminal::new` 无调用方，整条路径目前是死的，保留以支撑将来的本地 shell 标签页。
    pub fn new(
        id: u64,
        pty_event_proxy_sender: mpsc::UnboundedSender<Event>,
        settings: BackendSettings,
    ) -> Result<Self> {
        let pty_config = tty::Options {
            shell: Some(tty::Shell::new(settings.program, settings.args)),
            working_directory: settings.working_directory,
            env: settings.env,
            ..tty::Options::default()
        };

        let pty = tty::new(&pty_config, TerminalSize::default().into(), id)?;
        Self::from_pty(
            id,
            pty_event_proxy_sender,
            pty,
            settings.scrollback,
            settings.trim_trailing_whitespace,
        )
    }

    /// SSH 场景：直接桥接 russh shell 通道，不经过本地 PTY 子进程。
    pub fn new_with_pty(
        id: u64,
        pty_event_proxy_sender: mpsc::UnboundedSender<Event>,
        pty: RusshPty,
        scrollback: usize,
        trim_trailing_whitespace: bool,
    ) -> Result<Self> {
        Self::from_pty(
            id,
            pty_event_proxy_sender,
            pty,
            scrollback,
            trim_trailing_whitespace,
        )
    }

    /// 以给定 PTY 构造后端，初始化 alacritty 终端与 event loop。
    fn from_pty<Pty>(
        _id: u64,
        pty_event_proxy_sender: mpsc::UnboundedSender<Event>,
        pty: Pty,
        scrollback: usize,
        trim_trailing_whitespace: bool,
    ) -> Result<Self>
    where
        Pty: EventedPty + OnResize + Send + 'static,
    {
        let config = term::Config {
            scrolling_history: scrollback,
            ..Default::default()
        };
        let terminal_size = TerminalSize::default();

        let event_proxy = EventProxy(pty_event_proxy_sender);

        let mut term = Term::new(config, &terminal_size, event_proxy.clone());

        let mut initial_content = RenderableContent::default();
        Self::capture_viewport(&mut initial_content, &mut term, terminal_size);

        let term = Arc::new(FairMutex::new(term));

        let pty_event_loop = EventLoop::new(term.clone(), event_proxy.clone(), pty, false, false)?;

        let notifier = Notifier(pty_event_loop.channel());

        let _ = pty_event_loop.spawn();

        Ok(Self {
            term: term.clone(),
            size: terminal_size,
            notifier,
            event_proxy,
            last_content: initial_content,
            last_pty_size: None,
            url_regex: RegexSearch::new(URL_REGEX).expect("invalid url regexp"),
            trim_trailing_whitespace,
        })
    }

    /// 就地换接一条新 PTY（断线重连）：网格、历史与选区原样保留，仅重建 alacritty event loop。
    ///
    /// 旧循环以 `Msg::Shutdown` 确定性停掉，免其在异常路径下一直持有已死管道空转。
    pub fn reattach(&mut self, pty: RusshPty) -> Result<()> {
        // 先建新循环再停旧循环：`EventLoop::new` 失败（如 poller 创建失败）时
        // 保留旧 notifier 不动，避免后端停在半死状态（旧循环已停、新循环没有）。
        let pty_event_loop = EventLoop::new(
            self.term.clone(),
            self.event_proxy.clone(),
            pty,
            false,
            false,
        )?;
        let _ = self.notifier.0.send(Msg::Shutdown);
        self.notifier = Notifier(pty_event_loop.channel());
        let _ = pty_event_loop.spawn();
        // 重置尺寸记忆：新通道在建桥时按自己的 cols/rows 开局，若旧值恰与新尺寸相同，
        // 不重置则后续 Resize 会被去重吞掉，新 shell 将按错尺寸一直活着。
        self.last_pty_size = None;
        Ok(())
    }
    /// 处理一条宿主下发的 `Command`，返回需要宿主执行的 `Action`。
    ///
    /// 终端锁按需获取：仅 `Wakeup` 这类纯重绘信号的事件不取锁，避免与解析线程争抢。
    pub fn handle(&mut self, cmd: Command) -> Action {
        match cmd {
            Command::ProcessAlacrittyEvent(event) => match event {
                // PtyWrite 需要访问 notifier，单独分支；其余事件走无状态的纯映射。
                Event::PtyWrite(pty) => {
                    self.notifier.notify(pty.into_bytes());
                    Action::default()
                }
                event => action_for_event(event),
            },
            Command::Write(input) => {
                self.write(input);
                let term = self.term.clone();
                term.lock().scroll_display(Scroll::Bottom);
                Action::default()
            }
            Command::Scroll(delta) => {
                let term = self.term.clone();
                let mut term = term.lock();
                self.scroll(&mut term, delta);
                Action::default()
            }
            Command::Resize(layout_size, font_measure) => {
                let term = self.term.clone();
                let mut term = term.lock();
                self.resize(&mut term, layout_size, font_measure);
                Action::default()
            }
            Command::SelectStart(selection_type, (x, y)) => {
                let term = self.term.clone();
                let mut term = term.lock();
                self.start_selection(&mut term, selection_type, x, y);
                Action::default()
            }
            Command::SelectUpdate((x, y)) => {
                let term = self.term.clone();
                let mut term = term.lock();
                self.update_selection(&mut term, x, y);
                Action::default()
            }
            Command::ProcessLink(link_action, point) => {
                let term = self.term.clone();
                let term = term.lock();
                self.process_link_action(&term, link_action, point);
                Action::default()
            }
            Command::MouseReport(button, modifiers, point, pressed) => {
                self.process_mouse_report(button, modifiers, point, pressed);
                Action::default()
            }
        }
    }
    /// 当前网格的行列数（列, 行），供重连给新 PTY 定开局尺寸——重连不会再触发 resize，
    /// 尺寸只能在建桥时给定。
    pub fn grid_size(&self) -> (u16, u16) {
        (self.size.num_cols, self.size.num_lines)
    }
    /// 把视口拉回底部（贴住实时输出）。
    ///
    /// 直接滚网格而不用 [`Command::Scroll`]：后者在备用屏会改发方向键字节到 pty，对新 shell 是误输入。
    pub fn scroll_to_bottom(&mut self) {
        let term = self.term.clone();
        term.lock().scroll_display(Scroll::Bottom);
    }
    /// 返回最近一次同步的可渲染内容快照。
    pub fn renderable_content(&self) -> &RenderableContent {
        &self.last_content
    }
}

impl Drop for Backend {
    /// 析构时向 event loop 发送关闭消息。
    fn drop(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}
/// 实现 alacritty `EventListener`：将事件转发给宿主的消息通道。
#[derive(Clone)]
pub struct EventProxy(mpsc::UnboundedSender<Event>);

impl EventListener for EventProxy {
    /// 以非阻塞方式将 alacritty 事件发送到宿主通道。
    ///
    /// 必须是**无界非阻塞**发送：alacritty 在持有终端锁时回调本方法（PTY 读取线程解析
    /// 输出期间会发出 `Wakeup`/`Title`/`Bell`/`PtyWrite` 等），若此处阻塞在有界通道上，
    /// 一旦宿主订阅转发滞后，读取线程就会持锁卡死，UI 线程随后的 `term.lock()` 永久挂起。
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

#[cfg(test)]
mod tests;
