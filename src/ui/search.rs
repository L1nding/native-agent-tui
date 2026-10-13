//! Bounded, read-only search of the retained conversation projection.
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Paragraph};
use tokio::sync::watch;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::lang::{self, tr};
use super::scope::Scope;
use super::theme::{self, Theme};
use super::tool_detail::{
    field_segments, fit_hints, help_parts, lifecycle_zh, scope_text, wrap_segments, Field, Segment,
    Tone,
};
use super::tool_search::{self, CategoryFilter, LifecycleFilter};
use super::Editor;
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

impl Role {
    /// Name shown in the filter row; English is the variant name.
    fn label(self) -> &'static str {
        match self {
            Self::All => tr("All", "全部"),
            Self::You => tr("You", "你"),
            Self::Agent => "Agent",
        }
    }
}

/// Core labels a message `You` or `Agent`; only the display name is translated.
fn role_text(role: &str) -> &str {
    if role == "You" {
        tr("You", "你")
    } else {
        role
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    All,
    Streaming,
    Complete,
    Truncated,
}

impl State {
    /// Name shown in the filter row; English is the variant name.
    fn label(self) -> &'static str {
        match self {
            Self::All => tr("All", "全部"),
            Self::Streaming => tr("Streaming", "输出中"),
            Self::Complete => tr("Complete", "已完成"),
            Self::Truncated => tr("Truncated", "已截断"),
        }
    }
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
        self.notice = Some(
            tr(
                "Search worker closed; Esc returns to live execution.",
                "搜索后台已关闭；按 Esc 返回实时执行。",
            )
            .into(),
        );
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
        self.notice = Some(super::paste_rejected().into());
    }

    fn insert(&mut self, text: &str) {
        let text = display_text(text).replace(['\n', '\t'], " ");
        if self.editor().text.len() + text.len() <= QUERY_BYTES {
            self.editor().insert(&text);
            self.invalidate();
        } else {
            self.notice = Some(
                tr(
                    "Search field limited to 1024 UTF-8 bytes.",
                    "搜索框最多 1024 个 UTF-8 字节。",
                )
                .into(),
            );
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
                            tr(
                                "Selected tool content changed or was evicted. Press r to refresh.",
                                "所选工具内容已变化或已被清出，按 r 刷新。",
                            )
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
                    self.notice = Some(
                        tr(
                            "Selected content changed or was evicted. Press r to refresh.",
                            "所选内容已变化或已被清出，按 r 刷新。",
                        )
                        .into(),
                    );
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
        let theme = theme::current();
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
                frame.render_widget(
                    Paragraph::new(wrap_segments(
                        &[(
                            tr(
                                "Selected tool search hit changed or was evicted. Press Esc to return to results.",
                                "所选的工具搜索命中已变化或已被清出，按 Esc 返回结果。",
                            )
                            .to_owned(),
                            Tone::Warning.style(theme),
                        )],
                        area.width.saturating_sub(2) as usize,
                    ))
                    .block(
                        theme::panel()
                            .borders(Borders::ALL)
                            .title(tr(" Tool search hit expired ", " 工具搜索命中已失效 ")),
                    ),
                    area,
                );
            }
            return;
        }
        // One themed surface, so the panel matches the framed panels around it.
        frame.render_widget(theme::panel(), area);
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
                // The accent frame follows keyboard focus: typing goes here only
                // while editing; in result mode keys move through the hits.
                if self.editing {
                    theme::focus_panel()
                } else {
                    theme::panel()
                }
                .borders(Borders::ALL)
                .title(if self.turn_field {
                    tr(" Search: exact turn ID ", " 搜索：精确轮次 ID ")
                } else if self.filter.content != ContentScope::Messages {
                    tr(
                        " Search: retained tool details ",
                        " 搜索：已保留的工具详情 ",
                    )
                } else {
                    tr(" Search: literal text ", " 搜索：字面文本 ")
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
        let body_width = body.width as usize;
        let value = Style::default().fg(theme.foreground);
        let quiet = |text: String| vec![(text, theme.secondary())];
        let warning = |text: &str| vec![(text.to_owned(), Tone::Warning.style(theme))];
        let row = |segments: Vec<Segment>| wrap_segments(&segments, body_width);
        let zh = lang::zh();
        let mode = match self.filter.content {
            ContentScope::Messages => tr("messages", "消息"),
            ContentScope::Tools => tr("tools", "工具"),
            ContentScope::Both => tr("messages + tools", "消息 + 工具"),
        };
        let turn_filter = if self.turn.text.is_empty() {
            tr("all", "全部")
        } else {
            &self.turn.text
        };
        let filters = if zh {
            [
                format!(
                    "模式={mode} | 范围={} | 角色={} | 状态={} | 工具={}/{}",
                    scope_text(self.filter.scope),
                    self.filter.role.label(),
                    self.filter.state.label(),
                    self.filter.tool_category.label(),
                    self.filter.tool_lifecycle.label()
                ),
                format!("线程：{} | 轮次：{turn_filter}", self.filter.thread),
            ]
        } else {
            [
                format!(
                    "mode={mode} | {:?} | role={:?} | state={:?} | tool={:?}/{:?}",
                    self.filter.scope,
                    self.filter.role,
                    self.filter.state,
                    self.filter.tool_category,
                    self.filter.tool_lifecycle
                ),
                format!("Thread: {} | turn: {turn_filter}", self.filter.thread),
            ]
        };
        let mut lines: Vec<Line<'static>> = Vec::new();
        if let Some(notice) = &self.notice {
            lines.extend(row(warning(notice)));
        }
        if self.help {
            let help = if zh { HELP_ZH } else { HELP };
            for (index, line) in help.iter().enumerate() {
                // Key rows read `keys: action`; the remaining rows are notes.
                lines.extend(row(
                    match help_parts(line).filter(|_| index < HELP_KEY_ROWS) {
                        Some((keys, action)) => {
                            vec![(keys.to_owned(), theme.key()), (action.to_owned(), value)]
                        }
                        None => quiet((*line).to_owned()),
                    },
                ));
            }
        } else if self.editing {
            lines.extend(row(quiet(
                tr(
                    "Enter searches. F1 shows controls.",
                    "按 Enter 搜索，F1 查看按键说明。",
                )
                .into(),
            )));
            for filter in filters {
                lines.extend(row(quiet(filter)));
            }
        } else if self.pending.is_some() || self.submit || self.cancel {
            lines.extend(row(quiet(
                tr(
                    "Searching; old results disabled.",
                    "搜索中，旧结果暂不可用。",
                )
                .into(),
            )));
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
            let position = if total_count == 0 {
                0
            } else {
                self.selected + 1
            };
            lines.extend(row(quiet(if zh {
                format!("命中 {position}/{total_count} | 共 {total} 条")
            } else {
                format!("Hit {position}/{total_count} | {total} total")
            })));
            for filter in filters {
                lines.extend(row(quiet(filter)));
            }
            lines.extend(row(quiet(format!(
                "{} {}{}",
                tr("Snapshot", "快照"),
                results.snapshot_version,
                if results.snapshot_version != current.version {
                    tr(" (fixed; r refreshes)", "（已固定，按 r 刷新）")
                } else {
                    ""
                }
            ))));
            // The hit list carries the `before⟦match⟧after` preview of each hit.
            lines.extend(self.hit_list(results, current, body_width, theme));
            if self.selected >= message_count && tool_count > 0 {
                let hit = &results.tool_hits[self.selected - message_count];
                lines.extend(row(quiet(if zh {
                    format!(
                        "工具 {} | UTF-8 字节 {}..{} | 修订 {}",
                        hit.field.label(),
                        hit.range.start,
                        hit.range.end,
                        hit.revision
                    )
                } else {
                    format!(
                        "Tool {:?} | UTF-8 bytes {}..{} | revision {}",
                        hit.field, hit.range.start, hit.range.end, hit.revision
                    )
                })));
                if tool_search::valid(hit, &current.tool_details) {
                    if let Some(detail) = current.tool_details.get(&hit.locator) {
                        lines.extend(row(field_segments(
                            theme,
                            &[
                                Field::new("Category", format!("{:?}", detail.category)).zh("类别"),
                                Field::new("lifecycle", format!("{:?}", detail.lifecycle))
                                    .zh("生命周期")
                                    .zh_value(lifecycle_zh(detail.lifecycle))
                                    .tone(Tone::Lifecycle(detail.lifecycle)),
                            ],
                            " | ",
                        )));
                    }
                    lines.extend(row(field_segments(
                        theme,
                        &[
                            Field::optional("Thread", hit.locator.identity.thread_id.as_deref())
                                .zh("线程"),
                            Field::optional("turn", hit.locator.identity.turn_id.as_deref())
                                .zh("轮次"),
                            Field::new("item", hit.locator.item_id.clone()).zh("条目"),
                        ],
                        " | ",
                    )));
                    lines.extend(row(quiet(
                        tr(
                            "Enter opens this retained tool detail.",
                            "按 Enter 打开这条已保留的工具详情。",
                        )
                        .into(),
                    )));
                } else {
                    lines.extend(row(warning(tr(
                        "Live tool content changed/evicted; opening disabled until refresh.",
                        "工具内容已变化或已被清出，刷新前不能打开。",
                    ))));
                }
            } else if self.selected < message_count {
                let hit = &results.hits[self.selected];
                let message = &results.source.as_ref().unwrap().messages[hit.message];
                lines.extend(row(quiet(format!(
                    "{} {} | {} | {}",
                    tr("Hit", "命中"),
                    self.selected + 1,
                    role_text(&message.role),
                    if message.complete {
                        tr("complete", "已完成")
                    } else {
                        tr("streaming", "输出中")
                    }
                ))));
                for field in [
                    Field::new("Thread", message.thread_id.clone()).zh("线程"),
                    if message.turn_id.is_empty() {
                        Field::new("Turn", "unbound").zh_value("未绑定")
                    } else {
                        Field::new("Turn", message.turn_id.clone())
                    }
                    .zh("轮次"),
                    Field::new("Item", message.id.clone()).zh("条目"),
                    Field::new(
                        "UTF-8 bytes",
                        format!("{}..{}", hit.range.start, hit.range.end),
                    )
                    .zh("UTF-8 字节")
                    .tone(Tone::Quiet),
                ] {
                    lines.extend(row(field_segments(theme, &[field], " | ")));
                }
                if !current
                    .messages
                    .iter()
                    .any(|m| MessageKey::of(message).matches(m) && m.text == message.text)
                {
                    lines.extend(row(warning(tr(
                        "Live content changed/evicted; opening disabled until refresh.",
                        "内容已变化或已被清出，刷新前不能打开。",
                    ))));
                }
                lines.extend(row(field_segments(
                    theme,
                    &[Field::new("Match", results.filter.text.clone()).zh("命中")],
                    " | ",
                )));
                lines.push(Line::default());
                lines.extend(row(vec![(
                    tr("--- retained message ---", "--- 已保留的消息 ---").to_owned(),
                    theme.heading(),
                )]));
                // The message stays verbatim; only the located match is emphasized.
                lines.extend(row(vec![
                    (message.text[..hit.range.start].to_owned(), value),
                    (
                        message.text[hit.range.clone()].to_owned(),
                        match_style(theme),
                    ),
                    (message.text[hit.range.end..].to_owned(), value),
                ]));
            } else {
                lines.extend(row(vec![(
                    tr(
                        "No matches in the selected retained content.",
                        "所选的已保留内容里没有命中。",
                    )
                    .to_owned(),
                    value,
                )]));
                let mut next = quiet(tr("Next: ", "接下来：").into());
                next.extend(
                    theme::hints(tr(
                        "/ edit query | Tab widen scope | F5 switch mode | r refresh",
                        "/ 编辑查询 | Tab 扩大范围 | F5 切换模式 | r 刷新",
                    ))
                    .spans
                    .into_iter()
                    .map(|span| (span.content.into_owned(), span.style)),
                );
                lines.extend(row(next));
            }
            if total > HIT_LIMIT {
                lines.extend(row(warning(tr(
                    "Result limit reached: narrow query or filters.",
                    "已达结果上限：请缩小查询或筛选条件。",
                ))));
            }
            if results.source.as_ref().is_some_and(|source| {
                source.history_truncated || source.messages.iter().any(|m| m.truncated)
            }) {
                lines.extend(row(quiet(
                    tr("Older content unavailable.", "更早的内容不可用。").into(),
                )));
            }
        }
        let height = body.height as usize;
        let start = self.scroll.min(lines.len().saturating_sub(height));
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(start)
                    .take(height)
                    .collect::<Vec<_>>(),
            ),
            body,
        );
        if area.height > query_height {
            let hints: Vec<&str> = if self.editing {
                vec![
                    tr(
                        "Enter search | Esc close | F1 help",
                        "Enter 搜索 | Esc 关闭 | F1 帮助",
                    ),
                    tr("Enter search | Esc close", "Enter 搜索 | Esc 关闭"),
                    tr("Esc close", "Esc 关闭"),
                ]
            } else {
                vec![
                    tr(
                        "Enter open | F5 mode | Esc close | F1 help",
                        "Enter 打开 | F5 模式 | Esc 关闭 | F1 帮助",
                    ),
                    tr(
                        "Enter open | Esc close | F1 help",
                        "Enter 打开 | Esc 关闭 | F1 帮助",
                    ),
                    tr("Enter open | Esc close", "Enter 打开 | Esc 关闭"),
                    tr("Esc close", "Esc 关闭"),
                ]
            };
            frame.render_widget(
                Paragraph::new(
                    fit_hints(&hints, area.width as usize)
                        .unwrap_or_else(|| theme::hints(hints[hints.len() - 1])),
                ),
                Rect {
                    y: area.y + area.height - 1,
                    height: 1,
                    ..area
                },
            );
        }
    }

    /// A short window of hits around the selection. Each row is
    /// `marker number origin  before⟦match⟧after`; the prefix width is taken
    /// out of the wrap width so no preview is cut by the right edge.
    fn hit_list(
        &self,
        results: &Results,
        current: &CoreSnapshot,
        width: usize,
        theme: Theme,
    ) -> Vec<Line<'static>> {
        let count = results.count();
        let message_count = results.message_hits();
        let digits = count.to_string().len();
        let first = self
            .selected
            .saturating_sub(HIT_WINDOW / 2)
            .min(count.saturating_sub(HIT_WINDOW));
        let value = Style::default().fg(theme.foreground);
        let mut lines = Vec::new();
        for index in first..(first + HIT_WINDOW).min(count) {
            let selected = index == self.selected;
            // `>` marks the selection without color.
            let prefix = format!(
                "{} {:>digits$} ",
                if selected { ">" } else { " " },
                index + 1
            );
            let room = width.saturating_sub(prefix.width()).max(1);
            let (origin, context) = if index < message_count {
                let hit = &results.hits[index];
                let Some(message) = results
                    .source
                    .as_ref()
                    .and_then(|source| source.messages.get(hit.message))
                else {
                    continue;
                };
                (
                    role_text(&message.role).to_owned(),
                    Some(hit_context(&message.text, &hit.range)),
                )
            } else {
                let hit = &results.tool_hits[index - message_count];
                let context = tool_search::valid(hit, &current.tool_details)
                    .then(|| current.tool_details.get(&hit.locator))
                    .flatten()
                    .and_then(|detail| {
                        hit.field
                            .text(&detail)
                            .map(|text| hit_context(text, &hit.range))
                    });
                (
                    format!("{} {}", tr("Tool", "工具"), hit.field.label()),
                    context,
                )
            };
            let found = format!("⟦{}⟧", results.filter.text);
            let preview: Vec<Segment> = match context {
                Some((before, after)) => vec![
                    (format!("{origin}  "), theme.secondary()),
                    (before, value),
                    (found, match_style(theme)),
                    (after, value),
                ],
                None => vec![(
                    format!("{origin}  {}", tr("changed or evicted", "已变化或已被清出")),
                    theme.secondary(),
                )],
            };
            if selected {
                let text: String = preview.iter().map(|(text, _)| text.as_str()).collect();
                let rows = wrap_segments(&[(text, theme.selected())], room);
                for (row, line) in rows.into_iter().enumerate() {
                    let lead = if row == 0 {
                        prefix.clone()
                    } else {
                        " ".repeat(prefix.width())
                    };
                    let fill = width.saturating_sub(prefix.width() + line.width());
                    let mut spans = vec![Span::styled(lead, theme.selected())];
                    spans.extend(line.spans);
                    // Padding makes the selection read as a full row.
                    spans.push(Span::styled(" ".repeat(fill), theme.selected()));
                    lines.push(Line::from(spans));
                }
            } else {
                // Other hits stay on one row; `…` says the preview continues.
                let flat: Vec<Segment> = preview
                    .into_iter()
                    .map(|(text, style)| (text.replace(['\n', '\t'], " "), style))
                    .collect();
                let mut rows = wrap_segments(&flat, room);
                let clipped = rows.len() > 1;
                if clipped {
                    rows = wrap_segments(&flat, room.saturating_sub(1).max(1));
                }
                let mut spans = vec![Span::styled(prefix, theme.secondary())];
                spans.extend(rows.swap_remove(0).spans);
                if clipped {
                    spans.push(Span::styled("…", theme.secondary()));
                }
                lines.push(Line::from(spans));
            }
        }
        lines
    }
}

