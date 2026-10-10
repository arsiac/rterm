use super::*;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::viewport_to_point;
use alacritty_terminal::vte::ansi::Processor;
use iced_core::Size;

/// 构造无 PTY 的终端：历史缓冲 10 行，视口取 [`TerminalSize::default`]（80×50）。
fn test_term() -> Term<EventProxy> {
    let (tx, _rx) = mpsc::unbounded_channel();
    let config = term::Config {
        scrolling_history: 10,
        ..Default::default()
    };
    Term::new(config, &TerminalSize::default(), EventProxy(tx))
}

/// 把字节流喂给终端解析（不涉及 PTY，仅驱动 vt 状态机）。
fn feed(term: &mut Term<EventProxy>, input: &[u8]) {
    let mut parser: Processor = Processor::new();
    parser.advance(term, input);
}

/// 写入字节并捕获一次视口快照。
fn snapshot_after(term: &mut Term<EventProxy>, input: &[u8]) -> RenderableContent {
    feed(term, input);
    let mut content = RenderableContent::default();
    Backend::capture_viewport(&mut content, term, TerminalSize::default());
    content
}

#[test]
fn viewport_snapshot_covers_visible_area_only() {
    let mut term = test_term();
    let content = snapshot_after(&mut term, b"abc\r\ndefg");
    let size = TerminalSize::default();

    assert_eq!(content.columns, size.columns());
    assert_eq!(content.cells.len(), size.screen_lines() * size.columns());
    assert_eq!(content.display_offset, 0);
}

/// 历史行数须取自网格：`TerminalSize` 的 `total_lines()` 是视口行数，其 `history_size()`
/// 恒为 0，照它画滚动条会永远没有可滚动区间。
#[test]
fn viewport_snapshot_reports_scrollback_history_size() {
    // 未溢出：无历史可回滚。
    let mut term = test_term();
    let content = snapshot_after(&mut term, b"abc");
    assert_eq!(content.history_size, 0);
    assert_eq!(TerminalSize::default().history_size(), 0);

    // 写满一屏并溢出：历史被 `scrolling_history = 10` 封顶。
    let input: String = (0..60).map(|i| format!("L{i}\r\n")).collect();
    let content = snapshot_after(&mut term, input.as_bytes());
    assert_eq!(content.history_size, 10);

    // 回滚只改偏移，不改历史总量。
    term.grid_mut().scroll_display(Scroll::Delta(3));
    let mut content = RenderableContent::default();
    Backend::capture_viewport(&mut content, &mut term, TerminalSize::default());
    assert_eq!(content.history_size, 10);
    assert_eq!(content.display_offset, 3);
}

#[test]
fn viewport_snapshot_maps_row_major_from_top_visible_line() {
    let mut term = test_term();
    let content = snapshot_after(&mut term, b"abc\r\ndefg");
    let columns = content.columns;

    assert_eq!(content.cells[0].c, 'a');
    assert_eq!(content.cells[2].c, 'c');
    assert_eq!(content.cells[columns].c, 'd');
    assert_eq!(content.cells[columns + 3].c, 'g');
    assert_eq!(content.cursor_point, Point::new(Line(1), Column(4)));
}

#[test]
fn viewport_snapshot_matches_alacritty_viewport_mapping() {
    let mut term = test_term();
    // 写满一屏并溢出一部分，再回滚 5 行，覆盖 display_offset != 0 的历史行。
    let input: String = (0..60).map(|i| format!("L{i}\r\n")).collect();
    feed(&mut term, input.as_bytes());
    term.grid_mut().scroll_display(Scroll::Delta(5));

    let mut content = RenderableContent::default();
    Backend::capture_viewport(&mut content, &mut term, TerminalSize::default());

    assert_eq!(content.display_offset, 5);
    let size = TerminalSize::default();
    let columns = content.columns;
    assert_eq!(columns, size.columns());
    assert_eq!(content.cells.len(), size.screen_lines() * columns);

    // 以 alacritty 自身的「屏幕点 → 网格点」换算为对照，逐格核对下标映射与滚动偏移。
    let grid = term.grid();
    for row in 0..size.screen_lines() {
        for col in 0..columns {
            let point = viewport_to_point(5, Point::new(row, Column(col)));
            assert_eq!(
                content.cells[row * columns + col],
                grid[point],
                "row {row} col {col}"
            );
        }
    }
}

