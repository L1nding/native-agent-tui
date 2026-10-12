//! Interactive and read-only terminal runtime loops.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEventKind};

use crate::client::{ClientHandle, Command};
use crate::history::HistoryHandle;
use crate::scheduler::RootTaskSpec;

use super::history::HistoryPanel;
use super::input::InputEvent;
use super::lang;
use super::search::SearchHandle;
use super::terminal::TerminalGuard;
use super::{
    draw, handle_key, handle_paste, paste_rejected, reject_paste, sync_local_requests, LocalState,
    UiError,
};

/// Coalesce ordinary output snapshots while keeping input and state transitions immediate.
struct RedrawSchedule {
    immediate: bool,
    stream_pending: bool,
    next_stream_frame: tokio::time::Instant,
}

impl RedrawSchedule {
    fn new(now: tokio::time::Instant) -> Self {
        Self {
            immediate: true,
            stream_pending: false,
            next_stream_frame: now,
        }
    }

    fn request(&mut self) {
        self.immediate = true;
    }

    fn snapshot(&mut self, urgent: bool) {
        if urgent {
            self.request();
        } else {
            self.stream_pending = true;
        }
    }

    fn ready(&self, now: tokio::time::Instant) -> bool {
        self.immediate || (self.stream_pending && now >= self.next_stream_frame)
    }

    fn drawn(&mut self, now: tokio::time::Instant) {
        self.immediate = false;
        self.stream_pending = false;
        self.next_stream_frame = now + Duration::from_millis(33);
    }
}

/// Progress counters, usage and output text may be coalesced. Execution,
/// approvals and attention transitions must remain visible without that delay.
fn urgent_snapshot_change(
    old: &crate::state::CoreSnapshot,
    new: &crate::state::CoreSnapshot,
) -> bool {
    old.phase != new.phase
        || old.thread_id != new.thread_id
        || old.turn_id != new.turn_id
        || old.requests != new.requests
        || old.gate != new.gate
        || old.scheduler != new.scheduler
        || old.notice != new.notice
        || old.last_error != new.last_error
        || old.model != new.model
        || old.permission != new.permission
        || old.models != new.models
        || old.skills != new.skills
        || old.conversations != new.conversations
        || old.resuming != new.resuming
        || old.creating_conversation != new.creating_conversation
        || old.saved_settings != new.saved_settings
        || old.startup_blocked != new.startup_blocked
        || old.agents.len() != new.agents.len()
        || old.agents.iter().zip(&new.agents).any(|(old, new)| {
            old.info != new.info
                || old.generation != new.generation
                || old.turn_id != new.turn_id
                || old.outcome != new.outcome
                || old.awaiting_turn != new.awaiting_turn
        })
        || old.messages.len() != new.messages.len()
        || old.messages.iter().zip(&new.messages).any(|(old, new)| {
            old.id != new.id
                || old.thread_id != new.thread_id
                || old.turn_id != new.turn_id
                || old.role != new.role
                || old.complete != new.complete
                || (new.role == "Tool" && old.text != new.text)
        })
        || old.observation.activities.len() != new.observation.activities.len()
        || old
            .observation
            .activities
            .iter()
            .zip(&new.observation.activities)
            .any(|(old, new)| {
                old.activity_id != new.activity_id
                    || old.identity != new.identity
                    || old.execution_state != new.execution_state
                    || old.interaction_state != new.interaction_state
                    || old.attention != new.attention
                    || old.wait_reason != new.wait_reason
            })
}

