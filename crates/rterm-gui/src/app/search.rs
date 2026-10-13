//! 终端内搜索（Ctrl+Shift+F）的 UI 态与搜索条。
//!
//! 模块持有搜索条的开关、查询串、两个匹配开关与计数文案；PTY 网格的搜索执行在
//! 终端后端锁内完成：意图经 [`Event::Run`] 上行，父层对绑定的目标标签下发
//! [`BackendCommand::Search*`] 并把回带的 [`SearchOutcome`] 经
//! [`Message::SetOutcome`] 自回路写回本模块。快捷键（Ctrl+Shift+F）由终端部件
//! 拦截后经 [`Message::Search`] 进入，右键菜单「查找」项同样落到 [`Message::Open`]。

use crate::icons::{Icon, icon_button};
use crate::t;
use crate::widget::term::{BackendCommand, SearchFlags, SearchOutcome};
use iced::alignment::Vertical;
use iced::widget::tooltip::Position;
use iced::widget::{button, row, text, text_input, tooltip};
use iced::{Element, Length, Task};

/// 搜索条图标按钮的像素尺寸（与标签栏图标同档）。
const BTN_ICON_SIZE: f32 = 14.0;

/// 终端内搜索模块状态。
pub struct State {
    /// 搜索条是否打开。
    open: bool,
    /// 搜索绑定的标签 id（打开时确定；后端命令只下发给它）。
    target: Option<u64>,
    /// 当前查询串。
    query: String,
    /// 大小写 / 正则开关（不持久化，关条即复位）。
    flags: SearchFlags,
    /// 最近一次后端执行结果（`None` = 无计数可显示，如空查询）。
    outcome: Option<SearchOutcome>,
    /// 输入框部件 id（打开时父层据此抢键盘焦点）。
    input_id: iced::widget::Id,
}

impl State {
    /// 构造关闭态的搜索模块。
    pub fn new() -> Self {
        Self {
            open: false,
            target: None,
            query: String::new(),
            flags: SearchFlags::default(),
            outcome: None,
            input_id: iced::widget::Id::unique(),
        }
    }

    /// 搜索条是否打开（渲染与 Esc 链的判据）。
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// 搜索绑定的标签 id（父层下发后端命令的目标）。
    pub fn target(&self) -> Option<u64> {
        self.target
    }

    /// 输入框部件 id（父层执行聚焦命令时使用）。
    pub fn input_id(&self) -> &iced::widget::Id {
        &self.input_id
    }

    /// 关闭搜索条的全部 UI 态而不上行后端命令。
    ///
    /// 供标签销毁 / 终端重挂载路径调用：此时后端搜索态随标签作废或已被重连接管，
    /// 只需收起搜索条本身。
    pub fn reset(&mut self) {
        self.open = false;
        self.target = None;
        self.query.clear();
        self.flags = SearchFlags::default();
        self.outcome = None;
    }

    /// 关闭某标签：若它正是搜索绑定的标签则收起搜索条。
    pub fn remove_for_tab(&mut self, tab_id: u64) {
        if self.target == Some(tab_id) {
            self.reset();
        }
    }

    /// 模块更新：维护 UI 态并上行走后端命令 / 焦点意图，自身绝不修改 `App`。
    pub fn update(&mut self, msg: Message) -> Task<Event> {
        match msg {
            Message::Open { tab_id } => {
                // 重新触发只把焦点抢回输入框，已输入的查询保留。
                if self.open {
                    return Task::done(Event::FocusInput);
                }
                self.open = true;
                self.target = Some(tab_id);
                self.query.clear();
                self.flags = SearchFlags::default();
                self.outcome = None;
                Task::batch([
                    Task::done(Event::SetTerminalFocused(false)),
                    Task::done(Event::FocusInput),
                ])
            }
            Message::Close => {
                if !self.open {
                    return Task::none();
                }
                self.reset();
                // 关条同时把键盘焦点交还终端；父层落焦时自会处理「窗口失焦不写」的守卫。
                Task::batch([
                    Task::done(Event::Run(BackendCommand::SearchClose)),
                    Task::done(Event::SetTerminalFocused(true)),
                ])
            }
            Message::Query(query) => {
                self.query = query;
                // 逐键清空旧计数（无论文案随后是否回填，视觉上跟随输入）。
                self.outcome = None;
                Task::done(Event::Run(BackendCommand::SearchSetQuery(
                    self.query.clone(),
                    self.flags,
                )))
            }
            // 上下跳转来自全局按键监听（订阅不分焦点，其它输入框按下 Enter / 方向键
            // 同样会到达），故门控：关条时整体无操作，绝不下发后端命令（每条都有同步成本）。
            Message::Next if self.open => Task::done(Event::Run(BackendCommand::SearchStep(
                alacritty_terminal::index::Direction::Right,
            ))),
            Message::Previous if self.open => Task::done(Event::Run(BackendCommand::SearchStep(
                alacritty_terminal::index::Direction::Left,
            ))),
            Message::Next | Message::Previous => Task::none(),
            Message::ToggleCase => {
                self.flags.case_sensitive = !self.flags.case_sensitive;
                self.rerun()
            }
            Message::ToggleRegex => {
                self.flags.regex = !self.flags.regex;
                self.rerun()
            }
            Message::SetOutcome(outcome) => {
                self.outcome = Some(outcome);
                Task::none()
            }
        }
    }

