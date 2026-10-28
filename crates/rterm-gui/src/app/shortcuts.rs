//! 快捷键管理模块
//!
//! 持有快捷键弹窗的显示开关、过滤词与当前键位表 `Keymap`（默认表叠加稀疏覆盖）；
//! 键位数据模型与持久化见 `rterm_config::keybindings`。

use iced::Task;
use log::warn;
use rterm_config::keybindings::{Chord, ChordKey, Keybindings, Keymap};
use std::collections::BTreeMap;

/// 应用级可绑定命令：快捷键弹窗的可编辑区逐条列出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// 打开设置弹窗。
    OpenSettings,
    /// 打开快捷键弹窗。
    OpenShortcuts,
    /// 收起 / 展开中间面板（会话 / 文件 / 传输列表）。
    ToggleSidebar,
    /// 新建会话。
    NewSession,
    /// 会话快速选择弹窗。
    OpenSessions,
    /// 终端内搜索。
    OpenSearch,
    /// 常用命令快速弹窗。
    OpenSnippets,
    /// 重命名选中的文件条目。
    SftpRename,
}

impl Command {
    /// 全部命令（按 [`CommandGroup`] 的展示顺序排列）。
    pub const ALL: [Command; 8] = [
        Command::OpenSettings,
        Command::OpenShortcuts,
        Command::ToggleSidebar,
        Command::NewSession,
        Command::OpenSessions,
        Command::OpenSearch,
        Command::OpenSnippets,
        Command::SftpRename,
    ];

    /// 稳定标识：作为 `keybindings.toml` 的键与键位表 `Keymap` 的键。
    pub fn as_str(self) -> &'static str {
        match self {
            Command::OpenSettings => "open_settings",
            Command::OpenShortcuts => "open_shortcuts",
            Command::ToggleSidebar => "toggle_sidebar",
            Command::NewSession => "new_session",
            Command::OpenSessions => "open_sessions",
            Command::OpenSearch => "open_search",
            Command::OpenSnippets => "open_snippets",
            Command::SftpRename => "sftp_rename",
        }
    }

    /// 默认和弦；`None` 表示默认不绑定。
    pub fn default_chord(self) -> Option<Chord> {
        match self {
            Command::OpenSettings => Some(Chord {
                ctrl: true,
                shift: false,
                alt: false,
                logo: false,
                key: ChordKey::Char(','),
            }),
            Command::OpenSessions => Some(Chord {
                ctrl: true,
                shift: true,
                alt: false,
                logo: false,
                key: ChordKey::Char('K'),
            }),
            Command::OpenSearch => Some(Chord {
                ctrl: true,
                shift: true,
                alt: false,
                logo: false,
                key: ChordKey::Char('F'),
            }),
            Command::OpenSnippets => Some(Chord {
                ctrl: true,
                shift: true,
                alt: false,
                logo: false,
                key: ChordKey::Char('P'),
            }),
            Command::SftpRename => Some(Chord {
                ctrl: false,
                shift: false,
                alt: false,
                logo: false,
                key: ChordKey::F(2),
            }),
            _ => None,
        }
    }

    /// 由稳定标识解析命令；未知标识返回 `None`。
    pub fn from_id(id: &str) -> Option<Command> {
        Command::ALL.into_iter().find(|c| c.as_str() == id)
    }

    /// 终端聚焦时是否应由终端拦下该命令的和弦（不写入远端）。
    ///
    /// 仅 [`Command::OpenSearch`]、[`Command::OpenSnippets`] 与 [`Command::OpenSessions`]：
    /// 组合键已被客户端占用，写入远端只会干扰远端程序（`Ctrl+Shift+P` 不拦会当 `\x10` 写出）。
    /// 其余命令沿用终端既有的功能键透传（如 `F2` 仍写 `\x1bOQ`），故不拦截。
    pub fn terminal_intercepts(self) -> bool {
        matches!(
            self,
            Command::OpenSearch | Command::OpenSnippets | Command::OpenSessions
        )
    }

    /// 所属分组。
    pub fn group(self) -> CommandGroup {
        match self {
            Command::OpenSettings | Command::OpenShortcuts | Command::ToggleSidebar => {
                CommandGroup::General
            }
            Command::NewSession | Command::OpenSessions => CommandGroup::Session,
            Command::OpenSearch | Command::OpenSnippets => CommandGroup::Terminal,
            Command::SftpRename => CommandGroup::Files,
        }
    }
}

