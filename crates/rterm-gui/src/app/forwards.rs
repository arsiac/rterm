//! 端口转发管理模块。
//!
//! 持有转发规则的编辑弹窗草稿、作用域会话与各标签的运行时行状态；规则增改 / 启停经
//! `Event::SessionForwardsChanged` 上行，由父层持久化到会话配置并对所有已连接的同会话
//! 标签做引擎增量同步（apply_rule / remove_rule），模块自身绝不写 App。
//!
//! 运行时事件流：连接就绪后父层调 `start_for_tab` 在该标签上拉起
//! `ForwardEngine`，事件经 `Message::Runtime` 流回并写入 `State::runtime`，
//! 供转发面板按标签渲染各行状态。

use crate::app::App;
use crate::message::Message as TopMessage;
use crate::state::ToastKind;
use crate::t;
use futures::SinkExt;
use iced::Task;
use rterm_config::{ForwardKind, ForwardRule};
use rterm_core::{ForwardEngine, ForwardEvent, ForwardFailure, ForwardHandle, RuleEvent};
use std::collections::HashMap;
use std::sync::Arc;

/// 单条转发规则在某标签上的运行时状态（引擎事件驱动，见 `State::apply_runtime`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowStatus {
    /// 引擎已携规则启动，尚未收到首条事件。
    Starting,
    /// 监听中（携带当前活跃连接数）。
    Listening {
        /// 当前经此规则转发的活跃连接数。
        conns: usize,
    },
    /// 规则建立失败（不影响其他规则与连接本身）。
    Failed,
    /// 已停止（被移除或监听循环退出）。
    Stopped,
}

/// 规则编辑草稿：进入编辑即克隆，点保存才校验并落盘，取消 / Esc 丢弃。
///
/// 端口与目的字段以字符串暂存（输入框直连），保存时才解析校验。
pub struct RuleDraft {
    /// 编辑目标的规则下标；`None` 表示「新建」（保存时追加到末尾）。
    pub index: Option<usize>,
    /// 转发类型。
    pub kind: ForwardKind,
    /// 绑定地址（留空按 127.0.0.1 处理）。
    pub bind_address: String,
    /// 绑定端口（待解析的字符串）。
    pub bind_port: String,
    /// 目的主机（Dynamic 不使用）。
    pub target_host: String,
    /// 目的端口（待解析的字符串，Dynamic 不使用）。
    pub target_port: String,
    /// 校验失败原因（点过保存才写入；有值时弹窗保留并显示）。
    pub error: Option<String>,
}

impl RuleDraft {
    /// 构造空白草稿（用于新建规则；类型默认本地转发）。
    pub fn new() -> Self {
        Self {
            index: None,
            kind: ForwardKind::Local,
            bind_address: String::new(),
            bind_port: String::new(),
            target_host: String::new(),
            target_port: String::new(),
            error: None,
        }
    }

    /// 由已有规则构造草稿（用于编辑），保留其清单下标与启停状态。
    pub fn from_rule(index: usize, rule: &ForwardRule) -> Self {
        Self {
            index: Some(index),
            kind: rule.kind,
            bind_address: rule.bind_address.clone(),
            bind_port: rule.bind_port.to_string(),
            target_host: rule.target_host.clone().unwrap_or_default(),
            target_port: rule.target_port.map(|p| p.to_string()).unwrap_or_default(),
            error: None,
        }
    }

    /// 是否为「新建」草稿（尚无清单下标）。
    pub fn is_new(&self) -> bool {
        self.index.is_none()
    }
}

/// 模块状态：作用域会话、编辑弹窗草稿与各标签的运行时行状态。
#[derive(Default)]
pub struct State {
    /// 无活动标签时的作用域会话 id（会话行右键「端口转发…」入口设置）。
    ///
    /// 活动标签存在时作用域始终跟随活动标签，本值仅在无标签时生效。
    pub scope: Option<String>,
    /// 规则编辑弹窗草稿（无则未编辑）。
    pub editor: Option<RuleDraft>,
    /// 各标签的规则行运行时状态（按下标对齐会话 forwards 列表；仅已连接标签有条目）。
    pub runtime: HashMap<u64, Vec<RowStatus>>,
}

impl State {
    /// 引擎已在某标签上带 count 条规则启动：把该标签运行时初始化为全「启动中」。
    pub fn note_handle(&mut self, tab_id: u64, rules: &[ForwardRule]) {
        self.runtime.insert(
            tab_id,
            rules
                .iter()
                .map(|rule| {
                    if rule.enabled {
                        RowStatus::Starting
                    } else {
                        // 禁用规则不会被引擎启动、收不到任何事件，直接呈已停止。
                        RowStatus::Stopped
                    }
                })
                .collect(),
        );
    }