/// 响铃必须显式映射为 `Action::Bell`，不能落进兜底分支被静默吞掉。
#[test]
fn bell_event_maps_to_a_bell_action() {
    assert_eq!(action_for_event(Event::Bell), Action::Bell);
    assert_eq!(
        action_for_event(Event::Title("t".into())),
        Action::ChangeTitle("t".to_string())
    );
    // 无宿主响应义务的事件仍须静默。
    assert_eq!(action_for_event(Event::Wakeup), Action::Ignore);
}

/// 端到端钉住「BEL 字节到达解析层就会产生 `Event::Bell`」这一前提：
/// 喂真实字节流（而非直接构造事件），上游 alacritty 若改了行为这里有红灯。
#[test]
fn a_bel_byte_in_the_stream_emits_a_bell_event() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut term = Term::new(
        term::Config::default(),
        &TerminalSize::default(),
        EventProxy(tx),
    );
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, b"\x07");

    let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        events.iter().any(|e| matches!(e, Event::Bell)),
        "\\x07 应产生 Bell 事件，实际：{events:?}"
    );
}

/// `Event::Exit`（pty 子进程事件使 `Term::exit()` 发出）必须映射为 `Action::Shutdown`：
/// GUI 侧据此识别「终端自身收尾」并明确忽略，绝不当作关标签信号
/// （见 `app::terminal_bridge::handle_terminal_event` 的分流注释）。
#[test]
fn exit_event_maps_to_a_shutdown_action() {
    assert_eq!(action_for_event(Event::Exit), Action::Shutdown);
}

// 断线重连（reattach）用例：真实 `RusshPty` 挂在 socketpair 上模拟桥接（命名管道不便在
// 测试里搭，故 `cfg(unix)`），测试线程代替 pump 喂输出，断言直接落在锁内的 `Term` 上。

/// 一条模拟桥接的测试装具。
#[cfg(unix)]
struct Fixture {
    /// 挂着事件循环的后端（其 `term` 供断言直接查看）。
    backend: Backend,
    /// 模拟桥接异步端的写端：测试写进去的内容即「远端 shell 输出」。
    remote: std::fs::File,
    /// 桥接结束状态（`request_stop` 驱动结束观察线程，见 `russh_pty`）。
    bridge: Arc<rterm_core::BridgeState>,
    /// 尺寸变更接收端（pty 转发 window-change 的去向）。
    resizes: mpsc::Receiver<(u32, u32)>,
    /// 后端事件通道的接收端（宿主订阅的原型）。
    events: mpsc::UnboundedReceiver<Event>,
}

#[cfg(unix)]
impl Fixture {
    /// 建装具；`scrollback` 为历史缓冲行数。
    fn new(scrollback: usize) -> Self {
        let (local, remote) = socket_pair();
        let bridge = Arc::new(rterm_core::BridgeState::new());
        let (resize_tx, resizes) = mpsc::channel(8);
        let pty = RusshPty::new(
            local.try_clone().expect("clone the sync end"),
            local,
            bridge.clone(),
            resize_tx,
        )
        .expect("wrap the sync end as a pty");
        let (event_tx, events) = mpsc::unbounded_channel();
        let backend =
            Backend::new_with_pty(1, event_tx, pty, scrollback, true).expect("create the backend");
        Self {
            backend,
            remote,
            bridge,
            resizes,
            events,
        }
    }
}

