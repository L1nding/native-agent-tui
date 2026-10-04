//! Local browsing state for Core's bounded evidence archive. No execution sender.
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::scope::Scope;
use super::{search, wrap, Editor};
use crate::interactions::RequestRef;
use crate::observation::{EvidenceKind, ExecutionState};
use crate::state::{display_text, CoreSnapshot};
use crate::timeline::{TimelineEntry, BYTE_LIMIT, ENTRY_LIMIT};

const FIELD_BYTES: usize = 1024;
const BOOKMARK_LIMIT: usize = 64;
const BOOKMARK_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Category {
    #[default]
    All,
    Lifecycle,
    Output,
    Tool,
    Request,
    Waiting,
}

impl Category {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Lifecycle,
            Self::Lifecycle => Self::Output,
            Self::Output => Self::Tool,
            Self::Tool => Self::Request,
            Self::Request => Self::Waiting,
            Self::Waiting => Self::All,
        }
    }

    fn accepts(self, kind: EvidenceKind) -> bool {
        use EvidenceKind::*;
        let category = match kind {
            Output | MessageFinalized => Self::Output,
            ToolStarted | ToolCompleted => Self::Tool,
            RequestCreated | RequestAnswered | RequestResolved | RequestExpired => Self::Request,
            GateEntered | GateReleased | ChildTurnBound | ChildTerminal => Self::Waiting,
            _ => Self::Lifecycle,
        };
        self == Self::All || self == category
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    All,
    Active,
    Terminal,
    Unknown,
}

#[derive(Clone, Copy)]
enum Field {
    Query,
    Turn,
}

struct Bookmark {
    session: String,
    entry: Arc<TimelineEntry>,
}

impl Bookmark {
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.session.capacity() + self.entry.metadata_bytes()
    }
}

pub(super) enum Locate {
    Message(search::Focus),
    Request(RequestRef),
}

#[derive(Default)]
pub(super) struct TimelinePanel {
    pub visible: bool,
    session: String,
    thread: String,
    scope: Scope,
    category: Category,
    state: State,
    query: Editor,
    turn: Editor,
    editing: Option<Field>,
    selected: Option<u64>,
    bookmarks: Vec<Bookmark>,
    bookmarks_only: bool,
    pending_only: bool,
    scroll: usize,
    help: bool,
    notice: Option<String>,
}

impl TimelinePanel {
    pub fn open(&mut self, thread: String, current: &CoreSnapshot) {
        self.visible = true;
        self.help = false;
        self.editing = None;
        if self.thread != thread || self.session != current.timeline.session_id {
            self.thread = thread;
            self.session = current.timeline.session_id.clone();
            self.selected = None;
        }
        if self.thread.is_empty() {
            self.scope = Scope::All;
        }
        self.sync(current);
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.editing = None;
    }

    pub fn sync(&mut self, current: &CoreSnapshot) {
        if !self.visible {
            return;
        }
        if self.session != current.timeline.session_id {
            self.session = current.timeline.session_id.clone();
            self.selected = None;
        }
        if self.selected.is_none() {
            self.select_latest(current);
        }
    }

    fn rows(&self, current: &CoreSnapshot) -> Vec<Arc<TimelineEntry>> {
        let threads = self.scope.threads(&self.thread, current);
        let accepts = |entry: &TimelineEntry| {
            (self.scope == Scope::All
                || entry
                    .identity
                    .thread_id
                    .as_ref()
                    .is_some_and(|id| threads.contains(id)))
                && (self.turn.text.is_empty()
                    || entry.identity.turn_id.as_ref() == Some(&self.turn.text))
                && self.category.accepts(entry.evidence.kind)
                && match self.state {
                    State::All => true,
                    State::Active => active(entry.execution_state),
                    State::Terminal => matches!(
                        entry.execution_state,
                        ExecutionState::Completed
                            | ExecutionState::Failed
                            | ExecutionState::Interrupted
                    ),
                    State::Unknown => entry.execution_state == ExecutionState::Unknown,
                }
                && (!self.pending_only
                    || entry.request.as_ref().is_some_and(|reference| {
                        current
                            .requests
                            .iter()
                            .any(|request| request.matches(reference) && !request.responding)
                    }))
                && (self.query.text.is_empty()
                    || metadata(entry).join("\n").contains(&self.query.text))
        };
        let mut rows: Vec<_> = if self.bookmarks_only {
            self.bookmarks
                .iter()
                .filter(|bookmark| bookmark.session == self.session)
                .map(|bookmark| &bookmark.entry)
                .filter(|entry| accepts(entry))
                .cloned()
                .collect()
        } else {
            current
                .timeline
                .entries
                .iter()
                .filter(|entry| accepts(entry))
                .cloned()
                .collect()
        };
        rows.sort_by_key(|entry| entry.evidence.id);
        rows
    }

