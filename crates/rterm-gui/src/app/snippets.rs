//! 命令片段管理模块
//!
//! 持有片段清单内存态、编辑弹窗草稿与列表交互态；发送把片段原文（加可选回车）上行给父层，
//! 由父层经 `BackendCommand::Write` 直达远端——刻意不经粘贴编码（见 [`crate::app::events`]）。

use crate::i18n::localize_config_error;
use crate::state::{TerminalTab, ToastKind};
use crate::t;
use iced::Task;
use iced::widget::text_editor;
use log::error;
use rterm_config::{Snippet, SnippetIssue, Snippets};
use rterm_core::ConnectionStatus;
use std::collections::HashSet;

/// 把片段文本组合成写入终端的字节序列。
///
/// `\r\n` 一律归一为 `\n`；`submit` 时剪掉行尾换行再补**单个** `\r`（否则会多出一个空提示符），不提交则只归一。文本为空不写入。
pub fn compose_send_bytes(text: &str, submit: bool) -> Vec<u8> {
    if text.is_empty() {
        return Vec::new();
    }
    let normalized = text.replace("\r\n", "\n");
    if !submit {
        return normalized.into_bytes();
    }
    let trimmed = normalized.trim_end_matches(['\n', '\r']);
    let mut bytes = Vec::with_capacity(trimmed.len() + 1);
    bytes.extend_from_slice(trimmed.as_bytes());
    bytes.push(b'\r');
    bytes
}

/// 片段编辑草稿：进入编辑即克隆，点保存才落盘，取消 / Esc 丢弃。
///
/// 文本常驻 [`text_editor::Content`]，故本类型不参与 `Clone` 派生。
pub struct EditorDraft {
    /// 编辑目标的清单下标；`None` 表示「新建」（保存时追加到末尾）。
    pub index: Option<usize>,
    /// 片段名称。
    pub name: String,
    /// 片段文本（发送到终端的原始内容）。
    pub text: text_editor::Content,
    /// 发送后是否补一个回车使命令直接执行。
    pub submit: bool,
    /// 是否已点过保存；未点过则弹窗不显示校验原因（新建草稿必然「名称为空」，提前报错是误报）。
    pub save_attempted: bool,
}

impl EditorDraft {
    /// 构造空白草稿（用于新建条目）。
    pub fn new() -> Self {
        Self {
            index: None,
            name: String::new(),
            text: text_editor::Content::new(),
            submit: true,
            save_attempted: false,
        }
    }

    /// 由已有片段构造草稿（用于编辑），保留其清单下标。
    pub fn from_snippet(index: usize, snippet: &Snippet) -> Self {
        Self {
            index: Some(index),
            name: snippet.name.clone(),
            text: text_editor::Content::with_text(&snippet.text),
            submit: snippet.submit,
            save_attempted: false,
        }
    }

    /// 是否为「新建」草稿（尚无清单下标）。
    pub fn is_new(&self) -> bool {
        self.index.is_none()
    }

    /// 草稿的渲染目标标识：新建取哨兵 `usize::MAX`（部件的动作绑定无法随 `Option` 变化）。
    ///
    /// 回带进 [`Message::EditorAction`] 比对，使「草稿已关闭 / 已换目标」时到达的动作被丢弃。
    pub fn target(&self) -> usize {
        self.index.unwrap_or(usize::MAX)
    }

    /// 由草稿构建片段（保存用）。
    pub fn to_snippet(&self) -> Snippet {
        Snippet {
            name: self.name.clone(),
            text: self.text.text(),
            submit: self.submit,
        }
    }

    /// 校验草稿可否保存；失败原因供视图渲染行内提示。规则与上限见 [`Snippet::validate`]。
    pub fn issue(&self) -> Option<SnippetIssue> {
        self.to_snippet().validate().err()
    }
}

/// 模块状态：片段清单、持久化器、加载故障与列表 / 编辑器的交互态。
pub struct State {
    /// 片段清单，数组顺序即展示顺序。
    pub items: Vec<Snippet>,
    /// 片段文件持久化器（配置目录不可用时为 `None`）。
    pub store: Option<Snippets>,
    /// 片段文件不可用的原因（加载失败时置位）；置位后禁止编辑与落盘。
    pub load_problem: Option<String>,
    /// 列表过滤词：对名称与文本做大小写不敏感子串匹配，空串即全量。
    pub filter: String,
    /// 当前被鼠标悬浮的片段下标（`None` 表示无悬浮）。
    pub hovered: Option<usize>,
    /// 当前选中的片段下标（`None` 表示无选中）。
    pub selected: Option<usize>,
    /// 编辑弹窗草稿（无则未编辑）。
    pub editor: Option<EditorDraft>,
    /// 快速命令弹窗（Ctrl+Shift+P）的交互态。
    pub palette: Palette,
}

/// 快速命令弹窗的交互态：过滤词与命中集内的高亮位置。
pub struct Palette {
    /// 弹窗是否打开。
    pub open: bool,
    /// 过滤词：对名称与文本做大小写不敏感子串匹配，空串即全量。
    pub query: String,
    /// 高亮位置（过滤命中集内的下标，非清单下标）；`None` 表示无命中。
    pub highlight: Option<usize>,
    /// 过滤输入框部件 id（打开时父层据此抢键盘焦点）。
    pub input_id: iced::widget::Id,
    /// 命中列表滚动部件 id（键盘移动高亮时据此编程式滚动）。
    pub list_id: iced::widget::Id,
    /// 命中列表当前滚动 y 的镜像（on_scroll 回写，键盘滚动时自行同步）。
    pub scroll_y: f32,
}

