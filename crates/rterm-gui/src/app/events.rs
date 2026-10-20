//! 子组件上行事件落地：把各 `Message::XxxEvent` 的 `Xxx::Event` 写回父状态。

use crate::app::App;
use crate::app::connect;
use crate::app::contexts;
use crate::app::terminal_bridge;
use crate::app::{
    forwards, masterpw, paste, remote_edit, search, session, session_io, settings, sftp, snippets,
    tabs, transfer, updates, window,
};
use crate::message::Message;
use crate::state::{SftpDialog, ToastKind};
use crate::t;
use crate::vault_keyring;
use crate::widget::term::actions::Action;
use crate::widget::term::{BackendCommand, Command as TermCommand, Event as TermEvent};
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
            // 标签被整体移除，不会逐个走 `tabs::Event::RemoveHostKeyForTab`，故这里按返回值
            // 补发传输清理，否则被关标签的队列记录与并发额度都会残留。
            // 转发引擎同理：先收走各标签的句柄（整体移除后无处可取），逐个 clear
            // （远程转发同时向服务端注销）并清理面板的运行时行状态。
            let handles: Vec<(u64, std::sync::Arc<rterm_core::ForwardHandle>)> = app
                .tabs
                .list()
                .iter()
                .filter(|t| t.session_id == id)
                .filter_map(|t| t.forwards.clone().map(|h| (t.id, h)))
                .collect();
            let closed = connect::close_session_tabs(app, &id);
            let mut tasks = Vec::with_capacity(closed.len());
            for tab_id in closed {
                app.paste.remove_for_tab(tab_id);
                // 同关标签：删除会话会移除其全部标签，编辑会话随宿主一起终止。
                app.remote_edit.end_for_tab(tab_id);
                app.forwards.remove_for_tab(tab_id);
                tasks.push(close_tab_transfers(app, tab_id));
            }
            for (tab_id, handle) in handles {
                tasks.push(Task::perform(
                    async move { handle.clear().await },
                    move |_| Message::Noop,
                ));
                let _ = tab_id;
            }
            Task::batch(tasks)
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
        // 会话文件操作意图：转成交给会话文件模块的入口消息（面板视图发不了该模块的类型）。
        session::Event::SessionFile(flow) => {
            let msg = match flow {
                session::FileFlow::Import => session_io::Message::BeginImport,
                session::FileFlow::Export => session_io::Message::BeginExport,
            };
            Task::done(Message::SessionIo(msg))
        }
        // 右键「端口转发…」：转交转发模块设置作用域并打开面板（经 OpenPanel 落地导航）。
        session::Event::OpenForwards(id) => {
            Task::done(Message::Forwards(forwards::Message::ScopeTo(id)))
        }
    }
}

/// 落地 `forwards::Event`：把转发模块上行的规则变更写回会话存储并同步各标签的
/// 转发引擎，其余 toast / 导航由父层直接落地。
pub(crate) fn apply_forwards_event(app: &mut App, e: forwards::Event) -> Task<Message> {
    match e {
        forwards::Event::Toast(kind, msg) => {
            contexts::set_toast(app, kind, msg);
            Task::none()
        }
        forwards::Event::OpenPanel(id) => {
            app.forwards.scope = Some(id);
            app.center = crate::state::CenterView::Forwards;
            Task::none()
        }
        forwards::Event::SessionForwardsChanged {
            session_id,
            forwards,
        } => {
            // (a) 变更前快照：引擎逐索引 diff 要拿它对照「旧清单里有哪些规则」。
            let old = app
                .session
                .sessions
                .iter()
                .find(|s| s.id == session_id)
                .map(|s| s.forwards.clone())
                .unwrap_or_default();
            // (b) 持久化到会话存储（编辑入口已在保险库未解锁时拦下，这里仅为防御）。
            if let Some(vault) = app.vault.as_ref() {
                let header = vault.header().clone();
                if let Err(err) = app
                    .session
                    .set_forwards(&session_id, forwards.clone(), &header)
                {
                    contexts::set_toast(
                        app,
                        ToastKind::Error,
                        t!("forward.persist_failed", err => err),
                    );
                }
            }
            // (c) 引擎同步：对会话内已连接标签按 old / new 逐索引 diff——相等跳过、旧有→
            // remove_rule、新有或内容不同→apply_rule（增量 API 让存量桥自然排空）。先收齐标签：
            // 循环体要改 app.forwards，不能同时借用标签列表。
            let targets: Vec<(u64, std::sync::Arc<rterm_core::ForwardHandle>)> = app
                .tabs
                .list()
                .iter()
                .filter(|t| {
                    t.session_id == session_id
                        && t.status == rterm_core::ConnectionStatus::Connected
                })
                .filter_map(|t| t.forwards.clone().map(|h| (t.id, h)))
                .collect();
            let mut tasks: Vec<Task<Message>> = Vec::new();
            for (tab_id, handle) in targets {
                // 行状态按新清单重排：新增规则才有行承载后续事件。
                app.forwards.realign(tab_id, &old, &forwards);
                let len = old.len().max(forwards.len());
                for i in 0..len {
                    let old_rule = old.get(i);
                    let new_rule = forwards.get(i);
                    if old_rule == new_rule {
                        continue;
                    }
                    let handle = handle.clone();
                    let rule = new_rule.cloned();
                    tasks.push(Task::perform(
                        async move {
                            match rule {
                                Some(rule) => handle.apply_rule(i, &rule).await,
                                None => handle.remove_rule(i).await,
                            }
                        },
                        |_| Message::Noop,
                    ));
                }
            }
            Task::batch(tasks)
        }
    }
}

