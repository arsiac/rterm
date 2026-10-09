//! 自定义 pty 后端：把 russh shell 通道桥接到 alacritty 的 event loop。
//!
//! alacritty 的 event loop 用 `polling` 驱动**同步**的 `io::Read/Write`，且要求 pty 实现
//! `EventedPty`。russh `Channel` 是异步且无真实 fd，故这里用一层进程内字节管道适配：
//! 远端侧的另一个端点由主进程 pump 与 russh Channel 双向桥接（见 rterm-core 的 terminal_bridge）。
//! 本结构持有该管道在 GUI 侧的同步端（一个 `File`：Unix 下为非阻塞 socket fd，Windows 下为双向
//! 命名管道句柄），并提供 `EventedPty` 实现。
//!
//! 平台差异：
//! - Unix：管道同步端是「非阻塞 fd」，可直接交给 `polling` 的 `add_with_mode` 监听可读写。
//! - Windows：`polling` 在该平台仅支持 `AsSocket`（命名管道句柄不是 socket），无法直接轮询。
//!   故仿照 alacritty 的 Windows PTY，用后台线程在管道上做阻塞读写，并通过 IOCP 的
//!   `CompletionPacket` 把可读/可写事件投递给 `polling`，同步 `Read/Write` 端再从内存管道取放数据。
//!
//! 子进程事件的等价物：alacritty 事件循环只在收到 `PTY_CHILD_EVENT_TOKEN` 事件时才查询
//! [`EventedPty::next_child_event`]，并据此走「子进程退出」分支（`Term::exit()` → `Event::Exit`
//! → 循环收尾）。本 pty 没有真实子进程，桥接收场（结束 / 关标签请求停止）就是它的「子进程退出」；
//! 若不为该 token 注册事件源，事件循环便无从知晓，断开后旧循环会一直对着已 EOF 的管道空转。
//! 故本模块自行制造事件源——Unix 用一对控制 socketpair（读端按该 token 注册，结束观察线程在
//! 桥接收场后丢弃写端使其 EOF 可读）；Windows 直接由观察线程向事件循环的 poller 投递带该
//! token 的 completion packet——让旧循环在断开后自行收尾退出。

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
#[cfg(windows)]
use polling::os::iocp::{CompletionPacket, PollerIocpExt};
use polling::{Event as PollEvent, PollMode, Poller};
use rterm_core::BridgeState;
use std::fs::File;
use std::io;
#[cfg(not(windows))]
use std::os::fd::OwnedFd;
#[cfg(not(windows))]
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// alacritty event loop 用来识别「pty 可读写」的 poll key。
///
/// alacritty 在 `event_loop.rs` 中按 `interest.key` 分流：该 key 命中 `PTY_READ_WRITE_TOKEN`
/// 才当作 pty 读写事件处理。取值随平台不同——Unix 为 0，Windows 为 2（1 是子进程事件
/// `PTY_CHILD_EVENT_TOKEN`），因此**不能写死 0**，否则 Windows 上 pty 事件会被误判为子进程
/// 事件而收不到任何远端输出。Unix 下该常量在 alacritty 里是 `pub(crate)`，取不到，故按平台定义。
#[cfg(windows)]
const PTY_READ_WRITE_TOKEN: usize = alacritty_terminal::tty::PTY_READ_WRITE_TOKEN;
/// Unix 平台下 pty 读写事件的 poll key 取值（固定为 0）。
#[cfg(not(windows))]
const PTY_READ_WRITE_TOKEN: usize = 0;

/// alacritty 用来识别「子进程有事件」（该询问 [`EventedPty::next_child_event`]）的 poll key。
#[cfg(windows)]
const PTY_CHILD_EVENT_TOKEN: usize = alacritty_terminal::tty::PTY_CHILD_EVENT_TOKEN;
/// Unix 平台下子进程事件 token 的取值（固定为 1，上游为 `pub(crate)` 取不到）。
#[cfg(not(windows))]
const PTY_CHILD_EVENT_TOKEN: usize = 1;