impl State {
    /// 构造模块状态并读盘：文件缺失视为空清单，读不出 / 解析不了时记下不可用原因。
    pub fn new() -> Self {
        let mut state = Self {
            items: Vec::new(),
            store: None,
            load_problem: None,
            filter: String::new(),
            hovered: None,
            selected: None,
            editor: None,
            palette: Palette {
                open: false,
                query: String::new(),
                highlight: None,
                input_id: iced::widget::Id::unique(),
                list_id: iced::widget::Id::unique(),
                scroll_y: 0.0,
            },
        };
        match Snippets::new() {
            Ok(mut store) => {
                // 提示要带上文件路径：用户据此才能找到那个要手工修的文件。
                let path = store.path().display().to_string();
                match store.load() {
                    Ok(items) => {
                        state.items = items;
                        state.store = Some(store);
                    }
                    Err(e) => {
                        error!("failed to load snippets: {e}");
                        state.load_problem = Some(t!(
                            "snippet.load_failed_path",
                            path => path,
                            err => e.to_string()
                        ));
                        state.store = Some(store);
                    }
                }
            }
            Err(e) => {
                error!("failed to initialize snippet store: {e}");
                state.load_problem = Some(t!("snippet.load_failed", err => e.to_string()));
            }
        }
        state
    }

    /// 是否允许新增 / 编辑 / 保存：文件不可用（读不出或配置目录建不出来）时一律拒绝。
    pub fn can_edit(&self) -> bool {
        self.store.is_some() && self.load_problem.is_none()
    }

    /// 模块更新：只改自身状态；需要父层配合的事以 [`Event`] 经 `Task` 上行。
    pub fn update(&mut self, msg: Message, ctx: &Ctx) -> Task<Event> {
        match msg {
            Message::NewSnippet => {
                if self.can_edit() {
                    self.editor = Some(EditorDraft::new());
                }
                Task::none()
            }
            Message::EditSnippet(idx) => {
                if self.can_edit()
                    && let Some(snippet) = self.items.get(idx)
                {
                    self.editor = Some(EditorDraft::from_snippet(idx, snippet));
                }
                Task::none()
            }
            Message::DuplicateSnippet(idx) => {
                if !self.can_edit() {
                    return Task::none();
                }
                let Some(src) = self.items.get(idx) else {
                    return Task::none();
                };
                let copy = duplicated(src, &self.items);
                let mut next = self.items.clone();
                next.push(copy);
                match self.commit(next) {
                    Ok(()) => {
                        self.selected = Some(self.items.len() - 1);
                        self.saved()
                    }
                    Err(task) => task,
                }
            }
            Message::DeleteSnippet(idx) => {
                if idx >= self.items.len() {
                    return Task::none();
                }
                let mut next = self.items.clone();
                next.remove(idx);
                match self.commit(next) {
                    Ok(()) => {
                        self.shift_after_removal(idx);
                        self.saved()
                    }
                    Err(task) => task,
                }
            }
            Message::EditorName(name) => {
                if let Some(draft) = self.editor.as_mut() {
                    draft.name = name;
                }
                Task::none()
            }
            Message::EditorAction(target, action) => {
                if let Some(draft) = self.editor.as_mut()
                    && draft.target() == target
                {
                    draft.text.perform(action);
                }
                Task::none()
            }
            Message::EditorToggleSubmit => {
                if let Some(draft) = self.editor.as_mut() {
                    draft.submit = !draft.submit;
                }
                Task::none()
            }
            Message::MoveUp(idx) => self.move_item(idx, -1),
            Message::MoveDown(idx) => self.move_item(idx, 1),
            Message::EditorSave => self.save_editor(),
            Message::CancelEdit => {
                self.editor = None;
                Task::none()
            }
            Message::FilterChanged(query) => {
                self.filter = query;
                Task::none()
            }
            Message::SnippetSelect(idx) => {
                self.selected = Some(idx);
                Task::none()
            }
            Message::SnippetEnter(idx) => {
                if self.hovered != Some(idx) {
                    self.hovered = Some(idx);
                }
                Task::none()
            }
            Message::SnippetExit(idx) => {
                if self.hovered == Some(idx) {
                    self.hovered = None;
                }
                Task::none()
            }
            Message::SnippetSelectHovered => {
                // 悬浮项即光标下的片段；未在列表项上悬浮（右击终端 / 空白）时保持原选中。
                if let Some(idx) = self.hovered {
                    self.selected = Some(idx);
                }
                Task::none()
            }
            Message::SendCurrent(idx) => self.send_current(idx, ctx),
            Message::SendAll(idx) => self.send_all(idx, ctx),
            Message::TogglePalette => {
                if self.palette.open {
                    self.close_palette()
                } else {
                    self.open_palette()
                }
            }
            Message::ClosePalette => self.close_palette(),
            Message::PaletteQuery(query) => {
                self.palette.query = query;
                self.palette.highlight = self.first_hit();
                self.scroll_palette_to(0.0)
            }
            Message::PaletteNext => {
                self.move_palette_highlight(1);
                self.keep_highlight_visible()
            }
            Message::PalettePrevious => {
                self.move_palette_highlight(-1);
                self.keep_highlight_visible()
            }
            Message::PaletteScrolled(y) => {
                self.palette.scroll_y = y;
                Task::none()
            }
            Message::PalettePick(pos) => {
                // 单击命中行：先对齐高亮再发送，与键盘回车同一落点。
                if pos < self.palette_hits().len() {
                    self.palette.highlight = Some(pos);
                    self.palette_send(ctx)
                } else {
                    Task::none()
                }
            }
            Message::PaletteSend => self.palette_send(ctx),
        }
    }