/// 落地 `session_io::Event`：导入合并后的会话列表写回会话模块状态，其余交父层通用动作。
pub(crate) fn apply_session_io_event(app: &mut App, e: session_io::Event) -> Task<Message> {
    match e {
        // 模块已自行落盘（用本机保险库的头），这里只同步内存视图。
        session_io::Event::SetSessions(sessions) => {
            app.session.sessions = sessions;
            Task::none()
        }
        session_io::Event::Toast(kind, msg) => {
            contexts::set_toast(app, kind, msg);
            Task::none()
        }
        // 自回路：系统文件对话框完成、或口令流程的阻塞阶段结束，都由此派发回模块推进下一步。
        session_io::Event::Emit(m) => {
            let ctx = contexts::session_io_ctx(app);
            app.session_io.update(*m, &ctx).map(Message::SessionIoEvent)
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
        // 为已连接标签拉起终端桥接（标签 / 连接生命周期属父层，不在标签模块内）；
        // 桥接就绪即按会话当前规则在同一连接上拉起转发引擎。
        tabs::Event::OpenTerminalBridge(tab_id, conn) => Task::batch([
            terminal_bridge::open_terminal_bridge(app, tab_id, conn),
            forwards::start_for_tab(app, tab_id),
        ]),
        // 重连请求（回车 / 横幅按钮 / 覆盖层重试）：在发起请求的标签上原地重建连接、
        // 不新开标签；结果回流到同一标签：已有终端走原地换接，尚无终端（首连失败）走新建。
        tabs::Event::ReconnectTab(tab_id, session_id) => {
            connect::connect_session(app, tab_id, &session_id)
        }
        // 桥接就绪后挂载终端组件（widget 生命周期属父层）。
        tabs::Event::SpawnTerminal(tab_id, conout, conin, disc, resize) => {
            terminal_bridge::spawn_terminal_widget(app, tab_id, conout, conin, disc, resize)
        }
        // 关闭标签时清理其挂起的主机密钥确认、粘贴确认、搜索条与传输态。
        tabs::Event::RemoveHostKeyForTab(tab_id) => {
            app.hostkey.remove_for_tab(tab_id);
            app.paste.remove_for_tab(tab_id);
            app.search.remove_for_tab(tab_id);
            // 该标签发起的本地编辑会话一并终止：回传的宿主（队列与客户端）随标签消失，
            // 编辑缓存副本按既定策略保留，由用户从缓存目录取回或重新发起。
            app.remote_edit.end_for_tab(tab_id);
            // 转发面板的运行时行状态一并清理（引擎收尾由 ClearForwards 分支执行）。
            app.forwards.remove_for_tab(tab_id);
            close_tab_transfers(app, tab_id)
        }
        // 标签已移除（关标签 / 关窗口）：收走其转发引擎——clear 会终止全部监听任务
        // 并向服务端注销远程转发，确保不残留。句柄由标签模块在移除前收进事件。
        tabs::Event::ClearForwards(tab_id, handle) => {
            app.forwards.remove_for_tab(tab_id);
            Task::perform(async move { handle.clear().await }, |_| Message::Noop)
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

/// 通知传输模块某个标签已被关闭：中止其在跑任务、清空其队列并归还并发额度。
///
/// 标签被移除后，其传输不会再收到任何事件，故这是「额度只在 `TransferDone` 释放」的唯一例外。
/// 两处调用：`tabs::Event::RemoveHostKeyForTab`（逐标签关闭）与 `session::Event::SessionDeleted`
/// （删除会话，一次移除多个标签，不逐个走关闭事件）。
pub(crate) fn close_tab_transfers(app: &mut App, tab_id: u64) -> Task<Message> {
    let ctx = contexts::transfer_ctx(app);
    app.transfer
        .update(transfer::Message::TabClosed(tab_id), &ctx)
        .map(Message::TransferEvent)
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
        // 右键「本地编辑」：转交给远程编辑模块发起会话（下载到缓存 → 编辑器 → 轮询回传）。
        sftp::Event::EditLocal {
            tab_id,
            session_id,
            remote_path,
            name,
        } => Task::done(Message::RemoteEdit(remote_edit::Message::Start {
            tab_id,
            session_id,
            remote_path,
            name,
        })),
        // 冲突框「覆盖 / 取消」：SFTP 模块只拥有对话框生命周期，决定本身归编辑会话。
        sftp::Event::EditConflictOverwrite(key) => Task::done(Message::RemoteEdit(
            remote_edit::Message::ConflictOverwrite { key },
        )),
        sftp::Event::EditConflictCancel(key) => {
            Task::done(Message::RemoteEdit(remote_edit::Message::ConflictCancel {
                key,
            }))
        }
        // 回传确认框「回传 / 取消」：同上，SFTP 只持有对话框，决定归编辑会话。
        sftp::Event::EditConfirmUpload(key) => {
            Task::done(Message::RemoteEdit(remote_edit::Message::ConfirmUpload {
                key,
            }))
        }
        sftp::Event::EditConfirmDismiss(key) => {
            Task::done(Message::RemoteEdit(remote_edit::Message::ConfirmDismiss {
                key,
            }))
        }
        sftp::Event::StartDownload(name, local) => {
            let tab_id = app.tabs.active().unwrap_or(0);
            let ctx = contexts::transfer_ctx(app);
            app.transfer
                .update(transfer::Message::Download(tab_id, name, local), &ctx)
                .map(Message::TransferEvent)
        }
        sftp::Event::StartDownloadFolder(name, local) => {
            let tab_id = app.tabs.active().unwrap_or(0);
            let ctx = contexts::transfer_ctx(app);
            app.transfer
                .update(transfer::Message::DownloadFolder(tab_id, name, local), &ctx)
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
        // 远程编辑的取回 / 回传完成：转投编辑会话模块（它据此更新基准或收尾）。
        transfer::Event::EditDownloadDone(key, ok) => {
            Task::done(Message::RemoteEdit(remote_edit::Message::DownloadDone {
                key,
                ok,
            }))
        }
        transfer::Event::EditUploadDone(key, failure) => {
            Task::done(Message::RemoteEdit(remote_edit::Message::UploadDone {
                key,
                failure,
            }))
        }
        // 自回路：把内部消息派发回传输模块自身，形成「进度 / 完成 → 下一传输」闭环。
        transfer::Event::Emit(m) => {
            let ctx = contexts::transfer_ctx(app);
            app.transfer.update(*m, &ctx).map(Message::TransferEvent)
        }
    }
}

/// 落地 `remote_edit::Event`：toast / 下载与回传入队（转传输模块）/ 冲突框（写 SFTP
/// 视图对话框槽位）/ 自回路。
pub(crate) fn apply_remote_edit_event(app: &mut App, e: remote_edit::Event) -> Task<Message> {
    match e {
        remote_edit::Event::Toast(kind, msg) => {
            contexts::set_toast(app, kind, msg);
            Task::none()
        }
        // 取回 / 回传入队：转传输模块执行（`.part` 暂存、重试、进度全部沿用队列语义）。
        remote_edit::Event::StartEditDownload {
            tab_id,
            key,
            local,
            remote,
        } => {
            let ctx = contexts::transfer_ctx(app);
            app.transfer
                .update(
                    transfer::Message::EditDownload(tab_id, key, remote, local),
                    &ctx,
                )
                .map(Message::TransferEvent)
        }
        remote_edit::Event::StartEditUpload {
            tab_id,
            key,
            local,
            remote,
        } => {
            let ctx = contexts::transfer_ctx(app);
            app.transfer
                .update(
                    transfer::Message::EditUpload(tab_id, key, local, remote),
                    &ctx,
                )
                .map(Message::TransferEvent)
        }
        // 冲突框落在发起编辑那个标签的 SFTP 对话框槽位（父写子，规则允许）；槽位被占时不覆盖，
        // 会话的冲突状态未置，下一轮轮询会自然重试，不会把这次冲突悄悄吞掉。
        remote_edit::Event::RequestConflict { tab_id, key, name } => {
            if let Some(view) = app.sftp.tab_mut(tab_id)
                && view.dialog.is_none()
            {
                view.dialog = Some(SftpDialog::EditConflict {
                    session_id: key.0,
                    remote_path: key.1,
                    name,
                });
            }
            Task::none()
        }
        // 回传确认框：与冲突框同一槽位、同一「占用即跳过、下轮重试」策略（不排队——
        // 排队是 UploadConflicts 批量决策的特例）。跳过安全：`seen` 保持触发水位，下轮重新上行。
        remote_edit::Event::RequestUploadConfirm { tab_id, key, name } => {
            let mut tasks: Vec<Task<Message>> = Vec::new();
            if let Some(view) = app.sftp.tab_mut(tab_id)
                && view.dialog.is_none()
            {
                view.dialog = Some(SftpDialog::EditUploadConfirm {
                    session_id: key.0,
                    remote_path: key.1,
                    name,
                });
                // 观众多半不在盯 rterm——他们刚在外部编辑器里保存完。请求「信息级」注意拉回注意力：
                // Windows 闪任务栏、macOS dock 弹一次、X11 置 urgency hint；Wayland 下 winit 静默 no-op。
                if let Some(id) = app.window.id() {
                    tasks.push(iced::window::request_user_attention(
                        id,
                        Some(iced::window::UserAttention::Informational),
                    ));
                }
            }
            Task::batch(tasks)
        }
        // 自回路：把内部消息派发回远程编辑模块自身，形成「探测 / 观测 / 收尾」闭环。
        remote_edit::Event::Emit(m) => {
            let ctx = contexts::remote_edit_ctx(app);
            app.remote_edit
                .update(*m, &ctx)
                .map(Message::RemoteEditEvent)
        }
    }
}

/// 落地 `paste::Event`：把放行的粘贴编码后经终端写入通道落笔。
pub(crate) fn apply_paste_event(app: &mut App, e: paste::Event) -> Task<Message> {
    match e {
        paste::Event::Paste { tab_id, text } => terminal_bridge::write_paste(app, tab_id, text),
    }
}

/// 落地 `search::Event`：把搜索命令下发到绑定的目标终端（锁内执行，同步返回），
/// 回带的命中统计转成 `SetOutcome` 自回路写回模块；焦点意图写回父状态。
pub(crate) fn apply_search_event(app: &mut App, e: search::Event) -> Task<Message> {
    match e {
        search::Event::Run(cmd) => {
            let action = if matches!(cmd, BackendCommand::SearchClose) {
                // 清除广播到所有标签：关条事件异步送达时目标已被复位、活动标签已切走，
                // 单发会打错对象；旧标签的残留高亮必须随关条一并消失。
                for tab in app.tabs.list_mut() {
                    if let Some(term) = tab.terminal.as_mut() {
                        let _ =
                            term.handle(TermCommand::ProxyToBackend(BackendCommand::SearchClose));
                    }
                }
                None
            } else {
                let target = app.search.target().or(app.tabs.active());
                target
                    .and_then(|id| app.tabs.list_mut().iter_mut().find(|t| t.id == id))
                    .and_then(|tab| tab.terminal.as_mut())
                    .map(|term| term.handle(TermCommand::ProxyToBackend(cmd)))
            };
            match action {
                Some(Action::SearchStats(outcome)) => {
                    Task::done(Message::Search(search::Message::SetOutcome(outcome)))
                }
                _ => Task::none(),
            }
        }
        // 关闭交还焦点时，若窗口本身失焦（存在保存态）则不写：置位会绕过
        // 「窗口重新聚焦时按保存态还原」的机制，把用户离开前的其它控件焦点弄丢。
        search::Event::SetTerminalFocused(b) => {
            if !(b && app.window_focus_saved.is_some()) {
                app.terminal_focused = b;
            }
            Task::none()
        }
        search::Event::FocusInput => iced::widget::operation::focus(app.search.input_id().clone()),
    }
}

/// 落地 `snippets::Event`：把命令片段模块上行的字节写入目标终端、弹 toast、更新状态栏。
pub(crate) fn apply_snippets_event(app: &mut App, e: snippets::Event) -> Task<Message> {
    match e {
        // 片段原文直写终端（不经粘贴编码）：点击即发送要求远端直接执行。
        snippets::Event::Send { tab_id, bytes } => terminal_bridge::handle_terminal_event(
            app,
            TermEvent::BackendCall(tab_id, BackendCommand::Write(bytes)),
        ),
        snippets::Event::Toast(kind, msg) => {
            contexts::set_toast(app, kind, msg);
            Task::none()
        }
        snippets::Event::Saved { count } => {
            app.status = Some(t!("snippet.saved", count => count));
            Task::none()
        }
        snippets::Event::FocusPaletteInput => {
            iced::widget::operation::focus(app.snippets.palette.input_id.clone())
        }
        // 弹窗开关的焦点交接；关闭交还焦点时若窗口本身失焦（存在保存态）则不写，
        // 同搜索条：置位会绕过「窗口重新聚焦时按保存态还原」的机制。
        snippets::Event::SetTerminalFocused(b) => {
            if !(b && app.window_focus_saved.is_some()) {
                app.terminal_focused = b;
            }
            Task::none()
        }
    }
}

/// 落地 `settings::Event`：把设置弹窗上行的各配置值写回 `AppConfig` 并落盘，
/// 其中外观相关项（字号 / 终端字体 / 终端配色）还即时热替换到所有已打开的终端标签。
pub(crate) fn apply_settings_event(app: &mut App, e: settings::Event) -> Task<Message> {
    match e {
        // 以下各分支把模块上行的配置值写回 `AppConfig` 并落盘；模块绝不写父状态。
        settings::Event::ConnectTimeout(v) => {
            app.config.connection.timeout = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::Keepalive(v) => {
            app.config.connection.keepalive = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::KeepaliveMax(v) => {
            // 只写内存、不落盘（同并发数：拖动期间每个 step 都会触发本分支，落盘交给滑块释放时）。
            // 也不必通知任何已打开的连接：保活参数在建连时已固化进 russh `Config`，
            // 改动只作用于后续新建的连接。
            app.config.connection.keepalive_max = v;
            Task::none()
        }
        settings::Event::KeepaliveMaxPersist => {
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::Scrollback(v) => {
            // 仅对新建终端标签生效（alacritty `scrolling_history` 为构造期参数，无法热替换）。
            app.config.terminal.scrollback = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::MaxConcurrent(v) => {
            // 写回内存值（已由模块裁剪到合法区间）后立即重新调度传输队列：调大即刻补位、
            // 调小不打断在跑任务。刻意不在此落盘——拖动滑块期间每个 step 都可能触发本分支，
            // 落盘交给滑块释放时的 `MaxConcurrentPersist`，避免反复写 `config.toml`。
            app.config.transfer.max_concurrent = v;
            // `transfer_ctx` 在此刻重建，读到的是**刚写入的新值**，故同一帧即生效。
            let ctx = contexts::transfer_ctx(app);
            app.transfer
                .update(transfer::Message::ConcurrencyChanged, &ctx)
                .map(Message::TransferEvent)
        }
        settings::Event::MaxConcurrentPersist => {
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::RetryAttempts(v) => {
            // 只写内存、不落盘（同并发数：拖动期间每个 step 都会触发本分支，落盘交给释放时）。
            // 也**不**追加任何传输模块消息：该值在「失败那一刻」经 `transfer_ctx` 读取，
            // 故下一次失败即生效，队列无需重排。
            app.config.transfer.retry_attempts = v;
            Task::none()
        }
        settings::Event::RetryAttemptsPersist => {
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::Editor(editor) => {
            // 本地编辑器命令：下次发起「本地编辑」时经 remote_edit 上下文读取，无需热应用。
            app.config.files.editor = editor;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::EditCacheDays(days) => {
            // 下一次启动清理时生效：运行中不清理，故无需热应用。
            app.config.files.edit_cache_days = days;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::FontSize(v) => {
            app.config.terminal.font_size = v;
            contexts::save_config(app);
            // 即时热替换到所有已打开的终端标签（沿用当前已选终端字体）。
            terminal_bridge::apply_terminal_font(&mut app.tabs, &app.config.terminal.font, v);
            Task::none()
        }
        settings::Event::Theme(v) => {
            app.config.appearance.theme = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::UiFont(v) => {
            app.config.appearance.ui_font = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::TerminalFont(v) => {
            // 先按当前值热替换所有已打开的终端标签，再移动 `v` 写入配置。
            terminal_bridge::apply_terminal_font(&mut app.tabs, &v, app.config.terminal.font_size);
            app.config.terminal.font = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::TerminalTheme(v) => {
            // 先按当前值解析调色板热替换所有已打开的终端标签，再移动 `v` 写入配置。
            terminal_bridge::apply_terminal_theme(&mut app.tabs, &v);
            app.config.terminal.theme = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::LogLevel(v) => {
            app.config.logging.level = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::Language(v) => {
            // 即时切换全局 locale 并重绘（view 每帧重建）；下拉框重建已在模块内完成。
            app.config.appearance.language = v;
            rust_i18n::set_locale(v.as_locale());
            contexts::save_config(app);
            Task::none()
        }
        // 打开日志目录为纯副作用，已在模块内完成，此处仅需确认（无状态需写）。
        settings::Event::OpenLogFolder => Task::none(),
        settings::Event::AutoCheckUpdates(v) => {
            app.config.updates.auto_check = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::CwdBootstrap(v) => {
            app.config.terminal.cwd_bootstrap = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::SuppressBootstrapEcho(v) => {
            app.config.terminal.suppress_bootstrap_echo = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::RunStartupCommand(v) => {
            app.config.terminal.run_startup_command = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::RestoreCwdOnReconnect(v) => {
            app.config.terminal.restore_cwd_on_reconnect = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::TrimTrailingWhitespace(v) => {
            app.config.terminal.trim_trailing_whitespace = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::CopyOnSelect(v) => {
            app.config.terminal.copy_on_select = v;
            contexts::save_config(app);
            // 即时热替换到所有已打开的终端标签。
            terminal_bridge::apply_copy_on_select(&mut app.tabs, v);
            Task::none()
        }
        settings::Event::Bell(v) => {
            app.config.terminal.bell = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::Scrollbar(v) => {
            app.config.terminal.show_scrollbar = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::PasteWarnEnabled(v) => {
            app.config.terminal.paste_warn_enabled = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::PasteWarnControlChars(v) => {
            app.config.terminal.paste_warn_control_chars = v;
            contexts::save_config(app);
            Task::none()
        }
        settings::Event::RememberWindowSize(v) => {
            app.config.window.remember_size = v;
            // 关闭「记住窗口大小」时顺带清除已保存尺寸（一次），避免下次启动仍恢复旧尺寸；
            // 重新开启后从当前会话的下一次关闭重新记录。
            if !v {
                app.config.clear_window_size();
            }
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
            app.config.updates.last_check_unix = ts;
            contexts::save_config(app);
            Task::none()
        }
        // 自回路：把内部消息派发回更新检查模块自身，形成「检查完成 → 更新横幅」闭环。
        updates::Event::Emit(m) => {
            let ctx = contexts::updates_ctx(app);
            app.updates.update(*m, &ctx).map(Message::UpdatesEvent)
        }
    }
}

/// 落地 `window::Event`：执行窗口查询、按需写回窗口尺寸并关闭窗口。
///
/// 关闭前先通知所有标签的桥接断开，使后台 pump / 线程尽快退出，避免进程残留；
/// 窗口本身由父层在查询完成后显式 `window::close` 关闭（应用已关闭默认关闭行为）。
pub(crate) fn apply_window_event(app: &mut App, e: window::Event) -> Task<Message> {
    match e {
        // 关闭前查询是否最大化：查询结果回派 `MaximizedForClose`。
        // （标签桥接断开已在路由层随关闭请求完成。）
        window::Event::QueryMaximized(id) => iced::window::is_maximized(id).map(move |maximized| {
            Message::Window(window::Message::MaximizedForClose(id, maximized))
        }),
        // 非最大化：查询当前尺寸，回派 `SizeForClose`。
        window::Event::QuerySize(id) => iced::window::size(id)
            .map(move |size| Message::Window(window::Message::SizeForClose(id, size))),
        // 最大化 / 未开启记忆：直接关闭，保留配置中上一次的非最大化尺寸。
        window::Event::Close(id) => iced::window::close(id),
        // 写回非最大化尺寸后关闭窗口。
        window::Event::PersistAndClose(id, size) => {
            if app.config.window.remember_size {
                app.config.window.width = Some(size.width);
                app.config.window.height = Some(size.height);
                contexts::save_config(app);
            }
            iced::window::close(id)
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
            app.config.security.remember_master_key = v;
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
    if app.config.security.remember_master_key {
        if let Some(vault) = app.vault.as_ref() {
            vault_keyring::store_dek_quietly(&vault.dek_bytes());
        }
    } else {
        vault_keyring::delete_dek_quietly();
    }
}
