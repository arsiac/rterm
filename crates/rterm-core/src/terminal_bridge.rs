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
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// 终端当前目录（cwd）共享容器：核心层桥接 pump 扫描 OSC 7 序列后写入，
/// GUI 侧在「进入终端目录」按钮点击时读取。按标签独立持有（见 `TerminalTab::cwd`）。
///
/// 用 `Option` 包裹以便「不追踪 cwd」的场景（如本地 PTY 或调用方未提供）直接传 `None`，
/// 此时 pump 跳过 OSC 7 扫描且不向 shell 注入任何内容。
pub type CwdTracker = Option<Arc<Mutex<Option<String>>>>;

/// 桥接 pump 的退出原因：回答「为什么这条终端通道结束了」，供 GUI 区分
/// 「远端会话已结束」与「连接已断开」——两者的后续动作不同（前者是用户 `exit`
/// 了，后者是网络问题），文案也不同。
///
/// 编码进 [`BridgeState`] 的 `AtomicU8`（见 [`DisconnectReason::decode`]）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DisconnectReason {
    /// 尚未判定（桥接仍在运行，或退出路径无法归因）。
    #[default]
    Unknown = 0,
    /// 传输层已死（拔网线、NAT 掉表、sshd 重启、保活超时）：pump 读到通道
    /// EOF / 错误的同时会话任务已收尾。
    TransportDied = 1,
    /// 连接仍活、shell 通道结束（`exit` / `logout`）：会话任务仍在，
    /// 只是这条通道的发送端被远端丢弃。
    ChannelEof = 2,
    /// 本地主动停：关标签 / 关窗口时置位断开标志使 pump 退出。
    /// 不是故障，仅用于把「正常退出」从另外两类里摘出来。
    LocalStop = 3,
}

impl DisconnectReason {
    /// 解码 `AtomicU8` 取值；未知编码按 [`DisconnectReason::Unknown`] 处理。
    fn decode(value: u8) -> Self {
        match value {
            1 => Self::TransportDied,
            2 => Self::ChannelEof,
            3 => Self::LocalStop,
            _ => Self::Unknown,
        }
    }
}

/// 桥接的共享结束状态。
///
/// 拆成两个标志是刻意的：`stop_requested` 是**输入**（GUI 关标签 / 关窗口时置位，
/// 请求 pump 退出），`finished` 是**输出**（pump 退出、原因已写下）。合并成一个
/// 标志会让「已请求停止但 pump 尚未归因」的窗口里，观察者读到 [`DisconnectReason::Unknown`]
/// ——对「关闭标签」是良性，对「重连后横幅要显示断开原因」则是错误答案。
pub struct BridgeState {
    /// 停止请求：置位后 pump 尽快退出。
    stop_requested: AtomicBool,
    /// 结束标志：pump 已退出且退出原因已写入（观察者见 `true` 即保证原因已就绪）。
    finished: AtomicBool,
    /// 退出原因（编码见 [`DisconnectReason`]）。
    reason: AtomicU8,
}

impl BridgeState {
    /// 新建未结束、未归因的桥接状态。
    pub fn new() -> Self {
        Self {
            stop_requested: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            reason: AtomicU8::new(DisconnectReason::Unknown as u8),
        }
    }

    /// 请求 pump 退出（关标签 / 关窗口）。只是请求，不代表桥接已经结束
    /// ——结束与否看 [`Self::is_finished`]。
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
    }

    /// 是否已请求停止（pump 侧与「关闭中」判定读它）。
    pub fn is_stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::SeqCst)
    }

    /// 桥接是否已结束、退出原因是否已就绪。
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// 当前退出原因；pump 尚未归因时为 [`DisconnectReason::Unknown`]。
    pub fn reason(&self) -> DisconnectReason {
        DisconnectReason::decode(self.reason.load(Ordering::SeqCst))
    }

    /// pump 收尾：先写原因、再翻结束标志，保证观察者见 `finished` 时原因必已就绪。
    fn finish(&self, reason: DisconnectReason) {
        self.reason.store(reason as u8, Ordering::SeqCst);
        self.finished.store(true, Ordering::SeqCst);
    }
}

impl Default for BridgeState {
    /// 同 [`BridgeState::new`]（未结束、未归因）。
    fn default() -> Self {
        Self::new()
    }
}