    /// 发送到当前会话（活动标签）：无活动标签时提示且不写入。
    fn send_current(&self, idx: usize, ctx: &Ctx) -> Task<Event> {
        let Some(snippet) = self.items.get(idx) else {
            return Task::none();
        };
        let bytes = compose_send_bytes(&snippet.text, snippet.submit);
        if bytes.is_empty() {
            return Task::none();
        }
        match ctx.active {
            Some(tab_id) => Task::done(Event::Send { tab_id, bytes }),
            None => Task::done(Event::Toast(ToastKind::Error, t!("snippet.no_target"))),
        }
    }

    /// 当前过滤词下的命中下标（清单原始下标，顺序即展示顺序）。
    fn palette_hits(&self) -> Vec<usize> {
        filtered_snippets(&self.items, &self.palette.query)
    }

    /// 命中集中的首个高亮位置；无命中为 `None`。
    fn first_hit(&self) -> Option<usize> {
        (!self.palette_hits().is_empty()).then_some(0)
    }

    /// 在命中集内移动高亮（夹紧不环绕）；无命中无操作。
    fn move_palette_highlight(&mut self, delta: isize) {
        let Some(current) = self.palette.highlight else {
            return;
        };
        let count = self.palette_hits().len();
        if count == 0 {
            return;
        }
        let next = (current as isize + delta).clamp(0, count as isize - 1);
        self.palette.highlight = Some(next as usize);
    }

    /// 编程式滚动命中列表到绝对 y（只动纵向），镜像同步写入以兜住 on_scroll 不回触的情况。
    fn scroll_palette_to(&mut self, y: f32) -> Task<Event> {
        self.palette.scroll_y = y;
        iced::widget::operation::scroll_to(
            self.palette.list_id.clone(),
            iced::widget::scrollable::AbsoluteOffset {
                x: None,
                y: Some(y),
            },
        )
    }

    /// 键盘移动高亮后把该行滚回视口：越上缘贴顶、越下缘贴底，视口内不动。
    fn keep_highlight_visible(&mut self) -> Task<Event> {
        let Some(pos) = self.palette.highlight else {
            return Task::none();
        };
        let Some(y) = crate::snippet_palette::scroll_target(
            self.palette.scroll_y,
            pos,
            self.palette_hits().len(),
        ) else {
            return Task::none();
        };
        self.scroll_palette_to(y)
    }

    /// 打开弹窗：清空过滤与高亮，收走终端焦点并把键盘交给过滤框。
    fn open_palette(&mut self) -> Task<Event> {
        self.palette.open = true;
        self.palette.query.clear();
        self.palette.highlight = self.first_hit();
        // 视图重建后 scrollable 从 0 起滚，镜像随之归零即可。
        self.palette.scroll_y = 0.0;
        Task::batch([
            Task::done(Event::SetTerminalFocused(false)),
            Task::done(Event::FocusPaletteInput),
        ])
    }

    /// 关闭弹窗并复位交互态；焦点交还终端（父层自会处理「窗口失焦不写」的守卫）。
    fn close_palette(&mut self) -> Task<Event> {
        self.palette.open = false;
        self.palette.query.clear();
        self.palette.highlight = None;
        self.palette.scroll_y = 0.0;
        Task::done(Event::SetTerminalFocused(true))
    }

    /// 发送高亮命中项到当前会话（活动标签）：成功即关闭弹窗；无活动标签只提示、保持打开。
    fn palette_send(&mut self, ctx: &Ctx) -> Task<Event> {
        let Some(pos) = self.palette.highlight else {
            return Task::none();
        };
        let Some(&idx) = self.palette_hits().get(pos) else {
            return Task::none();
        };
        let Some(snippet) = self.items.get(idx) else {
            return Task::none();
        };
        let bytes = compose_send_bytes(&snippet.text, snippet.submit);
        if bytes.is_empty() {
            return Task::none();
        }
        match ctx.active {
            Some(tab_id) => Task::batch([
                Task::done(Event::Send { tab_id, bytes }),
                self.close_palette(),
            ]),
            None => Task::done(Event::Toast(ToastKind::Error, t!("snippet.no_target"))),
        }
    }

    /// 发送到所有打开的会话：按会话去重后逐目标写入同一份字节，并汇报已发送 / 跳过数。
    fn send_all(&self, idx: usize, ctx: &Ctx) -> Task<Event> {
        let Some(snippet) = self.items.get(idx) else {
            return Task::none();
        };
        let bytes = compose_send_bytes(&snippet.text, snippet.submit);
        if bytes.is_empty() {
            return Task::none();
        }
        if ctx.sessions.is_empty() {
            return Task::done(Event::Toast(ToastKind::Error, t!("snippet.all_no_target")));
        }
        let sent = ctx.sessions.len();
        let skipped = ctx.total_sessions.saturating_sub(sent);
        let mut tasks: Vec<Task<Event>> = ctx
            .sessions
            .iter()
            .map(|tab_id| {
                Task::done(Event::Send {
                    tab_id: *tab_id,
                    bytes: bytes.clone(),
                })
            })
            .collect();
        tasks.push(Task::done(Event::Toast(
            ToastKind::Success,
            t!("snippet.sent_all", sent => sent, skipped => skipped),
        )));
        Task::batch(tasks)
    }