/// 建一对非阻塞 socketpair 并转成 `File`，返回 `(近端, 远端)`。
/// 近端交给 pty（事件循环侧），远端由测试持作桥接异步端的替身。
#[cfg(unix)]
fn socket_pair() -> (std::fs::File, std::fs::File) {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    let (near, far) = UnixStream::pair().expect("create a socketpair");
    near.set_nonblocking(true)
        .expect("set the near end non-blocking");
    far.set_nonblocking(true)
        .expect("set the far end non-blocking");
    (
        std::fs::File::from(OwnedFd::from(near)),
        std::fs::File::from(OwnedFd::from(far)),
    )
}

/// 再造一条模拟桥接供 reattach 换接，返回 `(pty, 远端写端, 桥接状态, 尺寸接收端)`。
#[cfg(unix)]
fn extra_bridge() -> (
    RusshPty,
    std::fs::File,
    Arc<rterm_core::BridgeState>,
    mpsc::Receiver<(u32, u32)>,
) {
    let (local, remote) = socket_pair();
    let bridge = Arc::new(rterm_core::BridgeState::new());
    let (resize_tx, resizes) = mpsc::channel(8);
    let pty = RusshPty::new(
        local.try_clone().expect("clone the sync end"),
        local,
        bridge.clone(),
        resize_tx,
    )
    .expect("wrap the sync end as a pty");
    (pty, remote, bridge, resizes)
}

/// 代替 pump 向「远端」写一段输出。
#[cfg(unix)]
fn feed_remote(remote: &mut std::fs::File, data: &[u8]) {
    use std::io::Write as _;
    remote.write_all(data).expect("write into the fake bridge");
}

/// 取整格文本（历史缓冲 + 视口），行间以换行分隔。
#[cfg(unix)]
fn grid_text(term: &Arc<FairMutex<Term<EventProxy>>>) -> String {
    let term = term.lock();
    let grid = term.grid();
    // `GridIterator` 先推进游标再产出（起点本身被跳过），故从「顶行上一行」的末列
    // 起步，使第一个产出的格子恰为历史缓冲首行首列。
    let start = Point::new(grid.topmost_line() - 1, grid.last_column());
    let mut text = String::new();
    let mut last_line = i32::MIN;
    for indexed in grid.iter_from(start) {
        if indexed.point.line.0 != last_line {
            if !text.is_empty() {
                text.push('\n');
            }
            last_line = indexed.point.line.0;
        }
        text.push(indexed.c);
    }
    text
}

/// 取视口文本（仅可见区，不含历史缓冲）。
#[cfg(unix)]
fn viewport_text(term: &Arc<FairMutex<Term<EventProxy>>>) -> String {
    term.lock()
        .grid()
        .display_iter()
        .map(|indexed| indexed.c)
        .collect()
}

/// 轮询等整格文本出现 `needle`（事件循环异步解析，需等待）；超时打印网格判失败。
#[cfg(unix)]
fn wait_grid_contains(term: &Arc<FairMutex<Term<EventProxy>>>, needle: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if grid_text(term).contains(needle) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!(
        "timed out waiting for {needle:?} in the terminal grid; current grid:\n{}",
        grid_text(term)
    );
}

/// 等一条满足条件的事件（其余事件跳过）；超时或通道关闭判失败。
#[cfg(unix)]
async fn wait_event(
    rx: &mut mpsc::UnboundedReceiver<Event>,
    pred: impl Fn(&Event) -> bool,
) -> Event {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let event = match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(event)) => event,
            Ok(None) => panic!("event channel closed while waiting"),
            Err(_) => panic!("timed out waiting for a matching event"),
        };
        if pred(&event) {
            return event;
        }
    }
}

/// 等一条 resize 通知；超时判失败。
#[cfg(unix)]
async fn wait_resize(rx: &mut mpsc::Receiver<(u32, u32)>) -> (u32, u32) {
    tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for a resize notification")
        .expect("resize channel closed")
}