    /// 标签已关闭 / 即将重建：丢弃其运行时行状态。
    pub fn remove_for_tab(&mut self, tab_id: u64) {
        self.runtime.remove(&tab_id);
    }

    /// 按引擎事件更新某标签的行状态（下标越界为防御性 no-op）。
    pub fn apply_runtime(&mut self, tab_id: u64, event: &ForwardEvent) {
        let Some(rows) = self.runtime.get_mut(&tab_id) else {
            return;
        };
        let Some(row) = rows.get_mut(event.rule_index) else {
            return;
        };
        match &event.event {
            RuleEvent::Listening => *row = RowStatus::Listening { conns: 0 },
            RuleEvent::Connections(n) => match row {
                // 首个连接计数事件同时宣告就绪（引擎只在监听成功后才会上报计数）。
                RowStatus::Starting => *row = RowStatus::Listening { conns: *n },
                RowStatus::Listening { conns } => *conns = *n,
                _ => {}
            },
            RuleEvent::Failed(_) => *row = RowStatus::Failed,
            RuleEvent::Closed => *row = RowStatus::Stopped,
        }
    }

    /// 模块更新：只改自身状态；需要父层配合的事以 `Event` 经 `Task` 上行。
    pub fn update(&mut self, msg: Message, ctx: &Ctx) -> Task<Event> {
        match msg {
            Message::ToggleRule(idx) => self.toggle_rule(idx, ctx),
            Message::RemoveRule(idx) => self.remove_rule(idx, ctx),
            Message::StartEdit(index) => {
                self.editor = Some(match index {
                    Some(i) => ctx
                        .rules
                        .get(i)
                        .map(|rule| RuleDraft::from_rule(i, rule))
                        .unwrap_or_else(RuleDraft::new),
                    None => RuleDraft::new(),
                });
                Task::none()
            }
            Message::EditorKind(kind) => {
                if let Some(draft) = self.editor.as_mut()
                    && let Some(kind) = parse_kind(&kind)
                {
                    draft.kind = kind;
                }
                Task::none()
            }
            Message::EditorBindAddress(v) => {
                if let Some(draft) = self.editor.as_mut() {
                    draft.bind_address = v;
                }
                Task::none()
            }
            Message::EditorBindPort(v) => {
                if let Some(draft) = self.editor.as_mut() {
                    draft.bind_port = v;
                }
                Task::none()
            }
            Message::EditorTargetHost(v) => {
                if let Some(draft) = self.editor.as_mut() {
                    draft.target_host = v;
                }
                Task::none()
            }
            Message::EditorTargetPort(v) => {
                if let Some(draft) = self.editor.as_mut() {
                    draft.target_port = v;
                }
                Task::none()
            }
            Message::SaveRule => self.save_rule(ctx),
            Message::CancelEdit => {
                self.editor = None;
                Task::none()
            }
            Message::ScopeTo(id) => {
                self.scope = Some(id.clone());
                Task::done(Event::OpenPanel(id))
            }
            Message::Runtime { tab_id, event } => {
                self.apply_runtime(tab_id, &event);
                match &event.event {
                    RuleEvent::Failed(failure) => {
                        Task::done(Event::Toast(ToastKind::Warning, failure_text(failure)))
                    }
                    _ => Task::none(),
                }
            }
            // 引擎句柄抵达由父层直接落地（写标签状态），模块无状态可改。
            Message::HandleArrived { .. } => Task::none(),
        }
    }

    /// 切换某条规则的启停：改快照副本后上行，由父层持久化并同步引擎。
    fn toggle_rule(&mut self, idx: usize, ctx: &Ctx) -> Task<Event> {
        if !ctx.writable {
            return readonly();
        }
        let Some(session_id) = ctx.session_id.clone() else {
            return Task::none();
        };
        let mut forwards = ctx.rules.clone();
        let Some(rule) = forwards.get_mut(idx) else {
            return Task::none();
        };
        rule.enabled = !rule.enabled;
        Task::done(Event::SessionForwardsChanged {
            session_id,
            forwards,
        })
    }