    /// 保存编辑器草稿：校验通过才落盘，失败时保持草稿打开且内存清单不变。
    fn save_editor(&mut self) -> Task<Event> {
        if !self.can_edit() {
            return Task::none();
        }
        let Some(draft) = self.editor.as_mut() else {
            return Task::none();
        };
        // 点过保存才开始显示校验原因：否则新建草稿一打开就报「名称不能为空」。
        draft.save_attempted = true;
        if draft.issue().is_some() {
            // 校验不过：保持草稿打开，原因由弹窗页脚显示。
            return Task::none();
        }
        let snippet = draft.to_snippet();
        let target = draft.index;
        let mut next = self.items.clone();
        let saved_index = match target {
            Some(i) if i < next.len() => {
                next[i] = snippet;
                i
            }
            _ => {
                next.push(snippet);
                next.len() - 1
            }
        };
        match self.commit(next) {
            Ok(()) => {
                self.editor = None;
                self.selected = Some(saved_index);
                self.saved()
            }
            Err(task) => task,
        }
    }

    /// 在清单内移动某条片段（`delta` 为 ±1）；越界无操作。
    fn move_item(&mut self, idx: usize, delta: isize) -> Task<Event> {
        if idx >= self.items.len() {
            return Task::none();
        }
        let Some(new_idx) = idx
            .checked_add_signed(delta)
            .filter(|i| *i < self.items.len())
        else {
            return Task::none();
        };
        let mut next = self.items.clone();
        next.swap(idx, new_idx);
        match self.commit(next) {
            Ok(()) => {
                swap_index(&mut self.selected, idx, new_idx);
                swap_index(&mut self.hovered, idx, new_idx);
                self.saved()
            }
            Err(task) => task,
        }
    }

    /// 先把新清单落盘、成功后才替换内存清单：失败时内存不变，不出现「界面已改、磁盘没有」。
    fn commit(&mut self, next: Vec<Snippet>) -> Result<(), Task<Event>> {
        self.try_save(&next)?;
        self.items = next;
        Ok(())
    }

    /// 落盘一份清单（不改变内存态）。失败时返回承载错误提示的任务。
    fn try_save(&self, items: &[Snippet]) -> Result<(), Task<Event>> {
        let Some(store) = self.store.as_ref() else {
            return Err(Task::done(Event::Toast(
                ToastKind::Error,
                t!("snippet.save_failed", err => t!("snippet.store_unavailable")),
            )));
        };
        store.save(items).map_err(|e| {
            error!("failed to save snippets: {e}");
            Task::done(Event::Toast(
                ToastKind::Error,
                t!("snippet.save_failed", err => localize_config_error(&e)),
            ))
        })
    }

    /// 清单变更后的成功提示（状态栏）。
    fn saved(&self) -> Task<Event> {
        Task::done(Event::Saved {
            count: self.items.len(),
        })
    }

    /// 移除某下标后修正选中 / 悬浮 / 编辑目标：命中者清空，其后整体前移一位。
    fn shift_after_removal(&mut self, removed: usize) {
        shift_index(&mut self.selected, removed);
        shift_index(&mut self.hovered, removed);
        match self.editor.as_ref().and_then(|d| d.index) {
            Some(i) if i == removed => self.editor = None,
            Some(i) if i > removed => {
                if let Some(draft) = self.editor.as_mut() {
                    draft.index = Some(i - 1);
                }
            }
            _ => {}
        }
    }
}

/// 把某个下标槽位按下标移除语义修正：等于被移除者清空，大于者前移一位。
fn shift_index(slot: &mut Option<usize>, removed: usize) {
    match *slot {
        Some(i) if i == removed => *slot = None,
        Some(i) if i > removed => *slot = Some(i - 1),
        _ => {}
    }
}

/// 把某个下标槽位按换位语义修正：等于被换位的两个下标时各自指向对方，其余不动。
fn swap_index(slot: &mut Option<usize>, a: usize, b: usize) {
    match *slot {
        Some(i) if i == a => *slot = Some(b),
        Some(i) if i == b => *slot = Some(a),
        _ => {}
    }
}

/// 由现有片段生成副本：名称追加「空格 + 数字」后缀，文本与 `submit` 原样克隆。
fn duplicated(src: &Snippet, existing: &[Snippet]) -> Snippet {
    let mut n = 2;
    let mut name = format!("{} {n}", src.name);
    while existing.iter().any(|s| s.name == name) {
        n += 1;
        name = format!("{} {n}", src.name);
    }
    Snippet {
        name,
        text: src.text.clone(),
        submit: src.submit,
    }
}

/// 按名称 + 文本过滤，返回**原始下标**（使行消息在过滤后仍指向稳定目标）。
pub(crate) fn filtered_snippets(items: &[Snippet], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return (0..items.len()).collect();
    }
    items
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.name.to_lowercase().contains(&query) || s.text.to_lowercase().contains(&query)
        })
        .map(|(i, _)| i)
        .collect()
}

