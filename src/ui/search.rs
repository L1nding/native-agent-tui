//! Bounded, read-only search of the retained conversation projection.
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Filter {
    text: String,
    thread: String,
    turn: String,
    scope: Scope,
    role: Role,
    state: State,
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
    source: Arc<CoreSnapshot>,
    filter: Filter,
    hits: Vec<Hit>,
    total: usize,
}

async fn scan(job: Arc<Job>) -> Results {
    let threads = job.filter.threads(&job.source);
    let mut hits = Vec::new();
    let mut total = 0;
    if !job.filter.text.is_empty() {
        for (message, item) in job.source.messages.iter().enumerate() {
            if job.filter.accepts(item, &threads) {
                for (offset, _) in item.text.match_indices(&job.filter.text) {
                    total += 1;
                    if hits.len() < HIT_LIMIT {
                        hits.push(Hit {
                            message,
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
    Results {
        id: job.id,
        source: job.source.clone(),
        filter: job.filter.clone(),
        hits,
        total,
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
}

impl SearchPanel {
    pub fn open(&mut self, thread: String) {
        self.visible = true;
        self.editing = true;
        self.turn_field = false;
        self.help = false;
        self.filter.thread = thread;
        self.invalidate();
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
        let old = self.results.as_ref().and_then(|r| {
            r.hits.get(self.selected).map(|hit| {
                (
                    MessageKey::of(&r.source.messages[hit.message]),
                    hit.range.clone(),
                )
            })
        });
        self.selected = old
            .and_then(|(key, range)| {
                results.hits.iter().position(|hit| {
                    key.matches(&results.source.messages[hit.message]) && range == hit.range
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
        if key.code == KeyCode::Esc {
            self.close();
            return None;
        }
        if key.code == KeyCode::F(1) {
            self.help = !self.help;
            self.scroll = 0;
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
                        .map_or(0, |r| r.hits.len().saturating_sub(1)),
                );
                self.scroll = 0;
            }
            KeyCode::Enter if self.pending.is_none() => {
                let results = self.results.as_ref()?;
                let hit = results.hits.get(self.selected)?;
                let message = &results.source.messages[hit.message];
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
                "{:?} | role={:?} | state={:?}",
                self.filter.scope, self.filter.role, self.filter.state
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
                    "Up/Down or N/n: previous/next retained hit",
                    "PgUp/Dn: scroll one row",
                    "Ctrl+Home/End: first/last",
                    "r: refresh retained messages",
                    "F2: pending requests",
                    "Ctrl+C: interrupt root",
                    "Ctrl+Q: quit",
                    "Literal, case-sensitive Unicode; no regex",
                    "Only retained conversation text is searched",
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
            rows.push(format!(
                "Hit {}/{} | {} total",
                if results.hits.is_empty() {
                    0
                } else {
                    self.selected + 1
                },
                results.hits.len(),
                results.total
            ));
            if let Some(hit) = results.hits.get(self.selected) {
                let message = &results.source.messages[hit.message];
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
            }
            rows.extend(filters);
            rows.push(format!(
                "Snapshot {}{}",
                results.source.version,
                if results.source.version != current.version {
                    " (fixed; r refreshes)"
                } else {
                    ""
                }
            ));
            if results.total > HIT_LIMIT {
                rows.push("Result limit reached: narrow text, turn, role or scope.".into());
            }
            if results.source.history_truncated
                || results.source.messages.iter().any(|m| m.truncated)
            {
                rows.push(
                    "Older content unavailable; some retained messages may be truncated.".into(),
                );
            }
            if let Some(hit) = results.hits.get(self.selected) {
                let message = &results.source.messages[hit.message];
                rows.push(format!(
                    "Hit {}/{} | {} | {}",
                    self.selected + 1,
                    results.hits.len(),
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
                rows.push("No matches in this retained range.".into());
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
                } else {
                    "Enter open  Esc close  F1 help"
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
                    .map(|hit| results.source.messages[hit.message].thread_id.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
        }
        source.messages[1].role = "You".into();
        source.messages[2].complete = false;
        source.messages[3].truncated = true;
        source.messages[4].turn_id = "two".into();
        for (role, state, turn, expected) in [
            (Role::You, State::Complete, "one", vec![1]),
            (Role::Agent, State::Streaming, "one", vec![2]),
            (Role::All, State::Truncated, "one", vec![3]),
            (Role::All, State::All, "two", vec![4]),
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
                    .map(|hit| hit.message)
                    .collect::<Vec<_>>(),
                expected
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
        assert!(results.source.history_truncated);
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
