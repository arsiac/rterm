//! 终端桥接：把 russh shell 通道经进程内字节管道直接喂给 GUI 的终端渲染层。
//!
//! 进程内创建两条独立连通的字节管道——OUT（远端→本地输出）与 IN（本地→远端输入），
//! 同步端交给 GUI 的 alacritty event loop（包装成 `RusshPty`），异步端在此处与 russh
//! shell 通道双向泵接；不派生子进程、不占用端口、也不创建本地 PTY。拆成两条管道是为
//! 避免 Windows 命名管道同一端点同步读写被内核串行化而死锁（见 [`create_bridge`] 注释）。
//! 两端的具体实现随平台而变：
//! - Unix：单个 `socketpair`（`UnixStream`），同步端以非阻塞 fd 重建为 `File`，conout/conin 克隆自同一 fd。
//! - Windows：两条独立命名管道，同步端为管道 `File` 句柄（OUT 管客户端供 conout 读、IN 管客户端供 conin 写）。
//!
//! 之所以不用 [`tokio::io::copy_bidirectional`]，是因为 `Channel` 仅在 `into_stream` 后才有
//! `AsyncRead`/`AsyncWrite`，而那样会丢失可调用 `window_change` 的通道句柄。故用
//! [`russh::Channel::make_reader`] / [`russh::Channel::make_writer`] 在每个 I/O 周期临时借用
//! 通道，并在两次等待之间排空尺寸变更请求。

use crate::CoreError;
use crate::connection::SshConnection;
use russh::client::Msg;
use std::fs::File;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// 创建终端桥接所需的一切，返回供 GUI 直接消费的对象。
///
/// 该函数会：
/// 1. 建立一对进程内双向字节管道；
/// 2. 在已建立的连接上打开带 PTY 的 shell 通道；
/// 3. 后台启动桥接任务（含窗口尺寸转发）。
///
/// # 参数
/// - `conn`：已建立的 SSH 连接（内部句柄可被并发共享）。
/// - `cols` / `rows`：初始终端列数与行数，用于首帧 PTY 尺寸。
///
/// # 返回
/// - `local`：同步端 `File`，GUI 应交给 `RusshPty` 包装后接入终端渲染层。
/// - `disconnect`：共享的断开标志；桥接结束时本模块置位，供 GUI 感知远端关闭。
/// - `resize_tx`：本地终端尺寸变更（`(列数, 行数)`）的发送端，GUI 在收到终端
///   resize 事件时调用。**丢弃它只停止尺寸转发，并不会结束桥接**——桥接要等远端
///   shell 通道 EOF（或本模块的 pump 退出）才结束，并置位 `disconnect`。
pub async fn spawn_terminal_bridge(
    conn: &SshConnection,
    cols: u32,
    rows: u32,
) -> Result<(File, File, Arc<AtomicBool>, mpsc::Sender<(u32, u32)>), CoreError> {
    // 进程内管道：拆成 OUT（远端→本地输出）与 IN（本地→远端输入）两条独立管道。
    // 同步端（conout 读端 / conin 写端）交 GUI，异步端（out_stream / in_stream）在此泵接 russh 通道。
    let (conout_file, conin_file, out_stream, in_stream) = create_bridge()?;

    let disconnect = Arc::new(AtomicBool::new(false));

    // 打开 shell 通道（含 PTY 与 shell 进程）；这是整条链路上唯一的远端资源获取点。
    let channel = conn.open_shell_channel(cols, rows).await?;

    // 尺寸变更通道：容量 8，GUI 侧 resize 突发时丢弃最旧也不阻塞渲染。
    let (resize_tx, resize_rx) = mpsc::channel(8);

    let bridge_disconnect = disconnect.clone();
    // 泵接监听的断开标志需独立克隆：关标签 / 关窗口时由 GUI 置位使其尽快退出。
    let pump_stop = disconnect.clone();
    tokio::spawn(async move {
        pump(out_stream, in_stream, channel, resize_rx, pump_stop).await;
        log::debug!("终端桥接任务结束");
        // 桥接结束即视为连接断开（远端关闭或本地流 EOF），通知 GUI 更新状态。
        bridge_disconnect.store(true, Ordering::SeqCst);
    });

    Ok((conout_file, conin_file, disconnect, resize_tx))
}

