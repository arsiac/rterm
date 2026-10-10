use crate::widget::term::AlacrittyEvent;
use crate::widget::term::RusshPty;
use crate::widget::term::actions::Action;
use crate::widget::term::backend;
use crate::widget::term::bindings::{Binding, BindingAction, BindingsLayout, InputKind};
use crate::widget::term::font::TermFont;
use crate::widget::term::settings::{FontSettings, Settings, ThemeSettings};
use crate::widget::term::theme::{ColorPalette, Theme};
use crate::widget::term::view::paste_bytes;
use alacritty_terminal::term::TermMode;
use iced::Subscription;
use iced::futures::stream::BoxStream;
use iced::futures::{SinkExt, StreamExt};
use iced::widget::canvas::Cache;
use std::hash::{Hash, Hasher};
use std::io::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::{self, UnboundedReceiver};

#[derive(Debug, Clone)]
/// 终端部件向宿主抛出的事件。
pub enum Event {
    /// 携带标签 id 与一条后端命令，由后端事件订阅流回送。
    BackendCall(u64, backend::Command),
    /// 终端未持键盘焦点时用户在终端区域按下鼠标：请求把键盘焦点交还终端。
    ///
    /// 因 `handle_mouse_event` 在 `!focused` 时会早返回、不产出任何 `BackendCall`，
    /// 故需独立的聚焦请求事件，否则失去焦点后只能靠切标签页才能找回焦点。
    FocusRequest(u64),
}

#[derive(Debug, Clone)]
/// 宿主发给终端部件的命令。
pub enum Command {
    /// 切换配色板（主题）。
    ChangeTheme(Box<ColorPalette>),
    /// 切换字体设置。
    ChangeFont(FontSettings),
    /// 追加一批按键 / 鼠标绑定。
    AddBindings(Vec<(Binding<InputKind>, BindingAction)>),
    /// 透传给后端直接执行。
    ProxyToBackend(backend::Command),
}

/// 终端部件实例：封装后端、字体、主题、绑定与渲染缓存。
pub struct Terminal {
    /// 标签唯一 id。
    pub id: u64,
    /// iced 部件 id（标识用）。
    widget_id: iced::widget::Id,
    /// 终端渲染所用字体（含字号、DPI 测量等）。
    pub(crate) font: TermFont,
    /// 当前终端配色主题。
    pub(crate) theme: Theme,
    /// 几何缓存，复用以避免每帧重建文本/背景图元。
    pub(crate) cache: Cache,
    /// 按键绑定布局（快捷键 → 动作映射）。
    pub(crate) bindings: BindingsLayout,
    /// 终端后端：负责 PTY/SSH 数据收发与内容解析。
    pub(crate) backend: backend::Backend,
    /// 后端事件接收端（被订阅流共享）。无界通道：见 `EventProxy::send_event` 的说明。
    backend_event_rx: Arc<Mutex<UnboundedReceiver<AlacrittyEvent>>>,
}

impl Terminal {
    /// 以本地 PTY 路径创建终端实例（非 SSH 场景）。
    pub fn new(id: u64, settings: Settings) -> Result<Self> {
        let (backend_event_tx, backend_event_rx) = mpsc::unbounded_channel();
        let theme = Theme::new(settings.theme);
        let font = TermFont::new(settings.font);

        Ok(Self {
            id,
            widget_id: iced::widget::Id::unique(),
            font,
            theme,
            bindings: BindingsLayout::default(),
            cache: Cache::default(),
            backend: backend::Backend::new(id, backend_event_tx, settings.backend)?,
            backend_event_rx: Arc::new(Mutex::new(backend_event_rx)),
        })
    }

    /// SSH 场景：复用已建立的 russh shell 通道，跳过本地 PTY 子进程。
    pub fn new_with_pty(id: u64, settings: Settings, pty: RusshPty) -> Result<Self> {
        let (backend_event_tx, backend_event_rx) = mpsc::unbounded_channel();
        let theme = Theme::new(settings.theme);
        let font = TermFont::new(settings.font);

        Ok(Self {
            id,
            widget_id: iced::widget::Id::unique(),
            font,
            theme,
            bindings: BindingsLayout::default(),
            cache: Cache::default(),
            backend: backend::Backend::new_with_pty(
                id,
                backend_event_tx,
                pty,
                settings.backend.scrollback,
                settings.backend.trim_trailing_whitespace,
            )?,
            backend_event_rx: Arc::new(Mutex::new(backend_event_rx)),
        })
    }

