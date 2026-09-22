//! 基于 russh-sftp 的远程文件管理操作封装。
//!
//! [`SftpClient`] 持有已建立的 [`SftpSession`]，向 GUI 提供目录列表、
//! 上传 / 下载 / 重命名 / 删除 / 建目录等高层操作。

use crate::{CoreError, CoreErrorKind, FileEntry};
use log::{debug, warn};
use russh_sftp::client::SftpSession;
use russh_sftp::client::fs::File;
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use std::io::SeekFrom;
use std::path::Path;
use std::time::SystemTime;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt, BufReader};

/// 源端指纹：续传判定中用来确认「还是同一个文件」。
///
/// 必须含修改时间而非只有大小：只比大小时，源端被换成一个更长的同名文件会被判为可续传，
/// 产出「旧前段 + 新后段」的损坏文件。宁可误判为不可续传。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    /// 源端字节数（服务端不返回大小时为 0）。
    pub len: u64,
    /// 源端修改时间（SFTP v3 只有**秒**级精度；服务端不返回时为 `None`）。
    pub modified: Option<SystemTime>,
}

/// 一次传输的起点。
///
/// 用具名枚举而非裸 `u64`：这是个搞错就会静默损坏文件的参数，调用点上必须自解释。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResumeAt {
    /// 从头开始：截断目标。
    #[default]
    Start,
    /// 从指定字节继续。
    Offset(u64),
}

impl ResumeAt {
    /// 起始字节数（[`Start`](Self::Start) 即 0）。
    pub fn offset(self) -> u64 {
        match self {
            Self::Start => 0,
            Self::Offset(n) => n,
        }
    }
}

/// 远程文件管理客户端：持有已建立的 [`SftpSession`]，向 GUI 提供高层文件操作。
pub struct SftpClient {
    /// 已建立的 SFTP 会话（底层 russh 连接）。
    session: SftpSession,
}

impl SftpClient {
    /// 基于已建立的 SFTP 会话构造客户端。
    pub fn new(session: SftpSession) -> Self {
        Self { session }
    }

    /// 将传入路径解析为远端绝对路径（调用 SFTP REALPATH）。
    ///
    /// `~` 这类客户端侧的展开是否生效完全取决于远端服务器对 REALPATH 的实现；
    /// 传入空串或 “.” 可取回服务端当前工作目录。
    pub async fn resolve_path(&self, path: &str) -> Result<String, CoreError> {
        debug!("Resolving remote absolute path: {path}");
        self.session
            .canonicalize(path)
            .await
            .map_err(|e| CoreError::sftp(CoreErrorKind::ParsePath, e))
    }

    /// 列出远程目录内容。
    ///
    /// 结果按 [`FileEntry`] 返回，便于 UI 直接渲染。
    pub async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>, CoreError> {
        debug!("Listing directory: {path}");
        let mut dir = self
            .session
            .read_dir(path)
            .await
            .map_err(|e| CoreError::sftp(CoreErrorKind::ReadDir, e))?;
        let mut entries = Vec::new();
        for entry in dir.by_ref() {
            let meta = entry.metadata();
            let is_dir = entry.file_type().is_dir();
            let size = meta.len();
            // 将远端返回的 `SystemTime` 格式化为本地可读时间（避免直接 `{:?}` 打印成结构体）。
            let modified = meta.modified().ok().map(|t| {
                let dt: chrono::DateTime<chrono::Local> = t.into();
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            });
            entries.push(FileEntry {
                name: entry.file_name(),
                is_dir,
                size,
                modified,
                // 服务端（尤其 Windows 上的 SFTP 实现）可能不返回属主 / 属组，
                // 此时退化为 uid / gid 数字，二者都缺则为 None。
                permissions: meta.permissions,
                user: meta
                    .user
                    .clone()
                    .or_else(|| meta.uid.map(|u| u.to_string())),
                group: meta
                    .group
                    .clone()
                    .or_else(|| meta.gid.map(|g| g.to_string())),
            });
        }
        debug!("Directory {} has {} item(s)", path, entries.len());
        // 先按类型（目录在前）排序，再按名称字典序，便于用户浏览。
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(entries)
    }