/// 在异步字节流与远端 shell 通道之间双向转发数据，并在 I/O 等待间隙应用尺寸变更。
///
/// `out_stream` 用于把远端数据写到本地（GUI 的 conout 会读它）；`in_stream` 用于读
/// 取本地输入（GUI 的 conin 会写它）。二者是**两条独立管道**，故 conout 读线程与
/// conin 写线程不会落在同一管道端点上互相串行化阻塞。
///
/// `stop` 为断开标志：关标签 / 关窗口时由 GUI 置位，使泵接尽快退出（否则泵接持有
/// 服务端管道、win_io 读线程持有客户端管道并被 `ReadFile` 阻塞，二者互相等待对方
/// 关闭句柄而死锁，导致后台线程与进程残留）。
async fn pump<W, R>(
    mut out_stream: W,
    mut in_stream: R,
    mut channel: russh::Channel<Msg>,
    mut resize_rx: mpsc::Receiver<(u32, u32)>,
    stop: Arc<AtomicBool>,
) where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    // 复用的收发缓冲区（8 KiB 已满足交互式终端吞吐）。
    let mut socket_buf = [0u8; 8192];
    let mut channel_buf = [0u8; 8192];
    let mut total_remote = 0usize;
    let mut total_local = 0usize;
    log::debug!("终端桥接 pump 启动");

    loop {
        // 在两次 I/O 等待之间，将积压的窗口尺寸变更下发到远端。
        while let Ok((cols, rows)) = resize_rx.try_recv() {
            if let Err(e) = channel.window_change(cols, rows, 0, 0).await {
                log::debug!("转发窗口尺寸变更失败: {e}");
            }
        }

        tokio::select! {
            // 收到断开信号：立即退出，释放服务端管道句柄。
            _ = wait_stop(stop.clone()) => {
                log::debug!("pump: 收到断开信号，退出");
                break;
            }
            // 远端 -> 本地：从通道读取并写入本地 OUT 管道端。
            n = async {
                let mut reader = channel.make_reader();
                reader.read(&mut channel_buf).await
            } => {
                match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out_stream.write_all(&channel_buf[..n]).await.is_err() {
                            break;
                        }
                        total_remote += n;
                    }
                }
            }
            // 本地 -> 远端：从本地 IN 管道端读取并写入通道。
            n = in_stream.read(&mut socket_buf) => {
                match n {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut writer = channel.make_writer();
                        if writer.write_all(&socket_buf[..n]).await.is_err() {
                            break;
                        }
                        total_local += n;
                    }
                    Err(_) => break,
                }
            }
        }
    }
    log::debug!(
        "终端桥接 pump 退出（远端→本地 {total_remote} 字节，本地→远端 {total_local} 字节）"
    );
}

/// 在断开标志置位前让出，供 `tokio::select!` 监听泵接退出信号。
async fn wait_stop(stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
}

/// 创建一对双向连通的字节管道，返回
/// `(GUI 输出读端 File, GUI 输入写端 File, 异步 OUT 流, 异步 IN 流)`。
///
/// Unix 下 socketpair 本身是双向的，本地 fd 既可被 conout 读、也可被 conin 写，
/// 因此两个 `File` 克隆自同一 fd；异步端按 [`tokio::io::split`] 拆成写半（OUT）与
/// 读半（IN）。两条逻辑通道复用同一条物理 socketpair，互不干扰。
#[cfg(unix)]
fn create_bridge() -> io::Result<(File, File, impl AsyncWrite + Unpin, impl AsyncRead + Unpin)> {
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    let (local, remote) = UnixStream::pair()?;
    // socketpair 默认阻塞；同步端交给 polling 前必须非阻塞，异步端交给 tokio 前也必须非阻塞。
    local
        .set_nonblocking(true)
        .expect("设置 socketpair 同步端为非阻塞失败");
    let local = unsafe { File::from_raw_fd(local.into_raw_fd()) };
    remote
        .set_nonblocking(true)
        .expect("设置 socketpair 异步端为非阻塞失败");
    let remote = tokio::net::UnixStream::from_std(remote)?;
    // 同步端克隆两份：conout 读、conin 写（共用同一双向 fd）。
    let conout_file = local.try_clone()?;
    let conin_file = local;
    // 异步端拆分：写半把远端数据写入本地（OUT 方向），读半读取本地输入（IN 方向）。
    let (in_stream, out_stream) = tokio::io::split(remote);
    Ok((conout_file, conin_file, out_stream, in_stream))
}