/// 快捷键弹窗的分组（按用户心智分类，而非按命令实现）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandGroup {
    /// 通用：设置、快捷键、侧边栏。
    General,
    /// 会话：新建会话、快速选择。
    Session,
    /// 终端：终端内搜索。
    Terminal,
    /// 文件传输：重命名等。
    Files,
}

impl CommandGroup {
    /// 全部分组（弹窗分区顺序）。
    pub const ALL: [CommandGroup; 4] = [
        CommandGroup::General,
        CommandGroup::Session,
        CommandGroup::Terminal,
        CommandGroup::Files,
    ];

    /// 该分组下的命令（按 [`Command::ALL`] 顺序）。
    pub fn commands(self) -> impl Iterator<Item = Command> {
        Command::ALL.into_iter().filter(move |c| c.group() == self)
    }
}

/// 一条录制受阻的原因，附在对应命令行上提示用户。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingNotice {
    /// 裸键（无修饰键且非功能键）会吞掉终端输入，拒绝绑定。
    NeedsModifier,
    /// 目标和弦已被另一命令占用，用户可「重新分配」抢占。
    Conflict {
        /// 当前占用该和弦的命令。
        occupant: Command,
        /// 用户试图绑定的和弦。
        chord: Chord,
    },
}

/// 模块状态：快捷键弹窗的 UI 私有字段与当前键位表。
pub struct State {
    /// 快捷键管理弹窗是否显示。
    pub show_shortcuts: bool,
    /// 过滤词：按命令名 / 和弦做大小写不敏感子串匹配，空串 = 全量。
    pub filter: String,
    /// 当前键位表（默认表叠加稀疏覆盖），弹窗据此展示各命令的和弦。
    keymap: Keymap,
    /// 稀疏覆盖表（命令 id → 和弦文本）：唯一真相，`keymap` 与落盘均由此派生。
    overrides: BTreeMap<String, String>,
    /// 覆盖表的持久化目标；目录 / IO 不可用时为 `None`，改动仅内存生效。
    store: Option<Keybindings>,
    /// 正在录制的命令；`None` 表示未处于录制态。
    recording: Option<Command>,
    /// 录制受阻提示，附在具体命令上（同时至多一条）。
    notice: Option<(Command, RecordingNotice)>,
}

impl State {
    /// 构造初始状态：弹窗关闭，从 `keybindings.toml` 读取覆盖并构建键位表。
    pub fn new() -> Self {
        let mut store = Keybindings::new().ok();
        if let Some(store) = store.as_mut()
            && let Err(e) = store.load()
        {
            warn!("failed to load keybindings: {e}");
        }
        let overrides = store
            .as_ref()
            .map(|s| s.overrides().clone())
            .unwrap_or_default();
        Self {
            show_shortcuts: false,
            filter: String::new(),
            keymap: Self::build_keymap(&overrides),
            overrides,
            store,
            recording: None,
            notice: None,
        }
    }

    /// 用各命令的默认和弦叠加覆盖构建键位表。
    fn build_keymap(overrides: &BTreeMap<String, String>) -> Keymap {
        let defaults: Vec<(&str, Option<Chord>)> = Command::ALL
            .iter()
            .map(|c| (c.as_str(), c.default_chord()))
            .collect();
        Keymap::build(&defaults, overrides)
    }

    /// 查询某命令当前绑定的和弦；未绑定返回 `None`。
    pub fn chord_for(&self, command: Command) -> Option<&Chord> {
        self.keymap.chord_for(command.as_str())
    }

    /// 查询某和弦当前绑定的命令；未占用返回 `None`。
    pub fn command_for(&self, chord: &Chord) -> Option<Command> {
        self.keymap.command_for(chord).and_then(Command::from_id)
    }

    /// 当前键位表（终端部件据此判定需拦下的保留键）。
    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// 正在录制的命令；未处于录制态返回 `None`。
    pub fn recording(&self) -> Option<Command> {
        self.recording
    }