    /// 在远端创建目录。
    ///
    /// 只发一次 `mkdir`、**不会**逐级创建父目录（russh-sftp 无 `mkdir -p` 语义），
    /// 父目录不存在时直接返回错误。
    pub async fn create_dir(&self, path: &str) -> Result<(), CoreError> {
        debug!("Creating directory: {path}");
        self.session
            .create_dir(path)
            .await
            .map_err(|e| CoreError::sftp(CoreErrorKind::CreateDir, e))
    }

    /// 删除远端单个文件（无法删除目录）。
    pub async fn remove_file(&self, path: &str) -> Result<(), CoreError> {
        debug!("Removing file: {path}");
        self.session
            .remove_file(path)
            .await
            .map_err(|e| CoreError::sftp(CoreErrorKind::DeleteFile, e))
    }

    /// 仅能删除空目录。
    pub async fn remove_dir(&self, path: &str) -> Result<(), CoreError> {
        debug!("Removing directory: {path}");
        self.session
            .remove_dir(path)
            .await
            .map_err(|e| CoreError::sftp(CoreErrorKind::DeleteDir, e))
    }

    /// 重命名 / 移动远端文件或目录（跨目录即移动语义）。
    pub async fn rename(&self, from: &str, to: &str) -> Result<(), CoreError> {
        debug!("Renaming: {from} -> {to}");
        self.session
            .rename(from, to)
            .await
            .map_err(|e| CoreError::sftp(CoreErrorKind::Rename, e))
    }

