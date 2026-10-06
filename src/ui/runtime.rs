//! Interactive and read-only terminal runtime loops.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEventKind};

use crate::client::{ClientHandle, Command};
use crate::history::HistoryHandle;
use crate::scheduler::RootTaskSpec;

use super::history::HistoryPanel;
use super::input::InputEvent;
use super::search::SearchHandle;
use super::terminal::TerminalGuard;
use super::{
    draw, handle_key, handle_paste, reject_paste, sync_local_requests, LocalState, UiError,
    PASTE_REJECTED,
};
pub async fn run_tasks_with_history(
    mut client: ClientHandle,
    tasks: Vec<RootTaskSpec>,
    mut history: HistoryHandle,
) -> Result<(), UiError> {
    let mut history_panel = HistoryPanel::default();
    let mut history_open = true;
    let mut search = SearchHandle::spawn();
    let mut search_open = true;
    let result = async {
        let mut terminal = TerminalGuard::enter()?;
        let mut local = LocalState::default();
        if !tasks.is_empty() {
            client.commands.send(Command::QueueRootTasks { tasks }).await
                .map_err(|_| UiError::Shutdown("client is closed".into()))?;
        }
        let mut dirty = true;
        let mut snapshots_open = true;
        let mut tick = tokio::time::interval(Duration::from_millis(25));
        loop {
            if dirty {
                let size = terminal.terminal.size()?;
                local.viewport = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                let snapshot = client.snapshots.borrow().clone();
                local.reminders.sync(&snapshot.observation);
                sync_local_requests(&mut local, &snapshot);
                local.search.dispatch(snapshot.clone(), &mut search);
                local.timeline.sync(&snapshot);
                terminal.terminal.draw(|frame| {
                    if local.palette.visible { draw(frame, &snapshot, &local); }
                    else if history_panel.visible { history_panel.draw(frame, Some(&snapshot)); }
                    else { draw(frame, &snapshot, &local); }
                })?;
                dirty = false;
            }
            tokio::select! {
                change = search.changed(), if search_open => {
                    search_open = change.is_ok();
                    if search_open { local.search.updated(&search); }
                    else { local.search.unavailable(); }
                    dirty = true;
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
                        history_panel.notice = Some("History reader closed; return to live execution.".into());
                    }
                    dirty = true;
                }
                change = client.snapshots.changed(), if snapshots_open => {
                    snapshots_open = change.is_ok();
                    dirty = true;
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
                                } else if history_panel.visible {
                                    if history_panel.key(key, &mut history, false) { return Ok(()); }
                                } else if key.code == KeyCode::F(12) {
                                    local.search.close();
                                    local.timeline.close();
                                    history_panel.open(&mut history, None);
                                } else if handle_key(key, &snapshot, &mut local, &client.commands) { return Ok(()); }
                                dirty = true;
                            }
                            InputEvent::Paste(text) => {
                                if local.palette.visible {
                                    local.palette.paste(&text);
                                    dirty = true;
                                    continue;
                                }
                                if history_panel.visible { history_panel.paste(&text); dirty = true; continue; }
                                let snapshot = client.snapshots.borrow().clone();
                                handle_paste(&text, &snapshot, &mut local);
                                dirty = true;
                            }
                            InputEvent::PasteRejected => {
                                if local.palette.visible {
                                    local.palette.reject_paste();
                                } else if history_panel.visible {
                                    history_panel.notice = Some(PASTE_REJECTED.into());
                                } else {
                                    reject_paste(&mut local);
                                }
                                dirty = true;
                            }
                            InputEvent::Resize(_, _) => dirty = true,
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
                        panel.notice = Some("History reader closed. Ctrl+Q exits.".into());
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
                                panel.notice = Some(PASTE_REJECTED.into());
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
