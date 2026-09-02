//! 窗口两栏布局模块

use iced::Task;
use iced::widget::pane_grid;

/// 初始 / 默认中心（左）栏像素宽度。
const INITIAL_LEFT_WIDTH: f32 = 320.0;
/// 初始窗口宽度（与 iced 窗口默认宽度 1144 一致），用于换算初始比例。
const INITIAL_WINDOW_WIDTH: f32 = 1144.0;

/// 模块状态：两栏 `pane_grid` 的布局态。
pub struct State {
    /// 中心（左）栏固定像素宽度：拖拽分隔条时记录，窗口缩放时据此反算 `pane_grid` 比例。
    pub left_pane_width: f32,
    /// 中心 ↔ 右侧终端区 `pane_grid` 状态（原生提供拖拽与悬停高亮）。
    pub pane_grid_state: pane_grid::State<()>,
    /// 中心面板所在的 `pane` 标识。
    pub center_pane: pane_grid::Pane,
    /// 右侧终端区所在的 `pane` 标识。
    pub right_pane: pane_grid::Pane,
    /// 两栏之间的分隔条标识（用于窗口缩放时重设比例）。
    pub split: pane_grid::Split,
    /// 当前窗口宽度（像素），用于按固定左宽反算比例。
    pub window_width: f32,
}

impl State {
    /// 构建初始两栏布局：中心面板（左）+ 终端区（右），左栏为固定像素宽度。
    pub fn new() -> Self {
        let left_pane_width = INITIAL_LEFT_WIDTH;
        let window_width = INITIAL_WINDOW_WIDTH;
        let (mut pane_grid_state, center_pane) = pane_grid::State::new(());
        let (right_pane, split) = pane_grid_state
            .split(pane_grid::Axis::Vertical, center_pane, ())
            .expect("初始 split 必然成功");
        // 比例按「左栏像素宽 / 可用宽度」换算：可用宽度需扣掉固定宽度的活动栏。
        let total = window_width - crate::theme::ACTIVITY_BAR_WIDTH;
        pane_grid_state.resize(split, (left_pane_width / total).clamp(0.1, 0.9));
        Self {
            left_pane_width,
            pane_grid_state,
            center_pane,
            right_pane,
            split,
            window_width,
        }
    }

    /// 模块更新：只改自身 `State`；布局比例自包含，无需上行任何事件。
    pub fn update(&mut self, msg: Message) -> Task<Event> {
        match msg {
            // 用户拖拽分隔条：按新比例换算并记录左栏像素宽度（受区间约束）。
            Message::Resized(event) => {
                self.pane_grid_state.resize(event.split, event.ratio);
                let total = self.window_width - crate::theme::ACTIVITY_BAR_WIDTH;
                self.left_pane_width = (event.ratio * total).clamp(200.0, 800.0);
                Task::none()
            }
            // 窗口缩放：按已记录的左栏像素宽反算比例，使左栏宽度保持恒定。
            Message::WindowResized(width) => {
                // 宽度无实质变化（部分平台在焦点切换时也发 resize）时忽略，避免无谓重算。
                if (width - self.window_width).abs() < 1.0 {
                    return Task::none();
                }
                self.window_width = width;
                let total = width - crate::theme::ACTIVITY_BAR_WIDTH;
                let ratio = (self.left_pane_width / total).clamp(0.1, 0.9);
                self.pane_grid_state.resize(self.split, ratio);
                Task::none()
            }
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

/// 模块内部消息：两栏比例的变更意图。
///
/// 由父层经 `Message::Panes` 路由进来；模块 `update` 自行消费，不外泄。
#[derive(Clone)]
pub enum Message {
    /// 用户拖拽分隔条改变两栏比例。
    Resized(pane_grid::ResizeEvent),
    /// 窗口宽度变化（携带新宽度，用于按固定左宽重算比例）。
    WindowResized(f32),
}

/// 模块上行事件：当前为空。
///
/// 布局比例完全自包含（只影响本模块自己的 pane 几何），无需父层配合，故暂无需上行事件。
/// 保留空枚举以对齐「模块上行事件」范式（同 `super::hostkey`）。
#[derive(Clone)]
pub enum Event {}