    /// 读取远端文件的大小与修改时间，作为续传判定的源端指纹。只做一次 stat，不读内容。
    ///
    /// 失败时把错误原样交回：「拿不到指纹就当作不可续传」是调用方的策略，核心层不替它决定。
    pub async fn remote_fingerprint(&self, remote: &str) -> Result<Fingerprint, CoreError> {
        debug!("Reading remote fingerprint: {remote}");
        let meta = self
            .session
            .metadata(remote)
            .await
            .map_err(|e| CoreError::sftp(CoreErrorKind::RemoteMetadata, e))?;
        Ok(Fingerprint {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }

    /// 分块上传本地文件到远端，并通过回调上报进度。
    ///
    /// `on_progress` 在开始（已传字节 = `resume` 的偏移）与每写入一块后各调用一次，
    /// 参数为（文件名, 已传字节, 总字节）。总字节数取本地文件元数据。
    ///
    /// `resume` 决定起点：`Start` 截断远端目标，`Offset(n)` 定位到第 n 字节续写。
    ///
    /// **失败时远端目标会被截回已确证前缀**（见 [`Watermark`]），让下一轮按 `len` 算出的断点
    /// 仍指向一段连续的数据。
    pub async fn upload_with_progress(
        &self,
        local: &Path,
        remote: &str,
        resume: ResumeAt,
        on_progress: impl FnMut(&str, u64, u64) + Send,
    ) -> Result<(), CoreError> {
        let start = resume.offset();
        debug!("Uploading: {} -> {remote} ({:?})", local.display(), resume);
        let name = local
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| remote.to_string());
        let total = tokio::fs::metadata(local)
            .await
            .map_err(CoreError::Io)?
            .len();
        let local_file = tokio::fs::File::open(local).await.map_err(CoreError::Io)?;
        let mut reader = BufReader::new(local_file);
        // 两侧都要移到第 n 字节：只 seek 远端会把本地前 n 字节再写一遍。
        if start > 0 {
            reader
                .seek(SeekFrom::Start(start))
                .await
                .map_err(CoreError::Io)?;
        }
        let mut remote_file = if start == 0 {
            self.session
                .create(remote)
                .await
                .map_err(|e| CoreError::sftp(CoreErrorKind::CreateRemoteFile, e))?
        } else {
            // 只有 WRITE：半成品必须已存在（故不带 CREATE），且绝不能截断已传的字节（故不带 TRUNCATE）。
            self.session
                .open_with_flags(remote, OpenFlags::WRITE)
                .await
                .map_err(|e| CoreError::sftp(CoreErrorKind::OpenRemoteFile, e))?
        };
        if start > 0 {
            remote_file
                .seek(SeekFrom::Start(start))
                .await
                .map_err(CoreError::Io)?;
        }
        let mut watermark = Watermark::tracking(start);
        let copied = copy_with_progress(
            CopyInfo {
                name: &name,
                total,
                start,
            },
            reader,
            &mut remote_file,
            on_progress,
            &mut watermark,
            CoreError::Io,
            |e| CoreError::sftp(CoreErrorKind::WriteRemote, e),
        )
        .await;
        // 排空未确认的写再关句柄：否则调用方随后立刻改名，可能改到一个还不完整的文件。
        let ended = match copied {
            Ok(_) => remote_file
                .shutdown()
                .await
                .map_err(|e| CoreError::sftp(CoreErrorKind::CloseRemoteFile, e)),
            Err(e) => Err(e),
        };
        if let Err(e) = ended {
            // 失败轮次把远端截回已确证前缀：在途的写可能已经越过失败点，留下全零空洞，
            // 而调用方（和下一轮的续传判定）只看得到 `len`。best-effort：截不动时后果
            // 与不修相同，绝不能让它掩盖本轮真正的失败原因。
            trim_remote(&remote_file, watermark.confirmed, remote).await;
            return Err(e);
        }
        debug!("Upload complete: {remote} ({total} bytes)");
        Ok(())
    }

    /// 分块下载远端文件到本地，并通过回调上报进度。
    ///
    /// `on_progress` 在开始（已传字节 = `resume` 的偏移）与每读取一块后各调用一次，
    /// 参数为（文件名, 已传字节, 总字节）。总字节数由远端元数据获取，获取失败时为 0
    /// （调用方据此仅显示已传字节）。
    ///
    /// `resume` 决定起点：`Start` 截断本地目标，`Offset(n)` 定位到第 n 字节续写。
    pub async fn download_with_progress(
        &self,
        remote: &str,
        local: &Path,
        resume: ResumeAt,
        on_progress: impl FnMut(&str, u64, u64) + Send,
    ) -> Result<(), CoreError> {
        debug!(
            "Downloading: {remote} -> {} ({:?})",
            local.display(),
            resume
        );
        let name = remote
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(remote)
            .to_string();
        let total = self
            .session
            .metadata(remote)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let start = resume.offset();
        let mut remote_file = self
            .session
            .open(remote)
            .await
            .map_err(|e| CoreError::sftp(CoreErrorKind::OpenRemoteFile, e))?;
        // `Offset(0)` 与 `Start` 等价：起点为 0 一律走截断路径。
        let mut local_file = if start == 0 {
            tokio::fs::File::create(local)
                .await
                .map_err(CoreError::Io)?
        } else {
            // 不能 create：那会截断上一轮已落盘的字节。
            tokio::fs::OpenOptions::new()
                .write(true)
                .open(local)
                .await
                .map_err(CoreError::Io)?
        };
        if start > 0 {
            remote_file
                .seek(SeekFrom::Start(start))
                .await
                .map_err(CoreError::Io)?;
            local_file
                .seek(SeekFrom::Start(start))
                .await
                .map_err(CoreError::Io)?;
        }
        let mut watermark = Watermark::disabled(start);
        let written = copy_with_progress(
            CopyInfo {
                name: &name,
                total,
                start,
            },
            &mut remote_file,
            &mut local_file,
            on_progress,
            &mut watermark,
            |e| CoreError::sftp(CoreErrorKind::ReadRemote, e),
            CoreError::Io,
        )
        .await?;
        local_file.flush().await.map_err(CoreError::Io)?;
        // 截到本轮实际到达的字节数：源端中途变短时续传会留下上一轮的尾巴（从头下载时是空操作）。
        local_file.set_len(written).await.map_err(CoreError::Io)?;
        debug!("Download complete: {} ({written} bytes)", local.display());
        Ok(())
    }
}

/// 上传确证水位步进：4 MiB。越小则失败后白传的越少，但每次确证都要排空一次写流水线——
/// 1 MiB 在慢链路（1 MB/s、RTT 200 ms）上吞吐掉近两成，4 MiB 约 1.5%。
const CONFIRM_INTERVAL: u64 = 4 * 1024 * 1024;

/// 「已确证前缀」水位：只有服务器收下过的偏移，才配当下一轮的断点。
///
/// 因为 russh-sftp 的 `File::poll_write` 把 ack 排队、只在深度触顶时才回收：第 j 个写的失败
/// 往往到第 j+N 个才被发现，其间的写早已落盘，于是暂存里出现全零空洞而 `len` 更大（详见设计
/// 文档 §15.1）。所以每写满 [`CONFIRM_INTERVAL`] 就 `flush` 一次并推进 `confirmed`；失败时调用方
/// 把远端截回该处，让 `len` 重新等价于「可信连续前缀」。
#[derive(Debug, Clone, Copy)]
struct Watermark {
    /// 每推进多少字节做一次确证；`0` = 不确证（下载侧的写入汇是本地文件，本就同步）。
    every: u64,
    /// 最后一个已确证的**绝对**偏移。
    confirmed: u64,
}

impl Watermark {
    fn tracking(start: u64) -> Self {
        Self {
            every: CONFIRM_INTERVAL,
            confirmed: start,
        }
    }