    /// 是否处于录制态（全局按键分发据此改走录制捕获）。
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// 某命令当前的录制提示。
    pub fn notice_for(&self, command: Command) -> Option<RecordingNotice> {
        match self.notice {
            Some((c, notice)) if c == command => Some(notice),
            _ => None,
        }
    }

    /// 某命令是否带用户覆盖（用于展示「恢复默认」按钮）。
    pub fn is_overridden(&self, command: Command) -> bool {
        self.overrides.contains_key(command.as_str())
    }

    /// 消费一个录制到的和弦：写入覆盖并落盘，或给出受阻提示。无录制态时为空操作。
    fn apply_recorded(&mut self, chord: Chord) {
        let Some(target) = self.recording.take() else {
            return;
        };
        // Esc：取消录制。
        if is_plain(&chord, |k| k == ChordKey::Escape) {
            return;
        }
        // Backspace / Delete（无修饰）：解绑该命令。
        if is_plain(&chord, |k| {
            matches!(k, ChordKey::Backspace | ChordKey::Delete)
        }) {
            self.set_binding(target, None);
            return;
        }
        // 裸键（无修饰且非功能键）会吞输入：拒绝并提示。
        if !has_modifier(&chord) && !matches!(chord.key, ChordKey::F(_)) {
            self.notice = Some((target, RecordingNotice::NeedsModifier));
            return;
        }
        // 冲突：和弦已被其它命令占用，交由用户决定是否重新分配。
        if let Some(occupant) = self.command_for(&chord)
            && occupant != target
        {
            self.notice = Some((target, RecordingNotice::Conflict { occupant, chord }));
            return;
        }
        self.set_binding(target, Some(chord));
    }

    /// 把一条命令的绑定写入覆盖表（稀疏：与默认相同则删除条目）。
    fn write_binding(&mut self, command: Command, chord: Option<Chord>) {
        let value = match chord {
            Some(c) if command.default_chord() == Some(c) => None,
            Some(c) => Some(c.to_string()),
            None if command.default_chord().is_none() => None,
            None => Some(String::new()),
        };
        match value {
            Some(v) => {
                self.overrides.insert(command.as_str().to_string(), v);
            }
            None => {
                self.overrides.remove(command.as_str());
            }
        }
    }

    /// 写入一条绑定、结束录制与提示，并重建键位表、落盘。
    fn set_binding(&mut self, command: Command, chord: Option<Chord>) {
        self.write_binding(command, chord);
        self.commit();
    }

    /// 复位单条命令到默认绑定。
    fn reset_command(&mut self, command: Command) {
        self.overrides.remove(command.as_str());
        self.commit();
    }

    /// 全部恢复默认绑定。
    fn reset_all(&mut self) {
        self.overrides.clear();
        self.commit();
    }

    /// 冲突「重新分配」：目标和弦抢占，原占用者解绑。
    fn reassign(&mut self, target: Command) {
        let Some((command, RecordingNotice::Conflict { occupant, chord })) = self.notice else {
            return;
        };
        if command != target {
            return;
        }
        self.notice = None;
        self.write_binding(occupant, None);
        self.write_binding(target, Some(chord));
        self.rebuild();
        self.persist();
    }

    /// 结束录制与提示，重建键位表并落盘。
    fn commit(&mut self) {
        self.recording = None;
        self.notice = None;
        self.rebuild();
        self.persist();
    }

    /// 由当前覆盖表重建键位表。
    fn rebuild(&mut self) {
        self.keymap = Self::build_keymap(&self.overrides);
    }

    /// 把当前覆盖表写回 `keybindings.toml`。
    fn persist(&mut self) {
        let Some(store) = self.store.as_mut() else {
            return;
        };
        store.reset_all();
        for (id, value) in &self.overrides {
            store.set(id, value);
        }
        if let Err(e) = store.save() {
            warn!("failed to save keybindings: {e}");
        }
    }

