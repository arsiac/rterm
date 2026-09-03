//! 子组件上行事件落地：把各 `Message::XxxEvent` 的 `Xxx::Event` 写回父状态。

use crate::app::App;
use crate::app::connect;
use crate::app::contexts;
use crate::app::terminal_bridge;
use crate::app::{masterpw, session, settings, sftp, tabs, transfer, updates};
use crate::message::Message;
use crate::vault_keyring;
use crate::widget::term::{BackendCommand, Command as TermCommand};
use iced::Task;
use rust_i18n;

/// 落地 `session::Event`：把会话模块上行的意图（连接 / 打开文件 / 删除 / 状态 / toast / 自回路）
/// 写回父状态或委托父层级操作。
pub(crate) fn apply_session_event(app: &mut App, e: session::Event) -> Task<Message> {
    match e {
        // 以下各分支把模块上行的意图落地为父状态变更；模块绝不写父状态。
        session::Event::Connect(id) => {
            // 开标签并异步发起连接（标签 / 连接生命周期属父层，不在会话模块内）。
            let tab_id = connect::open_tab(app, &id);
            connect::connect_session(app, tab_id, &id)
        }
        session::Event::OpenFiles(id) => {
            // 打开该会话的文件管理：委托 `open_files`（按当前活动标签建立 SFTP）。
            connect::open_files(app, &id).map(Message::Sftp)
        }
        session::Event::SessionDeleted(id) => {
            // 关闭该会话的全部终端标签（标签各自持有的连接随标签 drop 释放）。
            connect::close_session_tabs(app, &id);
            Task::none()
        }
        session::Event::Status(s) => {
            app.status = s;
            Task::none()
        }
        session::Event::Toast(kind, msg) => {
            contexts::set_toast(app, kind, msg);
            Task::none()
        }
        // 自回路：把内部消息派发回会话模块自身，形成「文件对话框完成 → 回填字段 / 导入导出」闭环。
        session::Event::Emit(m) => {
            let ctx = contexts::session_ctx(app);
            app.session.update(*m, &ctx).map(Message::SessionEvent)
        }
    }
}

/// 落地 `tabs::Event`：把终端标签模块上行的导航 / 焦点 / 状态意图写回父状态，
/// 或委托父层级操作（开 SFTP / 起桥接 / 挂组件 / 转终端事件 / 重绘 / 滚标签栏）。
pub(crate) fn apply_tabs_event(app: &mut App, e: tabs::Event) -> Task<Message> {
    match e {
        // 以下各分支把模块上行的意图落地为父状态变更；模块绝不写父状态。
        tabs::Event::SetActiveSession(s) => {
            app.active_session = s;
            Task::none()
        }
        tabs::Event::SetCenter(v) => {
            app.center = v;
            Task::none()
        }
        tabs::Event::SetTerminalFocused(b) => {
            app.terminal_focused = b;
            Task::none()
        }
        tabs::Event::SetWindowFocusSaved(o) => {
            app.window_focus_saved = o;
            Task::none()
        }
        tabs::Event::SetStatus(s) => {
            app.status = Some(s);
            Task::none()
        }
        // 自动打开该会话的 SFTP：委托 `open_files`（挑标签 / 切导航态 / 建通道）。
        tabs::Event::OpenSftp(id) => connect::open_files(app, &id).map(Message::Sftp),
        // 为已连接标签拉起终端桥接（标签 / 连接生命周期属父层，不在标签模块内）。
        tabs::Event::OpenTerminalBridge(tab_id, conn) => {
            terminal_bridge::open_terminal_bridge(app, tab_id, conn)
        }
        // 桥接就绪后挂载终端组件（widget 生命周期属父层）。
        tabs::Event::SpawnTerminal(tab_id, conout, conin, disc, resize) => {
            terminal_bridge::spawn_terminal_widget(app, tab_id, conout, conin, disc, resize)
        }
        // 关闭标签时清理其挂起的主机密钥确认。
        tabs::Event::RemoveHostKeyForTab(tab_id) => {
            app.hostkey.remove_for_tab(tab_id);
            Task::none()
        }
        // 转发终端部件事件给父层（父层拥有的 widget 交互逻辑）。
        tabs::Event::TerminalEvent(ev) => terminal_bridge::handle_terminal_event(app, ev),
        // 终端挂载完成后强制重绘一次，避免 canvas 缓存停留在空白首帧。
        tabs::Event::TerminalReady(tab_id) => refresh_terminal(app, tab_id),
        // 切标签后滚动标签栏到目标位置。
        tabs::Event::ScrollTo(x) => iced::widget::operation::scroll_to(
            app.tabs.scroll_id(),
            iced::widget::scrollable::AbsoluteOffset { x, y: 0.0 },
        ),
        // 自回路：把内部消息派发回标签模块自身，形成「SwitchTab → SelectTab」闭环。
        tabs::Event::Emit(m) => {
            let ctx = contexts::tabs_ctx(app);
            app.tabs.update(*m, &ctx, &app.sftp).map(Message::TabsEvent)
        }
    }
}