    /// 原地换接新桥接的 PTY（断线重连）：复用同一网格、滚动历史与后端事件订阅，仅把
    /// 后端 event loop 换到新通道上，随后强制重绘并把视口拉回底部（旧内容由重连注入的
    /// 清屏字节稍后作废；先回底使清屏前的窗口也贴着实时输出）。
    pub fn reattach(&mut self, pty: RusshPty) -> Result<()> {
        self.backend.reattach(pty)?;
        self.backend.scroll_to_bottom();
        self.backend.sync();
        self.redraw();
        Ok(())
    }

    /// 当前网格的行列数（列, 行；像素布局折算后的实际值），供重连按现尺寸重开 PTY。
    pub fn grid_size(&self) -> (u32, u32) {
        let (cols, rows) = self.backend.grid_size();
        (u32::from(cols), u32::from(rows))
    }

    /// 远端程序是否已开启鼠标上报：开启时鼠标动作归远端应用，宿主菜单让位。
    pub(crate) fn mouse_reporting(&self) -> bool {
        self.backend
            .renderable_content()
            .terminal_mode
            .intersects(TermMode::MOUSE_MODE)
    }

    /// 当前是否存在可复制的选区。
    ///
    /// 空选区（含未拖动的单击）已在 `Selection::to_range` 内滤成 `None`，故只需判 `Some`；
    /// 不能改判起止点是否相同——拖过单格时二者相同却仍是有效的一字符选区。
    pub(crate) fn has_selection(&self) -> bool {
        self.backend.renderable_content().selectable_range.is_some()
    }

    /// 当前选区文本（无选区为空串）。
    pub(crate) fn selection_text(&self) -> String {
        self.backend.selectable_content()
    }

    /// 以当前括号粘贴模式把剪贴板文本写入 PTY。
    pub(crate) fn paste(&mut self, data: &str) -> Action {
        let bracketed = self
            .backend
            .renderable_content()
            .terminal_mode
            .contains(TermMode::BRACKETED_PASTE);
        self.handle(Command::ProxyToBackend(backend::Command::Write(
            paste_bytes(data, bracketed),
        )))
    }

    /// 返回 iced 部件 id。
    ///
    /// 当前仅作标识预留：焦点判定走 `App::terminal_focused`、标签栏滚动走
    /// `App::tab_bar_scroll`，尚无调用方。
    pub fn widget_id(&self) -> &iced::widget::Id {
        &self.widget_id
    }

    /// 返回订阅：持续接收后端事件并转成本部件 [`Event`] 回流到 App。
    pub fn subscription(&self) -> Subscription<Event> {
        let data = TerminalSubscriptionData {
            id: self.id,
            event_receiver: self.backend_event_rx.clone(),
        };

        Subscription::run_with(data, terminal_subscription_stream)
    }

    /// 处理一条命令，并同步重绘；返回需要 App 响应的回流动作。
    pub fn handle(&mut self, cmd: Command) -> Action {
        let mut action = Action::default();

        match cmd {
            Command::ChangeTheme(color_pallete) => {
                self.theme = Theme::new(ThemeSettings::new(color_pallete));
            }
            Command::ChangeFont(font_settings) => {
                self.font = TermFont::new(font_settings);
                // 只有字体变更才需重新测量：单字测量要构造段落走一遍文本管线，
                // 若挂在每条命令上，PTY 输出路径每条事件都要白付一次。
                self.sync_font();
            }
            Command::AddBindings(bindings) => {
                self.bindings.add_bindings(bindings);
            }
            Command::ProxyToBackend(cmd) => {
                action = self.backend.handle(cmd);
            }
        };

        self.backend.sync();
        self.redraw();
        action
    }

    /// 同步字体度量并通知后端按新字形尺寸重排。
    fn sync_font(&mut self) {
        self.font.sync();
        self.backend
            .handle(backend::Command::Resize(None, Some(self.font.measure)));
    }

    /// 清空渲染缓存以触发下次重绘。
    fn redraw(&mut self) {
        self.cache.clear();
    }
}

/// 终端订阅数据：携带标签 id 与后端事件接收端，供 `terminal_subscription_stream` 使用。
#[derive(Clone)]
struct TerminalSubscriptionData {
    /// 标签唯一 id。
    id: u64,
    /// 后端事件接收端（被多订阅共享）。
    event_receiver: Arc<Mutex<UnboundedReceiver<AlacrittyEvent>>>,
}