/// 选出「发送到所有打开的会话」的目标标签，并回报总会话数。
///
/// 按 `session_id` 去重、顺序随 `tabs`：每个会话优先取活动标签，否则取首个 `Connected` 标签；无已连接标签的会话不入选、只计入总数（父层据此算跳过数）。
pub(crate) fn broadcast_targets(tabs: &[TerminalTab], active: Option<u64>) -> (Vec<u64>, usize) {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut sessions: Vec<&str> = Vec::new();
    for tab in tabs {
        if seen.insert(tab.session_id.as_str()) {
            sessions.push(tab.session_id.as_str());
        }
    }
    let total = sessions.len();
    let mut targets = Vec::new();
    for session_id in sessions {
        // 优先该会话的活动标签（且需已连接），否则该会话内首个已连接标签。
        let active_here = active.and_then(|a| {
            tabs.iter().find(|t| {
                t.session_id == session_id && t.id == a && t.status == ConnectionStatus::Connected
            })
        });
        let chosen = active_here.or_else(|| {
            tabs.iter()
                .find(|t| t.session_id == session_id && t.status == ConnectionStatus::Connected)
        });
        if let Some(tab) = chosen {
            targets.push(tab.id);
        }
    }
    (targets, total)
}

/// 模块内部消息：片段列表与编辑器的 UI 意图。
#[derive(Clone)]
pub enum Message {
    /// 新建片段（打开空白编辑草稿）。
    NewSnippet,
    /// 编辑已有片段（携带清单下标）。
    EditSnippet(usize),
    /// 复制已有片段（携带清单下标）：副本名称追加数字后缀。
    DuplicateSnippet(usize),
    /// 删除片段（携带清单下标），即时删除、不二次确认。
    DeleteSnippet(usize),
    /// 把片段上移一位（携带清单下标）。
    MoveUp(usize),
    /// 把片段下移一位（携带清单下标）。
    MoveDown(usize),
    /// 编辑器名称字段变更。
    EditorName(String),
    /// 编辑器文本区动作（携带草稿目标标识，用于丢弃陈旧动作）。
    EditorAction(usize, text_editor::Action),
    /// 切换「发送后回车」开关。
    EditorToggleSubmit,
    /// 保存当前编辑草稿。
    EditorSave,
    /// 取消编辑（丢弃草稿）。
    CancelEdit,
    /// 过滤词变更（携带输入框最新文本）。
    FilterChanged(String),
    /// 左键选中片段（携带清单下标）。
    SnippetSelect(usize),
    /// 鼠标进入列表某一项（携带清单下标），用于渲染悬浮高亮。
    SnippetEnter(usize),
    /// 鼠标离开列表某一项（携带清单下标），用于清除悬浮高亮。
    SnippetExit(usize),
    /// 右键按下：把光标所在的片段（即当前悬浮项）标记为选中。
    SnippetSelectHovered,
    /// 发送到当前会话（携带清单下标）。
    SendCurrent(usize),
    /// 发送到所有打开的会话（携带清单下标）。
    SendAll(usize),
    /// 切换快速命令弹窗（开则关、关则开）。
    TogglePalette,
    /// 关闭快速命令弹窗（丢弃过滤与高亮）。
    ClosePalette,
    /// 弹窗过滤词变更（携带输入框最新文本）。
    PaletteQuery(String),
    /// 弹窗高亮移到下一个命中项（夹紧不环绕）。
    PaletteNext,
    /// 弹窗高亮移到上一个命中项（夹紧不环绕）。
    PalettePrevious,
    /// 弹窗命中列表滚动位置上报（携带滚动 y），维持镜像供键盘滚动判界。
    PaletteScrolled(f32),
    /// 单击弹窗命中行（携带命中集下标）：对齐高亮并发送。
    PalettePick(usize),
    /// 发送弹窗高亮项到当前会话（键盘回车入口）。
    PaletteSend,
}

/// 上行事件：由父层落地到终端写入通道、toast 与状态栏。
#[derive(Clone)]
pub enum Event {
    /// 把 `bytes` 写入指定标签的终端（父层转 `BackendCommand::Write`）。
    Send {
        /// 目标标签 id。
        tab_id: u64,
        /// 待写入的原始字节。
        bytes: Vec<u8>,
    },
    /// 弹 toast 通知（携带类型与文案）。
    Toast(ToastKind, String),
    /// 清单已成功落盘（携带当前条数），父层据此更新状态栏。
    Saved {
        /// 落盘后的片段条数。
        count: usize,
    },
    /// 请求把键盘焦点交给弹窗过滤输入框（打开时）。
    FocusPaletteInput,
    /// 请求写回「终端是否持有键盘焦点」（弹窗开关时的焦点交接，父层带窗口失焦守卫落地）。
    SetTerminalFocused(bool),
}

