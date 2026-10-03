//! Local history navigation; every operation goes through the read-only service.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{age, wrap, Editor};
use crate::history::search::{Category, Query, Results, FIELD_BYTES};
use crate::history::{ExportPreview, HistoricalView, HistoryHandle, HistoryRequest, HistoryResult};
use crate::journal::SessionInfo;
use crate::state::{display_text, CoreSnapshot, SessionPhase};

#[derive(Default)]
pub(super) struct HistoryPanel {
    pub visible: bool,
    pub pending: Option<u64>,
    pub sessions: Vec<SessionInfo>,
    pub selected: usize,
    pub view: Option<Box<HistoricalView>>,
    pub preview: Option<ExportPreview>,
    form: Option<Form>,
    pub notice: Option<String>,
    scroll: usize,
    search_pending: Option<u64>,
    search_query: String,
    search_editing: bool,
    search_category: Category,
    search_selected: usize,
    search_results: Option<std::sync::Arc<Results>>,
}

enum Form {
    Sequence(Editor),
    Range(Editor),
    Destination(Editor),
}

impl HistoryPanel {
    pub fn open(&mut self, service: &mut HistoryHandle, session: Option<String>) {
        self.visible = true;
        if self.pending.is_some() {
            return;
        }
        self.form = None;
        self.search_pending = None;
        self.search_results = None;
        service.search.cancel();
        self.preview = None;
        self.view = None;
        self.scroll = 0;
        self.submit(
            service,
            match session {
                Some(session) => HistoryRequest::Open {
                    session,
                    sequence: None,
                },
                None => HistoryRequest::List,
            },
        );
    }