    /// 删除某条规则：改快照副本后上行，由父层持久化并同步引擎。
    fn remove_rule(&mut self, idx: usize, ctx: &Ctx) -> Task<Event> {
        if !ctx.writable {
            return readonly();
        }
        let Some(session_id) = ctx.session_id.clone() else {
            return Task::none();
        };
        let mut forwards = ctx.rules.clone();
        if idx >= forwards.len() {
            return Task::none();
        }
        forwards.remove(idx);
        Task::done(Event::SessionForwardsChanged {
            session_id,
            forwards,
        })
    }

    /// 保存编辑器草稿：校验通过才 splice 进规则清单并上行；失败写 error 保留弹窗。
    fn save_rule(&mut self, ctx: &Ctx) -> Task<Event> {
        if !ctx.writable {
            return readonly();
        }
        let Some(session_id) = ctx.session_id.clone() else {
            return Task::none();
        };
        let Some(draft) = self.editor.as_ref() else {
            return Task::none();
        };
        // 逐字段校验；失败原因落在页脚，弹窗保留（输入不丢）。
        let bind_port = match draft.bind_port.trim().parse::<u16>() {
            Ok(p) if p >= 1 => p,
            _ => {
                if let Some(draft) = self.editor.as_mut() {
                    draft.error = Some(t!("forward.error_port"));
                }
                return Task::none();
            }
        };
        let dynamic = draft.kind == ForwardKind::Dynamic;
        let target_port = if dynamic {
            None
        } else {
            match draft.target_port.trim().parse::<u16>() {
                Ok(p) if p >= 1 => Some(p),
                _ => {
                    if let Some(draft) = self.editor.as_mut() {
                        draft.error = Some(t!("forward.error_target_port"));
                    }
                    return Task::none();
                }
            }
        };
        let target_host = if dynamic {
            None
        } else {
            let host = draft.target_host.trim();
            (!host.is_empty()).then(|| host.to_string())
        };
        let draft = self.editor.take().expect("draft checked above");
        let rule = ForwardRule {
            kind: draft.kind,
            bind_address: draft.bind_address.trim().to_string(),
            bind_port,
            target_host,
            target_port,
            // 编辑保留原启停状态；新建默认启用。
            enabled: draft
                .index
                .and_then(|i| ctx.rules.get(i))
                .map(|r| r.enabled)
                .unwrap_or(true),
        };
        let mut forwards = ctx.rules.clone();
        match draft.index {
            Some(i) if i < forwards.len() => forwards[i] = rule,
            _ => forwards.push(rule),
        }
        Task::done(Event::SessionForwardsChanged {
            session_id,
            forwards,
        })
    }
}

/// 保险库未解锁时的统一拒绝：提示后忽略改动（面板的编辑入口亦已置灰，此为兜底）。
fn readonly() -> Task<Event> {
    Task::done(Event::Toast(
        ToastKind::Error,
        t!("forward.readonly").to_string(),
    ))
}

/// 把编辑弹窗传回的类型名反解析为转发类型（见 `KindOption` 的 Display 约定）。
fn parse_kind(name: &str) -> Option<ForwardKind> {
    match name {
        "Local" => Some(ForwardKind::Local),
        "Remote" => Some(ForwardKind::Remote),
        "Dynamic" => Some(ForwardKind::Dynamic),
        _ => None,
    }
}

/// 结构化失败原因的本地化文案（面板 toast 用）。
pub(crate) fn failure_text(failure: &ForwardFailure) -> String {
    match failure {
        ForwardFailure::Bind { addr, port, detail } => {
            t!("forward.failed_bind", addr => addr, port => port, detail => detail)
        }
        ForwardFailure::RemoteDenied { addr, port } => {
            t!("forward.failed_remote_denied", addr => addr, port => port)
        }
        ForwardFailure::Invalid { detail } => t!("forward.failed_invalid", detail => detail),
    }
}

/// 规则类型下拉选项（包装 ForwardKind 以提供本地化显示文本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindOption(pub ForwardKind);

impl std::fmt::Display for KindOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self.0 {
            ForwardKind::Local => t!("forward.kind_local"),
            ForwardKind::Remote => t!("forward.kind_remote"),
            ForwardKind::Dynamic => t!("forward.kind_dynamic"),
        };
        f.write_str(&label)
    }
}