    fn disabled(start: u64) -> Self {
        Self {
            every: 0,
            confirmed: start,
        }
    }

    fn should_confirm(&self, transferred: u64) -> bool {
        self.every > 0 && transferred - self.confirmed >= self.every
    }

    fn confirm(&mut self, transferred: u64) {
        self.confirmed = transferred;
    }
}

/// 拷贝过程中回调给调用方的展示信息（三者总是同时出现，只服务进度回调）。
#[derive(Debug, Clone, Copy)]
struct CopyInfo<'a> {
    /// 显示名（上传取本地文件名，下载取远端文件名）。
    name: &'a str,
    /// 源端总字节数（服务端不返回时为 0）。
    total: u64,
    /// 本次拷贝的起始偏移。
    start: u64,
}

/// 用 SETSTAT 把远端文件的长度截回 `confirmed`，让 `len` 重新等价于「可信连续前缀」。
async fn trim_remote(file: &File, confirmed: u64, remote: &str) {
    let mut attrs = FileAttributes::empty();
    attrs.size = Some(confirmed);
    if let Err(e) = file.set_metadata(attrs).await {
        warn!("could not trim {remote} back to {confirmed} confirmed bytes: {e}");
    } else {
        debug!("trimmed {remote} back to {confirmed} confirmed bytes");
    }
}