/// 结束观察线程的轮询间隔：与 GUI 侧断开检测（`app/tasks.rs` 的 watcher）同节奏；
/// 该线程只把「桥接收场」翻译成一个事件循环可观测的事件，再密并无实益。
const FINISH_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// 桥接 russh shell 通道的 PTY 适配结构：持有管道在 GUI 侧的同步端，实现 alacritty 的 `EventedPty`。
pub struct RusshPty {
    /// GUI 侧持有的管道同步端 `File`：Unix 下为非阻塞 socket fd，Windows 下为双向命名管道句柄。
    #[cfg(not(windows))]
    file: File,
    /// 子进程事件源的观测端（仅 Unix）：控制 socketpair 的读端，注册时按
    /// [`PTY_CHILD_EVENT_TOKEN`] 进 poller；写端由结束观察线程持有，桥接收场即丢弃，
    /// 使本端变可读（EOF 即「可读」），事件循环随之走子进程事件分支收尾。
    #[cfg(not(windows))]
    child_event: File,
    /// GUI 侧持有的管道同步端（Windows 下为后台线程驱动的可读端 `conout` 与可写端 `conin`）。
    #[cfg(windows)]
    conout: win_io::UnblockedReader<File>,
    /// GUI 侧持有的管道同步端的可写端（仅 Windows）。
    #[cfg(windows)]
    conin: win_io::UnblockedWriter<File>,
    /// 子进程事件源的投递位（仅 Windows）：事件循环注册时记下 poller，结束观察线程
    /// 借它直接投递带 [`PTY_CHILD_EVENT_TOKEN`] 的 completion packet（IOCP 无需可轮询 fd）。
    #[cfg(windows)]
    child_wake: Arc<std::sync::Mutex<Option<Arc<Poller>>>>,
    /// 桥接结束状态：pump 结束时置位结束标志，使 alacritty 认为子进程退出并清理终端。
    bridge: Arc<BridgeState>,
    /// 尺寸变更发送端，向主进程 pump 的 resize 通道下发 SSH window-change 请求。
    resize_tx: mpsc::Sender<(u32, u32)>,
}

impl RusshPty {
    /// 由主进程桥接层传入管道在 GUI 侧的两个同步端：
    /// - `conout`：供 `conout` 后台读线程读取远端输出（Unix 下与 `conin` 克隆自同一 socketpair fd）。
    /// - `conin`：供 `conin` 后台写线程写入本地输入（Windows 下为另一条独立命名管道的客户端句柄）。
    ///
    /// 两者已在创建时被置为非阻塞（Unix）或本就支持异步轮询（Windows），可直接交给
    /// alacritty polling 使用。同时建好子进程事件源并拉起结束观察线程（见模块文档）。
    pub fn new(
        conout: File,
        conin: File,
        bridge: Arc<BridgeState>,
        resize_tx: mpsc::Sender<(u32, u32)>,
    ) -> io::Result<Self> {
        #[cfg(not(windows))]
        {
            // Unix 下 conout 与 conin 克隆自同一 socketpair fd，共用一个 file 即可；
            // 丢弃 conin 克隆不会关闭底层 fd（仍由 conout 持有）。
            let _ = conin;
            // 子进程事件源：读端留下注册，写端交给观察线程，桥接收场时随线程丢弃。
            let (child_event, child_wake) = UnixStream::pair()?;
            child_event.set_nonblocking(true)?;
            spawn_finish_watcher(bridge.clone(), move || drop(child_wake))?;
            Ok(Self {
                file: conout,
                child_event: File::from(OwnedFd::from(child_event)),
                bridge,
                resize_tx,
            })
        }
        #[cfg(windows)]
        {
            // 后台线程的读写缓冲容量，与桥接侧缓冲区规模一致即可。
            const PIPE_CAPACITY: usize = 8192;
            // OUT/IN 两条独立管道：conout 读 out 管客户端，conin 写 in 管客户端，互不串行化。
            let conout = win_io::UnblockedReader::new(conout, PIPE_CAPACITY)?;
            let conin = win_io::UnblockedWriter::new(conin, PIPE_CAPACITY)?;
            // 子进程事件源：poller 在事件循环注册时写入该槽位，观察线程借槽位投递事件。
            let child_wake = Arc::new(std::sync::Mutex::new(None));
            let slot = child_wake.clone();
            spawn_finish_watcher(bridge.clone(), move || post_child_event(&slot))?;
            Ok(Self {
                conout,
                conin,
                child_wake,
                bridge,
                resize_tx,
            })
        }
    }
}