    fn submit(&mut self, service: &mut HistoryHandle, request: HistoryRequest) {
        if self.pending.is_some() {
            self.notice = Some("Wait for the current history operation.".into());
            return;
        }
        match service.request(request) {
            Ok(id) => {
                self.pending = Some(id);
                self.notice = None;
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
    }

    pub fn updated(&mut self, service: &mut HistoryHandle) {
        let update = service.status.borrow();
        if self.pending != Some(update.request_id) {
            return;
        }
        let Some(result) = &update.result else {
            return;
        };
        self.pending = None;
        match result {
            Ok(HistoryResult::Sessions(sessions)) => {
                let identity = self
                    .sessions
                    .get(self.selected)
                    .map(|session| session.session_id.clone());
                self.sessions = sessions.clone();
                self.selected = identity
                    .and_then(|id| {
                        self.sessions
                            .iter()
                            .position(|session| session.session_id == id)
                    })
                    .unwrap_or(0);
                self.view = None;
                self.preview = None;
                self.form = None;
            }
            Ok(HistoryResult::Loaded(view)) => {
                self.view = Some(view.clone());
                self.preview = None;
                self.form = None;
                self.scroll = 0;
                self.search_results = None;
                self.search_pending = None;
                service.search.cancel();
            }
            Ok(HistoryResult::Preview(preview)) => {
                self.preview = Some(preview.clone());
                let mut editor = Editor::default();
                editor.insert("native-agent-export.jsonl");
                self.form = Some(Form::Destination(editor));
                self.scroll = 0;
            }
            Ok(HistoryResult::Exported { bytes }) => {
                self.notice = Some(format!("Export saved: {bytes} bytes. Original identities were replaced by stable aliases."));
                self.form = None;
                self.preview = None;
            }
            Err(error) => {
                self.notice = Some(error.to_string());
                self.preview = None;
                self.form = None;
            }
        }
    }

    pub fn search_updated(&mut self, service: &HistoryHandle) -> bool {
        let status = service.search.status.borrow();
        if self.search_pending != Some(status.as_ref().map_or(0, |status| status.id)) {
            return false;
        }
        let Some(status) = status.as_ref() else {
            return false;
        };
        self.search_pending = None;
        match &status.result {
            Ok(results) => {
                self.search_results = Some(results.clone());
                self.search_selected = self
                    .search_selected
                    .min(results.hits.len().saturating_sub(1));
                self.notice = None;
            }
            Err(error) => {
                self.search_results = None;
                self.notice = Some(error.to_string());
            }
        }
        true
    }

    fn submit_search(&mut self, service: &mut HistoryHandle) {
        let Some(view) = &self.view else {
            self.notice = Some("Open a retained session before searching.".into());
            return;
        };
        let query = Query {
            text: self.search_query.clone(),
            category: self.search_category,
            ..Default::default()
        };
        match service.search.submit(view.info.session_id.clone(), query) {
            Ok(id) => {
                self.search_pending = Some(id);
                self.search_results = None;
                self.search_selected = 0;
                self.notice = None;
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
    }

    pub fn paste(&mut self, text: &str) {
        if self.search_editing {
            for character in text.chars().filter(|character| !character.is_control()) {
                if self.search_query.len() + character.len_utf8() > FIELD_BYTES {
                    self.notice = Some(format!(
                        "Historical search field limited to {FIELD_BYTES} UTF-8 bytes."
                    ));
                    break;
                }
                self.search_query.push(character);
            }
            return;
        }
        match &mut self.form {
            Some(Form::Destination(editor)) => editor.insert(&text.replace(['\n', '\r'], "")),
            Some(Form::Sequence(editor) | Form::Range(editor)) => {
                for character in text.chars().filter(char::is_ascii_digit).take(20) {
                    editor.insert(&character.to_string());
                }
            }
            None => {}
        }
    }

    /// Returns true only for application quit; historical keys never reach Core.
    pub fn key(&mut self, key: KeyEvent, service: &mut HistoryHandle, offline: bool) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && matches!(key.code, KeyCode::Char('q' | 'd')) {
            return true;
        }
        if key.code == KeyCode::Esc && self.pending.is_some() {
            self.notice =
                Some("Wait for the current operation; F12 returns to live execution.".into());
            return false;
        }
        if key.code == KeyCode::Esc {
            if self.form.is_some() || self.preview.is_some() {
                self.form = None;
                self.preview = None;
            } else if self.view.is_some() {
                service.search.cancel();
                self.search_pending = None;
                self.search_results = None;
                self.view = None;
                self.submit(service, HistoryRequest::List);
            } else if offline {
                return true;
            } else {
                self.visible = false;
            }
            return false;
        }
        if key.code == KeyCode::F(12) && !offline {
            service.search.cancel();
            self.search_pending = None;
            self.visible = false;
            return false;
        }
        if self.pending.is_some() {
            return false;
        }

        if self.view.is_some()
            && (self.search_editing
                || key.code == KeyCode::Char('/')
                || (control && key.code == KeyCode::Char('f')))
        {
            self.search_editing = true;
            if key.code == KeyCode::Char('/') || key.code == KeyCode::Char('f') {
                return false;
            }
        }
        if self.search_editing {
            match key.code {
                KeyCode::Enter => {
                    self.search_editing = false;
                    self.submit_search(service);
                }
                KeyCode::Esc => self.search_editing = false,
                KeyCode::Char('u') if control => self.search_query.clear(),
                KeyCode::Backspace => {
                    self.search_query.pop();
                }
                KeyCode::Char(character)
                    if !control && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    if self.search_query.len() + character.len_utf8() <= FIELD_BYTES {
                        self.search_query.push(character);
                    } else {
                        self.notice = Some(format!(
                            "Historical search field limited to {FIELD_BYTES} UTF-8 bytes."
                        ));
                    }
                }
                _ => {}
            }
            return false;
        }

        if self.view.is_some() {
            match key.code {
                KeyCode::F(1) => {
                    self.notice = Some("/ or Ctrl+F search metadata | F6 category | Up/Down hit | Enter locate | Esc closes".into());
                }
                KeyCode::F(6) => {
                    self.search_category = self.search_category.next();
                    self.submit_search(service);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.search_selected = self.search_selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('n') => {
                    if let Some(results) = &self.search_results {
                        self.search_selected =
                            (self.search_selected + 1).min(results.hits.len().saturating_sub(1));
                    }
                }
                KeyCode::Enter if self.search_results.is_some() => {
                    if let Some(hit) = self
                        .search_results
                        .as_ref()
                        .and_then(|results| results.hits.get(self.search_selected))
                    {
                        self.submit(
                            service,
                            HistoryRequest::Open {
                                session: hit.session_id.clone(),
                                sequence: Some(hit.event_seq),
                            },
                        );
                    } else {
                        self.notice = Some("No retained evidence hit is selected.".into());
                    }
                    return false;
                }
                _ => {}
            }
        }
        if let Some(form) = &mut self.form {
            let destination_form = matches!(form, Form::Destination(_));
            let editor = match form {
                Form::Sequence(editor) | Form::Range(editor) | Form::Destination(editor) => editor,
            };
            match key.code {
                KeyCode::Enter => {
                    let Some(view) = &self.view else {
                        return false;
                    };
                    let request = match form {
                        Form::Sequence(editor) | Form::Range(editor) => {
                            let Ok(sequence) = editor.text.parse::<u64>() else {
                                self.notice = Some("Enter an unsigned event sequence.".into());
                                return false;
                            };
                            if sequence > view.info.committed_seq {
                                self.notice = Some(format!(
                                    "Available event range: 0..={}",
                                    view.info.committed_seq
                                ));
                                return false;
                            }
                            if matches!(form, Form::Sequence(_)) {
                                HistoryRequest::Open {
                                    session: view.info.session_id.clone(),
                                    sequence: Some(sequence),
                                }
                            } else {
                                HistoryRequest::Preview {
                                    session: view.info.session_id.clone(),
                                    since: sequence,
                                }
                            }
                        }
                        Form::Destination(editor) => {
                            if editor.text.trim().is_empty() {
                                self.notice = Some("Choose a new output file.".into());
                                return false;
                            }
                            HistoryRequest::Export {
                                destination: editor.text.clone().into(),
                            }
                        }
                    };
                    self.submit(service, request);
                }
                KeyCode::Char('u') if control => editor.clear(),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(8),
                KeyCode::Left => editor.left(),
                KeyCode::Right => editor.right(),
                KeyCode::Home => editor.cursor = 0,
                KeyCode::End => editor.cursor = editor.text.len(),
                KeyCode::Backspace => editor.backspace(),
                KeyCode::Delete => editor.delete(),
                KeyCode::Char(character)
                    if !control
                        && !key.modifiers.contains(KeyModifiers::ALT)
                        && (destination_form || character.is_ascii_digit()) =>
                {
                    editor.insert(&character.to_string())
                }
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Up if self.view.is_none() => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down if self.view.is_none() => self.selected = (self.selected + 1).min(self.sessions.len().saturating_sub(1)),
            KeyCode::Enter if self.view.is_none() => {
                if let Some(session) = self.sessions.get(self.selected) {
                    self.submit(service, HistoryRequest::Open { session: session.session_id.clone(), sequence: None });
                }
            }
            KeyCode::Char('r') if !control => {
                if let Some(view) = &self.view {
                    self.submit(service, HistoryRequest::Open { session: view.info.session_id.clone(), sequence: None });
                } else { self.submit(service, HistoryRequest::List); }
            }
            KeyCode::Char('g' | 'e') if !control && self.view.is_some() => {
                let mut editor = Editor::default();
                editor.insert(if key.code == KeyCode::Char('e') { "0" } else { "" });
                self.form = Some(if key.code == KeyCode::Char('e') { Form::Range(editor) } else { Form::Sequence(editor) });
            }
            KeyCode::Left | KeyCode::Right if self.view.is_some() => {
                let view = self.view.as_ref().unwrap();
                let current = view.selected.event_seq;
                let sequence = if key.code == KeyCode::Left { current.saturating_sub(1) } else { (current + 1).min(view.info.committed_seq) };
                self.submit(service, HistoryRequest::Open { session: view.info.session_id.clone(), sequence: Some(sequence) });
            }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(8),
            KeyCode::Char('c' | 'y' | 'n') if control => self.notice = Some("History is read-only. Return to live execution to control the current session.".into()),
            KeyCode::F(7) | KeyCode::F(8) | KeyCode::F(9) => self.notice = Some("Historical tasks cannot resume. Their prompt was not retained; start a new task explicitly. Prior side effects may already have occurred.".into()),
            _ => {}
        }
        false
    }

    pub fn draw(&self, frame: &mut ratatui::Frame<'_>, live: Option<&CoreSnapshot>) {
        let area = frame.area();
        if area.height < 8 || area.width < 24 {
            frame.render_widget(
                Paragraph::new("History: read-only\nResize terminal\nCtrl+Q quit"),
                area,
            );
            return;
        }
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(if area.height < 16 { 3 } else { 4 }),
                Constraint::Min(1),
                Constraint::Length(if self.form.is_some() && area.height >= 16 {
                    5
                } else {
                    3
                }),
                Constraint::Length(1),
            ])
            .split(area);
        let mut header = vec!["HISTORY | read-only".into()];
        header.push(if let Some(live) = live {
            format!(
                "Live: {:?} | requests: {} | F12 return",
                live.phase,
                live.requests.len()
            )
        } else {
            "Offline | no Codex execution".into()
        });
        header.push(
            "Recorded ages are frozen. Missing prompts, text and answers are unavailable.".into(),
        );
        frame.render_widget(
            Paragraph::new(header.join("\n")).block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );
        let mut rows = Vec::new();
        if let Some(notice) = &self.notice {
            rows.push(notice.clone());
        }
        if let Some(preview) = &self.preview {
            rows.push("EXPORT PREVIEW | stable identity aliases; no reverse mapping".into());
            rows.push(format!(
                "Selected range: baseline {} through committed {} | historical result: {}",
                preview.manifest["since"],
                preview.manifest["high_watermark"],
                preview.manifest["execution_result"]
            ));
            rows.push(
                "Only retained state/evidence is exported. Existing files are never replaced."
                    .into(),
            );
            rows.push("Preview excerpt (up to 8 KiB):".into());
            rows.push(preview.excerpt.clone());
        } else if let Some(view) = &self.view {
            let result = view.info.execution_result.unwrap_or(SessionPhase::Unknown);
            rows.push(format!(
                "Result: {result:?} | {}",
                if view.info.needs_recovery {
                    "REVIEW REQUIRED"
                } else {
                    "closed history"
                }
            ));
            rows.push(format!(
                "Session {} | selected event {} / {} | schema {}",
                view.info.session_id,
                view.selected.event_seq,
                view.info.committed_seq,
                view.info.schema_version
            ));
            rows.push(format!("Latest recorded result: {result:?} | closed: {} | needs review: {} | uncommitted tail: {}", view.info.session_closed, view.info.needs_recovery, view.uncommitted_tail));
            rows.push(format!(
                "Search metadata: {:?} | category {} | {}",
                self.search_query,
                self.search_category.label(),
                if self.search_editing {
                    "editing; Enter runs search"
                } else {
                    "/ or Ctrl+F edit; F6 changes category"
                }
            ));
            if let Some(results) = &self.search_results {
                let retained = results.hits.len() as u64;
                let omitted = results.total.saturating_sub(retained);
                rows.push(format!(
                    "Search scope: committed journal prefix through event {} | uncommitted tail excluded | hits {}/{} | omitted {} | deduplicated evidence omitted {}",
                    results.info.committed_seq,
                    retained,
                    results.total,
                    omitted,
                    results.omitted_evidence
                ));
                if let Some(hit) = results.hits.get(self.search_selected) {
                    rows.push(format!(
                        "Selected hit {}/{}: event {} | {}",
                        self.search_selected + 1,
                        results.hits.len(),
                        hit.event_seq,
                        hit.metadata()
                    ));
                } else if results.total == 0 {
                    rows.push("No retained metadata matched this query. Prompts, answers, secrets, commands and raw output are never searched.".into());
                }
            } else if self.search_pending.is_some() {
                rows.push("Searching retained metadata… uncommitted tail excluded; prompts, answers, secrets, commands and raw output are never searched.".into());
            } else {
                rows.push("Search scope: fixed committed journal prefix; prompts, answers, secrets, commands and raw output are never searched.".into());
            }
            if let Ok(state) = view.selected.state() {
                rows.push(format!(
                    "Selected recorded phase: {:?} | cleanup confirmed: {:?} | issue: {:?}",
                    state.phase, state.cleanup_confirmed, state.issue
                ));
                if view.info.needs_recovery {
                    rows.push("Unconfirmed external outcomes remain unknown. Inspect evidence before a new task; prior side effects may already have occurred.".into());
                }
                for task in &state.tasks {
                    rows.push(format!(
                        "Task {} / attempt {} | {:?} | recorded {:?} | dependencies {:?}",
                        task.id.0, task.attempt, task.kind, task.state, task.dependencies
                    ));
                }
                for activity in &state.observation.activities {
                    rows.push(format!("{} | {:?} / {:?} | {:?} | historical quiet {} | progress {} | freshness {:?}", activity.identity.agent_id, activity.scope, activity.kind, activity.execution_state, age(activity.silence_ms), activity.progress_seq, activity.freshness));
                    rows.push(format!(
                        "  thread {:?} turn {:?} generation {:?} item {:?} request {:?}",
                        activity.identity.thread_id,
                        activity.identity.turn_id,
                        activity.identity.generation,
                        activity.item_id,
                        activity.request_id
                    ));
                    rows.push(format!(
                        "  attention {:?} / {:?} | source {:?} | last evidence {:?}",
                        activity.attention.level,
                        activity.attention.reason,
                        activity.attention.config_source,
                        activity.last_evidence
                    ));
                    if activity.request_id.is_some() {
                        rows.push(
                            "  Historical request: unavailable; answers cannot be sent.".into(),
                        );
                    }
                    if !activity.wait_targets.is_empty() {
                        rows.push(format!(
                            "  Waiting {:?} -> {:?} | resume {:?}",
                            activity.wait_reason, activity.wait_targets, activity.resume_condition
                        ));
                    }
                }
            }
        } else if self.sessions.is_empty() {
            rows.push("No retained sessions in this workspace. r refresh | Esc return".into());
        } else {
            rows.push(
                "Select a retained session. Enter opens its latest committed evidence.".into(),
            );
            for (index, session) in self.sessions.iter().enumerate() {
                rows.push(format!(
                    "{} {} | {:?} | event {} | {}",
                    if index == self.selected { ">" } else { " " },
                    session.session_id,
                    session.execution_result.unwrap_or(SessionPhase::Unknown),
                    session.committed_seq,
                    if session.needs_recovery {
                        "REVIEW REQUIRED"
                    } else {
                        "closed"
                    }
                ));
            }
        }
        let width = chunks[1].width.saturating_sub(2) as usize;
        let body_height = chunks[1].height.saturating_sub(2) as usize;
        let selected_offset = if self.view.is_none() && !self.sessions.is_empty() {
            rows.iter()
                .take(self.selected + 1)
                .map(|row| wrap(&display_text(row), width).len())
                .sum::<usize>()
                .saturating_sub(body_height / 2)
        } else {
            0
        };
        let rows: Vec<Line> = rows
            .iter()
            .flat_map(|row| {
                wrap(
                    &display_text(row),
                    chunks[1].width.saturating_sub(2) as usize,
                )
            })
            .skip(selected_offset.saturating_add(self.scroll))
            .take(body_height)
            .map(Line::from)
            .collect();
        frame.render_widget(
            Paragraph::new(rows).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Historical observation "),
            ),
            chunks[1],
        );
        let (title, editor) = match &self.form {
            Some(Form::Sequence(editor)) => (" Go to event · Enter opens ", Some(editor)),
            Some(Form::Range(editor)) => {
                (" Export baseline · 0 = all · Enter previews ", Some(editor))
            }
            Some(Form::Destination(editor)) => {
                (" NEW export file · Enter saves preview ", Some(editor))
            }
            None => (" Actions ", None),
        };
        let mut prompt = "Return to live execution or use --run with a fresh task. Prior side effects may have occurred.".to_owned();
        let mut input_cursor = None;
        if let Some(editor) = editor {
            let mut left = editor.text[..editor.cursor].to_owned();
            let width = chunks[2].width.saturating_sub(2) as usize;
            while left.width() >= width && !left.is_empty() {
                let length = left.graphemes(true).next().unwrap().len();
                left.drain(..length);
            }
            input_cursor = Some(left.width());
            prompt = format!("{left}{}", &editor.text[editor.cursor..]);
        }
        let notice = if self.pending.is_some() {
            "Loading history / export; live execution continues."
        } else {
            self.notice.as_deref().unwrap_or("")
        };
        frame.render_widget(
            Paragraph::new(
                wrap(
                    &format!("{prompt}\n{notice}"),
                    chunks[2].width.saturating_sub(2) as usize,
                )
                .into_iter()
                .map(Line::from)
                .collect::<Vec<_>>(),
            )
            .block(Block::default().borders(Borders::ALL).title(title)),
            chunks[2],
        );
        if let Some(cursor) = input_cursor {
            frame.set_cursor_position((chunks[2].x + 1 + cursor as u16, chunks[2].y + 1));
        }
        frame.render_widget(Paragraph::new("Enter open | Left/Right event | g sequence | e export | r refresh | PgUp/PgDn | Esc back | Ctrl+Q quit"), chunks[3]);
    }
}

#[cfg(test)]
mod tests;