/// 连接建立后向远端 shell 注入的 prompt 钩子：让 shell 在每个提示符输出 OSC 7
/// 序列（`ESC ]7;file://<pwd> ESC \`），从而把当前工作目录上报给本桥接。
///
/// 兼容 bash 与 zsh：分别挂到 `PROMPT_COMMAND` / `precmd_functions`，且用
/// `BASH_VERSION` / `ZSH_VERSION` 守卫，非对应 shell 时静默跳过、绝不报错。
/// 赋值前用 `declare -p` / `typeset -p` 探测目标变量是否被声明为只读（部分发行版
/// 的 `/etc/profile.d` 会 `readonly PROMPT_COMMAND`），只读时跳过注入以避免打印
/// 「只读变量」错误；正则仅匹配属性标志段，故 `-r` / `-rx` / `-ar` 均可识别。
/// 末尾 `:` 为无害空命令，确保整段以换行执行；`$PWD` 本身以 `/` 开头，故
/// 输出形如 `file:///home/user`，桥接侧按 `file://` 后内容解析即可。
const CWD_BOOTSTRAP: &[u8] = b"\
    __rterm_cwd(){ \
        printf '\\033]7;file://%s\\033\\\\' \"$PWD\"; \
    }; \
    case \"$BASH_VERSION\" in \
        ?*) if [[ ! \"$(declare -p PROMPT_COMMAND 2>/dev/null)\" =~ ^declare\\ -[a-zA-Z]*r ]]; then \
                PROMPT_COMMAND=\"__rterm_cwd${PROMPT_COMMAND:+;${PROMPT_COMMAND}}\"; \
            fi ;; \
    esac; \
    case \"$ZSH_VERSION\" in \
        ?*) if [[ ! \"$(typeset -p precmd_functions 2>/dev/null)\" =~ ^typeset\\ -[a-zA-Z]*r ]]; then \
                precmd_functions+=(__rterm_cwd); \
            fi ;; \
    esac; \
    :\n";

/// 创建终端桥接所需的一切，返回供 GUI 直接消费的对象。
///
/// 该函数会：
/// 1. 建立一对进程内双向字节管道；
/// 2. 在已建立的连接上打开带 PTY 的 shell 通道；
/// 3. 后台启动桥接任务（含窗口尺寸转发）。
///
/// # 参数
/// - `conn`：已建立的 SSH 连接（内部句柄可被并发共享）。pump 在通道结束时用它探测
///   传输层是否还活着（见 [`SshConnection::is_closed`]），故按 `Arc` 接收以随桥接任务
///   一同存活。
/// - `cols` / `rows`：初始终端列数与行数，用于首帧 PTY 尺寸。
///
/// # 返回
/// - `local`：同步端 `File`，GUI 应交给 `RusshPty` 包装后接入终端渲染层。
/// - `state`：共享的桥接结束状态；桥接结束时置位断开标志并写下退出原因
///   （[`BridgeState::reason`]，供 GUI 感知远端关闭并区分「会话结束」与「连接断开」）。
/// - `resize_tx`：本地终端尺寸变更（`(列数, 行数)`）的发送端，GUI 在收到终端
///   resize 事件时调用。**丢弃它只停止尺寸转发，并不会结束桥接**——桥接要等远端
///   shell 通道 EOF（或本模块的 pump 退出）才结束，并置位 `disconnect`。
pub async fn spawn_terminal_bridge(
    conn: Arc<SshConnection>,
    cols: u32,
    rows: u32,
    cwd: CwdTracker,
    cwd_bootstrap: bool,
    suppress_bootstrap_echo: bool,
) -> Result<(File, File, Arc<BridgeState>, mpsc::Sender<(u32, u32)>), CoreError> {
    // 进程内管道：拆成 OUT（远端→本地输出）与 IN（本地→远端输入）两条独立管道。
    // 同步端（conout 读端 / conin 写端）交 GUI，异步端（out_stream / in_stream）在此泵接 russh 通道。
    let (conout_file, conin_file, out_stream, in_stream) = create_bridge()?;

    let state = Arc::new(BridgeState::new());

    // 打开 shell 通道（含 PTY 与 shell 进程）；这是整条链路上唯一的远端资源获取点。
    let channel = conn
        .open_shell_channel(cols, rows, cwd_bootstrap, suppress_bootstrap_echo)
        .await?;

    // 仅在需要追踪 cwd 且开启了 CWD_BOOTSTRAP 时，向 shell 注入 prompt 钩子，使其持续上报 OSC 7。
    // 钩子在 shell 读就绪后自动执行，无需等待 pump 启动。
    // 若同时启用了 suppress_bootstrap_echo，在脚本末尾拼接 stty echo 以恢复回显。
    if cwd.is_some() && cwd_bootstrap {
        let mut writer = channel.make_writer();
        let cmd = if suppress_bootstrap_echo {
            let mut buf = CWD_BOOTSTRAP[..CWD_BOOTSTRAP.len() - 2].to_vec();
            buf.extend_from_slice(b"stty echo\n");
            buf
        } else {
            CWD_BOOTSTRAP.to_vec()
        };
        if let Err(e) = writer.write_all(&cmd).await {
            log::debug!("Failed to inject cwd bootstrap: {e}");
        }
    }

    // 尺寸变更通道：容量 8，GUI 侧 resize 突发时丢弃最旧也不阻塞渲染。
    let (resize_tx, resize_rx) = mpsc::channel(8);

    // 泵接监听的停止请求需独立克隆：关标签 / 关窗口时由 GUI 置位使其尽快退出。
    let pump_state = state.clone();
    tokio::spawn(async move {
        // pump 退出即写下退出原因并翻转结束标志（见 `BridgeState::finish`）。
        pump(
            out_stream, in_stream, channel, resize_rx, pump_state, cwd, &conn,
        )
        .await;
        log::debug!("Terminal bridge task finished");
    });

    Ok((conout_file, conin_file, state, resize_tx))
}