    fn select_latest(&mut self, current: &CoreSnapshot) {
        self.selected = self.rows(current).last().map(|entry| entry.evidence.id);
        self.scroll = 0;
        self.notice = None;
    }

    fn selected(&self, current: &CoreSnapshot) -> Option<Arc<TimelineEntry>> {
        self.rows(current)
            .into_iter()
            .find(|entry| Some(entry.evidence.id) == self.selected)
    }

    fn retained(&self, entry: &TimelineEntry, current: &CoreSnapshot) -> bool {
        self.session == current.timeline.session_id
            && current
                .timeline
                .entries
                .iter()
                .any(|retained| retained.evidence.id == entry.evidence.id)
    }

    fn editor(&mut self) -> &mut Editor {
        match self.editing {
            Some(Field::Turn) => &mut self.turn,
            _ => &mut self.query,
        }
    }

    fn insert(&mut self, text: &str) {
        let text = display_text(text).replace(['\n', '\t'], " ");
        if self.editor().text.len() + text.len() <= FIELD_BYTES {
            self.editor().insert(&text);
        } else {
            self.notice = Some("Timeline field limited to 1024 UTF-8 bytes.".into());
        }
    }

    pub fn paste(&mut self, text: &str) {
        if self.editing.is_some() {
            self.insert(text);
        }
    }
    pub fn reject_paste(&mut self) {
        self.notice = Some(super::PASTE_REJECTED.into());
    }

    fn toggle_bookmark(&mut self, current: &CoreSnapshot) {
        let Some(entry) = self.selected(current) else {
            self.notice =
                Some("Selected evidence was evicted or filtered; select a retained event.".into());
            return;
        };
        if let Some(index) = self.bookmarks.iter().position(|bookmark| {
            bookmark.session == self.session && bookmark.entry.evidence.id == entry.evidence.id
        }) {
            self.bookmarks.remove(index);
            self.notice = Some("Bookmark removed.".into());
            return;
        }
        let bookmark = Bookmark {
            session: self.session.clone(),
            entry,
        };
        if self.bookmarks.len() >= BOOKMARK_LIMIT
            || self.bookmarks.iter().map(Bookmark::bytes).sum::<usize>() + bookmark.bytes()
                > BOOKMARK_BYTES
        {
            self.notice = Some(
                "Bookmark budget reached (64 entries / 64 KiB); remove a bookmark first.".into(),
            );
        } else {
            self.bookmarks.push(bookmark);
            self.notice = Some("Bookmark saved locally for this live session.".into());
        }
    }

