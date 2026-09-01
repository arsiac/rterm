//! 连接状态与 SFTP 文件条目等运行时数据模型定义。
//!
//! 注意：会话连接配置（`SessionConfig` / `AuthMethod`）已迁移至配置层
//! [`rterm_config`]，此处仅保留与 GUI / SFTP 运行时相关的类型。

/// 单个 SSH 连接的运行时状态，供 GUI 展示连接指示灯与提示文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// 尚未建立连接或已断开。
    Disconnected,
    /// 正在握手 / 认证中。
    Connecting,
    /// 连接已建立，可正常使用。
    Connected,
    /// 连接出错（认证失败、传输异常等）。
    Error,
}

/// SFTP 远程目录中的单个文件或文件夹条目。
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// 文件或目录名称。
    pub name: String,
    /// 是否为目录（决定在文件面板中的图标与可展开性）。
    pub is_dir: bool,
    /// 文件大小（字节）；目录通常为 0。
    pub size: u64,
    /// 最后修改时间（已格式化字符串）；未知时为 `None`。
    pub modified: Option<String>,
}