impl Hash for TerminalSubscriptionData {
    /// 仅以标签 id 计算哈希，保证同一终端的订阅去重。
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// 纯输出突发的合并窗口：窗口内到达的 `Wakeup` 合并为一次同步，等价于按帧节流。
const EVENT_BATCH_WINDOW: Duration = Duration::from_millis(8);

/// 一批待转发的后端事件：`Wakeup` 只保留一次，其余事件按原顺序逐条保留。
#[derive(Default)]
struct EventBatch {
    /// 需要逐条透传的事件（标题、PTY 写入、退出等带副作用的事件）。
    others: Vec<AlacrittyEvent>,
    /// 是否收到过 `Wakeup`（重绘信号，合并为一次即可）。
    wakeup: bool,
}

impl EventBatch {
    /// 收下一条事件：`Wakeup` 只置位，其余入队。
    fn push(&mut self, event: AlacrittyEvent) {
        if matches!(event, AlacrittyEvent::Wakeup) {
            self.wakeup = true;
        } else {
            self.others.push(event);
        }
    }

    /// 本批是否为纯输出突发（只含 `Wakeup`）。
    fn is_wakeup_only(&self) -> bool {
        self.wakeup && self.others.is_empty()
    }
}

/// 阻塞等待一条后端事件；通道关闭（终端部件已销毁）时返回 `None`。
async fn recv_event(
    receiver: &Arc<Mutex<UnboundedReceiver<AlacrittyEvent>>>,
) -> Option<AlacrittyEvent> {
    receiver.lock().await.recv().await
}

/// 取走通道中当前已就绪的全部事件（不等待），并入 `batch`。
async fn drain_events(
    receiver: &Arc<Mutex<UnboundedReceiver<AlacrittyEvent>>>,
    batch: &mut EventBatch,
) {
    let mut receiver = receiver.lock().await;
    loop {
        match receiver.try_recv() {
            Ok(event) => batch.push(event),
            Err(TryRecvError::Empty) => return,
            // 发送端已丢弃：后续不会再有事件，收完当前批次即退出。
            Err(TryRecvError::Disconnected) => return,
        }
    }
}

/// 订阅流：循环接收后端事件并封装为 [`Event`] 回流到 App。
///
/// PTY 输出产生的 `Wakeup` 是电平信号（「有变化，去重绘」），高频输出时可达每秒上百条；
/// 若逐条回流，每条都会触发一次网格同步。故同一突发内的 `Wakeup` 合并为一条消息，
/// 并按 [`EVENT_BATCH_WINDOW`] 节流（等价于每帧最多同步一次）。
/// 其余事件带副作用（标题、PTY 写入、退出等），一律原样逐条透传，不参与合并。
///
/// 流的生命周期由 iced 管理：标签页关闭 / 连接断开时，宿主会丢弃终端部件并取消订阅，
/// 此时后端事件通道（`Sender`）被丢弃，`recv()` 返回 `None`；或 iced 在拆栈时先丢弃
/// `output` 使后续 `send` 失败。这两种情况都属于**正常的终端拆栈**，需静默退出流。
fn terminal_subscription_stream(data: &TerminalSubscriptionData) -> BoxStream<'static, Event> {
    let id = data.id;
    let event_receiver = data.event_receiver.clone();
    iced::stream::channel(1000, async move |mut output| {
        loop {
            let Some(first) = recv_event(&event_receiver).await else {
                break;
            };

            let mut batch = EventBatch::default();
            batch.push(first);
            // 先取走已积压的事件，把同一波突发尽量压成一条消息。
            drain_events(&event_receiver, &mut batch).await;

            // 纯输出突发：等一个合并窗口再收一轮，使窗口内的输出并入同一次同步。
            if batch.is_wakeup_only() {
                tokio::time::sleep(EVENT_BATCH_WINDOW).await;
                drain_events(&event_receiver, &mut batch).await;
            }

            // 带副作用的事件先按原顺序透传，最后再补一条合并后的 `Wakeup` 触发重绘。
            if batch.wakeup {
                batch.others.push(AlacrittyEvent::Wakeup);
            }
            for event in batch.others {
                let message =
                    Event::BackendCall(id, backend::Command::ProcessAlacrittyEvent(event));
                // 订阅被 iced 拆栈（标签页已关闭）时 `output` 已失效，`send` 失败属正常，直接退出。
                if output.send(message).await.is_err() {
                    return;
                }
            }
        }
    })
    .boxed()
}