    pub fn key(&mut self, key: KeyEvent, current: &CoreSnapshot) -> Option<Locate> {
        self.sync(current);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Esc {
            self.close();
            return None;
        }
        if key.code == KeyCode::F(1) {
            self.help = !self.help;
            self.scroll = 0;
            return None;
        }
        if self.help {
            match key.code {
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(1),
                KeyCode::Home => self.scroll = 0,
                KeyCode::End => self.scroll = usize::MAX,
                _ => {}
            }
            return None;
        }
        if ctrl && key.code == KeyCode::Char('t') {
            self.editing = Some(Field::Turn);
            return None;
        }
        if ctrl && key.code == KeyCode::Char('f')
            || self.editing.is_none() && key.code == KeyCode::Char('/')
        {
            self.editing = Some(Field::Query);
            return None;
        }
        if self.editing.is_some() {
            match key.code {
                KeyCode::Enter => {
                    self.editing = None;
                    self.select_latest(current);
                }
                KeyCode::Char('u') if ctrl => self.editor().clear(),
                KeyCode::Left => self.editor().left(),
                KeyCode::Right => self.editor().right(),
                KeyCode::Home => self.editor().cursor = 0,
                KeyCode::End => self.editor().cursor = self.editor().text.len(),
                KeyCode::Backspace => self.editor().backspace(),
                KeyCode::Delete => self.editor().delete(),
                KeyCode::Char(ch) if !ctrl || key.modifiers.contains(KeyModifiers::ALT) => {
                    self.insert(&ch.to_string())
                }
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Tab => {
                self.scope = self.scope.next();
                self.select_latest(current);
            }
            KeyCode::F(6) => {
                self.category = self.category.next();
                self.select_latest(current);
            }
            KeyCode::F(7) => {
                self.state = match self.state {
                    State::All => State::Active,
                    State::Active => State::Terminal,
                    State::Terminal => State::Unknown,
                    State::Unknown => State::All,
                };
                self.select_latest(current);
            }
            KeyCode::F(8) => {
                self.pending_only = !self.pending_only;
                self.select_latest(current);
            }
            KeyCode::Char('b') if !ctrl => self.toggle_bookmark(current),
            KeyCode::Char('B') if !ctrl => {
                self.bookmarks_only = !self.bookmarks_only;
                self.select_latest(current);
            }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Char('n')
            | KeyCode::Char('N')
            | KeyCode::Char('j')
            | KeyCode::Char('k')
                if !ctrl =>
            {
                let rows = self.rows(current);
                let index = rows
                    .iter()
                    .position(|entry| Some(entry.evidence.id) == self.selected);
                let next = match key.code {
                    KeyCode::Home => 0,
                    KeyCode::End => rows.len().saturating_sub(1),
                    KeyCode::Up | KeyCode::Char('N') | KeyCode::Char('k') => index
                        .map_or(rows.len().saturating_sub(1), |index| {
                            index.saturating_sub(1)
                        }),
                    _ => index.map_or(0, |index| (index + 1).min(rows.len().saturating_sub(1))),
                };
                self.selected = rows.get(next).map(|entry| entry.evidence.id);
                self.scroll = 0;
                self.notice = None;
            }
            KeyCode::Enter if !ctrl => {
                let Some(entry) = self.selected(current) else {
                    self.notice = Some(
                        "Selected evidence was evicted or filtered; navigation required.".into(),
                    );
                    return None;
                };
                if !self.retained(&entry, current) {
                    self.notice =
                        Some("Bookmarked content was evicted; original metadata only.".into());
                    return None;
                }
                if let Some(reference) = &entry.request {
                    if current
                        .requests
                        .iter()
                        .any(|request| request.matches(reference))
                    {
                        self.close();
                        return Some(Locate::Request(reference.clone()));
                    }
                    self.notice =
                        Some("This request delivery has ended; no current request matches.".into());
                } else if matches!(
                    entry.evidence.kind,
                    EvidenceKind::Output | EvidenceKind::MessageFinalized
                ) {
                    if let Some(message) = current.messages.iter().find(|message| {
                        message.role == "Agent"
                            && Some(&message.thread_id) == entry.identity.thread_id.as_ref()
                            && Some(&message.turn_id) == entry.identity.turn_id.as_ref()
                            && Some(&message.id) == entry.item_id.as_ref()
                    }) {
                        self.close();
                        return Some(Locate::Message(search::Focus::message(message)));
                    }
                    self.notice = Some(
                        "Message body unavailable or evicted; evidence metadata remains.".into(),
                    );
                } else {
                    self.notice =
                        Some("Evidence details shown; no retained message or request link.".into());
                }
            }
            _ => {}
        }
        None
    }

    pub fn draw(&self, frame: &mut ratatui::Frame<'_>, area: Rect, current: &CoreSnapshot) {
        frame.render_widget(Clear, area);
        if area.height == 0 {
            return;
        }
        let rows = self.rows(current);
        let index = rows
            .iter()
            .position(|entry| Some(entry.evidence.id) == self.selected);
        let retained = current
            .timeline
            .entries
            .front()
            .zip(current.timeline.entries.back())
            .map_or_else(
                || "empty".into(),
                |(first, last)| format!("#{}..#{}", first.evidence.id, last.evidence.id),
            );
        let range = format!(
            "Retained {}; accepted through #{}; omitted {} (gaps possible); budget {} / {} KiB",
            retained,
            current.timeline.high_water,
            current.timeline.dropped_entries,
            ENTRY_LIMIT,
            BYTE_LIMIT / 1024
        );
        let filters = format!(
            "{:?} / {:?} / {:?}{}{}",
            self.scope,
            self.category,
            self.state,
            if self.bookmarks_only {
                " / bookmarks"
            } else {
                ""
            },
            if self.pending_only {
                " / current pending"
            } else {
                ""
            }
        );
        let header_height = area.height.min(if area.height >= 12 { 4 } else { 2 });
        let mut header = vec![
            format!(
                "Evidence timeline {}/{} | {} bookmarks",
                index.map_or(0, |index| index + 1),
                rows.len(),
                self.bookmarks.len()
            ),
            range.clone(),
            filters.clone(),
            format!(
                "Thread: {} | turn: {} | query: {}",
                self.thread,
                if self.turn.text.is_empty() {
                    "all"
                } else {
                    &self.turn.text
                },
                self.query.text
            ),
        ];
        if let Some(field) = self.editing {
            let editor = match field {
                Field::Query => &self.query,
                Field::Turn => &self.turn,
            };
            let mut left = editor.text[..editor.cursor].to_string();
            let prefix = match field {
                Field::Query => "Metadata: ",
                Field::Turn => "Exact turn: ",
            };
            let width = (area.width as usize).saturating_sub(prefix.width()).max(1);
            while left.width() >= width && !left.is_empty() {
                let bytes = left.graphemes(true).next().unwrap().len();
                left.drain(..bytes);
            }
            header[0] = format!(
                "Timeline: edit {} (Enter apply)",
                match field {
                    Field::Query => "metadata",
                    Field::Turn => "turn",
                }
            );
            header[1] = format!("{prefix}{left}{}", &editor.text[editor.cursor..]);
            if area.width as usize > prefix.width() && area.height > 1 {
                frame.set_cursor_position((
                    area.x + (prefix.width() + left.width()) as u16,
                    area.y + 1,
                ));
            }
        }
        frame.render_widget(
            Paragraph::new(
                header
                    .into_iter()
                    .map(|text| Line::from(display_text(&text)))
                    .collect::<Vec<_>>(),
            ),
            Rect {
                height: header_height,
                ..area
            },
        );
        let body = Rect {
            y: area.y + header_height,
            height: area.height.saturating_sub(header_height + 1),
            ..area
        };
        let mut details = Vec::new();
        if let Some(notice) = &self.notice {
            details.push(notice.clone());
        }
        if self.help {
            details.extend(HELP.iter().map(|line| (*line).to_owned()));
        } else if let Some(entry) = index.and_then(|index| rows.get(index)) {
            details.push(format!(
                "#{} {:?} | {:?} / {:?}",
                entry.evidence.id, entry.evidence.kind, entry.scope, entry.execution_state
            ));
            if !self.retained(entry, current) {
                details.push("BOOKMARK: content evicted; original metadata only".into());
            }
            details.extend(metadata(entry));
            details.push("Enter: locate retained message / current request delivery".into());
            details.push("Raw tool output and reasoning: unavailable here".into());
        } else if self.selected.is_some() {
            details.push(
                "Selected evidence evicted or filtered. Up/Down selects an available event.".into(),
            );
        } else {
            details.push("No evidence matches this retained range and filter.".into());
        }
        let detail_area = if area.width >= 100 && body.height >= 4 && !self.help {
            let panels = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
                .split(body);
            let height = panels[0].height.saturating_sub(2) as usize;
            let start = index
                .unwrap_or(0)
                .saturating_sub(height / 2)
                .min(rows.len().saturating_sub(height));
            let list: Vec<_> = rows
                .iter()
                .skip(start)
                .take(height)
                .map(|entry| {
                    Line::from(display_text(&format!(
                        "{}{} #{} {:?}",
                        if Some(entry.evidence.id) == self.selected {
                            ">"
                        } else {
                            " "
                        },
                        if self
                            .bookmarks
                            .iter()
                            .any(|bookmark| bookmark.session == self.session
                                && bookmark.entry.evidence.id == entry.evidence.id)
                        {
                            "*"
                        } else {
                            " "
                        },
                        entry.evidence.id,
                        entry.evidence.kind
                    )))
                })
                .collect();
            frame.render_widget(
                Paragraph::new(list).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Evidence acceptance order "),
                ),
                panels[0],
            );
            panels[1]
        } else {
            body
        };
        if header_height < 4 && !self.help {
            details.push(filters);
            details.push(range);
        }
        let lines: Vec<_> = details
            .iter()
            .flat_map(|line| wrap(line, detail_area.width as usize))
            .collect();
        let start = self
            .scroll
            .min(lines.len().saturating_sub(detail_area.height as usize));
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(start)
                    .take(detail_area.height as usize)
                    .map(Line::from)
                    .collect::<Vec<_>>(),
            ),
            detail_area,
        );
        if area.height > header_height {
            frame.render_widget(
                Paragraph::new("Enter locate  b bookmark  / filter  Esc close  F1 help"),
                Rect {
                    y: area.y + area.height - 1,
                    height: 1,
                    ..area
                },
            );
        }
    }
}

