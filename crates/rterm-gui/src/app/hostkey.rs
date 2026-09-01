//! 主机密钥确认弹窗模块（窗口级安全模态）。
//!
//! 自包含 [`State`] / [`Message`] / [`Event`]，符合「模块化消息处理」架构：
//! - [`State`] 持有待确认的主机密钥弹窗队列（原散落在 `App::host_key_prompts`）；
//!   决策发生在 SSH 握手中途（连接尚未建立），故不属于任何标签的 SFTP 视图，而是独立队列。
//! - [`Message`] 仅模块内部消费：入队请求 / 用户决定 / Esc 取消，由父层经
//!   `Message::HostKey` 路由进来。
//! - [`Event`] 当前为空：决定仅回复模块内部持有的 `HostKeyReply` 句柄并出队，不写任何父状态，
//!   故暂无需上行事件；保留空枚举以对齐「模块上行事件」范式，后续若需 toast / 审计等父层协作再补变体。
//!
//! 注意：本文件**不要** `use iced::Event`，以免与下方 `pub enum Event` 撞名。
use crate::state::HostKeyPromptState;
use iced::{Subscription, Task};
use rterm_core::{HostKeyPrompt, HostKeyReply};

/// 主机密钥确认弹窗模块私有状态：待确认队列（渲染与决策均取队首）。
#[derive(Default)]
pub struct State {
    /// 待确认的主机密钥弹窗（连接握手暂停期间挂起）。
    prompts: Vec<HostKeyPromptState>,
}

impl State {
    /// 构造空状态。
    pub fn new() -> Self {
        Self::default()
    }

    /// 取队列首项（只读），供渲染层展示当前弹窗。
    pub fn head(&self) -> Option<&HostKeyPromptState> {
        self.prompts.first()
    }

    /// 队列是否为空（供 Esc 优先级判定等）。
    pub fn is_empty(&self) -> bool {
        self.prompts.is_empty()
    }

    /// 关闭某标签时，按标签 id 清理其悬挂的待确认项——并先以「拒绝」回复握手，
    /// 否则弹窗会挂在队列里等待一个已被关闭的连接。
    pub fn remove_for_tab(&mut self, tab_id: u64) {
        self.prompts.retain(|p| {
            if p.tab_id == tab_id {
                p.reply.reply(false);
                false
            } else {
                true
            }
        });
    }

    /// 模块更新：只改自身 `State`；当前流程不写父态，故返回的 [`Event`] 任务恒为 `Task::none`。
    pub fn update(&mut self, msg: Message) -> Task<Event> {
        match msg {
            // 收到主机密钥确认请求：入队等待用户决定。
            Message::Prompt(tab_id, prompt, reply) => {
                self.prompts.push(HostKeyPromptState {
                    tab_id,
                    prompt,
                    reply,
                });
                Task::none()
            }
            // 用户对队首弹窗做出信任决定（窗口级模态，只能对队首操作）。
            Message::Decision(trust) => {
                if let Some(state) = self.prompts.first() {
                    state.reply.reply(trust);
                }
                self.prompts.remove(0);
                Task::none()
            }
            // Esc / 取消：等价于拒绝队首（与 `Decision(false)` 同效，语义上区分来源）。
            Message::Dismiss => {
                if let Some(state) = self.prompts.first() {
                    state.reply.reply(false);
                }
                self.prompts.remove(0);
                Task::none()
            }
        }
    }

    /// 订阅：当前为占位（无内部流式逻辑）。
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }
}

/// 模块内部消息：入队请求 / 用户决定 / Esc 取消。
///
/// 由父层经 `Message::HostKey` 路由进来；模块 `update` 自行消费，不外泄。
///
/// 仅 `Clone`（不 `Debug`：含 `HostKeyReply`，而 `HostKeyReply` 未实现 `Debug`）。
#[derive(Clone)]
// 变体统一带 `HostKey` 语义前缀：与父层 `Message::HostKey` 路由命名一致，属刻意约定，故抑制此 lint。
#[allow(clippy::enum_variant_names)]
pub enum Message {
    /// 收到主机密钥确认请求（标签 id + 密钥信息 + 决定句柄），入队等待用户决定。
    Prompt(u64, HostKeyPrompt, HostKeyReply),
    /// 用户对队首弹窗做出信任决定（`true` = 信任，`false` = 拒绝）。
    Decision(bool),
    /// Esc / 取消：拒绝队首弹窗。
    Dismiss,
}

/// 模块上行事件：当前为空。
///
/// 主机密钥模块完全自包含——决定仅回复模块内部持有的 `HostKeyReply` 句柄并出队，
/// 不写任何父状态，故暂无需上行事件。保留空枚举以对齐「模块上行事件」范式。
#[derive(Clone)]
pub enum Event {}

#[cfg(test)]
mod tests {
    use super::*;
    use rterm_core::{HostKeyPrompt, HostKeyReply};

    fn prompt() -> HostKeyPrompt {
        HostKeyPrompt {
            host: "example.com".into(),
            port: 22,
            key_type: "ssh-ed25519".into(),
            fingerprint: "aa:bb:cc".into(),
            mismatch: None,
        }
    }

    fn reply() -> HostKeyReply {
        // `HostKeyReply::new()` 为 `rterm_core` 公开构造器，便于测试模拟用户决定。
        HostKeyReply::new()
    }

    #[test]
    fn new_state_is_empty() {
        let s = State::new();
        assert!(s.is_empty());
        assert!(s.head().is_none());
    }

    #[test]
    fn prompt_enqueues_and_head_is_first() {
        let mut s = State::new();
        let _ = s.update(Message::Prompt(1, prompt(), reply()));
        assert!(!s.is_empty());
        let h = s.head().expect("应有队首");
        assert_eq!(h.tab_id, 1);
        assert_eq!(h.prompt.host, "example.com");
    }

    #[test]
    fn decision_pops_only_head() {
        let mut s = State::new();
        let _ = s.update(Message::Prompt(1, prompt(), reply()));
        let _ = s.update(Message::Prompt(2, prompt(), reply()));
        // 决策只弹出队首（tab 1），队首变为 tab 2。
        let _ = s.update(Message::Decision(true));
        assert_eq!(s.head().expect("剩一个").tab_id, 2);
        // 再次决策弹出最后一个。
        let _ = s.update(Message::Decision(false));
        assert!(s.is_empty());
    }

    #[test]
    fn dismiss_pops_head() {
        let mut s = State::new();
        let _ = s.update(Message::Prompt(7, prompt(), reply()));
        let _ = s.update(Message::Dismiss);
        assert!(s.is_empty());
        assert!(s.head().is_none());
    }

    #[test]
    fn remove_for_tab_clears_only_matching() {
        let mut s = State::new();
        let _ = s.update(Message::Prompt(1, prompt(), reply()));
        let _ = s.update(Message::Prompt(2, prompt(), reply()));
        let _ = s.update(Message::Prompt(1, prompt(), reply()));
        // 关闭 tab 1 只清其两项，剩 tab 2。
        s.remove_for_tab(1);
        assert_eq!(s.head().expect("剩 tab 2").tab_id, 2);
        assert!(!s.is_empty());
    }
}