/// 终端挂载完成后强制重绘一次，避免 canvas 缓存停留在空白首帧。
pub(crate) fn refresh_terminal(app: &mut App, tab_id: u64) -> Task<Message> {
    if let Some(t) = app.tabs.tab_mut(tab_id)
        && let Some(term) = t.terminal.as_mut()
    {
        term.handle(TermCommand::ProxyToBackend(BackendCommand::Resize(
            None, None,
        )));
    }
    Task::none()
}

/// 落地 `sftp::Event`：把 SFTP 模块上行的 toast / 导航 / 焦点 / 传输意图写回父状态，
/// 或转发给传输模块（上传 / 下载）、自回路回派发。
pub(crate) fn apply_sftp_event(app: &mut App, e: sftp::Event) -> Task<Message> {
    match e {
        sftp::Event::Toast(kind, msg) => {
            contexts::set_toast(app, kind, msg);
            Task::none()
        }
        sftp::Event::NavigateTo(v) => {
            app.center = v;
            Task::none()
        }
        // 内联输入态退出：焦点回到终端这一主区域。
        sftp::Event::FocusTerminal => {
            app.terminal_focused = true;
            Task::none()
        }
        // 上传 / 下载意图上行：转发给传输模块，由其管理队列执行（父层只做路由，不碰传输态）。
        sftp::Event::StartUpload(paths) => {
            let tab_id = app.tabs.active().unwrap_or(0);
            let ctx = contexts::transfer_ctx(app);
            app.transfer
                .update(transfer::Message::Upload(tab_id, paths), &ctx)
                .map(Message::TransferEvent)
        }
        sftp::Event::StartDownload(name, local) => {
            let tab_id = app.tabs.active().unwrap_or(0);
            let ctx = contexts::transfer_ctx(app);
            app.transfer
                .update(transfer::Message::Download(tab_id, name, local), &ctx)
                .map(Message::TransferEvent)
        }
        // 自回路：把内部消息派发回 SFTP 模块自身，形成「写操作完成 → 重新列举」等闭环。
        sftp::Event::Emit(m) => app
            .sftp
            .update(*m, app.tabs.active().unwrap_or(0))
            .map(Message::SftpEvent),
    }
}

/// 落地 `transfer::Event`：把传输模块上行的 toast / 刷新目录 / 自回路意图写回父状态或转发。
pub(crate) fn apply_transfer_event(app: &mut App, e: transfer::Event) -> Task<Message> {
    match e {
        transfer::Event::Toast(kind, msg) => {
            contexts::set_toast(app, kind, msg);
            Task::none()
        }
        // 上传成功：刷新对应标签的 SFTP 目录——经 `Message::Sftp` 派发给 SFTP 模块，
        // 由父层中转，传输模块绝不直接写 SFTP 视图。
        transfer::Event::RefreshDir(tab_id) => app.sftp.refresh(tab_id).map(Message::SftpEvent),
        // 自回路：把内部消息派发回传输模块自身，形成「进度 / 完成 → 下一传输」闭环。
        transfer::Event::Emit(m) => {
            let ctx = contexts::transfer_ctx(app);
            app.transfer.update(*m, &ctx).map(Message::TransferEvent)
        }
    }
}