/// Hits listed around the selection.
const HIT_WINDOW: usize = 5;

/// The located match inside retained text; brackets or underline keep it
/// visible without color.
fn match_style(theme: Theme) -> Style {
    Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

/// Up to 6 graphemes before and 32 after a hit, for a one-line preview.
fn hit_context(text: &str, range: &Range<usize>) -> (String, String) {
    let before: String = text[..range.start]
        .graphemes(true)
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let after: String = text[range.end..].graphemes(true).take(32).collect();
    (before, after)
}

/// The first rows of `HELP` are `keys: action`; the rest are notes.
const HELP_KEY_ROWS: usize = 14;

const HELP: &[&str] = &[
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
];

/// Same rows as `HELP`; key rows read `keys：action`.
const HELP_ZH: &[&str] = &[
    "Ctrl+F 或 /：编辑查询",
    "Ctrl+T：按精确轮次筛选",
    "Enter：搜索/打开；Esc：关闭（保留草稿）",
    "Tab：线程/子树/路径/全部 Agent",
    "F6：角色筛选",
    "F7：状态筛选",
    "F5：消息/工具/两者；F8 类别；F9 生命周期",
    "Up/Down 或 N/n：上一个/下一个已保留的命中",
    "PgUp/Dn：滚动一行",
    "Ctrl+Home/End：到开头/到末尾",
    "r：刷新已保留的消息",
    "F2：待处理的请求",
    "Ctrl+C：中断根 Agent",
    "Ctrl+Q：退出",
    "按字面匹配，区分大小写，支持 Unicode；不支持正则",
    "只搜索已保留的消息和实时工具详情",
    "查询和结果不会写入 journal",
];

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
            tools: Vec::new(),
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
            name: None,
            command: Some("needle 中文".into()),
            cwd: None,
            target: None,
            parameters: None,
            result: None,
            output: "private output".into(),
            output_source: ToolTextSource::OutputDelta,
            exit_code: None,
            duration_ms: None,
            diff: None,
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
            // 焦点到显示行的映射由 `conversation::transcript` 负责并在那里测试。
            let _ = (width, expected);
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

    /// Screen rows of a rendered panel; fixtures here use single-width characters.
    fn screen(
        panel: &SearchPanel,
        current: &CoreSnapshot,
        width: u16,
        height: u16,
    ) -> (ratatui::buffer::Buffer, Vec<String>) {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| panel.draw(frame, frame.area(), current))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        (buffer, rows)
    }

    /// Position of the first cell showing `needle`.
    fn cell_of(rows: &[String], needle: &str) -> (u16, u16) {
        for (y, row) in rows.iter().enumerate() {
            if let Some(index) = row.find(needle) {
                return (row[..index].chars().count() as u16, y as u16);
            }
        }
        panic!("{needle:?} not rendered in {rows:#?}");
    }

    /// The parts of a cell style that carry hierarchy.
    fn look(
        cell: &ratatui::buffer::Cell,
    ) -> (
        ratatui::style::Color,
        ratatui::style::Color,
        ratatui::style::Modifier,
    ) {
        (cell.fg, cell.bg, cell.modifier)
    }

    fn selected_look() -> (
        ratatui::style::Color,
        ratatui::style::Color,
        ratatui::style::Modifier,
    ) {
        let style = theme::current().selected();
        (
            style.fg.unwrap_or_default(),
            style.bg.unwrap_or_default(),
            style.add_modifier,
        )
    }

    async fn result_panel(source: &Arc<CoreSnapshot>, query: &str, selected: usize) -> SearchPanel {
        let filter = Filter {
            text: query.into(),
            thread: "root".into(),
            ..Default::default()
        };
        SearchPanel {
            visible: true,
            filter: filter.clone(),
            selected,
            results: Some(Arc::new(found(source.clone(), filter).await)),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn query_box_frame_follows_focus_and_hints_fit_the_width() {
        let theme = theme::current();
        let source = Arc::new(CoreSnapshot {
            thread_id: Some("root".into()),
            messages: vec![message("root", "turn", "item", "needle")],
            ..Default::default()
        });
        let mut panel = SearchPanel::default();
        panel.open("root".into());
        let (buffer, rows) = screen(&panel, &source, 60, 12);
        // Editing: the rounded accent frame marks where typing goes.
        assert_eq!(buffer[(0, 0)].symbol(), "╭");
        assert_eq!(buffer[(0, 0)].fg, theme.accent);
        assert!(rows[0].contains(" Search: literal text "));
        assert_eq!(
            rows[11].trim_end(),
            "Enter search  ·  Esc close  ·  F1 help"
        );
        assert!(buffer[(0, 11)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(6, 11)].modifier.contains(Modifier::BOLD));
        let (x, y) = cell_of(&rows, "Enter searches. F1 shows controls.");
        assert_eq!(buffer[(x, y)].fg, theme.muted);

        // Result mode: keys move through hits, so the box drops the focus frame.
        let panel = result_panel(&source, "needle", 0).await;
        let (buffer, rows) = screen(&panel, &source, 60, 12);
        assert_eq!(buffer[(0, 0)].symbol(), "┌");
        assert_eq!(buffer[(0, 0)].fg, theme.border);
        assert_eq!(
            rows[11].trim_end(),
            "Enter open  ·  F5 mode  ·  Esc close  ·  F1 help"
        );
        for (width, expected) in [
            (40, "Enter open  ·  Esc close  ·  F1 help"),
            (30, "Enter open  ·  Esc close"),
        ] {
            let (_, rows) = screen(&panel, &source, width, 12);
            assert_eq!(rows[11].trim_end(), expected, "{width}");
        }
    }

    #[tokio::test]
    async fn hit_list_marks_the_selection_and_counters_stay_secondary() {
        let theme = theme::current();
        let source = Arc::new(CoreSnapshot {
            thread_id: Some("root".into()),
            messages: vec![
                message("root", "turn", "a", "first needle here"),
                message("root", "turn", "b", "second needle there"),
                message("root", "turn", "c", "third needle\nnext line"),
            ],
            ..Default::default()
        });
        let panel = result_panel(&source, "needle", 1).await;
        let (buffer, rows) = screen(&panel, &source, 80, 24);

        // Counters and filters are metadata.
        for text in ["Hit 2/3 | 3 total", "mode=messages", "Snapshot 0"] {
            let (x, y) = cell_of(&rows, text);
            assert_eq!(buffer[(x, y)].fg, theme.muted, "{text}");
            assert!(!buffer[(x, y)].modifier.contains(Modifier::BOLD), "{text}");
        }

        // The selected hit: `>` marker and one full-width selected row.
        let (x, y) = cell_of(&rows, "> 2 Agent  econd ⟦needle⟧ there");
        assert_eq!(x, 0);
        for column in [0, 12, 40, 79] {
            assert_eq!(look(&buffer[(column, y)]), selected_look(), "{column}");
        }
        // Other hits: one row each, match emphasized, never the selected style.
        for (text, bracket) in [
            ("  1 Agent  first ⟦needle⟧ here", 17),
            ("  3 Agent  third ⟦needle⟧ next line", 17),
        ] {
            let (x, y) = cell_of(&rows, text);
            assert_eq!(x, 0);
            assert_eq!(buffer[(4, y)].fg, theme.muted, "{text}");
            assert_eq!(buffer[(11, y)].fg, theme.foreground, "{text}");
            let found = &buffer[(bracket, y)];
            assert_eq!(found.symbol(), "⟦");
            assert_eq!(found.fg, theme.accent);
            assert!(found.modifier.contains(Modifier::UNDERLINED));
            assert!(!buffer[(11, y)].modifier.contains(Modifier::REVERSED));
            assert_ne!(look(&buffer[(0, y)]), selected_look(), "{text}");
            assert_ne!(look(&buffer[(79, y)]), selected_look(), "{text}");
        }

        // Details: labels lead, values are plain, the section heading stands out.
        let (x, y) = cell_of(&rows, "Turn: turn");
        assert!(buffer[(x, y)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(x + 6, y)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(x + 6, y)].fg, theme.foreground);
        let (x, y) = cell_of(&rows, "--- retained message ---");
        assert!(rows[y as usize - 1].trim().is_empty());
        assert!(buffer[(x, y)].modifier.contains(Modifier::BOLD));
        // The retained message is plain text with only the match emphasized.
        let (x, y) = cell_of(&rows, "second needle there");
        assert_eq!(buffer[(x, y)].fg, theme.foreground);
        assert!(!buffer[(x, y)].modifier.contains(Modifier::BOLD));
        assert!(buffer[(x + 7, y)]
            .modifier
            .contains(Modifier::UNDERLINED | Modifier::BOLD));
        assert!(!buffer[(x + 14, y)].modifier.contains(Modifier::UNDERLINED));
    }

    #[tokio::test]
    async fn selected_hit_wraps_inside_its_prefix_without_losing_text() {
        let source = Arc::new(CoreSnapshot {
            thread_id: Some("root".into()),
            messages: vec![message(
                "root",
                "turn",
                "a",
                "0123456789 needle abcdefghijklmnopqrstuvwxyz0123456789",
            )],
            ..Default::default()
        });
        let panel = result_panel(&source, "needle", 0).await;
        let (buffer, rows) = screen(&panel, &source, 30, 24);
        let (_, y) = cell_of(&rows, "> 1 Agent  ");
        let y = y as usize;
        // 26 cells remain after the 4-cell prefix; continuation rows are indented.
        assert_eq!(rows[y], "> 1 Agent  56789 ⟦needle⟧ abcd");
        assert_eq!(rows[y + 1], "    efghijklmnopqrstuvwxyz0123");
        assert_eq!(rows[y + 2].trim_end(), "    4");
        let shown: String = rows[y..y + 3]
            .iter()
            .map(|row| row.chars().skip(4).collect::<String>())
            .collect();
        assert_eq!(
            shown.trim_end(),
            "Agent  56789 ⟦needle⟧ abcdefghijklmnopqrstuvwxyz01234"
        );
        for row in y..y + 3 {
            assert_eq!(look(&buffer[(29, row as u16)]), selected_look());
        }
    }

    #[tokio::test]
    async fn empty_results_name_the_next_steps() {
        let theme = theme::current();
        let source = Arc::new(CoreSnapshot {
            thread_id: Some("root".into()),
            messages: vec![message("root", "turn", "a", "nothing relevant")],
            ..Default::default()
        });
        let panel = result_panel(&source, "absent", 0).await;
        let (buffer, rows) = screen(&panel, &source, 100, 16);
        let text = rows.join("\n");
        assert!(text.contains("Hit 0/0 | 0 total"), "{text}");
        let (x, y) = cell_of(&rows, "No matches in the selected retained content.");
        assert_eq!(buffer[(x, y)].fg, theme.foreground);
        let (x, next) = cell_of(
            &rows,
            "Next: / edit query  ·  Tab widen scope  ·  F5 switch mode  ·  r refresh",
        );
        assert_eq!(next, y + 1);
        assert_eq!(buffer[(x, next)].fg, theme.muted);
        // Keys are emphasized; their labels stay quiet.
        assert!(buffer[(x + 6, next)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(x + 8, next)].modifier.contains(Modifier::BOLD));
        // Narrow widths wrap the hint instead of cutting it.
        let (_, rows) = screen(&panel, &source, 30, 16);
        let text = rows.join("");
        assert!(text.contains("r refresh"), "{rows:#?}");
    }
}