#[cfg(not(windows))]
impl EventedReadWrite for RusshPty {
    /// 读端类型：GUI 侧同步管道的文件句柄。
    type Reader = File;
    /// 写端类型：GUI 侧同步管道的文件句柄。
    type Writer = File;

    /// 把管道文件与子进程事件源注册到 alacritty 的 `Poller`，并将 poll key 分别设为
    /// `PTY_READ_WRITE_TOKEN` 与 `PTY_CHILD_EVENT_TOKEN`。
    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        mut interest: PollEvent,
        poll_opts: PollMode,
    ) -> io::Result<()> {
        interest.key = PTY_READ_WRITE_TOKEN;
        unsafe { poll.add_with_mode(&self.file, interest, poll_opts) }?;
        // 子进程事件源：注册失败不致命——丢失的只是「断开后自动收尾」，读写通道照常，
        // 故记录日志后继续（若以 `?` 上抛，事件循环会因注册错误直接退出，终端反而全死）。
        let mut child_interest = interest;
        child_interest.key = PTY_CHILD_EVENT_TOKEN;
        child_interest.writable = false;
        if let Err(e) = unsafe { poll.add_with_mode(&self.child_event, child_interest, poll_opts) }
        {
            log::warn!("Failed to register the pty child-event source: {e}");
        }
        Ok(())
    }

    /// 重新设置管道文件在 `Poller` 上的监听兴趣，poll key 同样设为 `PTY_READ_WRITE_TOKEN`。
    ///
    /// 子进程事件源不参与重设：它的兴趣恒为「可读」，注册后无需再动。
    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        mut interest: PollEvent,
        poll_opts: PollMode,
    ) -> io::Result<()> {
        interest.key = PTY_READ_WRITE_TOKEN;
        poll.modify_with_mode(&self.file, interest, poll_opts)
    }

    /// 从 `Poller` 中移除管道文件与子进程事件源，结束对其可读写事件的轮询。
    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        // 子进程事件源可能未成功注册，其删除失败无关紧要；主通道的删除结果照常返回。
        let _ = poll.delete(&self.child_event);
        poll.delete(&self.file)
    }

    /// 返回作为读取端的管道 `File`，供 alacritty event loop 读取远端输出。
    fn reader(&mut self) -> &mut File {
        &mut self.file
    }

    /// 返回作为写入端的管道 `File`，供 alacritty event loop 写入本地输入。
    fn writer(&mut self) -> &mut File {
        &mut self.file
    }
}

#[cfg(windows)]
impl EventedReadWrite for RusshPty {
    type Reader = win_io::UnblockedReader<File>;
    type Writer = win_io::UnblockedWriter<File>;

    /// 把后台线程的读端、写端注册到 `Poller`（通过 IOCP 投递可读/可写事件）。
    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: PollEvent,
        poll_opts: PollMode,
    ) -> io::Result<()> {
        self.conin
            .register(poll, with_key(interest, PTY_READ_WRITE_TOKEN), poll_opts);
        self.conout
            .register(poll, with_key(interest, PTY_READ_WRITE_TOKEN), poll_opts);
        // 记录 poller 供子进程事件投递（IOCP 无需可轮询 fd，直接 post 即可）。
        // 若桥接在 pty 创建后立即收场（观察线程投递时 poller 尚未就位），这里补投一次，
        // 避免错过这次唯一的唤醒。
        *self.child_wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(poll.clone());
        if bridge_over(&self.bridge) {
            post_child_event(&self.child_wake);
        }
        Ok(())
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: PollEvent,
        poll_opts: PollMode,
    ) -> io::Result<()> {
        self.conin
            .register(poll, with_key(interest, PTY_READ_WRITE_TOKEN), poll_opts);
        self.conout
            .register(poll, with_key(interest, PTY_READ_WRITE_TOKEN), poll_opts);
        Ok(())
    }

    fn deregister(&mut self, _poll: &Arc<Poller>) -> io::Result<()> {
        self.conin.deregister();
        self.conout.deregister();
        Ok(())
    }

    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.conout
    }

    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.conin
    }
}

