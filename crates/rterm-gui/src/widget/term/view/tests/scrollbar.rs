//! 滚动条几何：轨道与滑块位置、拖拽映射及锁存收尾。

use super::common::*;
use crate::widget::term::backend::RenderableContent;
use crate::widget::term::view::TerminalViewState;
use crate::widget::term::view::scrollbar::{
    SCROLLBAR_INSET, SCROLLBAR_MIN_THUMB, SCROLLBAR_WIDTH, scrollbar_drag_step,
    scrollbar_drag_target, scrollbar_geometry, scrollbar_releases_drag,
};
use alacritty_terminal::term::TermMode;
use iced::{Point, Rectangle, Size};

/// 轨道贴住内容右缘、上下各内缩 `padding + SCROLLBAR_INSET`。
#[test]
fn track_hugs_the_right_content_edge() {
    let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));
    let (track, _thumb) =
        scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 50, 0)).unwrap();

    assert_eq!(track.x, 10.0 + 400.0 - TEST_PADDING - SCROLLBAR_WIDTH);
    assert_eq!(track.y, 20.0 + TEST_PADDING + SCROLLBAR_INSET);
    assert_eq!(track.width, SCROLLBAR_WIDTH);
    assert_eq!(track.height, 300.0 - (TEST_PADDING + SCROLLBAR_INSET) * 2.0);
}

/// 贴住实时输出（偏移 0）时滑块在轨道最下；滚到历史顶端时在最上。
#[test]
fn thumb_travels_between_track_ends() {
    let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));

    let (_track, bottom) =
        scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 50, 0)).unwrap();
    assert_eq!(bottom.y + bottom.height, 26.0 + 288.0);

    let (_track, top) =
        scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 50, 50)).unwrap();
    assert_eq!(top.y, 26.0);
}

/// 历史越长滑块越短，但不低于 [`SCROLLBAR_MIN_THUMB`]。
#[test]
fn thumb_shrinks_with_history_but_keeps_a_grabable_minimum() {
    let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));

    // 视口 50 行、历史 50 行 ⇒ 占总行数一半。
    let (_track, half) =
        scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 50, 0)).unwrap();
    assert_eq!(half.height, 288.0 / 2.0);

    // 历史极长时钳到最小高度，否则滑块细到抓不住。
    let (_track, tiny) =
        scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 100_000, 0)).unwrap();
    assert_eq!(tiny.height, SCROLLBAR_MIN_THUMB);
}

/// 无历史 / 备用屏 / 视口为空时不画滚动条。
#[test]
fn no_geometry_without_scrollback_or_on_the_alternate_screen() {
    let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));

    assert!(scrollbar_geometry(bounds, TEST_PADDING, &content_with(50, 80, 0, 0)).is_none());
    assert!(scrollbar_geometry(bounds, TEST_PADDING, &content_with(0, 80, 50, 0)).is_none());

    let alt_screen = RenderableContent {
        terminal_mode: TermMode::ALT_SCREEN,
        ..content_with(50, 80, 50, 0)
    };
    assert!(scrollbar_geometry(bounds, TEST_PADDING, &alt_screen).is_none());
}

/// 默认终端模式（含 [`TermMode::ALTERNATE_SCROLL`]）仍须显示滚动条。
///
/// 该位是 alacritty 默认就置位的修饰位而非「备用屏」信号，曾据它排除致普通会话无滚动条。
#[test]
fn default_term_mode_still_shows_scrollbar() {
    let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));
    assert!(TermMode::default().contains(TermMode::ALTERNATE_SCROLL));

    let content = RenderableContent {
        terminal_mode: TermMode::default(),
        ..content_with(50, 80, 50, 0)
    };
    assert!(scrollbar_geometry(bounds, TEST_PADDING, &content).is_some());
}

/// 拖动映射：抓取点跟手（端点与中点），越界钳到两端。
#[test]
fn drag_target_maps_cursor_to_scroll_offset() {
    let track = Rectangle::new(Point::new(400.0, 26.0), Size::new(6.0, 288.0));
    let thumb_height = 144.0;
    let span = 288.0 - thumb_height;

    // 抓在滑块顶部拖到轨道顶 ⇒ 完全回滚（偏移 = 历史长度）。
    assert_eq!(
        scrollbar_drag_target(track, thumb_height, 50, track.y, 0.0),
        50
    );
    // 拖到轨道底 ⇒ 贴住实时输出。
    assert_eq!(
        scrollbar_drag_target(track, thumb_height, 50, track.y + span, 0.0),
        0
    );
    // 中点 ⇒ 一半。
    assert_eq!(
        scrollbar_drag_target(track, thumb_height, 50, track.y + span / 2.0, 0.0),
        25
    );
    // 抓取偏移抵消光标位移：光标相对滑块顶部的落点未变则偏移不变。
    assert_eq!(
        scrollbar_drag_target(track, thumb_height, 50, track.y + 72.0, 72.0),
        50
    );
    // 拖出轨道两端钳到极限。
    assert_eq!(
        scrollbar_drag_target(track, thumb_height, 50, track.y - 500.0, 0.0),
        50
    );
    assert_eq!(
        scrollbar_drag_target(track, thumb_height, 50, track.y + 5000.0, 0.0),
        0
    );
}

/// 一步增量以「上次已下达的偏移」为基准，故一批多次移动的增量之和恰为最终目标。
///
/// 若改用晚一轮才更新的后端 `display_offset` 作基准，同批增量叠加会冲过目标再回弹——即抖动成因。
#[test]
fn drag_step_telescopes_across_a_batch() {
    let seed = 12usize;
    let mut tracked = seed;
    let mut applied = 0i32;

    for target in [30usize, 8, 45, 45] {
        let (delta, next) = scrollbar_drag_step(target, tracked);
        applied += delta;
        tracked = next;
    }

    // 总和即最终目标相对种子的位移，中途的来回都被抵消。
    assert_eq!(applied, 45 - seed as i32);
    assert_eq!(tracked, 45);
    // 目标与基准相同则不发命令。
    assert_eq!(scrollbar_drag_step(45, 45), (0, 45));
}

/// 仅「拖拽中 + 左键释放」解除锁存；未拖拽的释放留给选区 / 鼠标上报。
#[test]
fn only_left_release_while_dragging_ends_the_drag() {
    let release = iced_core::mouse::Event::ButtonReleased(iced_core::mouse::Button::Left);
    assert!(scrollbar_releases_drag(&release, true));
    assert!(!scrollbar_releases_drag(&release, false));

    let moved = iced_core::mouse::Event::CursorMoved {
        position: Point::ORIGIN,
    };
    assert!(!scrollbar_releases_drag(&moved, true));
}

/// 离窗 / 失焦收尾：清空拖拽与悬停锁存并报告需要重绘；全空时不动重绘。
#[test]
fn ending_pointer_interactions_clears_latches() {
    let mut state = TerminalViewState {
        scrollbar_drag: Some(3.0),
        is_dragged: true,
        scrollbar_hovered: true,
        ..TerminalViewState::new()
    };
    assert!(state.end_pointer_interactions());
    assert!(state.scrollbar_drag.is_none());
    assert!(!state.is_dragged);
    assert!(!state.scrollbar_hovered);

    // 已无任何锁存 / 悬停：不要求重绘。
    assert!(!state.end_pointer_interactions());
}