pub async fn run_tasks_with_history(
    mut client: ClientHandle,
    tasks: Vec<RootTaskSpec>,
    mut history: HistoryHandle,
) -> Result<(), UiError> {
    let mut history_panel = HistoryPanel::default();
    let mut sessions_panel = super::sessions::SessionsPanel::default();
    let mut history_open = true;
    let mut search = SearchHandle::spawn();
    let mut search_open = true;
    {
        // 环境变量优先；否则沿用上次在 /setting 里保存的语言和主题。
        use crate::preferences::Setting;
        let snapshot = client.snapshots.borrow();
        if let (None, Some(value)) = (
            lang::from_environment(),
            snapshot.saved_settings.get(&Setting::Language),
        ) {
            lang::set(value);
        }
        if let (None, Some(value)) = (
            super::theme::from_environment(),
            snapshot.saved_settings.get(&Setting::Theme),
        ) {
            super::theme::set(value);
        }
    }
    let result = async {
        let mut terminal = TerminalGuard::enter()?;
        let mut local = LocalState::default();
        if !tasks.is_empty() {
            client.commands.send(Command::QueueRootTasks { tasks }).await
                .map_err(|_| UiError::Shutdown("client is closed".into()))?;
        }
        let mut redraw = RedrawSchedule::new(tokio::time::Instant::now());
        let mut last_drawn = None;
        let mut snapshots_open = true;
        let mut tick = tokio::time::interval(Duration::from_millis(25));
        loop {
            if redraw.ready(tokio::time::Instant::now()) {
                let size = terminal.terminal.size()?;
                local.viewport = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                let snapshot = client.snapshots.borrow_and_update().clone();
                local.reminders.sync(&snapshot.observation);
                sync_local_requests(&mut local, &snapshot);
                local.search.dispatch(snapshot.clone(), &mut search);
                local.timeline.sync(&snapshot);
                terminal.terminal.draw(|frame| {
                    if local.palette.visible { draw(frame, &snapshot, &local); }
                    else if sessions_panel.visible { sessions_panel.draw(frame, &snapshot); }
                    else if history_panel.visible { history_panel.draw(frame, Some(&snapshot)); }
                    else { draw(frame, &snapshot, &local); }
                })?;
                last_drawn = Some(snapshot);
                redraw.drawn(tokio::time::Instant::now());
            }
            tokio::select! {
                _ = tokio::time::sleep_until(redraw.next_stream_frame), if redraw.stream_pending => {
                    redraw.request();
                }
                change = search.changed(), if search_open => {
                    search_open = change.is_ok();
                    if search_open { local.search.updated(&search); }
                    else { local.search.unavailable(); }
                    redraw.request();
                }
                change = async {
                    tokio::select! {
                        result = history.status.changed() => (false, result),
                        result = history.search.status.changed() => (true, result),
                    }
                }, if history_open => {
                    if let (is_search, Ok(())) = change {
                        if is_search {
                            history_panel.search_updated(&history);
                        } else {
                            history_panel.updated(&mut history);
                        }
                    } else {
                        history_open = false;
                        history_panel.notice = Some(lang::tr("History reader closed; return to live execution.", "历史读取已关闭，已回到实时会话。").into());
                    }
                    redraw.request();
                }
                change = client.snapshots.changed(), if snapshots_open => {
                    snapshots_open = change.is_ok();
                    let snapshot = client.snapshots.borrow();
                    let urgent = !snapshots_open || last_drawn.as_ref().is_none_or(|old| urgent_snapshot_change(old, &snapshot));
                    redraw.snapshot(urgent);
                }
                _ = tick.tick() => {
                    // Bound each input batch so snapshots and redraw remain responsive.
                    for _ in 0..128 {
                        let Some(event) = terminal.input.read_ready()? else { break; };
                        match event {
                            InputEvent::Key(key) if key.kind != KeyEventKind::Release => {
                                let snapshot = client.snapshots.borrow().clone();
                                if local.palette.visible {
                                    if handle_key(key, &snapshot, &mut local, &client.commands) { return Ok(()); }
                                } else if sessions_panel.visible {
                                    use super::sessions::SessionsEvent;
                                    match sessions_panel.key(key, &snapshot) {
                                        SessionsEvent::Quit => return Ok(()),
                                        SessionsEvent::Resume(id) => { let _ = client.commands.try_send(Command::ResumeConversation { id }); }
                                        SessionsEvent::Refresh => { let _ = client.commands.try_send(Command::ListConversations); }
                                        SessionsEvent::New => {
                                            if super::send(Command::NewConversation, &client.commands, &mut local) {
                                                sessions_panel.visible = false;
                                            }
                                        }
                                        SessionsEvent::Close | SessionsEvent::Consumed => {}
                                    }
                                } else if history_panel.visible {
                                    if history_panel.key(key, &mut history, false) { return Ok(()); }
                                } else if key.code == KeyCode::F(12) {
                                    local.search.close();
                                    local.timeline.close();
                                    history_panel.open(&mut history, None);
                                } else if super::left_opens_sessions(key, &snapshot, &local) {
                                    sessions_panel.open();
                                    let _ = client.commands.try_send(Command::ListConversations);
                                } else if handle_key(key, &snapshot, &mut local, &client.commands) { return Ok(()); }
                                redraw.request();
                            }
                            InputEvent::Paste(text) => {
                                if local.palette.visible {
                                    local.palette.paste(&text);
                                    redraw.request();
                                    continue;
                                }
                                if sessions_panel.visible { continue; }
                                if history_panel.visible { history_panel.paste(&text); redraw.request(); continue; }
                                let snapshot = client.snapshots.borrow().clone();
                                handle_paste(&text, &snapshot, &mut local);
                                redraw.request();
                            }
                            InputEvent::PasteRejected => {
                                if local.palette.visible {
                                    local.palette.reject_paste();
                                } else if history_panel.visible {
                                    history_panel.notice = Some(paste_rejected().into());
                                } else {
                                    reject_paste(&mut local);
                                }
                                redraw.request();
                            }
                            InputEvent::Mouse(mouse) => {
                                if !sessions_panel.visible && !history_panel.visible
                                    && super::handle_mouse(mouse, &mut local) {
                                    redraw.request();
                                }
                            }
                            InputEvent::Resize(_, _) => redraw.request(),
                            _ => {}
                        }
                    }
                }
            }
        }
    }.await;
    search.shutdown().await;
    let _ = client.commands.send(Command::Quit).await;
    let report = client
        .join
        .await
        .map_err(|error| UiError::Shutdown(error.to_string()))?;
    let history_result = history.shutdown().await.map_err(UiError::from);
    if let Some(error) = report.cleanup_error {
        return Err(UiError::Shutdown(error));
    }
    if let Some(error) = report.journal_error {
        return Err(UiError::Shutdown(error.to_string()));
    }
    result.and(history_result)
}