/// 在异步字节流与远端 shell 通道之间双向转发数据，并在 I/O 等待间隙应用尺寸变更。
///
/// `out_stream` 用于把远端数据写到本地（GUI 的 conout 会读它）；`in_stream` 用于读
/// 取本地输入（GUI 的 conin 会写它）。二者是**两条独立管道**，故 conout 读线程与
/// conin 写线程不会落在同一管道端点上互相串行化阻塞。
///
/// `state` 为桥接结束状态：其中断开标志在关标签 / 关窗口时由 GUI 置位，使泵接尽快退出
/// （否则泵接持有服务端管道、win_io 读线程持有客户端管道并被 `ReadFile` 阻塞，二者互相
/// 等待对方关闭句柄而死锁，导致后台线程与进程残留）；退出原因由本函数按判定写下
/// （见 [`DisconnectReason`]）。
///
/// `conn` 用于在通道 EOF / 读到错误时探测传输层是否还活着：russh 里「远端 shell 退出」
/// 与「传输死亡」都表现为读返回 `Ok(0)` 或错误，唯一区分依据是会话任务是否已收尾
/// （[`SshConnection::is_closed`]）。只在退出路径上锁，不影响泵接的热路径。
async fn pump<W, R>(
    mut out_stream: W,
    mut in_stream: R,
    mut channel: russh::Channel<Msg>,
    mut resize_rx: mpsc::Receiver<(u32, u32)>,
    state: Arc<BridgeState>,
    cwd: CwdTracker,
    conn: &SshConnection,
) where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    // 复用的收发缓冲区：远端大输出（如 cat 日志）时按块转发，块越大每字节的系统调用
    // 与事件唤醒越少，故取 64 KiB（8 KiB 会让同样流量产生 8 倍次数的读写与后续同步）。
    const PUMP_BUFFER: usize = 64 * 1024;
    let mut socket_buf = [0u8; PUMP_BUFFER];
    let mut channel_buf = [0u8; PUMP_BUFFER];
    // OSC 7 序列可能被缓冲边界切断，故跨读保留未终结的序列尾部，
    // 与下一读拼接后再解析（见 `scan_osc7_cwd`）。
    let mut osc_carry = Vec::new();
    let mut total_remote = 0usize;
    let mut total_local = 0usize;
    log::debug!("Terminal bridge pump started");
    // 退出原因：每个 break 出口在跳出前把原因算好，收尾时统一写入
    // （把归因集中在出口处，避免 `request_stop` 与远端事件谁先到不同的分支里各写一遍）。
    let reason;

    loop {
        // 在两次 I/O 等待之间，将积压的窗口尺寸变更下发到远端。
        while let Ok((cols, rows)) = resize_rx.try_recv() {
            if let Err(e) = channel.window_change(cols, rows, 0, 0).await {
                log::debug!("Failed to forward window size change: {e}");
            }
        }

        tokio::select! {
            // 收到断开信号：立即退出，释放服务端管道句柄。
            _ = wait_stop(state.clone()) => {
                log::debug!("pump: received stop signal, exiting");
                reason = DisconnectReason::LocalStop;
                break;
            }
            // 远端 -> 本地：从通道读取并写入本地 OUT 管道端。
            n = async {
                let mut reader = channel.make_reader();
                reader.read(&mut channel_buf).await
            } => {
                match n {
                    Ok(0) | Err(_) => {
                        // 通道 EOF 与传输死亡在 russh 里形态一致（都表现为读返回
                        // `Ok(0)`），唯一的区分依据是会话任务是否已收尾：仍是活的
                        // 说明只是这条 shell 通道结束了（远端 shell 退出）。
                        //
                        // 停止请求优先（同 `local_side_reason`）：关标签会先置位请求、
                        // 随后连接对象才被丢弃，若不先判这个，一次正常关标签会被记成
                        // 「传输死亡」。
                        reason = if state.is_stop_requested() {
                            DisconnectReason::LocalStop
                        } else if conn.is_closed().await {
                            DisconnectReason::TransportDied
                        } else {
                            DisconnectReason::ChannelEof
                        };
                        break;
                    }
                    Ok(n) => {
                        // 在写往本地前先扫描 OSC 7 序列，提取终端 cwd。
                        if let Some(cwd) = &cwd {
                            scan_osc7_cwd(&mut osc_carry, &channel_buf[..n], cwd);
                        }
                        if out_stream.write_all(&channel_buf[..n]).await.is_err() {
                            reason = DisconnectReason::Unknown;
                            break;
                        }
                        total_remote += n;
                    }
                }
            }
            // 本地 -> 远端：从本地 IN 管道端读取并写入通道。
            n = in_stream.read(&mut socket_buf) => {
                match n {
                    // 本地管道关端（GUI 侧丢弃 / 关标签）：停止请求优先，其余按传输状态归因。
                    Ok(0) | Err(_) => {
                        reason = local_side_reason(&state, conn).await;
                        break;
                    }
                    Ok(n) => {
                        let mut writer = channel.make_writer();
                        if writer.write_all(&socket_buf[..n]).await.is_err() {
                            reason = local_side_reason(&state, conn).await;
                            break;
                        }
                        total_local += n;
                    }
                }
            }
        }
    }
    state.finish(reason);
    log::debug!(
        "Terminal bridge pump exiting (remote→local {total_remote} bytes, local→remote {total_local} bytes, reason {reason:?})"
    );
}