/// 创建一对双向连通的字节管道，返回
/// `(GUI 输出读端 File, GUI 输入写端 File, 异步 OUT 流, 异步 IN 流)`。
///
/// Windows 下**必须**拆成两条独立的命名管道：一条 OUT（pump 写服务端、conout 读
/// 客户端）、一条 IN（conin 写客户端、pump 读服务端）。Windows 命名管道同一端点上的
/// 同步 `ReadFile` 与 `WriteFile` 会被内核串行化——若 conout 与 conin 共用同一端点，
/// conout 读线程长期阻塞在 `ReadFile`，会使 conin 写线程的 `WriteFile` 一直挂起，直到
/// 关窗释放服务端句柄才以 `ERROR_PIPE_CLOSING`（os error 232）失败，表现为「有输出但
/// 无法输入」。两条独立管道让读端与写端分属不同实例，互不串行化。
#[cfg(windows)]
fn create_bridge() -> io::Result<(File, File, impl AsyncWrite + Unpin, impl AsyncRead + Unpin)> {
    let (out_server, out_client) = make_pipe("out")?;
    let (in_server, in_client) = make_pipe("in")?;
    // out_client 给 conout 读，in_client 给 conin 写；out_server / in_server 供 pump。
    Ok((out_client, in_client, out_server, in_server))
}

/// 建一条进程内双向命名管道，自连后返回 `(异步服务端, 同步客户端 File)`。
#[cfg(windows)]
fn make_pipe(suffix: &str) -> io::Result<(tokio::net::windows::named_pipe::NamedPipeServer, File)> {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use std::sync::atomic::AtomicU64;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW};

    /// 进程内命名管道实例计数器，保证 Windows 命名管道名全局唯一。
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    // 形如 `\\.\pipe\rterm-<pid>-<suffix>-<n>\0` 的宽字符串（含结尾空字符）。
    let name: Vec<u16> = format!(
        "\\\\.\\pipe\\rterm-{}-{}-{}\0",
        std::process::id(),
        suffix,
        n
    )
    .encode_utf16()
    .collect();

    // 服务端：双向 + 重叠 I/O（异步前提）。
    let server = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            0,
            1,
            65536,
            65536,
            0,
            std::ptr::null(),
        )
    };
    if server == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    // 客户端：以**同步**方式打开同一管道（自己连自己，无需等待外部进程接入）。
    //
    // 注意：这里**绝不能**加 `FILE_FLAG_OVERLAPPED`。该句柄会交给 GUI 侧
    // `RusshPty` 的后台线程做同步 `File::read/write`（见 russh_pty.rs 的
    // `win_io`），而 Rust 标准库对「以异步方式打开的句柄」做同步 I/O 时会
    // 直接 `abort()` 进程（std `sys/pal/windows/handle.rs`，issue #81357，
    // 报错 `I/O error: operation failed to complete synchronously`）。
    // 服务端子端仍是重叠句柄（供 tokio 异步泵接），命名管道两端重叠标志可不同。
    let client = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        )
    };
    if client == INVALID_HANDLE_VALUE {
        unsafe { CloseHandle(server) };
        return Err(io::Error::last_os_error());
    }

    // 客户端已打开，服务端立即完成连接（ERROR_PIPE_CONNECTED 属正常情况，忽略）。
    unsafe { ConnectNamedPipe(server, std::ptr::null_mut()) };

    let client_file = unsafe { File::from_raw_handle(client as RawHandle) };
    // 服务端句柄转异步命名管道（IOCP 驱动），供 pump 使用。
    //
    // 注意：**不能**用 `tokio::fs::File`——它是为常规文件设计的，对命名管道会走
    // `spawn_blocking` 同步读写，在 Windows 上不可靠：远端数据写入后客户端一侧迟迟
    // 收不到、或读写死锁，表现为「终端没有任何输出 / 输入无回显」。正确做法是使用
    // tokio 专为本平台提供的 `NamedPipeServer`（基于 IOCP 的真正异步命名管道）。
    let server_async = unsafe {
        tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle(server as RawHandle)
    }
    .map_err(|e| io::Error::other(format!("创建异步命名管道失败: {e}")))?;
    Ok((server_async, client_file))
}