/// Offline observation recovery. This runtime has no ClientHandle or execution sender.
pub async fn run_history(
    mut service: HistoryHandle,
    session: Option<String>,
) -> Result<(), UiError> {
    let result = async {
        let mut terminal = TerminalGuard::enter()?;
        let mut panel = HistoryPanel::default();
        panel.open(&mut service, session);
        let mut dirty = true;
        let mut reader_open = true;
        let mut tick = tokio::time::interval(Duration::from_millis(25));
        loop {
            if dirty {
                terminal.terminal.draw(|frame| panel.draw(frame, None))?;
                dirty = false;
            }
            tokio::select! {
                changed = async {
                    tokio::select! {
                        result = service.status.changed() => (false, result),
                        result = service.search.status.changed() => (true, result),
                    }
                }, if reader_open => {
                    if let (is_search, Ok(())) = changed {
                        if is_search { panel.search_updated(&service); }
                        else { panel.updated(&mut service); }
                    } else {
                        reader_open = false;
                        panel.notice = Some(lang::tr("History reader closed. Ctrl+Q exits.", "历史读取已关闭。Ctrl+Q 退出。").into());
                    }
                    dirty = true;
                }
                _ = tick.tick() => {
                    for _ in 0..128 {
                        let Some(event) = terminal.input.read_ready()? else { break; };
                        match event {
                            InputEvent::Key(key) if key.kind != KeyEventKind::Release => {
                                if panel.key(key, &mut service, true) { return Ok(()); }
                                dirty = true;
                            }
                            InputEvent::Paste(text) => { panel.paste(&text); dirty = true; }
                            InputEvent::PasteRejected => {
                                panel.notice = Some(paste_rejected().into());
                                dirty = true;
                            }
                            InputEvent::Resize(_, _) => dirty = true,
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    .await;
    let stopped = service.shutdown().await.map_err(UiError::from);
    result.and(stopped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_deltas_coalesce_while_requests_and_final_messages_are_urgent() {
        use crate::state::{ConversationItem, CoreSnapshot, SessionPhase};
        let mut before = CoreSnapshot {
            phase: SessionPhase::Running,
            ..Default::default()
        };
        before.messages.push(ConversationItem {
            id: "message".into(),
            thread_id: "root".into(),
            turn_id: "turn".into(),
            role: "Agent".into(),
            text: "partial".into(),
            complete: false,
            truncated: false,
        });
        let mut after = before.clone();
        after.version += 1;
        after.messages[0].text.push_str(" response");
        assert!(!urgent_snapshot_change(&before, &after));
        after.messages[0].complete = true;
        assert!(urgent_snapshot_change(&before, &after));
        after.messages[0].complete = false;
        after.requests.push(crate::interactions::RequestView::decode(
            crate::protocol::RpcId::Number(1), "item/commandExecution/requestApproval",
            &serde_json::json!({"threadId":"root", "turnId":"turn", "availableDecisions":["accept","decline"]})
        ).unwrap());
        assert!(urgent_snapshot_change(&before, &after));
        after.requests.clear();
        after.phase = SessionPhase::Completed;
        assert!(urgent_snapshot_change(&before, &after));
        after.phase = SessionPhase::Disconnected;
        assert!(urgent_snapshot_change(&before, &after));
    }

    #[test]
    fn streaming_burst_is_coalesced_but_completion_and_input_draw_immediately() {
        let now = tokio::time::Instant::now();
        let mut redraw = RedrawSchedule::new(now);
        assert!(redraw.ready(now));
        redraw.drawn(now);
        for ms in 1..33 {
            redraw.snapshot(false);
            assert!(
                !redraw.ready(now + Duration::from_millis(ms)),
                "text delta produced an extra frame at {ms}ms"
            );
        }
        assert!(redraw.ready(now + Duration::from_millis(33)));
        redraw.drawn(now + Duration::from_millis(33));
        assert!(
            !redraw.ready(now + Duration::from_secs(10)),
            "idle UI should not render frames"
        );
        redraw.snapshot(true);
        assert!(redraw.ready(now + Duration::from_millis(34)));
        redraw.drawn(now + Duration::from_millis(34));
        redraw.request();
        assert!(redraw.ready(now + Duration::from_millis(35)));
    }
}