fn active(state: ExecutionState) -> bool {
    matches!(
        state,
        ExecutionState::Starting | ExecutionState::Running | ExecutionState::Waiting
    )
}

fn metadata(entry: &TimelineEntry) -> Vec<String> {
    let mut rows = vec![
        format!(
            "Evidence: #{} {:?} | scope: {:?} | execution: {:?}",
            entry.evidence.id, entry.evidence.kind, entry.scope, entry.execution_state
        ),
        format!(
            "Activity: {:?} | tool: {:?} | source: {:?}",
            entry.activity_kind, entry.tool_category, entry.evidence.source
        ),
        format!(
            "Thread: {}",
            entry.identity.thread_id.as_deref().unwrap_or("unavailable")
        ),
        format!(
            "Turn: {}",
            entry.identity.turn_id.as_deref().unwrap_or("unavailable")
        ),
        format!(
            "Agent: {} | task: {:?} | attempt: {:?} | generation: {:?}",
            entry.identity.agent_id,
            entry.identity.task_id,
            entry.identity.attempt_id,
            entry.identity.generation
        ),
        format!(
            "Item: {} | request: {:?}",
            entry.item_id.as_deref().unwrap_or("unavailable"),
            entry.evidence.request_id
        ),
        format!(
            "Interaction: {:?} | recorded wall ms: {:?} | output bytes: {}",
            entry.interaction_state, entry.evidence.recorded_at_ms, entry.evidence.output_bytes
        ),
    ];
    if let Some(request) = &entry.request {
        rows.push(format!(
            "Request delivery: {:?} / {} / {} / received {}",
            request.id, request.thread_id, request.turn_id, request.received_seq
        ));
    }
    if entry.tool_category == Some(crate::protocol::ToolCategory::Compaction)
        || entry.compaction.is_some()
    {
        rows.extend(super::compaction_fact_rows(entry.compaction.as_deref()));
    }
    for target in &entry.wait_targets {
        rows.push(format!(
            "Wait target: {} / turn {} / generation {} / {:?}",
            target.id,
            target.turn_id.as_deref().unwrap_or("unbound"),
            target.generation,
            target.outcome
        ));
    }
    rows
}

const HELP: &[&str] = &[
    "Up/Down or j/k, n/N: previous/next evidence",
    "Home/End: first/last match; PgUp/Dn: scroll details/help",
    "Tab: thread/subtree/path/all (confirmed relationships)",
    "F6: event category; F7: execution state at the event",
    "F8: only evidence linked to current pending requests",
    "/ or Ctrl+F: metadata text; Ctrl+T: exact turn ID; Enter applies",
    "b: toggle bookmark; B: bookmarks only (filters still apply)",
    "Enter: current request or retained Agent message; no execution",
    "Esc: close; F2: requests; F3: agent; F4: tasks; F11: activity",
    "Ctrl+C: interrupt root; Ctrl+Q: quit and clean up",
    "Acceptance order is evidence order, not RPC ingress or journal cursor",
    "State is frozen at the event; current state remains in the header",
    "Metadata only: no prompts, secrets, raw outputs or historical transcript",
    "Evicted bookmarks keep metadata; opening unavailable content is disabled",
    "Bookmarks are local to this TUI; not persisted, exported or restored",
    "Live archive: 512 entries / 256 KiB; bookmarks: 64 / 64 KiB",
];

#[cfg(test)]
mod tests;