/// 在停止请求置位前让出，供 `tokio::select!` 监听泵接退出信号。
async fn wait_stop(state: Arc<BridgeState>) {
    while !state.is_stop_requested() {
        tokio::task::yield_now().await;
    }
}

/// 本地一侧的退出归因（本地管道关端 / 写通道失败）：停止请求优先——
/// 关标签会先置位请求、随后连接对象才被丢弃，若不先判这个，一次正常关标签
/// 会被记成「传输死亡」；其余情况按传输层状态判定（本地端先没、传输已死
/// 属于同一场故障）。无法归因时返回 [`DisconnectReason::Unknown`]。
async fn local_side_reason(state: &BridgeState, conn: &SshConnection) -> DisconnectReason {
    if state.is_stop_requested() {
        DisconnectReason::LocalStop
    } else if conn.is_closed().await {
        DisconnectReason::TransportDied
    } else {
        DisconnectReason::Unknown
    }
}

/// 扫描字节流中的 OSC 7 序列（`ESC ]7;file://<path> BEL|ST`），提取路径写入 `cwd`。
///
/// OSC 7 序列可能跨多次 `read` 被截断，故用 `carry` 保留「已出现 `ESC ]7;` 起始、
/// 但尚未遇到终结符」的尾部，与下一读拼接后继续解析。本次扫描未出现任何起始标记时，
/// `carry` 只保留末尾几个字节（起始标记本身可能被读边界切断），使普通日志输出下
/// `carry` 的长度恒为常数，不会随输出累积、也不会被反复重扫。
///
/// 只认标准 `file://` 前缀：钩子输出形如 `file:///home/user`，故取 `file://` 之后
/// 的内容即为绝对路径（含开头 `/`）。其他内容（如 `file://host/path`）会被忽略，
/// 以免误取 host 段。
fn scan_osc7_cwd(carry: &mut Vec<u8>, chunk: &[u8], cwd: &Arc<Mutex<Option<String>>>) {
    carry.extend_from_slice(chunk);
    // OSC 序列起始标记：ESC ] 7 ;
    const MARK: [u8; 4] = [0x1b, 0x5d, 0x37, 0x3b];
    let mut i = 0;
    while i + MARK.len() <= carry.len() {
        if carry[i..i + MARK.len()] != MARK {
            i += 1;
            continue;
        }
        // 从标记后寻找终结符：BEL(0x07) 或 ST(ESC \) 。
        let mut end = None;
        let mut j = i + MARK.len();
        while j < carry.len() {
            if carry[j] == 0x07 || (carry[j] == 0x1b && j + 1 < carry.len() && carry[j + 1] == 0x5c)
            {
                end = Some(j);
                break;
            }
            j += 1;
        }
        let Some(end) = end else {
            // 起始标记后无终结符：序列可能被截断，保留其后内容待下一读拼接。
            break;
        };
        let payload = &carry[i + MARK.len()..end];
        if let Some(rest) = payload.strip_prefix(b"file://") {
            let path = String::from_utf8_lossy(rest);
            if !path.is_empty()
                && let Ok(mut g) = cwd.lock()
            {
                // 仅当目录真正变化时才写入并输出 debug 日志，避免每个提示符
                // 都重复打印（OSC 7 在每个 prompt 都会上报，cwd 通常不变）。
                if g.as_deref() != Some(path.as_ref()) {
                    log::debug!("terminal cwd changed: {:?}", path);
                    *g = Some(path.into_owned());
                }
            }
        }
        // 跳过已消费序列（含终结符；ST 占两字节）。
        i = if carry[end] == 0x1b { end + 2 } else { end + 1 };
    }
    // 仅保留未处理尾部：有进展时从上次消费处截断；本次完全没有出现起始标记时，
    // 只保留末尾几个字节（标记可能被读边界切断），避免下一次重复扫描整段累积内容。
    if i > 0 {
        *carry = carry.split_off(i);
    } else if carry.len() > MARK.len() - 1 {
        *carry = carry.split_off(carry.len() - (MARK.len() - 1));
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
    local.set_nonblocking(true).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("Failed to set socketpair sync end to non-blocking: {e}"),
        )
    })?;
    let local = unsafe { File::from_raw_fd(local.into_raw_fd()) };
    remote.set_nonblocking(true).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("Failed to set socketpair async end to non-blocking: {e}"),
        )
    })?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 停止请求与「已结束」是两个独立信号：请求停止不会提前把桥接标成已结束，
    /// 更不会动原因——原因只能由 pump 收尾时写下。
    #[test]
    fn a_stop_request_is_not_a_finish() {
        let state = BridgeState::new();
        assert!(!state.is_stop_requested());
        assert!(!state.is_finished());
        assert_eq!(state.reason(), DisconnectReason::Unknown);

        state.request_stop();
        assert!(state.is_stop_requested());
        assert!(!state.is_finished(), "停止只是请求，pump 尚未退出并归因");
        assert_eq!(state.reason(), DisconnectReason::Unknown);

        state.finish(DisconnectReason::LocalStop);
        assert!(state.is_finished());
        assert_eq!(state.reason(), DisconnectReason::LocalStop);
    }

    /// 三种原因的编码必须可原样解码回来，且每个枚举值都对应不同的 `u8`
    /// （否则 GUI 会读到错误的类别，或两类断开共用同一个数）。
    #[test]
    fn every_reason_round_trips_through_its_code() {
        for reason in [
            DisconnectReason::Unknown,
            DisconnectReason::TransportDied,
            DisconnectReason::ChannelEof,
            DisconnectReason::LocalStop,
        ] {
            assert_eq!(DisconnectReason::decode(reason as u8), reason);
        }

        let mut codes = [
            DisconnectReason::Unknown as u8,
            DisconnectReason::TransportDied as u8,
            DisconnectReason::ChannelEof as u8,
            DisconnectReason::LocalStop as u8,
        ]
        .to_vec();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 4, "四种原因不得共用编码");

        // 越界 / 未定义编码（如未来版本新增原因后旧 GUI 读到新字节）按 Unknown 处理。
        assert_eq!(DisconnectReason::decode(200), DisconnectReason::Unknown);
    }
}
