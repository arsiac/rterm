//! 终端桥接：把 russh shell 通道经进程内字节管道直接喂给 GUI 的终端渲染层。
//!
//! 进程内创建一对双向连通的字节管道，同步端交给 GUI 的 alacritty event loop
//! （包装成 `RusshPty`），异步端在此处与 russh shell 通道双向泵接；不派生子进程、
//! 不占用端口、也不创建本地 PTY。两端的具体实现随平台而变：
//! - Unix：socketpair（`UnixStream`），同步端以非阻塞 fd 重建为 `File`。
//! - Windows：双向命名管道，同步端为管道 `File` 句柄。
//!
//! 之所以不用 [`tokio::io::copy_bidirectional`]，是因为 `Channel` 仅在 `into_stream` 后才有
//! `AsyncRead`/`AsyncWrite`，而那样会丢失可调用 `window_change` 的通道句柄。故用
//! [`russh::Channel::make_reader`] / [`russh::Channel::make_writer`] 在每个 I/O 周期临时借用
//! 通道，并在两次等待之间排空尺寸变更请求。

use crate::CoreError;
use crate::connection::SshConnection;
use log::debug;
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
) -> Result<(File, Arc<AtomicBool>, mpsc::Sender<(u32, u32)>), CoreError> {
    // 进程内双向管道：同步端交 GUI，异步端在此泵接 russh 通道。
    let (local, remote) = create_bridge()?;

    let disconnect = Arc::new(AtomicBool::new(false));

    // 打开 shell 通道（含 PTY 与 shell 进程）；这是整条链路上唯一的远端资源获取点。
    let channel = conn.open_shell_channel(cols, rows).await?;

    // 尺寸变更通道：容量 8，GUI 侧 resize 突发时丢弃最旧也不阻塞渲染。
    let (resize_tx, resize_rx) = mpsc::channel(8);

    let bridge_disconnect = disconnect.clone();
    tokio::spawn(async move {
        pump(remote, channel, resize_rx).await;
        debug!("终端桥接任务结束");
        // 桥接结束即视为连接断开（远端关闭或本地流 EOF），通知 GUI 更新状态。
        bridge_disconnect.store(true, Ordering::SeqCst);
    });

    Ok((local, disconnect, resize_tx))
}

/// 在异步字节流与远端 shell 通道之间双向转发数据，并在 I/O 等待间隙应用尺寸变更。
async fn pump<S>(
    mut stream: S,
    mut channel: russh::Channel<Msg>,
    mut resize_rx: mpsc::Receiver<(u32, u32)>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // 复用的收发缓冲区（8 KiB 已满足交互式终端吞吐）。
    let mut socket_buf = [0u8; 8192];
    let mut channel_buf = [0u8; 8192];

    loop {
        // 在两次 I/O 等待之间，将积压的窗口尺寸变更下发到远端。
        while let Ok((cols, rows)) = resize_rx.try_recv() {
            if let Err(e) = channel.window_change(cols, rows, 0, 0).await {
                debug!("转发窗口尺寸变更失败: {e}");
            }
        }

        tokio::select! {
            // 远端 -> 本地：从通道读取并写入本地管道端。
            n = async {
                let mut reader = channel.make_reader();
                reader.read(&mut channel_buf).await
            } => {
                match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if stream.write_all(&channel_buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
            // 本地 -> 远端：从管道读取并写入通道。
            n = stream.read(&mut socket_buf) => {
                match n {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut writer = channel.make_writer();
                        if writer.write_all(&socket_buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

/// 创建一对双向连通的字节管道，返回 `(GUI 同步端 File, 异步泵接端)`。
#[cfg(unix)]
fn create_bridge() -> io::Result<(File, impl AsyncRead + AsyncWrite + Unpin)> {
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
    Ok((local, remote))
}

/// 创建一对双向连通的字节管道，返回 `(GUI 同步端 File, 异步泵接端)`。
#[cfg(windows)]
fn create_bridge() -> io::Result<(File, impl AsyncRead + AsyncWrite + Unpin)> {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use std::sync::atomic::AtomicU64;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, GENERIC_READ, GENERIC_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_ACCESS_DUPLEX,
    };

    /// 进程内命名管道实例计数器，保证 Windows 命名管道名全局唯一。
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    // 形如 `\\.\pipe\rterm-<pid>-<n>\0` 的宽字符串（含结尾空字符）。
    let name: Vec<u16> = format!("\\\\.\\pipe\\rterm-{}-{}\0", std::process::id(), n)
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

    // 客户端：以重叠方式打开同一管道（自己连自己，无需等待外部进程接入）。
    let client = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    };
    if client == INVALID_HANDLE_VALUE {
        unsafe { CloseHandle(server) };
        return Err(io::Error::last_os_error());
    }

    // 客户端已打开，服务端立即完成连接（ERROR_PIPE_CONNECTED 属正常情况，忽略）。
    unsafe { ConnectNamedPipe(server, std::ptr::null_mut()) };

    let client_file = unsafe { File::from_raw_handle(RawHandle(client as *mut _)) };
    let server_file = unsafe { File::from_raw_handle(RawHandle(server as *mut _)) };
    // 服务端句柄转异步文件，供 pump 使用。
    let server_async = tokio::fs::File::from_std(server_file);
    Ok((client_file, server_async))
}
