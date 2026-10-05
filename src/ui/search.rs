//! Bounded, read-only search of the retained conversation projection.
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tokio::sync::watch;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::scope::Scope;
use super::tool_search::{self, CategoryFilter, LifecycleFilter};
use super::{wrap, Editor};
use crate::state::{display_text, ConversationItem, CoreSnapshot};

const QUERY_BYTES: usize = 1024;
const HIT_LIMIT: usize = 512;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Role {
    #[default]
    All,
    You,
    Agent,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    All,
    Streaming,
    Complete,
    Truncated,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ContentScope {
    #[default]
    Messages,
    Tools,
    Both,
}

impl ContentScope {
    fn next(self) -> Self {
        match self {
            Self::Messages => Self::Tools,
            Self::Tools => Self::Both,
            Self::Both => Self::Messages,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Filter {
    text: String,
    thread: String,
    turn: String,
    scope: Scope,
    role: Role,
    state: State,
    content: ContentScope,
    tool_category: CategoryFilter,
    tool_lifecycle: LifecycleFilter,
}

impl Filter {
    fn threads(&self, source: &CoreSnapshot) -> Vec<String> {
        self.scope.threads(&self.thread, source)
    }

    fn accepts(&self, message: &ConversationItem, threads: &[String]) -> bool {
        (self.scope == Scope::All || threads.contains(&message.thread_id))
            && (self.turn.is_empty() || self.turn == message.turn_id)
            && match self.role {
                Role::All => true,
                Role::You => message.role == "You",
                Role::Agent => message.role == "Agent",
            }
            && match self.state {
                State::All => true,
                State::Streaming => !message.complete,
                State::Complete => message.complete,
                State::Truncated => message.truncated,
            }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MessageKey {
    thread: String,
    turn: String,
    item: String,
    role: String,
}

impl MessageKey {
    fn of(message: &ConversationItem) -> Self {
        Self {
            thread: message.thread_id.clone(),
            turn: message.turn_id.clone(),
            item: message.id.clone(),
            role: message.role.clone(),
        }
    }

    pub fn matches(&self, message: &ConversationItem) -> bool {
        self.thread == message.thread_id
            && self.turn == message.turn_id
            && self.item == message.id
            && self.role == message.role
    }
}

#[derive(Clone, Debug)]
pub(super) struct Focus {
    key: MessageKey,
    pub offset: usize,
    text: String,
}

impl Focus {
    pub fn message(message: &ConversationItem) -> Self {
        Self {
            key: MessageKey::of(message),
            offset: 0,
            text: message.text.clone(),
        }
    }

    pub fn matches(&self, message: &ConversationItem) -> bool {
        self.key.matches(message) && self.text == message.text
    }

    pub fn thread(&self) -> &str {
        &self.key.thread
    }

    pub fn row(&self, width: usize) -> usize {
        let text = display_text(&self.text);
        let offset = display_text(&self.text[..self.offset]).len();
        let mut row = 0;
        let mut cells = 0;
        for (index, grapheme) in text.grapheme_indices(true) {
            if grapheme == "\n" {
                if index >= offset {
                    return row;
                }
                row += 1;
                cells = 0;
            } else {
                let count = grapheme.width();
                if cells + count > width.max(1) && cells > 0 {
                    row += 1;
                    cells = 0;
                }
                if index + grapheme.len() > offset {
                    return row;
                }
                cells += count;
            }
        }
        row
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Hit {
    message: usize,
    range: Range<usize>,
}

struct Job {
    id: u64,
    source: Arc<CoreSnapshot>,
    filter: Filter,
}

struct Results {
    id: u64,
    source: Option<MessageSource>,
    snapshot_version: u64,
    filter: Filter,
    hits: Vec<Hit>,
    total: usize,
    tool_hits: Vec<tool_search::Hit>,
    tool_total: usize,
}

struct OpenedToolHit {
    locator: crate::tool_details::ToolDetailLocator,
    field: tool_search::Field,
    range: Range<usize>,
    revision: u64,
}

struct MessageSource {
    messages: Vec<ConversationItem>,
    history_truncated: bool,
}

impl Results {
    fn message_hits(&self) -> usize {
        if self.filter.content == ContentScope::Tools {
            0
        } else {
            self.hits.len()
        }
    }

    fn tool_hits(&self) -> usize {
        if self.filter.content == ContentScope::Messages {
            0
        } else {
            self.tool_hits.len()
        }
    }

    fn count(&self) -> usize {
        self.message_hits() + self.tool_hits()
    }
}

async fn scan(job: Arc<Job>) -> Results {
    let threads = job.filter.threads(&job.source);
    let mut hits = Vec::new();
    let mut matched_messages = Vec::new();
    let mut message_indexes = HashMap::new();
    let mut total = 0;
    if !job.filter.text.is_empty() && job.filter.content != ContentScope::Tools {
        for (message, item) in job.source.messages.iter().enumerate() {
            if job.filter.accepts(item, &threads) {
                for (offset, _) in item.text.match_indices(&job.filter.text) {
                    total += 1;
                    if hits.len() < HIT_LIMIT {
                        let projected = *message_indexes.entry(message).or_insert_with(|| {
                            matched_messages.push(item.clone());
                            matched_messages.len() - 1
                        });
                        hits.push(Hit {
                            message: projected,
                            range: offset..offset + job.filter.text.len(),
                        });
                    }
                    if total % 128 == 0 {
                        tokio::task::yield_now().await;
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    }
    let (tool_hits, tool_total) = if job.filter.content != ContentScope::Messages {
        tool_search::scan(
            &job.source.tool_details,
            &job.filter.text,
            if job.filter.scope == Scope::All {
                None
            } else {
                Some(threads.as_slice())
            },
            &job.filter.turn,
            job.filter.tool_category,
            job.filter.tool_lifecycle,
            HIT_LIMIT.saturating_sub(hits.len()),
        )
    } else {
        (Vec::new(), 0)
    };
    Results {
        id: job.id,
        source: (job.filter.content != ContentScope::Tools).then(|| MessageSource {
            messages: matched_messages,
            history_truncated: job.source.history_truncated,
        }),
        snapshot_version: job.source.version,
        filter: job.filter.clone(),
        hits,
        total,
        tool_hits,
        tool_total,
    }
}

pub(super) struct SearchHandle {
    jobs: watch::Sender<Option<Arc<Job>>>,
    status: watch::Receiver<Option<Arc<Results>>>,
    join: Option<tokio::task::JoinHandle<()>>,
    next_id: u64,
}

impl SearchHandle {
    pub fn spawn() -> Self {
        let (jobs, mut input) = watch::channel::<Option<Arc<Job>>>(None);
        let (output, status) = watch::channel(None);
        let join = tokio::spawn(async move {
            loop {
                if input.changed().await.is_err() {
                    break;
                }
                loop {
                    let Some(job) = input.borrow_and_update().clone() else {
                        output.send_replace(None);
                        break;
                    };
                    tokio::select! {
                        biased;
                        changed = input.changed() => { if changed.is_err() { return; } }
                        result = scan(job) => {
                            if !input.has_changed().unwrap_or(true) { output.send_replace(Some(Arc::new(result))); }
                            break;
                        }
                    }
                }
            }
        });
        Self {
            jobs,
            status,
            join: Some(join),
            next_id: 0,
        }
    }

    fn submit(&mut self, source: Arc<CoreSnapshot>, filter: Filter) -> Option<u64> {
        if self.jobs.is_closed() {
            return None;
        }
        self.next_id += 1;
        self.status.borrow_and_update();
        self.jobs.send_replace(Some(Arc::new(Job {
            id: self.next_id,
            source,
            filter,
        })));
        Some(self.next_id)
    }

    pub async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
        self.status.changed().await
    }

    fn cancel(&mut self) {
        self.jobs.send_replace(None);
    }

    pub async fn shutdown(&mut self) {
        self.cancel();
        if let Some(join) = self.join.take() {
            join.abort();
            let _ = join.await;
        }
    }
}

impl Drop for SearchHandle {
    fn drop(&mut self) {
        if let Some(join) = &self.join {
            join.abort();
        }
    }
}

#[derive(Default)]
pub(super) struct SearchPanel {
    pub visible: bool,
    query: Editor,
    turn: Editor,
    editing: bool,
    turn_field: bool,
    filter: Filter,
    submit: bool,
    cancel: bool,
    pending: Option<u64>,
    results: Option<Arc<Results>>,
    selected: usize,
    scroll: usize,
    help: bool,
    notice: Option<String>,
    opened_tool: Option<OpenedToolHit>,
}

impl SearchPanel {
    pub fn open(&mut self, thread: String) {
        self.visible = true;
        self.editing = true;
        self.turn_field = false;
        self.help = false;
        self.filter.thread = thread;
        self.filter.content = ContentScope::Messages;
        self.opened_tool = None;
        self.filter.tool_category = CategoryFilter::default();
        self.filter.tool_lifecycle = LifecycleFilter::default();
        self.invalidate();
    }

    pub fn open_tools(&mut self, thread: String) {
        self.open(thread);
        self.filter.content = ContentScope::Tools;
        self.submit = true;
        self.editing = false;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.invalidate();
        self.results = None;
    }

    fn invalidate(&mut self) {
        self.cancel = true;
        self.submit = false;
        self.pending = None;
        self.notice = None;
        self.scroll = 0;
    }

    pub fn dispatch(&mut self, source: Arc<CoreSnapshot>, service: &mut SearchHandle) {
        if std::mem::take(&mut self.cancel) {
            service.cancel();
        }
        if std::mem::take(&mut self.submit) && self.visible {
            self.filter.text = self.query.text.clone();
            self.filter.turn = self.turn.text.clone();
            self.pending = service.submit(source, self.filter.clone());
            if self.pending.is_none() {
                self.unavailable();
            }
        }
    }

    pub fn unavailable(&mut self) {
        self.pending = None;
        self.results = None;
        self.notice = Some("Search worker closed; Esc returns to live execution.".into());
    }

    pub fn updated(&mut self, service: &SearchHandle) {
        let status = service.status.borrow();
        let Some(results) = status.as_ref().filter(|r| self.pending == Some(r.id)) else {
            return;
        };
        let old_message = self.results.as_ref().and_then(|r| {
            (self.selected < r.message_hits())
                .then(|| {
                    r.hits.get(self.selected).and_then(|hit| {
                        r.source.as_ref().map(|source| {
                            (
                                MessageKey::of(&source.messages[hit.message]),
                                hit.range.clone(),
                            )
                        })
                    })
                })
                .flatten()
        });
        let old_tool = self.results.as_ref().and_then(|r| {
            if self.selected < r.message_hits() {
                return None;
            }
            let index = self.selected.saturating_sub(r.message_hits());
            r.tool_hits.get(index).map(|hit| {
                (
                    hit.locator.clone(),
                    hit.field,
                    hit.range.clone(),
                    hit.revision,
                )
            })
        });
        self.selected = old_message
            .and_then(|(key, range)| {
                results.hits.iter().position(|hit| {
                    results
                        .source
                        .as_ref()
                        .is_some_and(|source| key.matches(&source.messages[hit.message]))
                        && range == hit.range
                })
            })
            .or_else(|| {
                old_tool.and_then(|(locator, field, range, revision)| {
                    results
                        .tool_hits
                        .iter()
                        .position(|hit| {
                            hit.locator == locator
                                && hit.field == field
                                && hit.range == range
                                && hit.revision == revision
                        })
                        .map(|index| results.message_hits() + index)
                })
            })
            .unwrap_or(0);
        self.results = Some(results.clone());
        self.pending = None;
    }

    fn editor(&mut self) -> &mut Editor {
        if self.turn_field {
            &mut self.turn
        } else {
            &mut self.query
        }
    }

    pub fn paste(&mut self, text: &str) {
        if self.editing {
            self.insert(text);
        }
    }

    pub fn reject_paste(&mut self) {
        self.notice = Some(super::PASTE_REJECTED.into());
    }

    fn insert(&mut self, text: &str) {
        let text = display_text(text).replace(['\n', '\t'], " ");
        if self.editor().text.len() + text.len() <= QUERY_BYTES {
            self.editor().insert(&text);
            self.invalidate();
        } else {
            self.notice = Some("Search field limited to 1024 UTF-8 bytes.".into());
        }
    }

    pub fn key(&mut self, key: KeyEvent, current: &CoreSnapshot) -> Option<Focus> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.opened_tool.is_some() {
            if key.code == KeyCode::Esc {
                self.opened_tool = None;
                self.scroll = 0;
            } else if key.code == KeyCode::PageUp {
                self.scroll = self.scroll.saturating_sub(1);
            } else if key.code == KeyCode::PageDown {
                self.scroll = self.scroll.saturating_add(1);
            }
            return None;
        }
        if key.code == KeyCode::Esc {
            self.close();
            return None;
        }
        if key.code == KeyCode::F(1) {
            self.help = !self.help;
            self.scroll = 0;
            return None;
        }
        if key.code == KeyCode::F(5) {
            self.filter.content = self.filter.content.next();
            self.invalidate();
            if !self.editing {
                self.submit = true;
            }
            return None;
        }
        if key.code == KeyCode::F(8) && self.filter.content != ContentScope::Messages {
            self.filter.tool_category = self.filter.tool_category.next();
            self.invalidate();
            if !self.editing {
                self.submit = true;
            }
            return None;
        }
        if key.code == KeyCode::F(9) && self.filter.content != ContentScope::Messages {
            self.filter.tool_lifecycle = self.filter.tool_lifecycle.next();
            self.invalidate();
            if !self.editing {
                self.submit = true;
            }
            return None;
        }
        match key.code {
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(1);
                return None;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(1);
                return None;
            }
            KeyCode::Home if ctrl => {
                self.scroll = 0;
                return None;
            }
            KeyCode::End if ctrl => {
                self.scroll = usize::MAX;
                return None;
            }
            _ => {}
        }
        if self.help {
            return None;
        }
        if key.code == KeyCode::Tab {
            self.filter.scope = self.filter.scope.next();
            self.invalidate();
            if !self.editing {
                self.submit = true;
            }
            return None;
        }
        if key.code == KeyCode::F(6) {
            self.filter.role = match self.filter.role {
                Role::All => Role::You,
                Role::You => Role::Agent,
                Role::Agent => Role::All,
            };
            self.invalidate();
            if !self.editing {
                self.submit = true;
            }
            return None;
        }
        if key.code == KeyCode::F(7) {
            self.filter.state = match self.filter.state {
                State::All => State::Streaming,
                State::Streaming => State::Complete,
                State::Complete => State::Truncated,
                State::Truncated => State::All,
            };
            self.invalidate();
            if !self.editing {
                self.submit = true;
            }
            return None;
        }
        if ctrl && key.code == KeyCode::Char('f') || !self.editing && key.code == KeyCode::Char('/')
        {
            self.editing = true;
            self.invalidate();
            return None;
        }
        if ctrl && key.code == KeyCode::Char('t') {
            self.editing = true;
            self.turn_field = !self.turn_field;
            self.invalidate();
            return None;
        }
        if self.editing {
            match key.code {
                KeyCode::Enter => {
                    self.editing = false;
                    self.submit = true;
                    self.notice = None;
                }
                KeyCode::Char('u') if ctrl => {
                    self.editor().clear();
                    self.invalidate();
                }
                KeyCode::Left => self.editor().left(),
                KeyCode::Right => self.editor().right(),
                KeyCode::Home => self.editor().cursor = 0,
                KeyCode::End => self.editor().cursor = self.editor().text.len(),
                KeyCode::Backspace => {
                    self.editor().backspace();
                    self.invalidate();
                }
                KeyCode::Delete => {
                    self.editor().delete();
                    self.invalidate();
                }
                KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                    self.insert(&c.to_string())
                }
                _ => {}
            }
            return None;
        }
        if self.pending.is_some() || self.submit || self.cancel {
            return None;
        }
        match key.code {
            KeyCode::Char('r') if !ctrl => {
                self.invalidate();
                self.submit = true;
            }
            KeyCode::Up | KeyCode::Char('N') => {
                self.selected = self.selected.saturating_sub(1);
                self.scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('n') => {
                self.selected = (self.selected + 1).min(
                    self.results
                        .as_ref()
                        .map_or(0, |r| r.count().saturating_sub(1)),
                );
                self.scroll = 0;
            }
            KeyCode::Enter if self.pending.is_none() => {
                let results = self.results.as_ref()?;
                let message_hits = results.message_hits();
                if self.selected >= message_hits {
                    let hit = results.tool_hits.get(self.selected - message_hits)?;
                    if !tool_search::valid(hit, &current.tool_details) {
                        self.notice = Some(
                            "Selected tool content changed or was evicted. Press r to refresh."
                                .into(),
                        );
                        return None;
                    }
                    self.opened_tool = Some(OpenedToolHit {
                        locator: hit.locator.clone(),
                        field: hit.field,
                        range: hit.range.clone(),
                        revision: hit.revision,
                    });
                    self.scroll = 0;
                    return None;
                }
                let hit = results.hits.get(self.selected)?;
                let message = &results.source.as_ref()?.messages[hit.message];
                let focus = Focus {
                    key: MessageKey::of(message),
                    offset: hit.range.start,
                    text: message.text.clone(),
                };
                if !current.messages.iter().any(|m| focus.matches(m)) {
                    self.notice =
                        Some("Selected content changed or was evicted. Press r to refresh.".into());
                    return None;
                }
                self.close();
                return Some(focus);
            }
            _ => {}
        }
        None
    }

    pub fn draw(&self, frame: &mut ratatui::Frame<'_>, area: Rect, current: &CoreSnapshot) {
        frame.render_widget(Clear, area);
        if let Some(opened) = &self.opened_tool {
            let hit = tool_search::Hit {
                locator: opened.locator.clone(),
                field: opened.field,
                range: opened.range.clone(),
                revision: opened.revision,
            };
            if tool_search::valid(&hit, &current.tool_details) {
                let _ = super::tool_detail::draw(
                    frame,
                    area,
                    current,
                    &opened.locator,
                    self.scroll,
                    self.notice.as_deref(),
                );
            } else {
                frame.render_widget(Clear, area);
                frame.render_widget(
                    Paragraph::new("Selected tool search hit changed or was evicted. Press Esc to return to results.")
                        .block(Block::default().borders(Borders::ALL).title(" Tool search hit expired ")),
                    area,
                );
            }
            return;
        }
        let query_height = area.height.min(3);
        let editor = if self.turn_field {
            &self.turn
        } else {
            &self.query
        };
        let mut left = editor.text[..editor.cursor].to_string();
        let width = area.width.saturating_sub(2) as usize;
        while left.width() >= width.max(1) && !left.is_empty() {
            let bytes = left.graphemes(true).next().unwrap().len();
            left.drain(..bytes);
        }
        let cursor = left.width();
        frame.render_widget(
            Paragraph::new(format!("{left}{}", &editor.text[editor.cursor..])).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(if self.turn_field {
                        " Search: exact turn ID "
                    } else if self.filter.content != ContentScope::Messages {
                        " Search: retained tool details "
                    } else {
                        " Search: literal text "
                    }),
            ),
            Rect {
                height: query_height,
                ..area
            },
        );
        if self.editing && query_height >= 3 && area.width >= 3 {
            frame.set_cursor_position((area.x + 1 + cursor as u16, area.y + 1));
        }
        let body = Rect {
            y: area.y + query_height,
            height: area.height.saturating_sub(query_height + 1),
            ..area
        };
        let filters = vec![
            format!(
                "mode={} | {:?} | role={:?} | state={:?} | tool={:?}/{:?}",
                match self.filter.content {
                    ContentScope::Messages => "messages",
                    ContentScope::Tools => "tools",
                    ContentScope::Both => "messages + tools",
                },
                self.filter.scope,
                self.filter.role,
                self.filter.state,
                self.filter.tool_category,
                self.filter.tool_lifecycle
            ),
            format!(
                "Thread: {} | turn: {}",
                self.filter.thread,
                if self.turn.text.is_empty() {
                    "all"
                } else {
                    &self.turn.text
                }
            ),
        ];
        let mut rows = Vec::new();
        if self.help {
            rows.extend(
                [
                    "Ctrl+F or /: edit query",
                    "Ctrl+T: exact turn filter",
                    "Enter: search/open; Esc: close (drafts kept)",
                    "Tab: thread/subtree/path/all agents",
                    "F6: role filter",
                    "F7: state filter",
                    "F5: messages/tools/both; F8 category; F9 lifecycle",
                    "Up/Down or N/n: previous/next retained hit",
                    "PgUp/Dn: scroll one row",
                    "Ctrl+Home/End: first/last",
                    "r: refresh retained messages",
                    "F2: pending requests",
                    "Ctrl+C: interrupt root",
                    "Ctrl+Q: quit",
                    "Literal, case-sensitive Unicode; no regex",
                    "Only retained messages or live tool details are searched",
                    "Queries and results are never saved to journal",
                ]
                .into_iter()
                .map(String::from),
            );
        } else if self.editing {
            rows.push("Enter searches. F1 shows controls.".into());
            rows.extend(filters.clone());
        } else if self.pending.is_some() || self.submit || self.cancel {
            rows.push("Searching; old results disabled.".into());
        } else if let Some(results) = &self.results {
            let message_count = results.message_hits();
            let tool_count = results.tool_hits();
            let total_count = message_count + tool_count;
            let total = (if results.filter.content == ContentScope::Tools {
                0
            } else {
                results.total
            }) + (if results.filter.content == ContentScope::Messages {
                0
            } else {
                results.tool_total
            });
            rows.push(format!(
                "Hit {}/{} | {} total",
                if total_count == 0 {
                    0
                } else {
                    self.selected + 1
                },
                total_count,
                total
            ));
            rows.extend(filters);
            rows.push(format!(
                "Snapshot {}{}",
                results.snapshot_version,
                if results.snapshot_version != current.version {
                    " (fixed; r refreshes)"
                } else {
                    ""
                }
            ));
            if self.selected >= message_count && tool_count > 0 {
                let hit = &results.tool_hits[self.selected - message_count];
                rows.push(format!(
                    "Tool {:?} | UTF-8 bytes {}..{} | revision {}",
                    hit.field, hit.range.start, hit.range.end, hit.revision
                ));
                let detail = current.tool_details.get(&hit.locator);
                if tool_search::valid(hit, &current.tool_details) {
                    if let Some(detail) = detail {
                        let text = hit.field.text(&detail).unwrap_or_default();
                        let before: String = text[..hit.range.start]
                            .graphemes(true)
                            .rev()
                            .take(6)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        let after: String =
                            text[hit.range.end..].graphemes(true).take(32).collect();
                        rows.push(format!("{before}⟦{}⟧{after}", results.filter.text));
                        rows.push(format!(
                            "Category: {:?} | lifecycle: {:?}",
                            detail.category, detail.lifecycle
                        ));
                    }
                    rows.push(format!(
                        "Thread: {} | turn: {} | item: {}",
                        hit.locator
                            .identity
                            .thread_id
                            .as_deref()
                            .unwrap_or("unavailable"),
                        hit.locator
                            .identity
                            .turn_id
                            .as_deref()
                            .unwrap_or("unavailable"),
                        hit.locator.item_id
                    ));
                    rows.push("Enter opens this retained tool detail.".into());
                } else {
                    rows.push(
                        "Live tool content changed/evicted; opening disabled until refresh.".into(),
                    );
                }
            } else if self.selected < message_count {
                let hit = &results.hits[self.selected];
                let message = &results.source.as_ref().unwrap().messages[hit.message];
                let before: String = message.text[..hit.range.start]
                    .graphemes(true)
                    .rev()
                    .take(6)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let after: String = message.text[hit.range.end..]
                    .graphemes(true)
                    .take(32)
                    .collect();
                rows.push(format!("{before}⟦{}⟧{after}", results.filter.text));
                rows.push(format!(
                    "Hit {} | {} | {}",
                    self.selected + 1,
                    message.role,
                    if message.complete {
                        "complete"
                    } else {
                        "streaming"
                    }
                ));
                rows.push(format!(
                    "Thread: {}\nTurn: {}\nItem: {}\nUTF-8 bytes: {}..{}",
                    message.thread_id,
                    if message.turn_id.is_empty() {
                        "unbound"
                    } else {
                        &message.turn_id
                    },
                    message.id,
                    hit.range.start,
                    hit.range.end
                ));
                if !current
                    .messages
                    .iter()
                    .any(|m| MessageKey::of(message).matches(m) && m.text == message.text)
                {
                    rows.push(
                        "Live content changed/evicted; opening disabled until refresh.".into(),
                    );
                }
                rows.push(format!("Match: {}", results.filter.text));
                rows.push("--- retained message ---".into());
                rows.push(message.text.clone());
            } else {
                rows.push("No matches in the selected retained content.".into());
            }
            if total > HIT_LIMIT {
                rows.push("Result limit reached: narrow query or filters.".into());
            }
            if results.source.as_ref().is_some_and(|source| {
                source.history_truncated || source.messages.iter().any(|m| m.truncated)
            }) {
                rows.push("Older content unavailable.".into());
            }
        }
        if let Some(notice) = &self.notice {
            rows.insert(0, notice.clone());
        }
        let lines: Vec<_> = rows.iter().flat_map(|row| wrap(row, width)).collect();
        let height = body.height as usize;
        let start = self.scroll.min(lines.len().saturating_sub(height));
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(start)
                    .take(height)
                    .map(Line::from)
                    .collect::<Vec<_>>(),
            )
            .style(Style::default().fg(Color::Cyan)),
            body,
        );
        if area.height > query_height {
            frame.render_widget(
                Paragraph::new(if self.editing {
                    "Enter search  Esc close  F1 help"
                } else if self.opened_tool.is_some() {
                    "Esc back  PgUp/PgDn scroll"
                } else {
                    "Enter open  F5 mode  Esc close  F1 help"
                }),
                Rect {
                    y: area.y + area.height - 1,
                    height: 1,
                    ..area
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentInfo, AgentSnapshot};
    use crate::observation::ActivityIdentity;
    use crate::protocol::ToolCategory;
    use crate::tool_details::{
        ToolDetail, ToolDetailLocator, ToolDetailsSnapshot, ToolLifecycle, ToolTextSource,
    };
    use std::collections::VecDeque;

    fn message(thread: &str, turn: &str, item: &str, text: &str) -> ConversationItem {
        ConversationItem {
            thread_id: thread.into(),
            turn_id: turn.into(),
            id: item.into(),
            role: "Agent".into(),
            text: text.into(),
            complete: true,
            truncated: false,
        }
    }

    fn child(id: &str, parent: &str, confirmed: bool) -> AgentSnapshot {
        AgentSnapshot {
            info: AgentInfo {
                id: id.into(),
                parent_id: parent.into(),
                confirmed,
                path: None,
                nickname: None,
                role: None,
                model: None,
            },
            generation: 1,
            turn_id: None,
            outcome: None,
            awaiting_turn: false,
            usage: Default::default(),
        }
    }

    async fn found(source: Arc<CoreSnapshot>, filter: Filter) -> Results {
        scan(Arc::new(Job {
            id: 1,
            source,
            filter,
        }))
        .await
    }

    #[tokio::test]
    async fn tool_and_both_searches_keep_only_hit_metadata_and_matching_message_projection() {
        let detail = ToolDetail {
            locator: ToolDetailLocator {
                session_id: "session".into(),
                identity: ActivityIdentity {
                    agent_id: "root".into(),
                    task_id: None,
                    attempt_id: Some(1),
                    thread_id: Some("root".into()),
                    turn_id: Some("turn".into()),
                    generation: Some(1),
                },
                item_id: "tool-item".into(),
            },
            revision: 7,
            category: ToolCategory::Shell,
            lifecycle: ToolLifecycle::Running,
            command: Some("needle 中文".into()),
            cwd: None,
            parameters: None,
            result: None,
            output: "private output".into(),
            output_source: ToolTextSource::OutputDelta,
            exit_code: None,
            duration_ms: None,
            bytes_observed: 14,
            bytes_retained: 14,
            clipped: false,
            authoritative: false,
        };
        let source = Arc::new(CoreSnapshot {
            thread_id: Some("root".into()),
            messages: vec![message("root", "turn", "message-item", "needle message")],
            tool_details: ToolDetailsSnapshot {
                entries: Arc::new(VecDeque::from([Arc::new(detail)])),
                ..Default::default()
            },
            ..Default::default()
        });
        for content in [ContentScope::Tools, ContentScope::Both] {
            let results = found(
                source.clone(),
                Filter {
                    text: "needle".into(),
                    thread: "root".into(),
                    content,
                    ..Default::default()
                },
            )
            .await;
            assert_eq!(results.tool_hits.len(), 1);
            assert_eq!(results.tool_total, 1);
            assert_eq!(
                results.count(),
                if content == ContentScope::Tools { 1 } else { 2 }
            );
            if content == ContentScope::Tools {
                assert!(results.source.is_none());
            } else {
                let messages = &results.source.as_ref().unwrap().messages;
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].text, "needle message");
            }
            let tool_index = if content == ContentScope::Tools { 0 } else { 1 };
            let mut panel = SearchPanel {
                visible: true,
                filter: Filter {
                    content,
                    ..Default::default()
                },
                selected: tool_index,
                results: Some(Arc::new(results)),
                ..Default::default()
            };
            assert!(panel
                .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &source)
                .is_none());
            assert!(panel.opened_tool.is_some());

            let mut changed = source.as_ref().clone();
            let mut entries = (*changed.tool_details.entries).clone();
            Arc::make_mut(&mut entries[0]).revision += 1;
            changed.tool_details.entries = Arc::new(entries);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            terminal
                .draw(|frame| panel.draw(frame, frame.area(), &changed))
                .unwrap();
            let rendered = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(rendered.contains("expired"));
        }
    }

    #[tokio::test]
    async fn search_uses_confirmed_relationships_and_exact_turn_role_state_filters() {
        let mut source = CoreSnapshot {
            thread_id: Some("root".into()),
            agents: vec![
                child("child", "root", true),
                child("grandchild", "child", true),
                child("sibling", "root", true),
                child("unconfirmed", "child", false),
            ],
            messages: ["root", "child", "grandchild", "sibling", "unconfirmed"]
                .map(|thread| message(thread, "one", "same-id", "中文 match"))
                .to_vec(),
            ..Default::default()
        };
        for (scope, expected) in [
            (Scope::Thread, vec!["child"]),
            (Scope::Subtree, vec!["child", "grandchild"]),
            (Scope::Path, vec!["root", "child"]),
            (
                Scope::All,
                vec!["root", "child", "grandchild", "sibling", "unconfirmed"],
            ),
        ] {
            let results = found(
                Arc::new(source.clone()),
                Filter {
                    text: "中文".into(),
                    thread: "child".into(),
                    scope,
                    ..Default::default()
                },
            )
            .await;
            assert_eq!(
                results
                    .hits
                    .iter()
                    .map(|hit| results.source.as_ref().unwrap().messages[hit.message]
                        .thread_id
                        .as_str())
                    .collect::<Vec<_>>(),
                expected
            );
        }
        source.messages[1].role = "You".into();
        source.messages[2].complete = false;
        source.messages[3].truncated = true;
        source.messages[4].turn_id = "two".into();
        for (role, state, turn, expected_thread) in [
            (Role::You, State::Complete, "one", "child"),
            (Role::Agent, State::Streaming, "one", "grandchild"),
            (Role::All, State::Truncated, "one", "sibling"),
            (Role::All, State::All, "two", "unconfirmed"),
        ] {
            let results = found(
                Arc::new(source.clone()),
                Filter {
                    text: "match".into(),
                    scope: Scope::All,
                    role,
                    state,
                    turn: turn.into(),
                    ..Default::default()
                },
            )
            .await;
            assert_eq!(
                results
                    .hits
                    .iter()
                    .map(|hit| {
                        results.source.as_ref().unwrap().messages[hit.message]
                            .thread_id
                            .as_str()
                    })
                    .collect::<Vec<_>>(),
                vec![expected_thread]
            );
        }
    }

    #[tokio::test]
    async fn unicode_matches_keep_utf8_offsets_and_results_are_bounded_with_exact_counts() {
        let text = "é e\u{301} 👨‍👩‍👧‍👦 中文 中文";
        for query in ["é", "e\u{301}", "👨‍👩‍👧‍👦", "中文"] {
            let results = found(
                Arc::new(CoreSnapshot {
                    messages: vec![message("root", "one", "id", text)],
                    ..Default::default()
                }),
                Filter {
                    text: query.into(),
                    scope: Scope::All,
                    ..Default::default()
                },
            )
            .await;
            assert_eq!(results.total, if query == "中文" { 2 } else { 1 });
            for hit in results.hits {
                assert_eq!(&text[hit.range], query);
            }
        }
        let results = found(
            Arc::new(CoreSnapshot {
                messages: vec![message("root", "one", "id", &"x".repeat(700))],
                history_truncated: true,
                ..Default::default()
            }),
            Filter {
                text: "x".into(),
                scope: Scope::All,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(results.total, 700);
        assert_eq!(results.hits.len(), HIT_LIMIT);
        assert!(results.source.as_ref().unwrap().history_truncated);
    }

    #[test]
    fn match_focus_follows_graphemes_line_wraps_and_identity_instead_of_message_indices() {
        for (text, query, width, expected) in [
            ("abcd中文", "中文", 4, 1),
            ("a\n中文", "中文", 20, 1),
            ("👨‍👩‍👧‍👦中文", "中文", 2, 1),
            ("ab\u{001b}中文", "中文", 4, 0),
            ("e\u{301}中文", "中文", 2, 1),
        ] {
            let item = message("root", "one", "same", text);
            let focus = Focus {
                key: MessageKey::of(&item),
                offset: text.find(query).unwrap(),
                text: text.into(),
            };
            assert_eq!(focus.row(width), expected, "{text:?}");
            assert!(focus.matches(&item));
            assert!(!focus.matches(&message("child", "one", "same", text)));
            assert!(!focus.matches(&message("root", "two", "same", text)));
            assert!(!focus.matches(&message("root", "one", "same", "changed")));
        }
    }

    #[tokio::test]
    async fn replacement_queries_cancel_old_scans_and_close_releases_the_pinned_source() {
        let source = Arc::new(CoreSnapshot {
            messages: (0..128)
                .map(|i| message("root", "one", &i.to_string(), &"x".repeat(2048)))
                .collect(),
            ..Default::default()
        });
        let weak = Arc::downgrade(&source);
        let mut service = SearchHandle::spawn();
        let old = service
            .submit(
                source.clone(),
                Filter {
                    text: "x".into(),
                    scope: Scope::All,
                    ..Default::default()
                },
            )
            .unwrap();
        tokio::task::yield_now().await;
        let current = service
            .submit(
                source.clone(),
                Filter {
                    text: "absent".into(),
                    scope: Scope::All,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_ne!(old, current);
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            service
                .status
                .wait_for(|s| s.as_ref().is_some_and(|r| r.id == current)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(service.status.borrow().as_ref().unwrap().total, 0);
        drop(source);
        service.cancel();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            service.status.wait_for(Option::is_none),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(weak.upgrade().is_none());
        service.shutdown().await;
        assert!(service
            .submit(Arc::new(CoreSnapshot::default()), Filter::default())
            .is_none());
    }

    #[tokio::test]
    async fn old_results_cannot_open_a_changed_or_evicted_item_and_pastes_remain_bounded() {
        let source = Arc::new(CoreSnapshot {
            messages: vec![message("root", "one", "id", "中文👋")],
            ..Default::default()
        });
        let mut panel = SearchPanel::default();
        panel.open("root".into());
        panel.paste("中文");
        panel.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &source);
        let mut service = SearchHandle::spawn();
        panel.dispatch(source.clone(), &mut service);
        let id = panel.pending.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            service
                .status
                .wait_for(|s| s.as_ref().is_some_and(|r| r.id == id)),
        )
        .await
        .unwrap()
        .unwrap();
        panel.updated(&service);
        let mut current = source.as_ref().clone();
        current.messages[0].text = "Changed 中文".into();
        assert!(panel
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &current)
            .is_none());
        assert!(panel.visible);
        assert!(panel.notice.as_ref().unwrap().contains("changed"));
        current.messages.clear();
        assert!(panel
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &current)
            .is_none());
        let focus = panel
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &source)
            .unwrap();
        assert!(focus.matches(&source.messages[0]));
        assert!(!panel.visible);
        panel.open("root".into());
        panel.query.clear();
        panel.paste(&"中".repeat(341));
        panel.paste("👋");
        assert_eq!(panel.query.text.len(), 1023);
        assert!(panel.notice.as_ref().unwrap().contains("1024"));
        panel.key(
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &source,
        );
        panel.paste("👋");
        assert_eq!(panel.query.text.len(), 1024);
        panel.close();
        panel.dispatch(source, &mut service);
        service.shutdown().await;
    }

    #[tokio::test]
    async fn search_keys_and_paste_preserve_task_and_secret_drafts_without_core_commands() {
        use crate::interactions::RequestView;
        use crate::protocol::RpcId;
        use crate::state::{GateSnapshot, SessionPhase};
        use crate::ui::{handle_key, LocalState};
        let snapshot = Arc::new(CoreSnapshot {
            phase: SessionPhase::GatePending, root_start_requests: 1, thread_id: Some("root".into()),
            gate: Some(GateSnapshot { targets: Vec::new(), pending: true, root_starts_at_enter: 1, root_starts_at_release: None }),
            requests: vec![RequestView::decode(RpcId::Number(1), "item/tool/requestUserInput",
                &serde_json::json!({"threadId":"root","turnId":"one","questions":[{"id":"secret","header":"Secret","question":"Q","isSecret":true}]})).unwrap()],
            messages: vec![message("root", "one", "item", "中文 search")], ..Default::default()
        });
        let before = snapshot.clone();
        let mut local = LocalState::default();
        local.editor.insert("TASK_DRAFT中文👋");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let ctrl = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
        handle_key(
            KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        local.editor.insert("PRIVATE_SECRET👋");
        handle_key(ctrl('f'), &snapshot, &mut local, &tx);
        local.search.paste("中文");
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        let mut service = SearchHandle::spawn();
        local.search.dispatch(snapshot.clone(), &mut service);
        let id = local.search.pending.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            service
                .status
                .wait_for(|s| s.as_ref().is_some_and(|r| r.id == id)),
        )
        .await
        .unwrap()
        .unwrap();
        local.search.updated(&service);
        assert_eq!(local.search.results.as_ref().unwrap().total, 1);
        for key in [
            ctrl('y'),
            ctrl('n'),
            ctrl('b'),
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        ] {
            handle_key(key, &snapshot, &mut local, &tx);
        }
        handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(!local.search.visible);
        assert_eq!(local.editor.text, "PRIVATE_SECRET👋");
        assert_eq!(local.task_draft.as_ref().unwrap().text, "TASK_DRAFT中文👋");
        assert_eq!(snapshot, before);
        assert!(rx.try_recv().is_err());
        service.shutdown().await;
    }

    #[tokio::test]
    async fn queued_filter_changes_disable_old_hits_before_dispatch_and_reject_old_generation_updates(
    ) {
        let source = Arc::new(CoreSnapshot {
            messages: vec![message("root", "one", "id", "中文")],
            ..Default::default()
        });
        let mut service = SearchHandle::spawn();
        let mut panel = SearchPanel::default();
        panel.open("root".into());
        panel.paste("中文");
        panel.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &source);
        panel.dispatch(source.clone(), &mut service);
        let first = panel.pending.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            service
                .status
                .wait_for(|s| s.as_ref().is_some_and(|r| r.id == first)),
        )
        .await
        .unwrap()
        .unwrap();
        panel.updated(&service);
        assert_eq!(panel.results.as_ref().unwrap().total, 1);
        panel.key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE), &source);
        assert!(panel.submit);
        assert!(panel
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &source)
            .is_none());
        assert!(panel.visible);
        panel.dispatch(source.clone(), &mut service);
        let second = panel.pending.unwrap();
        assert_ne!(first, second);
        panel.updated(&service);
        assert_eq!(panel.pending, Some(second));
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            service
                .status
                .wait_for(|s| s.as_ref().is_some_and(|r| r.id == second)),
        )
        .await
        .unwrap()
        .unwrap();
        panel.updated(&service);
        assert_eq!(panel.results.as_ref().unwrap().total, 0);
        assert!(panel
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &source)
            .is_none());
        panel.close();
        panel.dispatch(source, &mut service);
        service.shutdown().await;
    }

    #[tokio::test]
    async fn search_results_and_help_are_reachable_at_supported_sizes_and_never_show_secret_drafts()
    {
        use crate::ui::{draw, LocalState};
        use ratatui::{backend::TestBackend, Terminal};
        let source = Arc::new(CoreSnapshot {
            version: 7,
            thread_id: Some("root".into()),
            phase: crate::state::SessionPhase::Running,
            history_truncated: true,
            messages: vec![message("root", "turn", "item", "RESULT 中文👋")],
            ..Default::default()
        });
        let results = Arc::new(
            found(
                source.clone(),
                Filter {
                    text: "中文".into(),
                    thread: "root".into(),
                    ..Default::default()
                },
            )
            .await,
        );
        for (width, height) in [
            (30, 10),
            (60, 20),
            (80, 24),
            (100, 30),
            (120, 40),
            (160, 50),
        ] {
            let mut local = LocalState::default();
            local.editor.insert("PRIVATE_SECRET_ANSWER");
            local.search.open("root".into());
            local.search.query.insert("中文");
            local.search.editing = false;
            local.search.cancel = false;
            local.search.results = Some(results.clone());
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut seen = String::new();
            for scroll in 0..65 {
                local.search.scroll = scroll;
                terminal.draw(|frame| draw(frame, &source, &local)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(!text.contains("PRIVATE_SECRET_ANSWER"));
                seen.push_str(&text);
            }
            for value in ["Hit 1/1", "RESULT", "unavailable", "Running", "Esc close"] {
                assert!(seen.contains(value), "missing {value} at {width}x{height}");
            }
            local
                .search
                .key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE), &source);
            seen.clear();
            for _ in 0..30 {
                terminal.draw(|frame| draw(frame, &source, &local)).unwrap();
                seen.extend(
                    terminal
                        .backend()
                        .buffer()
                        .content()
                        .iter()
                        .map(|cell| cell.symbol()),
                );
                local.search.key(
                    KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
                    &source,
                );
            }
            for value in [
                "Ctrl+F",
                "Ctrl+T",
                "F6:",
                "F7:",
                "F2:",
                "Ctrl+Q",
                "case-sensitive",
            ] {
                assert!(
                    seen.contains(value),
                    "missing {value} help at {width}x{height}"
                );
            }
            assert!(!seen.contains("PRIVATE_SECRET_ANSWER"));
        }
    }

    #[tokio::test]
    async fn opening_a_search_hit_locates_the_message_after_resize_and_scroll_continues_from_it() {
        use crate::ui::{draw, handle_key, LocalState};
        use ratatui::{backend::TestBackend, Terminal};
        let mut messages: Vec<_> = (0..55)
            .map(|i| {
                message(
                    "root",
                    "turn",
                    &i.to_string(),
                    &format!("line {i}\nmore line {i}"),
                )
            })
            .collect();
        messages[7].text = "abc\nMATCH中文👋\nmore".into();
        let source = Arc::new(CoreSnapshot {
            thread_id: Some("root".into()),
            phase: crate::state::SessionPhase::Running,
            messages,
            ..Default::default()
        });
        let results = Arc::new(
            found(
                source.clone(),
                Filter {
                    text: "MATCH中文👋".into(),
                    thread: "root".into(),
                    ..Default::default()
                },
            )
            .await,
        );
        let mut local = LocalState::default();
        local.search.open("root".into());
        local.search.results = Some(results);
        local.search.editing = false;
        local.search.cancel = false;
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &source,
            &mut local,
            &tx,
        );
        assert!(!local.search.visible);
        assert!(local
            .conversation_focus
            .as_ref()
            .unwrap()
            .matches(&source.messages[7]));
        for (width, height) in [(30, 10), (60, 20), (80, 24), (100, 30), (160, 50)] {
            local.viewport = Rect::new(0, 0, width, height);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &source, &local)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                text.contains("MATCH"),
                "match missing after resize {width}x{height}"
            );
        }
        handle_key(
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            &source,
            &mut local,
            &tx,
        );
        assert!(local.conversation_focus.is_none());
        assert!(local.scroll_from_bottom > 70);
        let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
        terminal.draw(|frame| draw(frame, &source, &local)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!text.contains("line 54"));
        assert!(rx.try_recv().is_err());
    }
}