/// 通用的分块拷贝主循环：从 `reader` 读、向 `writer` 写，每完成一块通过 `on_progress` 上报。
///
/// `info.start` 让首个回调即报 `(start, total)`（否则续传时 UI 先闪一次 0%），返回值是
/// **绝对**偏移 `start + 本轮写入字节数`。
///
/// 上传与下载只有数据源 / 数据汇与错误上下文不同，故共享此实现；`map_read` / `map_write`
/// 把底层 I/O 错误映射为对应的 [`CoreError`] 变体。`watermark` 见 [`Watermark`]。
async fn copy_with_progress<R, W, F>(
    info: CopyInfo<'_>,
    mut reader: R,
    mut writer: W,
    mut on_progress: F,
    watermark: &mut Watermark,
    map_read: impl Fn(std::io::Error) -> CoreError,
    map_write: impl Fn(std::io::Error) -> CoreError,
) -> Result<u64, CoreError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    F: FnMut(&str, u64, u64),
{
    let CopyInfo { name, total, start } = info;
    let mut buf = vec![0u8; 64 * 1024];
    let mut transferred: u64 = start;
    on_progress(name, transferred, total);
    loop {
        let n = reader.read(&mut buf).await.map_err(&map_read)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).await.map_err(&map_write)?;
        transferred += n as u64;
        on_progress(name, transferred, total);
        if watermark.should_confirm(transferred) {
            writer.flush().await.map_err(&map_write)?;
            watermark.confirm(transferred);
        }
    }
    Ok(transferred)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 在临时 tokio 运行时里跑一段异步逻辑（本模块不依赖 `#[tokio::test]`，保持依赖面收窄）。
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn resume_at_reports_its_offset() {
        assert_eq!(ResumeAt::Start.offset(), 0);
        assert_eq!(ResumeAt::Offset(4096).offset(), 4096);
        assert_eq!(ResumeAt::default(), ResumeAt::Start, "默认从头开始");
    }

    #[test]
    fn copy_reports_the_start_offset_on_its_first_callback() {
        // 首个回调必须是 `(start, total)`，否则续传时 UI 会先闪一次 0%。
        let (written, seen, written_bytes) = block_on(async {
            let mut reader: &[u8] = b"hello world";
            let mut writer: Vec<u8> = Vec::new();
            let mut seen: Vec<(u64, u64)> = Vec::new();
            let written = copy_with_progress(
                CopyInfo {
                    name: "f.bin",
                    total: 11,
                    start: 3,
                },
                &mut reader,
                &mut writer,
                |_name, transferred, total| seen.push((transferred, total)),
                &mut Watermark::disabled(0),
                CoreError::Io,
                CoreError::Io,
            )
            .await
            .unwrap();
            (written, seen, writer)
        });
        assert_eq!(seen.first().copied(), Some((3, 11)), "首个回调带起始偏移");
        assert_eq!(written, 14, "返回值是绝对偏移：3 + 本轮写入的 11");
        assert_eq!(written_bytes, b"hello world", "写入内容与读到的完全一致");
    }

    #[test]
    fn copy_stops_at_eof_and_keeps_the_offset_when_there_is_nothing_left() {
        // 半成品恰好等于源端长度时的重试：必须立刻 EOF 收尾，否则退化成重传整个文件。
        let (written, seen) = block_on(async {
            let mut reader: &[u8] = b"";
            let mut writer: Vec<u8> = Vec::new();
            let mut seen: Vec<(u64, u64)> = Vec::new();
            let written = copy_with_progress(
                CopyInfo {
                    name: "f.bin",
                    total: 11,
                    start: 11,
                },
                &mut reader,
                &mut writer,
                |_name, transferred, total| seen.push((transferred, total)),
                &mut Watermark::disabled(0),
                CoreError::Io,
                CoreError::Io,
            )
            .await
            .unwrap();
            (written, seen)
        });
        assert_eq!(written, 11, "没有可读字节 → 偏移不变");
        assert_eq!(seen, vec![(11, 11)], "只回调一次，且带的是断点而非 0");
    }

    #[test]
    fn copy_from_zero_calls_back_once_for_an_empty_source() {
        let (written, seen) = block_on(async {
            let mut reader: &[u8] = b"";
            let mut writer: Vec<u8> = Vec::new();
            let mut seen: Vec<(u64, u64)> = Vec::new();
            let written = copy_with_progress(
                CopyInfo {
                    name: "f.bin",
                    total: 0,
                    start: 0,
                },
                &mut reader,
                &mut writer,
                |_name, transferred, total| seen.push((transferred, total)),
                &mut Watermark::disabled(0),
                CoreError::Io,
                CoreError::Io,
            )
            .await
            .unwrap();
            (written, seen)
        });
        assert_eq!(written, 0);
        assert_eq!(seen, vec![(0, 0)], "空源只回调一次");
    }

    #[test]
    fn copy_reads_the_whole_source_in_chunks_and_reports_monotonic_offsets() {
        // 跨过 64 KiB 边界：源 200 KiB，期望 4 次回调（首块 + 3 块）且偏移单调递增到 total。
        let data = vec![7u8; 200 * 1024];
        let (written, seen, sink) = block_on(async {
            let mut reader: &[u8] = &data;
            let mut writer: Vec<u8> = Vec::new();
            let mut seen: Vec<u64> = Vec::new();
            let written = copy_with_progress(
                CopyInfo {
                    name: "f.bin",
                    total: data.len() as u64,
                    start: 0,
                },
                &mut reader,
                &mut writer,
                |_name, transferred, _total| seen.push(transferred),
                &mut Watermark::disabled(0),
                CoreError::Io,
                CoreError::Io,
            )
            .await
            .unwrap();
            (written, seen, writer)
        });
        assert_eq!(written, 200 * 1024, "返回值等于源长度");
        assert_eq!(sink.len(), 200 * 1024, "全部字节都写进去了");
        assert_eq!(seen[0], 0, "首个回调是 0（本例起点为 0）");
        assert!(
            seen.windows(2).all(|w| w[0] < w[1]),
            "偏移必须单调递增：{seen:?}"
        );
        assert_eq!(seen.last().copied(), Some(200 * 1024), "末次回调到 total");
    }
}