/// 落地 `settings::Event`：把设置弹窗上行的各配置值写回 `AppConfig` 并落盘，
/// 其中外观相关项（字号 / 终端字体 / 终端配色）还即时热替换到所有已打开的终端标签。
pub(crate) fn apply_settings_event(app: &mut App, e: settings::Event) -> Task<Message> {
    match e {
        // 以下各分支把模块上行的配置值写回 `AppConfig` 并落盘；模块绝不写父状态。
        settings::Event::ConnectTimeout(v) => {
            app.config.connect_timeout = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::FontSize(v) => {
            app.config.font_size = v;
            contexts::save_config(app);
            // 即时热替换到所有已打开的终端标签（沿用当前已选终端字体）。
            terminal_bridge::apply_terminal_font(&mut app.tabs, &app.config.terminal_font, v);
            Task::none()
        }
        settings::Event::Theme(v) => {
            app.config.theme = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::UiFont(v) => {
            app.config.ui_font = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::TerminalFont(v) => {
            // 先按当前值热替换所有已打开的终端标签，再移动 `v` 写入配置。
            terminal_bridge::apply_terminal_font(&mut app.tabs, &v, app.config.font_size);
            app.config.terminal_font = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::TerminalTheme(v) => {
            // 先按当前值解析调色板热替换所有已打开的终端标签，再移动 `v` 写入配置。
            terminal_bridge::apply_terminal_theme(&mut app.tabs, &v);
            app.config.terminal_theme = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::LogLevel(v) => {
            app.config.log_level = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::Language(v) => {
            // 即时切换全局 locale 并重绘（view 每帧重建）；下拉框重建已在模块内完成。
            app.config.language = v;
            rust_i18n::set_locale(v.as_locale());
            contexts::save_config(app);
            Task::none()
        }
        // 打开日志目录为纯副作用，已在模块内完成，此处仅需确认（无状态需写）。
        settings::Event::OpenLogFolder => Task::none(),
        settings::Event::AutoCheckUpdates(v) => {
            app.config.auto_check_updates = v;
            contexts::save_config(app);
            Task::none()
        }
    }
}

/// 落地 `updates::Event`：把更新检查模块上行的节流时间戳写回并落盘，或自回路回派发。
pub(crate) fn apply_updates_event(app: &mut App, e: updates::Event) -> Task<Message> {
    match e {
        // 确立节流窗口：写回「上次检查时间戳」并落盘（模块绝不写配置）。
        updates::Event::SetLastCheck(ts) => {
            app.config.last_update_check_unix = ts;
            contexts::save_config(app);
            Task::none()
        }
        // 自回路：把内部消息派发回更新检查模块自身，形成「检查完成 → 更新横幅」闭环。
        updates::Event::Emit(m) => {
            let ctx = contexts::updates_ctx(app);
            app.updates.update(*m, &ctx).map(Message::UpdatesEvent)
        }
        // 弹出 toast 通知。
        updates::Event::Toast(kind, msg) => {
            contexts::set_toast(app, kind, msg);
            Task::none()
        }
    }
}

/// 落地 `masterpw::Event`：把主密码模块上行的保险库 / 会话列表 / 记住开关 / toast 写回父状态。
pub(crate) fn apply_masterpw_event(app: &mut App, e: masterpw::Event) -> Task<Message> {
    match e {
        masterpw::Event::SetVault(v) => {
            app.vault = Some(v);
            Task::none()
        }
        masterpw::Event::SetSessions(s) => {
            app.session.sessions = s;
            Task::none()
        }
        masterpw::Event::SetRemember(v) => {
            app.config.remember_master_key = v;
            contexts::save_config(app);
            // 同步钥匙串 DEK：开启且保险库就绪 → 存入（下次自动解锁）；关闭 → 删除，
            // 回到每次启动输入主密码（钥匙串不可用时的错误被静默忽略）。
            sync_keyring(app);
            Task::none()
        }
        masterpw::Event::Toast(kind, msg) => {
            contexts::set_toast(app, kind, msg);
            Task::none()
        }
        // 自回路：把内部消息派发回主密码模块自身，形成「写操作完成 → 重新列举」等闭环。
        masterpw::Event::Emit(m) => {
            let ctx = contexts::masterpw_ctx(app);
            app.masterpw.update(*m, &ctx).map(Message::MasterPwEvent)
        }
    }
}

/// 同步钥匙串中的 DEK：开启且保险库就绪 → 存入（下次自动解锁）；关闭 → 删除，
/// 回到每次启动输入主密码（钥匙串不可用时的错误被静默忽略）。
pub(crate) fn sync_keyring(app: &mut App) {
    if app.config.remember_master_key {
        if let Some(vault) = app.vault.as_ref() {
            vault_keyring::store_dek_quietly(&vault.dek_bytes());
        }
    } else {
        vault_keyring::delete_dek_quietly();
    }
}