/// 喂一段输出 → 断开 → reattach → 再喂——
/// 旧行留在历史缓冲、新输出被同一终端解析、旧循环在断开时自行收尾（发出 `Event::Exit`
/// 即证明它没有对着已死的管道空转挂死）。
#[cfg(unix)]
#[tokio::test]
async fn reattach_keeps_scrollback_and_parses_new_output() {
    let mut fx = Fixture::new(100);

    // 1) 喂出把标记行顶出视口、只留在历史缓冲的输出。
    let mut old = String::from("old-marker\r\n");
    old.push_str(&"filler-line\r\n".repeat(60));
    feed_remote(&mut fx.remote, old.as_bytes());
    wait_grid_contains(&fx.backend.term, "old-marker");
    assert!(
        !viewport_text(&fx.backend.term).contains("old-marker"),
        "前提不成立：标记行应已被顶出视口，仅存于历史缓冲"
    );

    // 2) 断开：远端管道关端（pump 退出后的形态）+ 停止请求（关标签路径）。
    //    停止请求唤醒 pty 的结束观察线程 → 事件循环走子进程事件分支收尾，
    //    并向宿主发送 `Event::Exit`。
    drop(fx.remote);
    fx.bridge.request_stop();
    let exit = wait_event(&mut fx.events, |e| matches!(e, Event::Exit)).await;
    assert!(matches!(exit, Event::Exit));

    // 3) reattach 到一条新桥接。
    let (pty, mut remote, bridge, _resizes) = extra_bridge();
    fx.backend.reattach(pty).expect("reattach must succeed");

    // 4) 新输出由同一个终端解析；旧行仍在（reattach 未重建 Term）。
    feed_remote(&mut remote, b"new-marker\r\n");
    wait_grid_contains(&fx.backend.term, "new-marker");
    assert!(
        grid_text(&fx.backend.term).contains("old-marker"),
        "reattach 后历史缓冲必须原样保留"
    );

    // 收尾：让新循环也走子进程事件退出，测试进程不残留观察线程。
    bridge.request_stop();
}

/// reattach 必须重置尺寸记忆：否则新通道的实际尺寸恰与旧值相同时，
/// 后续 Resize 会被去重吞掉，新 shell 将按错尺寸一直活着。
#[cfg(unix)]
#[tokio::test]
async fn reattach_resets_pty_size_bookkeeping() {
    let mut fx = Fixture::new(10);
    let layout = Size::new(800.0, 600.0);
    let font = Size::new(10.0, 20.0);

    // 首次下发：新尺寸（80 列 × 30 行）被转发到旧通道。
    fx.backend.handle(Command::Resize(Some(layout), Some(font)));
    assert_eq!(wait_resize(&mut fx.resizes).await, (80, 30));

    // 同尺寸重复下发：被记忆去重，不再转发。
    fx.backend.handle(Command::Resize(Some(layout), Some(font)));
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(
        fx.resizes.try_recv().is_err(),
        "尺寸未变化时不应重复通知远端"
    );

    // 换接新 pty 后，同尺寸必须重新通知（记忆已重置）。
    let (pty, _remote, bridge, mut resizes) = extra_bridge();
    fx.backend.reattach(pty).expect("reattach must succeed");
    fx.backend.handle(Command::Resize(Some(layout), Some(font)));
    assert_eq!(wait_resize(&mut resizes).await, (80, 30));

    bridge.request_stop();
}

/// `grid_size` 反映最后一次布局折算出的行列数——重连按它给定新 PTY 的开局尺寸。
#[cfg(unix)]
#[tokio::test]
async fn grid_size_reflects_the_last_layout() {
    let mut fx = Fixture::new(10);
    // 尚未收到布局：取默认尺寸（80 列 × 50 行）。
    assert_eq!(fx.backend.grid_size(), (80, 50));

    // 布局 800×600 像素、单元格 10×20 像素 ⇒ 80 列 × 30 行。
    fx.backend.handle(Command::Resize(
        Some(Size::new(800.0, 600.0)),
        Some(Size::new(10.0, 20.0)),
    ));
    assert_eq!(fx.backend.grid_size(), (80, 30));

    // 收尾：让事件循环走子进程事件退出（同其它用例）。
    fx.bridge.request_stop();
}

