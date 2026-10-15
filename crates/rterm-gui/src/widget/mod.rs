//! 自定义 iced 部件集合。
//!
//! 包含终端部件 [`term`]（封装 PTY 后端、密钥绑定、主题与渲染，供右侧终端区复用）
//! 与 toast 通知部件 [`toast`]（基于 `iced_toaster` 源码纳入定制，关闭按钮与弹窗统一）。
//! 另含 [`measured_column::MeasuredColumn`]：两趟布局、宽度自适应最长子项的竖排列。
pub mod measured_column;
pub mod term;
pub mod toast;