/// 把事件 key 改写为 pty 读写 token（Windows 下分流用，避免与子进程事件混淆）。
#[cfg(windows)]
fn with_key(mut event: PollEvent, key: usize) -> PollEvent {
    event.key = key;
    event
}

/// 桥接是否已收场（已结束或已请求停止）：pty 的「子进程已退出」判定与结束观察线程的
/// 唤醒条件都取自这里。两者必须完全一致——若唤醒线程先动而 `next_child_event` 答 `None`，
/// 事件源保持可读会导致事件循环空转；反之则无人促成收尾。
fn bridge_over(bridge: &BridgeState) -> bool {
    bridge.is_finished() || bridge.is_stop_requested()
}

/// 起一个一次性线程：桥接收场后执行一次 `poke` 并退出。
///
/// `poke` 负责把「收场」翻译成 alacritty 事件循环能观测到的事件（见模块文档）。
/// 若桥接拖到整个进程结束都不收场，本线程也只是保持 200ms 一次的休眠轮询，无其他负担。
fn spawn_finish_watcher(
    bridge: Arc<BridgeState>,
    poke: impl FnOnce() + Send + 'static,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("rterm-pty-exit".into())
        .spawn(move || {
            while !bridge_over(&bridge) {
                std::thread::sleep(FINISH_POLL_INTERVAL);
            }
            poke();
        })
        .map(drop)
}

/// 向事件循环的 poller 投递一条子进程事件（仅 Windows；IOCP 下无需 fd）。
/// poller 未就位时静默丢弃，由 [`RusshPty::register`] 的补投兜底。
#[cfg(windows)]
fn post_child_event(slot: &Arc<std::sync::Mutex<Option<Arc<Poller>>>>) {
    let poller = slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(poller) = poller {
        poller
            .post(CompletionPacket::new(PollEvent::readable(
                PTY_CHILD_EVENT_TOKEN,
            )))
            .ok();
    }
}

impl EventedPty for RusshPty {
    /// 返回子进程退出事件：桥接收场（结束或已请求停止）时返回 `Exited`，否则返回 `None`。
    ///
    /// 只在子进程事件源（见模块文档）送来事件后才会被询问；判定条件与结束观察线程
    /// 共用 `bridge_over`。请求停止也纳入条件，是为了保留原语义（关标签立刻让
    /// alacritty 收尾，不等 pump 退出完成）；正常断开走 [`BridgeState::is_finished`]。
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        if bridge_over(&self.bridge) {
            Some(ChildEvent::Exited(None))
        } else {
            None
        }
    }
}

impl OnResize for RusshPty {
    /// 处理尺寸变更：把新行宽汇入 resize 通道，最终下发 SSH window-change。
    fn on_resize(&mut self, size: WindowSize) {
        let _ = self
            .resize_tx
            .try_send((size.num_cols as u32, size.num_lines as u32));
    }
}

// ---------------------------------------------------------------------------
// Windows 专用：后台线程驱动的可读/可写管道端。
//
// `polling` 在 Windows（IOCP）上只接受 `AsSocket`，无法直接轮询 `File` 句柄；这里的实现
// 与 alacritty 的 `tty::windows::blocking` 同构——后台线程对管道做阻塞读写，并通过
// `poller.post(CompletionPacket)` 把就绪事件喂回 event loop，同步 `Read/Write` 端则从内存
// 管道 `piper` 取放数据，从而不阻塞 alacritty 主线程。
// ---------------------------------------------------------------------------
#[cfg(windows)]
mod win_io {
    use std::io::{self, Read, Result, Write};
    use std::marker::PhantomData;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread;

    use piper::{Reader as PiperReader, Writer as PiperWriter, pipe};
    use polling::os::iocp::{CompletionPacket, PollerIocpExt};
    use polling::{Event, PollMode, Poller};

