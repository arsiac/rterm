//! 基于 russh-sftp 的远程文件管理操作封装。
//!
//! [`SftpClient`] 持有已建立的 [`SftpSession`]，向 GUI 提供目录列表、
//! 上传 / 下载 / 重命名 / 删除 / 建目录等高层操作。

use crate::{CoreError, FileEntry};
use log::debug;
use russh_sftp::client::SftpSession;
use std::path::Path;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

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
        debug!("解析远端绝对路径: {path}");
        self.session
            .canonicalize(path)
            .await
            .map_err(|e| CoreError::sftp("解析路径失败", e))
    }

    /// 列出远程目录内容。
    ///
    /// 结果按 [`FileEntry`] 返回，便于 UI 直接渲染。
    pub async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>, CoreError> {
        debug!("列举目录: {path}");
        let mut dir = self
            .session
            .read_dir(path)
            .await
            .map_err(|e| CoreError::sftp("读取目录失败", e))?;
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
        debug!("目录 {} 共 {} 项", path, entries.len());
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
        debug!("创建目录: {path}");
        self.session
            .create_dir(path)
            .await
            .map_err(|e| CoreError::sftp("创建目录失败", e))
    }

    /// 删除远端单个文件（无法删除目录）。
    pub async fn remove_file(&self, path: &str) -> Result<(), CoreError> {
        debug!("删除文件: {path}");
        self.session
            .remove_file(path)
            .await
            .map_err(|e| CoreError::sftp("删除文件失败", e))
    }

    /// 仅能删除空目录。
    pub async fn remove_dir(&self, path: &str) -> Result<(), CoreError> {
        debug!("删除目录: {path}");
        self.session
            .remove_dir(path)
            .await
            .map_err(|e| CoreError::sftp("删除目录失败", e))
    }

    /// 重命名 / 移动远端文件或目录（跨目录即移动语义）。
    pub async fn rename(&self, from: &str, to: &str) -> Result<(), CoreError> {
        debug!("重命名: {from} -> {to}");
        self.session
            .rename(from, to)
            .await
            .map_err(|e| CoreError::sftp("重命名失败", e))
    }

    /// 分块上传本地文件到远端，并通过回调上报进度。
    ///
    /// `on_progress` 在开始（已传 0 字节）与每写入一块后各调用一次，参数为（文件名, 已传字节, 总字节）。
    /// 适用于需要展示进度条的上传场景；文件过大时也比一次性读入内存更省内存。
    pub async fn upload_with_progress(
        &self,
        local: &Path,
        remote: &str,
        on_progress: impl FnMut(&str, u64, u64) + Send,
    ) -> Result<(), CoreError> {
        debug!("上传: {} -> {remote}", local.display());
        let name = local
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| remote.to_string());
        let total = tokio::fs::metadata(local)
            .await
            .map_err(CoreError::Io)?
            .len();
        let local_file = tokio::fs::File::open(local).await.map_err(CoreError::Io)?;
        let reader = BufReader::new(local_file);
        let mut remote_file = self
            .session
            .create(remote)
            .await
            .map_err(|e| CoreError::sftp("创建远端文件失败", e))?;
        copy_with_progress(
            &name,
            total,
            reader,
            &mut remote_file,
            on_progress,
            CoreError::Io,
            |e| CoreError::sftp("写入远端失败", e),
        )
        .await?;
        remote_file
            .shutdown()
            .await
            .map_err(|e| CoreError::sftp("关闭远端文件失败", e))?;
        debug!("上传完成: {remote} ({} 字节)", total);
        Ok(())
    }

    /// 分块下载远端文件到本地，并通过回调上报进度。
    ///
    /// `on_progress` 在开始（已传 0 字节）与每读取一块后各调用一次，参数为（文件名, 已传字节, 总字节）。
    /// 总字节数由远端元数据获取，获取失败时为 0（调用方据此仅显示已传字节）。
    pub async fn download_with_progress(
        &self,
        remote: &str,
        local: &Path,
        on_progress: impl FnMut(&str, u64, u64) + Send,
    ) -> Result<(), CoreError> {
        debug!("下载: {remote} -> {}", local.display());
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
        let mut remote_file = self
            .session
            .open(remote)
            .await
            .map_err(|e| CoreError::sftp("打开远端文件失败", e))?;
        let mut local_file = tokio::fs::File::create(local)
            .await
            .map_err(CoreError::Io)?;
        copy_with_progress(
            &name,
            total,
            &mut remote_file,
            &mut local_file,
            on_progress,
            |e| CoreError::sftp("读取远端失败", e),
            CoreError::Io,
        )
        .await?;
        local_file.flush().await.map_err(CoreError::Io)?;
        debug!("下载完成: {} ({} 字节)", local.display(), total);
        Ok(())
    }
}

/// 通用的分块拷贝主循环：从 `reader` 读、向 `writer` 写，每完成一块通过 `on_progress` 上报。
///
/// 上传与下载仅「数据源 / 数据汇」与「读 / 写错误上下文」不同，主循环结构完全对称，
/// 故抽此共享实现；`map_read` / `map_write` 将底层 I/O 错误映射为对应的 [`CoreError`]
/// （具体变体由调用方传入的闭包决定，本地侧通常为 `Io`、远端侧为 `Sftp`）。
async fn copy_with_progress<R, W, F>(
    name: &str,
    total: u64,
    mut reader: R,
    mut writer: W,
    mut on_progress: F,
    map_read: impl Fn(std::io::Error) -> CoreError,
    map_write: impl Fn(std::io::Error) -> CoreError,
) -> Result<(), CoreError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    F: FnMut(&str, u64, u64),
{
    let mut buf = vec![0u8; 64 * 1024];
    let mut transferred: u64 = 0;
    on_progress(name, 0, total);
    loop {
        let n = reader.read(&mut buf).await.map_err(&map_read)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).await.map_err(&map_write)?;
        transferred += n as u64;
        on_progress(name, transferred, total);
    }
    Ok(())
}
