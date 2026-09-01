use crate::widget::term::AlacrittyEvent;
use crate::widget::term::RusshPty;
use crate::widget::term::actions::Action;
use crate::widget::term::backend;
use crate::widget::term::bindings::{Binding, BindingAction, BindingsLayout, InputKind};
use crate::widget::term::font::TermFont;
use crate::widget::term::settings::{FontSettings, Settings, ThemeSettings};
use crate::widget::term::theme::{ColorPalette, Theme};
use iced::Subscription;
use iced::futures::stream::BoxStream;
use iced::futures::{SinkExt, StreamExt};
use iced::widget::canvas::Cache;
use std::hash::{Hash, Hasher};
use std::io::Result;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, Receiver};

#[derive(Debug, Clone)]
/// 终端部件向宿主抛出的事件。
pub enum Event {
    /// 携带标签 id 与一条后端命令，由后端事件订阅流回送。
    BackendCall(u64, backend::Command),
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
    /// 后端事件接收端（被订阅流共享）。
    backend_event_rx: Arc<Mutex<Receiver<AlacrittyEvent>>>,
}

impl Terminal {
    /// 以本地 PTY 路径创建终端实例（非 SSH 场景）。
    pub fn new(id: u64, settings: Settings) -> Result<Self> {
        let (backend_event_tx, backend_event_rx) = mpsc::channel(100);
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
        let (backend_event_tx, backend_event_rx) = mpsc::channel(100);
        let theme = Theme::new(settings.theme);
        let font = TermFont::new(settings.font);

        Ok(Self {
            id,
            widget_id: iced::widget::Id::unique(),
            font,
            theme,
            bindings: BindingsLayout::default(),
            cache: Cache::default(),
            backend: backend::Backend::new_with_pty(id, backend_event_tx, pty)?,
            backend_event_rx: Arc::new(Mutex::new(backend_event_rx)),
        })
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
            }
            Command::AddBindings(bindings) => {
                self.bindings.add_bindings(bindings);
            }
            Command::ProxyToBackend(cmd) => {
                action = self.backend.handle(cmd);
            }
        };

        self.sync_and_redraw();
        action
    }

    /// 同步字体与后端状态并清空缓存触发重绘。
    fn sync_and_redraw(&mut self) {
        self.sync_font();
        self.backend.sync();
        self.redraw();
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
    event_receiver: Arc<Mutex<Receiver<AlacrittyEvent>>>,
}

impl Hash for TerminalSubscriptionData {
    /// 仅以标签 id 计算哈希，保证同一终端的订阅去重。
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// 订阅流：循环接收后端事件并封装为 [`Event`] 回流到 App。
fn terminal_subscription_stream(data: &TerminalSubscriptionData) -> BoxStream<'static, Event> {
    let id = data.id;
    let event_receiver = data.event_receiver.clone();
    iced::stream::channel(1000, async move |mut output| {
        let mut shutdown = false;
        loop {
            let mut event_receiver = event_receiver.lock().await;
            match event_receiver.recv().await {
                Some(event) => {
                    if let AlacrittyEvent::Exit = event {
                        shutdown = true
                    };

                    output
                        .send(Event::BackendCall(
                            id,
                            backend::Command::ProcessAlacrittyEvent(event),
                        ))
                        .await
                        .unwrap_or_else(|_| {
                            panic!(
                                "terminal stream {}: sending BackendCall event is failed",
                                id
                            )
                        });
                }
                None => {
                    if !shutdown {
                        panic!(
                            "terminal stream {}: terminal event channel closed unexpected",
                            id
                        );
                    }
                }
            }
        }
    })
    .boxed()
}