/// 父层只读上下文：发送目标快照。
pub struct Ctx {
    /// 活动标签 id（「发送到当前会话」的目标）。
    pub active: Option<u64>,
    /// 「发送到所有打开的会话」的目标标签 id（已按会话去重、只含已连接）。
    pub sessions: Vec<u64>,
    /// 打开的会话总数（含无已连接标签者），用于计算跳过数。
    pub total_sessions: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::STATE_ROOT_LOCK as STATE_LOCK;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, MutexGuard};

    /// 便捷构造一条片段。
    fn snippet(name: &str, text: &str) -> Snippet {
        Snippet {
            name: name.to_string(),
            text: text.to_string(),
            submit: true,
        }
    }

    /// 便捷构造一个终端标签（只填广播目标选择关心的字段）。
    fn tab(id: u64, session_id: &str, status: ConnectionStatus) -> TerminalTab {
        TerminalTab {
            id,
            session_id: session_id.to_string(),
            status,
            error: None,
            conn: None,
            terminal: None,
            resize_tx: None,
            bridge: None,
            disconnect_reason: None,
            cwd: Arc::new(Mutex::new(None)),
            title: String::new(),
            bell_flash: None,
            banner_flash: None,
            forwards: None,
        }
    }

    /// 把状态根重定向到独占临时目录，返回该目录与持有到测试结束的全局锁。
    fn temp_state(tag: &str) -> (PathBuf, MutexGuard<'static, ()>) {
        let guard = STATE_LOCK.lock().unwrap();
        let tmp =
            std::env::temp_dir().join(format!("rterm_snips_gui_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).expect("应能创建临时状态根");
        rterm_config::paths::set_test_root(Some(tmp.clone()));
        (tmp, guard)
    }

    /// 结束一次使用临时状态根的测试。
    fn done(tmp: &PathBuf) {
        rterm_config::paths::set_test_root(None);
        let _ = fs::remove_dir_all(tmp);
    }

    /// 空上下文（无活动标签、无广播目标）。
    fn no_ctx() -> Ctx {
        Ctx {
            active: None,
            sessions: Vec::new(),
            total_sessions: 0,
        }
    }

    #[test]
    fn compose_send_bytes_normalizes_and_terminates() {
        let cases = [
            ("", true, ""),
            ("", false, ""),
            ("ls", true, "ls\r"),
            ("ls\n", true, "ls\r"),
            ("ls\r\n", true, "ls\r"),
            ("ls", false, "ls"),
            ("a\r\nb\r\n", true, "a\nb\r"),
            ("a\nb", false, "a\nb"),
            ("echo hi\r\n\r\n", true, "echo hi\r"),
        ];
        for (text, submit, expect) in cases {
            assert_eq!(
                compose_send_bytes(text, submit),
                expect.as_bytes(),
                "text={text:?} submit={submit}"
            );
        }
    }

    #[test]
    fn broadcast_targets_dedupes_and_prefers_active() {
        let tabs = vec![
            tab(1, "a", ConnectionStatus::Connecting),
            tab(2, "a", ConnectionStatus::Connected),
            tab(3, "b", ConnectionStatus::Connected),
            tab(4, "b", ConnectionStatus::Connected),
            tab(5, "c", ConnectionStatus::Connecting),
        ];
        // 无活动标签：每会话取首个已连接标签，c 无已连接故不入选。
        let (targets, total) = broadcast_targets(&tabs, None);
        assert_eq!(targets, vec![2, 3]);
        assert_eq!(total, 3);

        // 活动标签 4 属于会话 b：该会话改用活动标签 4。
        let (targets, _) = broadcast_targets(&tabs, Some(4));
        assert_eq!(targets, vec![2, 4]);

        // 活动标签 1 未连接：回落该会话首个已连接标签 2。
        let (targets, _) = broadcast_targets(&tabs, Some(1));
        assert_eq!(targets, vec![2, 3]);
    }

    #[test]
    fn broadcast_targets_counts_skipped_sessions() {
        let tabs = vec![
            tab(1, "a", ConnectionStatus::Error),
            tab(2, "b", ConnectionStatus::Connected),
        ];
        let (targets, total) = broadcast_targets(&tabs, None);
        assert_eq!(targets, vec![2]);
        assert_eq!(total, 2, "无已连接标签的会话仍计入总数，从而算作跳过");
        assert_eq!(broadcast_targets(&[], None), (Vec::new(), 0));
    }

    #[test]
    fn filtered_snippets_matches_name_and_text() {
        let items = vec![
            snippet("deploy", "kubectl apply -f k8s.yaml"),
            snippet("logs", "journalctl -fu nginx"),
            snippet("tail", "tail -f /var/log/deploy.log"),
        ];
        assert_eq!(filtered_snippets(&items, ""), vec![0, 1, 2]);
        assert_eq!(filtered_snippets(&items, "   "), vec![0, 1, 2]);
        // 名称命中（0）与文本命中（2）各一路。
        assert_eq!(filtered_snippets(&items, "DEPLOY"), vec![0, 2]);
        assert_eq!(filtered_snippets(&items, "nginx"), vec![1]);
        assert_eq!(filtered_snippets(&items, "zzz"), Vec::<usize>::new());
    }

    #[test]
    fn editor_creates_edits_moves_duplicates_and_deletes() {
        let (tmp, _guard) = temp_state("crud");
        let ctx = no_ctx();
        let mut s = State::new();
        assert!(s.can_edit(), "临时目录下应可编辑");

        for (name, text) in [("one", "echo 1"), ("two", "echo 2"), ("three", "echo 3")] {
            let _ = s.update(Message::NewSnippet, &ctx);
            let draft = s.editor.as_mut().unwrap();
            draft.name = name.to_string();
            draft.text = text_editor::Content::with_text(text);
            let _ = s.update(Message::EditorSave, &ctx);
        }
        let names = |s: &State| s.items.iter().map(|x| x.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&s), ["one", "two", "three"]);
        assert!(s.editor.is_none(), "保存后应关闭草稿");

        // 编辑首条：改名不新增条目。
        let _ = s.update(Message::EditSnippet(0), &ctx);
        let _ = s.update(Message::EditorName("uno".into()), &ctx);
        let _ = s.update(Message::EditorSave, &ctx);
        assert_eq!(names(&s), ["uno", "two", "three"]);

        // 列表右键菜单下移次条：与末条换位、选中随行移动；末行下移无操作。
        let _ = s.update(Message::SnippetSelect(1), &ctx);
        let _ = s.update(Message::MoveDown(1), &ctx);
        assert_eq!(names(&s), ["uno", "three", "two"]);
        assert_eq!(s.selected, Some(2), "选中随行移动");
        let _ = s.update(Message::MoveDown(2), &ctx);
        assert_eq!(names(&s), ["uno", "three", "two"], "末行下移无操作");

        // 连续上移至首行；再上移无操作。
        let _ = s.update(Message::MoveUp(2), &ctx);
        let _ = s.update(Message::MoveUp(1), &ctx);
        assert_eq!(names(&s), ["two", "uno", "three"]);
        assert_eq!(s.selected, Some(0));
        let _ = s.update(Message::MoveUp(0), &ctx);
        assert_eq!(names(&s), ["two", "uno", "three"], "首行上移无操作");
        assert_eq!(s.selected, Some(0), "越界移动不改选中");

        // 复制：名称追加后缀，选中副本。
        let _ = s.update(Message::DuplicateSnippet(0), &ctx);
        assert_eq!(names(&s), ["two", "uno", "three", "two 2"]);
        assert_eq!(s.selected, Some(3));

        // 删除：其后选中 / 编辑目标前移。
        let _ = s.update(Message::DeleteSnippet(0), &ctx);
        assert_eq!(names(&s), ["uno", "three", "two 2"]);

        // 新实例从盘读回，内容与顺序一致。
        let reopened = State::new();
        assert_eq!(reopened.items, s.items, "重启应读回同一清单");

        done(&tmp);
    }

    #[test]
    fn editor_save_rejects_blank_name_or_text() {
        let (tmp, _guard) = temp_state("validate");
        let ctx = no_ctx();
        let mut s = State::new();

        let _ = s.update(Message::NewSnippet, &ctx);
        assert!(
            !s.editor.as_ref().unwrap().save_attempted,
            "未点保存前不显示校验原因"
        );
        s.editor.as_mut().unwrap().text = text_editor::Content::with_text("echo hi");
        assert_eq!(
            s.editor.as_ref().unwrap().issue(),
            Some(SnippetIssue::EmptyName)
        );
        let _ = s.update(Message::EditorSave, &ctx);
        assert!(
            s.editor.as_ref().unwrap().save_attempted,
            "点过保存才开始显示原因"
        );
        assert!(s.items.is_empty(), "空名不得保存");
        assert!(s.editor.is_some(), "校验失败时草稿保持打开");

        let _ = s.update(Message::EditorName("ok".into()), &ctx);
        assert_eq!(s.editor.as_ref().unwrap().issue(), None);
        s.editor.as_mut().unwrap().text = text_editor::Content::with_text("  \n\t ");
        assert_eq!(
            s.editor.as_ref().unwrap().issue(),
            Some(SnippetIssue::EmptyText)
        );
        let _ = s.update(Message::EditorSave, &ctx);
        assert!(s.items.is_empty(), "空白文本不得保存");
        assert!(s.editor.is_some());

        done(&tmp);
    }

    #[test]
    fn editor_action_guards_against_stale_targets() {
        let (tmp, _guard) = temp_state("stale");
        let ctx = no_ctx();
        let mut s = State::new();

        let _ = s.update(Message::NewSnippet, &ctx);
        let target = s.editor.as_ref().unwrap().target();
        assert_eq!(target, usize::MAX, "新建草稿的目标为哨兵值");

        let _ = s.update(
            Message::EditorAction(
                target,
                text_editor::Action::Edit(text_editor::Edit::Insert('x')),
            ),
            &ctx,
        );
        assert_eq!(s.editor.as_ref().unwrap().text.text(), "x");

        // 目标不匹配（陈旧动作）被丢弃。
        let _ = s.update(
            Message::EditorAction(
                12_345,
                text_editor::Action::Edit(text_editor::Edit::Insert('y')),
            ),
            &ctx,
        );
        assert_eq!(s.editor.as_ref().unwrap().text.text(), "x");

        // 非编辑动作（滚动）不改内容但仍被放行。
        let _ = s.update(
            Message::EditorAction(target, text_editor::Action::Scroll { lines: 3 }),
            &ctx,
        );
        assert_eq!(s.editor.as_ref().unwrap().text.text(), "x");

        // 关闭草稿后到达的动作无处落地，不 panic。
        let _ = s.update(Message::CancelEdit, &ctx);
        let _ = s.update(
            Message::EditorAction(0, text_editor::Action::Edit(text_editor::Edit::Insert('z'))),
            &ctx,
        );
        assert!(s.editor.is_none());

        done(&tmp);
    }

    #[test]
    fn unusable_file_disables_editing() {
        let (tmp, _guard) = temp_state("unusable");
        let dir = tmp.join("config");
        fs::create_dir_all(&dir).expect("应能创建配置目录");
        fs::write(dir.join("snippets.toml"), "[[snippet]\nname = \"坏\"\n")
            .expect("应能写入坏文件");

        let mut s = State::new();
        let problem = s.load_problem.clone().expect("坏文件应记下不可用原因");
        assert!(problem.contains("snippets.toml"), "提示应带上文件路径");
        assert!(s.items.is_empty());
        assert!(!s.can_edit(), "不可用时不得编辑");

        let ctx = no_ctx();
        let _ = s.update(Message::NewSnippet, &ctx);
        assert!(s.editor.is_none(), "不可用时新建无操作");

        done(&tmp);
    }

    #[test]
    fn send_requires_an_available_snippet_and_does_not_panic() {
        let (tmp, _guard) = temp_state("send");
        let mut s = State::new();
        s.items = vec![
            snippet("go", "make test"),
            Snippet {
                name: "empty".to_string(),
                text: String::new(),
                submit: true,
            },
        ];
        let ctx = Ctx {
            active: Some(7),
            sessions: vec![7, 9],
            total_sessions: 3,
        };
        // 越界下标与空文本均不产生写入，也不 panic。
        let _ = s.update(Message::SendCurrent(0), &no_ctx());
        let _ = s.update(Message::SendCurrent(99), &ctx);
        let _ = s.update(Message::SendCurrent(1), &ctx);
        let _ = s.update(Message::SendAll(0), &ctx);
        let _ = s.update(Message::SendAll(99), &ctx);

        done(&tmp);
    }

    #[test]
    fn shift_index_moves_or_clears_slots() {
        let mut slot = Some(3);
        shift_index(&mut slot, 1);
        assert_eq!(slot, Some(2));
        shift_index(&mut slot, 2);
        assert_eq!(slot, None);
        shift_index(&mut None, 0);
        assert_eq!(slot, None);
    }

    #[test]
    fn palette_open_query_highlight_and_reset() {
        let (tmp, _guard) = temp_state("palette");
        let ctx = no_ctx();
        let mut s = State::new();
        s.items = vec![
            snippet("deploy", "kubectl apply"),
            snippet("logs", "journalctl"),
        ];

        // 打开：清空过滤、高亮首个命中。
        let _ = s.update(Message::TogglePalette, &ctx);
        assert!(s.palette.open);
        assert!(s.palette.query.is_empty());
        assert_eq!(s.palette.highlight, Some(0));

        // 过滤后高亮回到首个命中；无命中为 None，回车无操作且弹窗保持打开。
        let _ = s.update(Message::PaletteQuery("log".into()), &ctx);
        assert_eq!(s.palette.highlight, Some(0));
        let _ = s.update(Message::PaletteQuery("zzz".into()), &ctx);
        assert_eq!(s.palette.highlight, None);
        let _ = s.update(Message::PaletteSend, &ctx);
        assert!(s.palette.open, "无命中回车无操作");

        // ↑ / ↓ 在命中集内夹紧不环绕。
        let _ = s.update(Message::PaletteQuery(String::new()), &ctx);
        assert_eq!(s.palette.highlight, Some(0));
        let _ = s.update(Message::PaletteNext, &ctx);
        let _ = s.update(Message::PaletteNext, &ctx);
        assert_eq!(s.palette.highlight, Some(1), "末行下移夹紧");
        let _ = s.update(Message::PalettePrevious, &ctx);
        let _ = s.update(Message::PalettePrevious, &ctx);
        let _ = s.update(Message::PalettePrevious, &ctx);
        assert_eq!(s.palette.highlight, Some(0), "首行上移夹紧");

        // toggle 关闭与显式关闭都复位交互态。
        let _ = s.update(Message::TogglePalette, &ctx);
        assert!(!s.palette.open);
        assert!(s.palette.query.is_empty());
        assert_eq!(s.palette.highlight, None);
        let _ = s.update(Message::TogglePalette, &ctx);
        let _ = s.update(Message::ClosePalette, &ctx);
        assert!(!s.palette.open);

        done(&tmp);
    }

    #[test]
    fn palette_send_closes_on_success_and_stays_open_without_target() {
        let (tmp, _guard) = temp_state("palette_send");
        let mut s = State::new();
        s.items = vec![snippet("go", "make test")];
        let _ = s.update(Message::TogglePalette, &no_ctx());

        // 无活动标签：只提示、保持打开。
        let _ = s.update(Message::PaletteSend, &no_ctx());
        assert!(s.palette.open);

        // 有活动标签：回车发送成功即关闭并复位。
        let ctx = Ctx {
            active: Some(7),
            sessions: vec![7],
            total_sessions: 1,
        };
        let _ = s.update(Message::PaletteSend, &ctx);
        assert!(!s.palette.open);

        // 单击命中行同样发送并关闭；越界下标无操作。
        let _ = s.update(Message::TogglePalette, &no_ctx());
        let _ = s.update(Message::PalettePick(0), &ctx);
        assert!(!s.palette.open);
        let _ = s.update(Message::TogglePalette, &no_ctx());
        let _ = s.update(Message::PalettePick(9), &ctx);
        assert!(s.palette.open, "越界下标不改状态");

        done(&tmp);
    }
}