    /// 开关切换后以当前查询重跑一次（空查询无需执行）。
    fn rerun(&mut self) -> Task<Event> {
        if self.query.is_empty() {
            return Task::none();
        }
        self.outcome = None;
        Task::done(Event::Run(BackendCommand::SearchSetQuery(
            self.query.clone(),
            self.flags,
        )))
    }
}

impl Default for State {
    /// 经由 `new` 生成关闭态默认值。
    fn default() -> Self {
        Self::new()
    }
}

/// 模块内部消息：搜索条的 UI 意图与后端结果自回路。
#[derive(Clone)]
pub enum Message {
    /// 打开搜索条（快捷键 / 右键菜单入口，携带目标标签 id；已打开时只抢回输入焦点）。
    Open {
        /// 搜索绑定的标签 id。
        tab_id: u64,
    },
    /// 关闭搜索条（Esc / 关闭按钮），清除高亮并交还终端焦点。
    Close,
    /// 输入框内容变更：即输即搜。
    Query(String),
    /// 下一个命中（Enter / ↓）。
    Next,
    /// 上一个命中（Shift+Enter / ↑）。
    Previous,
    /// 切换大小写匹配开关。
    ToggleCase,
    /// 切换正则搜索开关。
    ToggleRegex,
    /// 后端执行结果自回路（仅非空查询会到达）。
    SetOutcome(SearchOutcome),
}

/// 模块上行事件：由父层落地到终端后端与焦点态。
#[derive(Clone)]
pub enum Event {
    /// 在目标终端上执行搜索命令；父层回带 `Action::SearchStats` 时转
    /// [`Message::SetOutcome`] 自回路。
    Run(BackendCommand),
    /// 置终端键盘焦点（打开时让位输入框，关闭时交还终端）。
    SetTerminalFocused(bool),
    /// 聚焦搜索输入框。
    FocusInput,
}

/// 计数文案（纯决策）：`None` 表示无内容可显示（空查询 / 结果未回）。
pub(crate) fn outcome_label(outcome: Option<&SearchOutcome>) -> Option<String> {
    match outcome {
        None => None,
        Some(SearchOutcome::Invalid) => Some(t!("terminal.search.invalid_pattern").to_string()),
        Some(SearchOutcome::NoMatches) => Some(t!("terminal.search.no_results").to_string()),
        Some(SearchOutcome::Matches { count, capped }) => Some(if *capped {
            t!("terminal.search.more").to_string()
        } else {
            t!("terminal.search.matches", count = count)
        }),
    }
}

