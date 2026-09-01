//! 自定义 pty 后端：把 russh shell 通道桥接到 alacritty 的 event loop。
//!
//! alacritty 的 event loop 用 `polling` 驱动**同步**的 `io::Read/Write`，且要求 pty 实现
//! `EventedPty`。russh `Channel` 是异步且无真实 fd，故这里用一层进程内字节管道适配：
//! 远端侧的另一个端点由主进程 pump 与 russh Channel 双向桥接（见 rterm-core 的 terminal_bridge）。
//! 本结构持有该管道在 GUI 侧的同步端（一个 `File`：Unix 下为非阻塞 socket fd，Windows 下为双向
//! 命名管道句柄），并提供 `EventedPty` 实现。

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event as PollEvent, PollMode, Poller};
use std::fs::File;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// 桥接 russh shell 通道的 PTY 适配结构：持有管道在 GUI 侧的同步端，实现 alacritty 的 `EventedPty`。
pub struct RusshPty {
    /// GUI 侧持有的管道同步端 `File`：Unix 下为非阻塞 socket fd，Windows 下为双向命名管道句柄。
    file: File,
    /// 远端断开标志；pump 结束时由桥接层置位，使 alacritty 认为子进程退出并清理终端。
    disconnect: Arc<AtomicBool>,
    /// 尺寸变更发送端，向主进程 pump 的 resize 通道下发 SSH window-change 请求。
    resize_tx: mpsc::Sender<(u32, u32)>,
}

impl RusshPty {
    /// 由主进程桥接层传入管道在 GUI 侧的同步端；该 `File` 已在创建时被置为非阻塞（Unix）
    /// 或本就支持异步轮询（Windows），可直接交给 alacritty polling 使用。
    pub fn new(
        file: File,
        disconnect: Arc<AtomicBool>,
        resize_tx: mpsc::Sender<(u32, u32)>,
    ) -> Self {
        Self {
            file,
            disconnect,
            resize_tx,
        }
    }
}

impl EventedReadWrite for RusshPty {
    /// 读端类型：GUI 侧同步管道的文件句柄。
    type Reader = File;
    /// 写端类型：GUI 侧同步管道的文件句柄。
    type Writer = File;

    /// 把管道文件注册到 alacritty 的 `Poller`，并将 poll key 设为 `PTY_READ_WRITE_TOKEN`。
    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        mut interest: PollEvent,
        poll_opts: PollMode,
    ) -> io::Result<()> {
        interest.key = PTY_READ_WRITE_TOKEN;
        unsafe { poll.add_with_mode(&self.file, interest, poll_opts) }
    }

    /// 重新设置管道文件在 `Poller` 上的监听兴趣，poll key 同样设为 `PTY_READ_WRITE_TOKEN`。
    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        mut interest: PollEvent,
        poll_opts: PollMode,
    ) -> io::Result<()> {
        interest.key = PTY_READ_WRITE_TOKEN;
        poll.modify_with_mode(&self.file, interest, poll_opts)
    }

    /// 从 `Poller` 中移除管道文件，结束对其可读写事件的轮询。
    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
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

impl EventedPty for RusshPty {
    /// 返回子进程退出事件：远端断开（pump 结束）时返回 `Exited`，否则返回 `None`。
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        if self.disconnect.load(Ordering::SeqCst) {
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