/// 模块内部消息：转发面板与编辑弹窗的 UI 意图、引擎事件流与句柄抵达。
#[derive(Clone)]
pub enum Message {
    /// 切换某条规则的启停（携带规则下标）。
    ToggleRule(usize),
    /// 删除某条规则（携带规则下标），即时删除、不二次确认。
    RemoveRule(usize),
    /// 打开编辑弹窗：`None` 新建，`Some(i)` 编辑第 i 条规则。
    StartEdit(Option<usize>),
    /// 编辑器类型字段变更（携带 ForwardKind 的类型名，见 `KindOption`）。
    EditorKind(String),
    /// 编辑器绑定地址字段变更。
    EditorBindAddress(String),
    /// 编辑器绑定端口字段变更。
    EditorBindPort(String),
    /// 编辑器目的主机字段变更。
    EditorTargetHost(String),
    /// 编辑器目的端口字段变更。
    EditorTargetPort(String),
    /// 保存当前编辑草稿。
    SaveRule,
    /// 取消编辑（丢弃草稿）。
    CancelEdit,
    /// 把作用域切到指定会话（会话行右键「端口转发…」入口）。
    ScopeTo(String),
    /// 引擎事件回流（携带标签 id 与事件）。
    Runtime {
        /// 事件所属标签。
        tab_id: u64,
        /// 引擎上报的事件。
        event: ForwardEvent,
    },
    /// 引擎句柄已就绪（父层直接写入标签状态；模块不处理）。
    HandleArrived {
        /// 句柄所属标签。
        tab_id: u64,
        /// 引擎句柄。
        handle: Arc<ForwardHandle>,
    },
}

/// 上行事件：由父层落地到会话持久化、引擎同步、toast 与导航。
#[derive(Clone)]
pub enum Event {
    /// 作用域会话的规则清单已变更（父层持久化并对该会话全部已连接标签做引擎同步）。
    SessionForwardsChanged {
        /// 规则所属会话 id。
        session_id: String,
        /// 变更后的完整规则清单。
        forwards: Vec<ForwardRule>,
    },
    /// 弹 toast 通知（携带类型与文案）。
    Toast(ToastKind, String),
    /// 打开转发面板并把作用域切到指定会话（无活动标签时的右键入口）。
    OpenPanel(String),
}

/// 父层只读上下文：作用域会话的规则快照与连接状态。
pub struct Ctx {
    /// 作用域会话 id：活动标签优先取其会话；否则取 scope；再否则 `None`。
    pub session_id: Option<String>,
    /// 作用域会话的转发规则快照。
    pub rules: Vec<ForwardRule>,
    /// 作用域是否来自活动标签且已连接（决定面板是否渲染运行时状态）。
    pub live: bool,
    /// live 时的标签 id（运行时状态按它查 `State::runtime`）。
    pub tab_id: Option<u64>,
    /// 是否可编辑（保险库已解锁）。
    pub writable: bool,
}

/// 在指定标签上拉起端口转发引擎（连接就绪后由父层调用）。
///
/// 重连场景先 `clear` 该标签的旧引擎（远程转发一并注销）再重建；句柄经
/// `Message::HandleArrived` 写回标签，事件经 `Message::Runtime` 持续回流。
pub(crate) fn start_for_tab(app: &mut App, tab_id: u64) -> Task<TopMessage> {
    // 重连场景：旧引擎先清（含向服务端注销远程转发），再建新引擎。
    let clear_old = app
        .tabs
        .tab_mut(tab_id)
        .and_then(|tab| tab.forwards.take())
        .map(|old| Task::perform(async move { old.clear().await }, |_| TopMessage::Noop));

    let Some(tab) = app.tabs.list().iter().find(|t| t.id == tab_id) else {
        return clear_old.unwrap_or_else(Task::none);
    };
    let Some(conn) = tab.conn.clone() else {
        return clear_old.unwrap_or_else(Task::none);
    };
    let rules = app
        .session
        .sessions
        .iter()
        .find(|s| s.id == tab.session_id)
        .map(|s| s.forwards.clone())
        .unwrap_or_default();
    app.forwards.note_handle(tab_id, &rules);

    let stream = Task::stream(iced::stream::channel(
        100,
        move |mut output: futures::channel::mpsc::Sender<TopMessage>| async move {
            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            let handle = ForwardEngine::spawn(conn, &rules, tx).await;
            let _ = output
                .send(TopMessage::Forwards(Message::HandleArrived {
                    tab_id,
                    handle: Arc::new(handle),
                }))
                .await;
            while let Some(event) = rx.recv().await {
                let _ = output
                    .send(TopMessage::Forwards(Message::Runtime { tab_id, event }))
                    .await;
            }
        },
    ));
    match clear_old {
        Some(clear) => Task::batch([clear, stream]),
        None => stream,
    }
}