    /// 模块更新：只改自身 `State`；当前无需父层配合，故不外泄事件。
    pub fn update(&mut self, msg: Message) -> Task<Event> {
        match msg {
            Message::Toggle => {
                self.show_shortcuts = !self.show_shortcuts;
                // 每次打开都从干净状态开始：清空过滤词并退出录制。
                if self.show_shortcuts {
                    self.filter.clear();
                    self.recording = None;
                    self.notice = None;
                }
                Task::none()
            }
            Message::FilterChanged(query) => {
                self.filter = query;
                Task::none()
            }
            Message::StartRecording(command) => {
                self.recording = Some(command);
                self.notice = None;
                Task::none()
            }
            Message::RecordChord(chord) => {
                self.apply_recorded(chord);
                Task::none()
            }
            Message::Reset(command) => {
                self.reset_command(command);
                Task::none()
            }
            Message::ResetAll => {
                self.reset_all();
                Task::none()
            }
            Message::Reassign(command) => {
                self.reassign(command);
                Task::none()
            }
        }
    }
}

/// 和弦是否至少含一个修饰键。
fn has_modifier(chord: &Chord) -> bool {
    chord.ctrl || chord.shift || chord.alt || chord.logo
}

/// 是否为无修饰键的指定按键。
fn is_plain(chord: &Chord, matches: impl Fn(ChordKey) -> bool) -> bool {
    !has_modifier(chord) && matches(chord.key)
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

/// 模块内部消息：快捷键弹窗的 UI 意图。
///
/// 父层经 `Message::Shortcuts` 路由进来，模块 `update` 自行消费，不外泄。
#[derive(Clone)]
pub enum Message {
    /// 打开 / 关闭快捷键管理弹窗。
    Toggle,
    /// 修改过滤词（携带输入框最新文本；空白等价于不过滤）。
    FilterChanged(String),
    /// 点键位字段：进入（或切换到）该命令的录制态。
    StartRecording(Command),
    /// 录制中捕获到和弦（由全局按键分发送入）。
    RecordChord(Chord),
    /// 复位单条命令到默认绑定。
    Reset(Command),
    /// 全部恢复默认绑定。
    ResetAll,
    /// 冲突时「重新分配」：目标和弦抢占、原占用者解绑。
    Reassign(Command),
}

/// 模块上行事件：当前为空。
///
/// 弹窗开关自包含，无需父层配合；保留空枚举以对齐「模块上行事件」范式（同 `super::panes`）。
#[derive(Clone)]
pub enum Event {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 无覆盖、不触盘的干净状态。
    fn state() -> State {
        State {
            show_shortcuts: false,
            filter: String::new(),
            keymap: State::build_keymap(&BTreeMap::new()),
            overrides: BTreeMap::new(),
            store: None,
            recording: None,
            notice: None,
        }
    }

    /// 构造一个和弦（省去逐字段书写；`logo` 恒为假）。
    fn chord(ctrl: bool, shift: bool, key: ChordKey) -> Chord {
        Chord {
            ctrl,
            shift,
            alt: false,
            logo: false,
            key,
        }
    }

    /// 录制一个和弦到指定命令（进入录制态并送入和弦）。
    fn record(s: &mut State, command: Command, c: Chord) {
        let _ = s.update(Message::StartRecording(command));
        let _ = s.update(Message::RecordChord(c));
    }

    #[test]
    fn recording_binds_a_new_chord() {
        let mut s = state();
        let new = chord(true, true, ChordKey::Char('Y'));
        record(&mut s, Command::SftpRename, new);
        assert_eq!(s.chord_for(Command::SftpRename), Some(&new));
        assert!(s.is_overridden(Command::SftpRename));
        assert!(!s.is_recording());
    }

    #[test]
    fn conflict_reports_occupant_then_reassign_steals() {
        let mut s = state();
        let taken = chord(true, false, ChordKey::Char('G'));
        record(&mut s, Command::OpenSettings, taken);
        assert_eq!(s.command_for(&taken), Some(Command::OpenSettings));

        record(&mut s, Command::ToggleSidebar, taken);
        assert_eq!(
            s.notice_for(Command::ToggleSidebar),
            Some(RecordingNotice::Conflict {
                occupant: Command::OpenSettings,
                chord: taken,
            })
        );
        assert_eq!(s.chord_for(Command::ToggleSidebar), None);

        let _ = s.update(Message::Reassign(Command::ToggleSidebar));
        assert_eq!(s.chord_for(Command::ToggleSidebar), Some(&taken));
        assert_eq!(s.command_for(&taken), Some(Command::ToggleSidebar));
        assert_eq!(s.chord_for(Command::OpenSettings), None);
        assert_eq!(s.notice_for(Command::ToggleSidebar), None);
    }

    #[test]
    fn bare_key_is_rejected_with_notice() {
        let mut s = state();
        record(
            &mut s,
            Command::ToggleSidebar,
            chord(false, false, ChordKey::Char('A')),
        );
        assert_eq!(
            s.notice_for(Command::ToggleSidebar),
            Some(RecordingNotice::NeedsModifier)
        );
        assert_eq!(s.chord_for(Command::ToggleSidebar), None);
    }

    #[test]
    fn bare_function_key_is_allowed() {
        let mut s = state();
        let f5 = chord(false, false, ChordKey::F(5));
        record(&mut s, Command::ToggleSidebar, f5);
        assert_eq!(s.chord_for(Command::ToggleSidebar), Some(&f5));
        assert_eq!(s.notice_for(Command::ToggleSidebar), None);
    }

    #[test]
    fn backspace_unbinds_a_command() {
        let mut s = state();
        assert!(s.chord_for(Command::SftpRename).is_some());
        record(
            &mut s,
            Command::SftpRename,
            chord(false, false, ChordKey::Backspace),
        );
        assert_eq!(s.chord_for(Command::SftpRename), None);
        assert!(s.is_overridden(Command::SftpRename));
    }

    #[test]
    fn escape_cancels_recording() {
        let mut s = state();
        let before = s.chord_for(Command::OpenSearch).copied();
        record(
            &mut s,
            Command::OpenSearch,
            chord(false, false, ChordKey::Escape),
        );
        assert!(!s.is_recording());
        assert_eq!(s.chord_for(Command::OpenSearch), before.as_ref());
        assert!(!s.is_overridden(Command::OpenSearch));
    }

    #[test]
    fn reset_restores_a_single_command() {
        let mut s = state();
        record(
            &mut s,
            Command::SftpRename,
            chord(true, false, ChordKey::Char('K')),
        );
        assert!(s.is_overridden(Command::SftpRename));

        let _ = s.update(Message::Reset(Command::SftpRename));
        assert!(!s.is_overridden(Command::SftpRename));
        assert_eq!(
            s.chord_for(Command::SftpRename),
            Command::SftpRename.default_chord().as_ref()
        );
    }

    #[test]
    fn reset_all_clears_every_override() {
        let mut s = state();
        record(
            &mut s,
            Command::OpenSettings,
            chord(true, false, ChordKey::Char('G')),
        );
        record(
            &mut s,
            Command::SftpRename,
            chord(true, false, ChordKey::Char('K')),
        );
        assert!(s.is_overridden(Command::OpenSettings));
        assert!(s.is_overridden(Command::SftpRename));

        let _ = s.update(Message::ResetAll);
        assert!(!s.is_overridden(Command::OpenSettings));
        assert!(!s.is_overridden(Command::SftpRename));
        assert_eq!(
            s.chord_for(Command::OpenSearch),
            Command::OpenSearch.default_chord().as_ref()
        );
    }

    #[test]
    fn recorded_binding_survives_a_restart() {
        let _guard = crate::app::STATE_ROOT_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("rterm_shortcuts_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("应能创建临时目录");
        rterm_config::paths::set_test_root(Some(dir.clone()));

        let new = chord(true, true, ChordKey::Char('Y'));
        {
            let mut s = State::new();
            assert_eq!(
                s.chord_for(Command::SftpRename),
                Command::SftpRename.default_chord().as_ref()
            );
            record(&mut s, Command::SftpRename, new);
        }

        // 新实例模拟重启：应从 keybindings.toml 读回覆盖。
        let reopened = State::new();
        rterm_config::paths::set_test_root(None);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(reopened.chord_for(Command::SftpRename), Some(&new));
        assert!(reopened.is_overridden(Command::SftpRename));
    }
}