    /// 记录当前需要投递给 `poller` 的兴趣事件；同时充当 `Waker`，在 IO 就绪时唤醒主线程。
    struct Registration {
        /// 当前注册的监听兴趣；为 `None` 表示已注销。
        interest: Mutex<Option<Interest>>,
        /// 该注册属于读端还是写端（决定唤醒时投递 readable 还是 writable）。
        end: PipeEnd,
    }

    #[derive(Copy, Clone)]
    enum PipeEnd {
        Reader,
        Writer,
    }

    struct Interest {
        /// 要投递给 event loop 的事件。
        event: Event,
        /// 事件要投递到的 poller。
        poller: Arc<Poller>,
        /// 监听模式（Oneshot 投递后即清空兴趣）。
        mode: PollMode,
    }

    /// 在另一线程上轮询一个 `Read` 源：后台读线程把数据灌入内存管道，主线程从中 `read`。
    pub struct UnblockedReader<R> {
        /// 当前监听兴趣（含 poller 引用），供后台线程就绪时投递事件。
        interest: Arc<Registration>,
        /// 主线程侧读取的内存管道端。
        pipe: PiperReader,
        /// 是否首次注册（首次总是投递一次事件以触发首轮读取）。
        first_register: bool,
        /// 仅用于标记逻辑所有权，不实际使用。
        _reader: PhantomData<R>,
    }

    impl<R: Read + Send + 'static> UnblockedReader<R> {
        /// 基于 `source` 启动一个后台读线程。
        pub fn new(mut source: R, pipe_capacity: usize) -> Result<Self> {
            let (reader, mut writer) = pipe(pipe_capacity);
            let interest = Arc::new(Registration {
                interest: Mutex::new(None),
                end: PipeEnd::Reader,
            });

            thread::Builder::new()
                .name("rterm-tty-reader".into())
                .spawn(move || {
                    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
                    let mut context = Context::from_waker(&waker);
                    loop {
                        // 从源读到内存管道。
                        match writer.poll_fill(&mut context, &mut source) {
                            Poll::Ready(Ok(0)) => {
                                // 源 EOF 或管道关闭，读线程退出。
                                return;
                            }
                            Poll::Ready(Ok(_)) => {
                                continue;
                            }
                            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
                                continue;
                            }
                            Poll::Ready(Err(e)) => {
                                log::error!("rterm tty read thread error: {e}");
                                return;
                            }
                            Poll::Pending => {
                                // 等待主线程唤醒（内存管道可写时）。
                                thread::park();
                            }
                        }
                    }
                })
                .map_err(|e| {
                    io::Error::other(format!("Failed to create TTY reader thread: {e}"))
                })?;