/// 「视口回到底部」：翻回历史后回到底部、贴住实时输出。
#[cfg(unix)]
#[test]
fn scroll_to_bottom_resets_a_scrolled_viewport() {
    let mut fx = Fixture::new(100);
    let mut output = String::new();
    for i in 0..80 {
        output.push_str(&format!("line-{i:02}\r\n"));
    }
    feed_remote(&mut fx.remote, output.as_bytes());
    wait_grid_contains(&fx.backend.term, "line-79");

    fx.backend
        .term
        .lock()
        .grid_mut()
        .scroll_display(Scroll::Delta(10));
    fx.backend.sync();
    assert_eq!(
        fx.backend.renderable_content().display_offset,
        10,
        "前提：视口已翻回历史"
    );

    fx.backend.scroll_to_bottom();
    fx.backend.sync();
    assert_eq!(
        fx.backend.renderable_content().display_offset,
        0,
        "视口应回到底部、贴住实时输出"
    );

    fx.bridge.request_stop();
}

// 复制选区（`selectable_content`）用例

/// 建一个简单拖拽选区：起点 → 终点（含两端所在格）。
fn select_simple(term: &mut Term<EventProxy>, start: Point, end: Point) {
    term.selection = Some(Selection::new(SelectionType::Simple, start, Side::Left));
    term.selection
        .as_mut()
        .expect("selection just set")
        .update(end, Side::Right);
}

/// 复制含 CJK 的行：宽字符占位格不得产出空格（「会话」不能被复制成「会 话」）。
#[test]
fn selection_text_skips_wide_char_spacers() {
    let mut term = test_term();
    feed(&mut term, "会话 abc".as_bytes());
    select_simple(
        &mut term,
        Point::new(Line(0), Column(0)),
        Point::new(Line(0), Column(7)),
    );

    assert_eq!(Backend::selection_text(&term, false), "会话 abc");
}

/// 选区起点落在宽字符右半的占位格上时，整字一并纳入（对齐上游起始列修正）。
#[test]
fn selection_text_backs_up_when_starting_on_a_spacer() {
    let mut term = test_term();
    feed(&mut term, "会话 abc".as_bytes());
    // Column(1) 是「会」的占位格。
    select_simple(
        &mut term,
        Point::new(Line(0), Column(1)),
        Point::new(Line(0), Column(7)),
    );

    assert_eq!(Backend::selection_text(&term, false), "会话 abc");
}

/// 组合字符（zerowidth）随主字符一并复制，不丢附加码点。
#[test]
fn selection_text_keeps_zerowidth_characters() {
    let mut term = test_term();
    feed(&mut term, "e\u{301}x".as_bytes());
    select_simple(
        &mut term,
        Point::new(Line(0), Column(0)),
        Point::new(Line(0), Column(1)),
    );

    assert_eq!(Backend::selection_text(&term, false), "e\u{301}x");
}

/// 选区落在视口之外的历史行同样完整复制（逐格取自网格，不受滚动位置影响）。
#[test]
fn selection_text_covers_history_outside_the_viewport() {
    let mut term = test_term();
    // 视口 50 行：写 51 行后上滚 2 行，L0/L1 进入历史缓冲。
    let input: String = (0..51).map(|i| format!("L{i}\r\n")).collect();
    feed(&mut term, input.as_bytes());
    assert_eq!(
        term.grid().topmost_line(),
        Line(-2),
        "前提：L0/L1 已被顶入历史缓冲，当前视口自 L2 起"
    );

    select_simple(
        &mut term,
        Point::new(Line(-2), Column(0)),
        Point::new(Line(-1), Column(1)),
    );

    assert_eq!(Backend::selection_text(&term, true), "L0\nL1");
}