/// 渲染搜索条窄条：前置图标、输入框、两个 toggle 按钮、上下跳转、计数与关闭。
pub(crate) fn view(state: &State) -> Element<'_, Message> {
    let case_btn = toggle_button(
        Icon::TextChangeCase,
        t!("terminal.search.case_sensitive").to_string(),
        state.flags.case_sensitive,
        Message::ToggleCase,
    );
    let regex_btn = toggle_button(
        Icon::TextPeriodAsterisk,
        t!("terminal.search.regex").to_string(),
        state.flags.regex,
        Message::ToggleRegex,
    );

    let mut items: Vec<Element<'_, Message>> = vec![
        Icon::Search.svg(BTN_ICON_SIZE).into(),
        text_input(&t!("terminal.search.placeholder"), &state.query)
            .id(state.input_id.clone())
            .on_input(Message::Query)
            .size(13)
            .padding([4, 8])
            .width(Length::Fill)
            .style(crate::ui::search_input_style)
            .into(),
        case_btn,
        regex_btn,
        icon_button(
            Icon::ArrowUp,
            BTN_ICON_SIZE,
            t!("terminal.search.prev"),
            Message::Previous,
            Position::Bottom,
        ),
        icon_button(
            Icon::ArrowDown,
            BTN_ICON_SIZE,
            t!("terminal.search.next"),
            Message::Next,
            Position::Bottom,
        ),
    ];
    if let Some(label) = outcome_label(state.outcome.as_ref()) {
        items.push(crate::ui::hint_text(label));
    }
    items.push(icon_button(
        Icon::Dismiss,
        BTN_ICON_SIZE,
        t!("terminal.search.close"),
        Message::Close,
        Position::Bottom,
    ));

    row(items)
        .spacing(6)
        .align_y(Vertical::Center)
        .width(Length::Fill)
        .into()
}

/// 开关态的 toggle 图标按钮：按下档用强调底色（同标签栏下拉按钮的展开态），带 tooltip。
fn toggle_button<'a>(
    icon: Icon,
    label: String,
    active: bool,
    msg: Message,
) -> Element<'a, Message> {
    let btn = button(icon.svg(BTN_ICON_SIZE))
        .on_press(msg)
        .padding(4)
        .style(move |theme, status| crate::theme::icon_button_style(theme, status, active));
    tooltip(btn, text(label), Position::Bottom)
        .delay(iced::time::Duration::from_millis(
            crate::theme::TOOLTIP_DELAY_MS,
        ))
        .style(crate::theme::tooltip_style)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_label_maps_each_backend_outcome() {
        assert_eq!(outcome_label(None), None);
        assert_eq!(
            outcome_label(Some(&SearchOutcome::Invalid)),
            Some(t!("terminal.search.invalid_pattern").to_string())
        );
        assert_eq!(
            outcome_label(Some(&SearchOutcome::NoMatches)),
            Some(t!("terminal.search.no_results").to_string())
        );
        assert_eq!(
            outcome_label(Some(&SearchOutcome::Matches {
                count: 7,
                capped: false
            })),
            Some(t!("terminal.search.matches", count = 7))
        );
        assert_eq!(
            outcome_label(Some(&SearchOutcome::Matches {
                count: 999,
                capped: true
            })),
            Some(t!("terminal.search.more").to_string())
        );
    }

    #[test]
    fn open_resets_state_and_hands_focus_to_input() {
        let mut s = State::new();
        let _ = s.update(Message::Open { tab_id: 1 });
        let _ = s.update(Message::Query("stale".into()));
        s.reset();

        let _ = s.update(Message::Open { tab_id: 2 });
        assert!(s.is_open());
        assert_eq!(s.target(), Some(2));
        assert_eq!(s.flags, SearchFlags::default());
        assert_eq!(s.outcome, None);
    }

    #[test]
    fn toggles_keep_query_and_rerun_with_new_flags() {
        let mut s = State::new();
        let _ = s.update(Message::Open { tab_id: 1 });
        let _ = s.update(Message::Query("abc".into()));
        let _ = s.update(Message::ToggleCase);
        assert!(s.flags.case_sensitive);
        assert_eq!(s.query, "abc", "开关切换不清查询串");
        // 空查询时 toggle 不发后端命令。
        let _ = s.update(Message::Query(String::new()));
        assert!(s.query.is_empty());
    }

    #[test]
    fn close_resets_all_ui_state() {
        let mut s = State::new();
        let _ = s.update(Message::Open { tab_id: 3 });
        let _ = s.update(Message::Query("abc".into()));
        let _ = s.update(Message::Close);
        assert!(!s.is_open());
        assert_eq!(s.target(), None);
        assert!(s.query.is_empty());
        assert_eq!(s.outcome, None);
    }

    #[test]
    fn remove_for_tab_only_closes_its_own_target() {
        let mut s = State::new();
        let _ = s.update(Message::Open { tab_id: 5 });
        // 关别的标签不打扰当前搜索。
        s.remove_for_tab(6);
        assert!(s.is_open());
        s.remove_for_tab(5);
        assert!(!s.is_open());
    }
}