            Ok(Self {
                interest,
                pipe: reader,
                first_register: true,
                _reader: PhantomData,
            })
        }

        /// 向 `poller` 注册可读兴趣；若已有数据或首次注册，立即投递一次事件。
        pub fn register(&mut self, poller: &Arc<Poller>, event: Event, mode: PollMode) {
            let mut interest = self.interest.interest.lock().unwrap();
            *interest = Some(Interest {
                event,
                poller: poller.clone(),
                mode,
            });

            if (!self.pipe.is_empty() && event.readable) || self.first_register {
                self.first_register = false;
                poller.post(CompletionPacket::new(event)).ok();
            }
        }

        /// 注销监听兴趣。
        pub fn deregister(&self) {
            let mut interest = self.interest.interest.lock().unwrap();
            *interest = None;
        }

        /// 尝试从内存管道读取（非阻塞）。
        ///
        /// 与 alacritty 自带的 Windows PTY 实现（`tty/windows/blocking.rs`）保持一致：
        /// 内存管道暂无可读数据（`Poll::Pending`）时返回 `0` 而非 `WouldBlock` 错误。
        /// alacritty 的 `pty_read` 对 `Ok(0)`（且尚无已解析数据）仅理解为「当前无可读
        /// 数据，回到 poll 等待」，并非 EOF——会触发 `Wakeup` 重绘并最终回到 `poll.wait`。
        /// 若此处返回 `WouldBlock`，语义等价（同样 break 回 poll），故沿用上游实现。
        pub fn try_read(&mut self, buf: &mut [u8]) -> usize {
            let waker = Waker::from(self.interest.clone());
            match self
                .pipe
                .poll_drain_bytes(&mut Context::from_waker(&waker), buf)
            {
                Poll::Pending => 0,
                Poll::Ready(n) => n,
            }
        }
    }

    impl<R: Read + Send + 'static> Read for UnblockedReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            Ok(self.try_read(buf))
        }
    }

    /// 在另一线程上轮询一个 `Write` 汇：主线程把数据写入内存管道，后台写线程排空到 `sink`。
    pub struct UnblockedWriter<W> {
        interest: Arc<Registration>,
        pipe: PiperWriter,
        _writer: PhantomData<W>,
    }

    impl<W: Write + Send + 'static> UnblockedWriter<W> {
        /// 基于 `sink` 启动一个后台写线程。
        pub fn new(mut sink: W, pipe_capacity: usize) -> Result<Self> {
            let (mut reader, writer) = pipe(pipe_capacity);
            let interest = Arc::new(Registration {
                interest: Mutex::new(None),
                end: PipeEnd::Writer,
            });

            thread::Builder::new()
                .name("rterm-tty-writer".into())
                .spawn(move || {
                    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
                    let mut context = Context::from_waker(&waker);
                    loop {
                        // 从内存管道排空到汇。
                        match reader.poll_drain(&mut context, &mut sink) {
                            Poll::Ready(Ok(0)) => return,
                            Poll::Ready(Ok(_)) => {
                                continue;
                            }
                            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
                                continue;
                            }
                            Poll::Ready(Err(e)) => {
                                log::error!("rterm tty write thread error: {e}");
                                return;
                            }
                            Poll::Pending => thread::park(),
                        }
                    }
                })
                .map_err(|e| {
                    io::Error::other(format!("Failed to create TTY writer thread: {e}"))
                })?;

            Ok(Self {
                interest,
                pipe: writer,
                _writer: PhantomData,
            })
        }

        /// 向 `poller` 注册可写兴趣；若管道有空位，立即投递一次事件。
        pub fn register(&self, poller: &Arc<Poller>, event: Event, mode: PollMode) {
            let mut interest = self.interest.interest.lock().unwrap();
            *interest = Some(Interest {
                event,
                poller: poller.clone(),
                mode,
            });

            if !self.pipe.is_full() && event.writable {
                poller.post(CompletionPacket::new(event)).ok();
            }
        }

        pub fn deregister(&self) {
            let mut interest = self.interest.interest.lock().unwrap();
            *interest = None;
        }

        /// 尝试向内存管道写入（非阻塞）。与 `try_read` 同理：管道满（`Poll::Pending`）
        /// 时返回 `0`，由 alacritty 的 `pty_write` 理解为「暂不可写」回到 poll 等待。
        pub fn try_write(&mut self, buf: &[u8]) -> usize {
            let waker = Waker::from(self.interest.clone());
            match self
                .pipe
                .poll_fill_bytes(&mut Context::from_waker(&waker), buf)
            {
                Poll::Pending => 0,
                Poll::Ready(n) => n,
            }
        }
    }

    impl<W: Write + Send + 'static> Write for UnblockedWriter<W> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(self.try_write(buf))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// 后台线程在 `poll_fill`/`poll_drain` 等待时进入 `Pending`，靠 `unpark` 唤醒。
    struct ThreadWaker(thread::Thread);

    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    impl Wake for Registration {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            let mut interest_lock = self.interest.lock().unwrap();
            if let Some(interest) = interest_lock.as_ref() {
                // 仅在该端对应方向被监听时才投递事件。
                let send_event = match self.end {
                    PipeEnd::Reader => interest.event.readable,
                    PipeEnd::Writer => interest.event.writable,
                };

                if send_event {
                    interest
                        .poller
                        .post(CompletionPacket::new(interest.event))
                        .ok();

                    // Oneshot 模式下投递后即清空兴趣（与 alacritty 行为一致）。
                    if matches!(interest.mode, PollMode::Oneshot | PollMode::EdgeOneshot) {
                        *interest_lock = None;
                    }
                }
            }
        }
    }
}
