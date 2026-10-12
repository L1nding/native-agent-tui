//! Local browsing state for Core's bounded evidence archive. No execution sender.
use std::cell::Cell;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::lang::{self, tr};
use super::scope::Scope;
use super::theme::{self, Theme};
use super::tool_detail::{
    self, activity_scope_zh, execution_text, execution_zh, field_segments, field_shown_text,
    field_text, help_parts, scope_text, wrap_segments, Field as Fact, Segment, Tone,
};
use super::{search, Editor};
use crate::interactions::RequestRef;
use crate::observation::{ActivityScope, EvidenceKind, ExecutionState};
use crate::state::{display_text, CoreSnapshot};
use crate::timeline::{TimelineEntry, BYTE_LIMIT, ENTRY_LIMIT};
use crate::tool_details::ToolDetailLocator;

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

    /// Name shown in the filter row; English is the variant name.
    fn label(self) -> &'static str {
        match self {
            Self::All => tr("All", "全部"),
            Self::Lifecycle => tr("Lifecycle", "生命周期"),
            Self::Output => tr("Output", "输出"),
            Self::Tool => tr("Tool", "工具"),
            Self::Request => tr("Request", "请求"),
            Self::Waiting => tr("Waiting", "等待"),
        }
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

impl State {
    /// Name shown in the filter row; English is the variant name.
    fn label(self) -> &'static str {
        match self {
            Self::All => tr("All", "全部"),
            Self::Active => tr("Active", "进行中"),
            Self::Terminal => tr("Terminal", "已结束"),
            Self::Unknown => tr("Unknown", "未知"),
        }
    }
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
    ToolSearch(String),
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
    scroll: Cell<usize>,
    tool_detail_max_scroll: Cell<usize>,
    tool_detail: Option<ToolDetailLocator>,
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
            self.tool_detail = None;
            self.scroll.set(0);
            self.tool_detail_max_scroll.set(0);
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
            self.tool_detail = None;
            self.scroll.set(0);
            self.tool_detail_max_scroll.set(0);
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
                && (self.query.text.is_empty() || metadata_matches(entry, &self.query.text))
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
        if self.category == Category::All && !self.bookmarks_only {
            // 流式输出每个片段都是一条证据，会淹没生命周期和工具事件；全部视图里
            // 同一 item 连续的 Output 只保留最新一条（已选中的保留），Output 分类可看全部。
            let same_stream = |a: &TimelineEntry, b: &TimelineEntry| {
                a.evidence.kind == EvidenceKind::Output
                    && b.evidence.kind == EvidenceKind::Output
                    && a.identity.thread_id == b.identity.thread_id
                    && a.identity.turn_id == b.identity.turn_id
                    && a.item_id == b.item_id
            };
            let keep: Vec<bool> = (0..rows.len())
                .map(|index| {
                    Some(rows[index].evidence.id) == self.selected
                        || rows
                            .get(index + 1)
                            .is_none_or(|next| !same_stream(&rows[index], next))
                })
                .collect();
            let mut keep = keep.into_iter();
            rows.retain(|_| keep.next().unwrap_or(true));
        }
        rows
    }

    fn select_latest(&mut self, current: &CoreSnapshot) {
        self.selected = self.rows(current).last().map(|entry| entry.evidence.id);
        self.scroll.set(0);
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
            self.notice = Some(
                tr(
                    "Timeline field limited to 1024 UTF-8 bytes.",
                    "时间线输入框最多 1024 个 UTF-8 字节。",
                )
                .into(),
            );
        }
    }

    pub fn paste(&mut self, text: &str) {
        if self.editing.is_some() {
            self.insert(text);
        }
    }
    pub fn reject_paste(&mut self) {
        self.notice = Some(super::paste_rejected().into());
    }

    fn toggle_bookmark(&mut self, current: &CoreSnapshot) {
        let Some(entry) = self.selected(current) else {
            self.notice = Some(
                tr(
                    "Selected evidence was evicted or filtered; select a retained event.",
                    "所选证据已被清出或被筛掉，请选择一条已保留的事件。",
                )
                .into(),
            );
            return;
        };
        if let Some(index) = self.bookmarks.iter().position(|bookmark| {
            bookmark.session == self.session && bookmark.entry.evidence.id == entry.evidence.id
        }) {
            self.bookmarks.remove(index);
            self.notice = Some(tr("Bookmark removed.", "书签已移除。").into());
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
                tr(
                    "Bookmark budget reached (64 entries / 64 KiB); remove a bookmark first.",
                    "书签已达上限（64 条 / 64 KiB），请先移除一个书签。",
                )
                .into(),
            );
        } else {
            self.bookmarks.push(bookmark);
            self.notice = Some(
                tr(
                    "Bookmark saved locally for this live session.",
                    "书签已保存在本地，仅本次实时会话有效。",
                )
                .into(),
            );
        }
    }

    pub fn key(&mut self, key: KeyEvent, current: &CoreSnapshot) -> Option<Locate> {
        self.sync(current);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.tool_detail.is_some() {
            match key.code {
                KeyCode::Char('s') if !ctrl => {
                    return Some(Locate::ToolSearch(self.thread.clone()));
                }
                KeyCode::Char('f') if ctrl => {
                    return Some(Locate::ToolSearch(self.thread.clone()));
                }
                KeyCode::Esc => {
                    self.tool_detail = None;
                    self.scroll.set(0);
                    self.tool_detail_max_scroll.set(0);
                    self.notice = None;
                }
                KeyCode::PageUp => self.scroll.set(self.scroll.get().saturating_sub(1)),
                KeyCode::PageDown => {
                    self.scroll.set(
                        self.scroll
                            .get()
                            .saturating_add(1)
                            .min(self.tool_detail_max_scroll.get()),
                    );
                }
                KeyCode::Home => self.scroll.set(0),
                KeyCode::End => self.scroll.set(self.tool_detail_max_scroll.get()),
                _ => {}
            }
            return None;
        }
        if key.code == KeyCode::Esc {
            self.close();
            return None;
        }
        if key.code == KeyCode::F(1) {
            self.help = !self.help;
            self.scroll.set(0);
            return None;
        }
        if self.help {
            match key.code {
                KeyCode::PageUp => self.scroll.set(self.scroll.get().saturating_sub(1)),
                KeyCode::PageDown => self.scroll.set(self.scroll.get().saturating_add(1)),
                KeyCode::Home => self.scroll.set(0),
                KeyCode::End => self.scroll.set(usize::MAX),
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
            KeyCode::PageUp => self.scroll.set(self.scroll.get().saturating_sub(1)),
            KeyCode::PageDown => self.scroll.set(self.scroll.get().saturating_add(1)),
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
                self.scroll.set(0);
                self.notice = None;
            }
            KeyCode::Enter if !ctrl => {
                let Some(entry) = self.selected(current) else {
                    self.notice = Some(
                        tr(
                            "Selected evidence was evicted or filtered; navigation required.",
                            "所选证据已被清出或被筛掉，请重新选择。",
                        )
                        .into(),
                    );
                    return None;
                };
                if !self.retained(&entry, current) {
                    self.notice = Some(
                        tr(
                            "Bookmarked content was evicted; original metadata only.",
                            "书签对应的内容已被清出，只剩原始元数据。",
                        )
                        .into(),
                    );
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
                    self.notice = Some(
                        tr(
                            "This request delivery has ended; no current request matches.",
                            "这次请求投递已结束，没有匹配的当前请求。",
                        )
                        .into(),
                    );
                } else if (entry.scope == ActivityScope::Tool
                    || has_exact_tool_detail(&entry, current))
                    && matches!(
                        entry.evidence.kind,
                        EvidenceKind::ToolStarted
                            | EvidenceKind::Output
                            | EvidenceKind::ToolCompleted
                            | EvidenceKind::ExecutionUnknown
                    )
                {
                    let Some(item_id) = entry.item_id.clone() else {
                        self.notice = Some(
                            tr(
                                "Tool detail unavailable: item identity is missing.",
                                "工具详情不可用：缺少条目标识。",
                            )
                            .into(),
                        );
                        return None;
                    };
                    let locator = ToolDetailLocator {
                        session_id: current.timeline.session_id.clone(),
                        identity: entry.identity.clone(),
                        item_id,
                    };
                    if current.tool_details.get(&locator).is_some()
                        || super::tool_trace::has_matching_metadata(&locator, current)
                    {
                        self.tool_detail = Some(locator);
                        self.scroll.set(0);
                        self.tool_detail_max_scroll.set(0);
                        self.notice = None;
                    } else {
                        self.notice = Some(
                            tr(
                                "Tool detail unavailable or evicted for this exact item identity.",
                                "这个条目的工具详情不可用或已被清出。",
                            )
                            .into(),
                        );
                    }
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
                        tr(
                            "Message body unavailable or evicted; evidence metadata remains.",
                            "消息正文不可用或已被清出，证据元数据仍在。",
                        )
                        .into(),
                    );
                } else {
                    self.notice = Some(
                        tr(
                            "Evidence details shown; no retained message or request link.",
                            "已显示证据详情；没有可定位的已保留消息或请求。",
                        )
                        .into(),
                    );
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
        if let Some(locator) = &self.tool_detail {
            let max = tool_detail::draw(
                frame,
                area,
                current,
                locator,
                self.scroll.get(),
                self.notice.as_deref(),
            );
            self.tool_detail_max_scroll.set(max);
            self.scroll.set(self.scroll.get().min(max));
            return;
        }
        let theme = theme::current();
        // One themed surface, so the panel matches the framed panels around it.
        frame.render_widget(theme::panel(), area);
        let value = Style::default().fg(theme.foreground);
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
                || tr("empty", "空").into(),
                |(first, last)| format!("#{}..#{}", first.evidence.id, last.evidence.id),
            );
        let zh = lang::zh();
        // Text: "Retained ..; accepted through #..; omitted N (gaps possible); budget ..".
        let range: Vec<Segment> = vec![
            (
                if zh {
                    format!(
                        "已保留 {}；已接收至 #{}；",
                        retained, current.timeline.high_water
                    )
                } else {
                    format!(
                        "Retained {}; accepted through #{}; ",
                        retained, current.timeline.high_water
                    )
                },
                theme.secondary(),
            ),
            (
                if zh {
                    format!("已省略 {}（可能有缺口）", current.timeline.dropped_entries)
                } else {
                    format!(
                        "omitted {} (gaps possible)",
                        current.timeline.dropped_entries
                    )
                },
                if current.timeline.dropped_entries > 0 {
                    Style::default().fg(theme.warning)
                } else {
                    theme.secondary()
                },
            ),
            (
                if zh {
                    format!("；预算 {} 条 / {} KiB", ENTRY_LIMIT, BYTE_LIMIT / 1024)
                } else {
                    format!("; budget {} / {} KiB", ENTRY_LIMIT, BYTE_LIMIT / 1024)
                },
                theme.secondary(),
            ),
        ];
        let filters: Vec<Segment> = vec![(
            format!(
                "{} / {} / {}{}{}",
                scope_text(self.scope),
                self.category.label(),
                self.state.label(),
                if self.bookmarks_only {
                    tr(" / bookmarks", " / 仅书签")
                } else {
                    ""
                },
                if self.pending_only {
                    tr(" / current pending", " / 当前待处理")
                } else {
                    ""
                }
            ),
            value,
        )];
        let header_height = area.height.min(if area.height >= 12 { 4 } else { 2 });
        let mut header: Vec<Vec<Segment>> = vec![
            vec![
                (
                    tr("Evidence timeline", "证据时间线").to_owned(),
                    theme.heading(),
                ),
                (
                    format!(
                        " {}/{} | {}",
                        index.map_or(0, |index| index + 1),
                        rows.len(),
                        if zh {
                            format!("{} 个书签", self.bookmarks.len())
                        } else {
                            format!("{} bookmarks", self.bookmarks.len())
                        }
                    ),
                    theme.secondary(),
                ),
            ],
            range.clone(),
            filters.clone(),
            field_segments(
                theme,
                &[
                    Fact::new("Thread", self.thread.clone()).zh("线程"),
                    if self.turn.text.is_empty() {
                        Fact::new("turn", "all").zh_value("全部")
                    } else {
                        Fact::new("turn", self.turn.text.clone())
                    }
                    .zh("轮次"),
                    Fact::new("query", self.query.text.clone()).zh("查询"),
                ],
                " | ",
            ),
        ];
        if let Some(field) = self.editing {
            let editor = match field {
                Field::Query => &self.query,
                Field::Turn => &self.turn,
            };
            let mut left = editor.text[..editor.cursor].to_string();
            let prefix = match field {
                Field::Query => tr("Metadata: ", "元数据："),
                Field::Turn => tr("Exact turn: ", "精确轮次："),
            };
            let width = (area.width as usize).saturating_sub(prefix.width()).max(1);
            while left.width() >= width && !left.is_empty() {
                let bytes = left.graphemes(true).next().unwrap().len();
                left.drain(..bytes);
            }
            header[0] = vec![
                (
                    format!(
                        "{}{}",
                        tr("Timeline: edit ", "时间线：编辑"),
                        match field {
                            Field::Query => tr("metadata", "元数据"),
                            Field::Turn => tr("turn", "轮次"),
                        }
                    ),
                    theme.heading(),
                ),
                (
                    tr(" (Enter apply)", "（Enter 应用）").to_owned(),
                    theme.secondary(),
                ),
            ];
            header[1] = vec![
                (prefix.to_owned(), theme.label()),
                (format!("{left}{}", &editor.text[editor.cursor..]), value),
            ];
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
                    .map(|segments| {
                        Line::from(
                            segments
                                .into_iter()
                                .map(|(text, style)| Span::styled(display_text(&text), style))
                                .collect::<Vec<_>>(),
                        )
                    })
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
        let quiet = |text: &str| vec![(text.to_owned(), theme.secondary())];
        let warning = |text: &str| vec![(text.to_owned(), Tone::Warning.style(theme))];
        let mut details: Vec<Vec<Segment>> = Vec::new();
        if let Some(notice) = &self.notice {
            details.push(warning(notice));
        }
        if self.help {
            let help = if zh { HELP_ZH } else { HELP };
            details.extend(help.iter().enumerate().map(|(row, line)| {
                // Key rows read `keys: action`; the remaining rows are notes.
                match help_parts(line).filter(|_| row < HELP_KEY_ROWS) {
                    Some((keys, action)) => {
                        vec![(keys.to_owned(), theme.key()), (action.to_owned(), value)]
                    }
                    None => quiet(line),
                }
            }));
        } else if let Some(entry) = index.and_then(|index| rows.get(index)) {
            details.push(vec![
                (
                    format!("#{} {:?}", entry.evidence.id, entry.evidence.kind),
                    theme.heading(),
                ),
                (
                    if zh {
                        format!(
                            " | {} / {}",
                            activity_scope_zh(entry.scope),
                            execution_text(entry.execution_state)
                        )
                    } else {
                        format!(" | {:?} / {:?}", entry.scope, entry.execution_state)
                    },
                    theme.secondary(),
                ),
            ]);
            if !self.retained(entry, current) {
                details.push(warning(tr(
                    "BOOKMARK: content evicted; original metadata only",
                    "书签：内容已被清出，只剩原始元数据",
                )));
            }
            details.extend(
                metadata_fields(entry)
                    .iter()
                    .map(|fields| field_segments(theme, fields, " | ")),
            );
            details.push(quiet(tr(
                "Enter: locate retained message / current request delivery",
                "Enter：定位已保留的消息 / 当前请求投递",
            )));
            details.push(quiet(tr(
                "Raw tool output and reasoning: unavailable here",
                "原始工具输出和推理：这里不可用",
            )));
        } else if self.selected.is_some() {
            details.push(warning(tr(
                "Selected evidence evicted or filtered. Up/Down selects an available event.",
                "所选证据已被清出或被筛掉。按 Up/Down 选择一条可用的事件。",
            )));
        } else {
            details.push(vec![(
                tr(
                    "No evidence matches this retained range and filter.",
                    "当前保留范围和筛选条件下没有匹配的证据。",
                )
                .to_owned(),
                value,
            )]);
        }
        let detail_area = if area.width >= 100 && body.height >= 4 && !self.help {
            let panels = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
                .split(body);
            let height = panels[0].height.saturating_sub(2) as usize;
            let list_width = panels[0].width.saturating_sub(2) as usize;
            let start = index
                .unwrap_or(0)
                .saturating_sub(height / 2)
                .min(rows.len().saturating_sub(height));
            let list: Vec<_> = rows
                .iter()
                .skip(start)
                .take(height)
                .map(|entry| {
                    let selected = Some(entry.evidence.id) == self.selected;
                    let bookmarked = self.bookmarks.iter().any(|bookmark| {
                        bookmark.session == self.session
                            && bookmark.entry.evidence.id == entry.evidence.id
                    });
                    // `>` and `*` mark selection and bookmarks without color.
                    let marker = if selected { ">" } else { " " };
                    let star = if bookmarked { "*" } else { " " };
                    let id = format!(" #{} ", entry.evidence.id);
                    let kind = format!("{:?}", entry.evidence.kind);
                    if selected {
                        // One padded span, so the selection reads as a full row.
                        // 按显示宽度补齐，双宽字符不会把这一行撑出边框。
                        let text = format!("{marker}{star}{id}{kind}");
                        let fill = list_width.saturating_sub(text.width());
                        Line::from(Span::styled(
                            format!("{text}{}", " ".repeat(fill)),
                            theme.selected(),
                        ))
                    } else {
                        Line::from(vec![
                            Span::raw(marker),
                            Span::styled(star, theme.heading()),
                            Span::styled(id, theme.secondary()),
                            Span::styled(kind, evidence_style(entry, theme)),
                        ])
                    }
                })
                .collect();
            frame.render_widget(
                Paragraph::new(list).block(
                    theme::panel()
                        .borders(Borders::ALL)
                        .title(tr(" Evidence acceptance order ", " 证据接收顺序 ")),
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
            .flat_map(|segments| wrap_segments(segments, detail_area.width as usize))
            .collect();
        let start = self
            .scroll
            .get()
            .min(lines.len().saturating_sub(detail_area.height as usize));
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(start)
                    .take(detail_area.height as usize)
                    .collect::<Vec<_>>(),
            ),
            detail_area,
        );
        if area.height > header_height {
            let hints = [
                tr(
                    "Enter locate | b bookmark | / filter | Esc close | F1 help",
                    "Enter 定位 | b 书签 | / 筛选 | Esc 关闭 | F1 帮助",
                ),
                tr(
                    "Enter locate | b bookmark | Esc close | F1 help",
                    "Enter 定位 | b 书签 | Esc 关闭 | F1 帮助",
                ),
                tr(
                    "Enter locate | Esc close | F1 help",
                    "Enter 定位 | Esc 关闭 | F1 帮助",
                ),
                tr("Esc close | F1 help", "Esc 关闭 | F1 帮助"),
            ];
            frame.render_widget(
                Paragraph::new(
                    tool_detail::fit_hints(&hints, area.width as usize)
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
}

/// Restrained emphasis for an evidence kind; the kind name stays in the text.
fn evidence_style(entry: &TimelineEntry, theme: Theme) -> Style {
    use EvidenceKind::*;
    let ended = |state| match state {
        ExecutionState::Failed | ExecutionState::Interrupted => Style::default().fg(theme.error),
        ExecutionState::Unknown => Style::default().fg(theme.warning),
        _ => Style::default().fg(theme.foreground),
    };
    match entry.evidence.kind {
        RequestCreated | RequestExpired | ExecutionUnknown => Style::default().fg(theme.warning),
        TurnFailed | TurnInterrupted => Style::default().fg(theme.error),
        ToolCompleted | ChildTerminal => ended(entry.execution_state),
        Output | StateChanged => theme.secondary(),
        _ => Style::default().fg(theme.foreground),
    }
}

fn active(state: ExecutionState) -> bool {
    matches!(
        state,
        ExecutionState::Starting | ExecutionState::Running | ExecutionState::Waiting
    )
}

fn has_exact_tool_detail(entry: &TimelineEntry, current: &CoreSnapshot) -> bool {
    let Some(item_id) = entry.item_id.clone() else {
        return false;
    };
    let locator = ToolDetailLocator {
        session_id: current.timeline.session_id.clone(),
        identity: entry.identity.clone(),
        item_id,
    };
    current.tool_details.get(&locator).is_some()
}

/// Whether a metadata query finds this entry. The English text always
/// matches, so a query finds the same rows in both languages; the Chinese
/// interface also matches the wording it shows.
fn metadata_matches(entry: &TimelineEntry, query: &str) -> bool {
    if metadata(entry).join("\n").contains(query) {
        return true;
    }
    lang::zh()
        && metadata_fields(entry)
            .iter()
            .map(|fields| field_shown_text(fields, " | "))
            .collect::<Vec<_>>()
            .join("\n")
            .contains(query)
}

/// The searchable metadata text of an entry, one row per fact group. Always
/// English, whatever the interface language.
fn metadata(entry: &TimelineEntry) -> Vec<String> {
    metadata_fields(entry)
        .iter()
        .map(|fields| field_text(fields, " | "))
        .collect()
}

/// Entry metadata as labelled facts; `metadata` is exactly their English text.
/// Kind, activity, category and source names are Core identifiers and stay as
/// they are; only labels and the small closed state sets get Chinese wording.
fn metadata_fields(entry: &TimelineEntry) -> Vec<Vec<Fact>> {
    let mut rows = vec![
        vec![
            Fact::new(
                "Evidence",
                format!("#{} {:?}", entry.evidence.id, entry.evidence.kind),
            )
            .zh("证据"),
            Fact::new("scope", format!("{:?}", entry.scope))
                .zh("范围")
                .zh_value(activity_scope_zh(entry.scope)),
            Fact::new("execution", format!("{:?}", entry.execution_state))
                .zh("执行")
                .zh_value(execution_zh(entry.execution_state))
                .tone(match entry.execution_state {
                    ExecutionState::Failed | ExecutionState::Interrupted => Tone::Error,
                    ExecutionState::Unknown => Tone::Warning,
                    _ => Tone::Value,
                }),
        ],
        vec![
            Fact::new("Activity", format!("{:?}", entry.activity_kind)).zh("活动"),
            Fact::new("tool", super::lang::show(&entry.tool_category)).zh("工具"),
            Fact::new("source", format!("{:?}", entry.evidence.source))
                .zh("来源")
                .tone(Tone::Quiet),
        ],
        vec![Fact::optional("Thread", entry.identity.thread_id.as_deref()).zh("线程")],
        vec![Fact::optional("Turn", entry.identity.turn_id.as_deref()).zh("轮次")],
        vec![
            Fact::new("Agent", entry.identity.agent_id.clone()).zh("Agent"),
            Fact::new("task", super::lang::show(&entry.identity.task_id)).zh("任务"),
            Fact::new("attempt", super::lang::show(&entry.identity.attempt_id)).zh("尝试"),
            Fact::new("generation", super::lang::show(&entry.identity.generation)).zh("代次"),
        ],
        vec![
            Fact::optional("Item", entry.item_id.as_deref()).zh("条目"),
            Fact::new("request", super::lang::show(&entry.evidence.request_id)).zh("请求"),
        ],
        vec![
            Fact::new("Interaction", super::lang::show(&entry.interaction_state)).zh("交互"),
            Fact::new(
                "recorded wall ms",
                super::lang::show(&entry.evidence.recorded_at_ms),
            )
            .zh("记录时间（ms）"),
            Fact::new("output bytes", entry.evidence.output_bytes.to_string()).zh("输出字节"),
        ],
    ];
    if let Some(request) = &entry.request {
        rows.push(vec![Fact::new(
            "Request delivery",
            format!(
                "{:?} / {} / {} / received {}",
                request.id, request.thread_id, request.turn_id, request.received_seq
            ),
        )
        .zh("请求投递")
        .zh_value(format!(
            "{:?} / {} / {} / 接收序号 {}",
            request.id, request.thread_id, request.turn_id, request.received_seq
        ))]);
    }
    if entry.tool_category == Some(crate::protocol::ToolCategory::Compaction)
        || entry.compaction.is_some()
    {
        // These rows are built as `Label: value` text by the activity view; a
        // Chinese row uses the full-width colon instead.
        rows.extend(
            super::compaction_fact_rows(entry.compaction.as_deref())
                .into_iter()
                .map(|row| match row.split_once(": ") {
                    Some((label, value)) => vec![Fact::new(label, value)],
                    None => match row.split_once('：') {
                        Some((label, value)) => vec![Fact::new(label, value).zh(label)],
                        None => vec![Fact::new("", row)],
                    },
                }),
        );
    }
    for target in &entry.wait_targets {
        rows.push(vec![Fact::new(
            "Wait target",
            format!(
                "{} / turn {} / generation {} / {:?}",
                target.id,
                target.turn_id.as_deref().unwrap_or("unbound"),
                target.generation,
                target.outcome
            ),
        )
        .zh("等待目标")
        .zh_value(format!(
            "{} / 轮次 {} / 代次 {} / {:?}",
            target.id,
            target.turn_id.as_deref().unwrap_or("未绑定"),
            target.generation,
            target.outcome
        ))]);
    }
    rows
}

/// The first rows of `HELP` are `keys: action`; the rest are notes.
const HELP_KEY_ROWS: usize = 10;

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

/// Same rows as `HELP`; key rows read `keys：action`.
const HELP_ZH: &[&str] = &[
    "Up/Down 或 j/k、n/N：上一条/下一条证据",
    "Home/End：第一条/最后一条匹配；PgUp/Dn：滚动详情/帮助",
    "Tab：线程/子树/路径/全部（仅已确认的关系）",
    "F6：事件类别；F7：事件发生时的执行状态",
    "F8：只看关联当前待处理请求的证据",
    "/ 或 Ctrl+F：元数据文本；Ctrl+T：精确轮次 ID；Enter 应用",
    "b：添加/移除书签；B：只看书签（筛选仍然生效）",
    "Enter：定位当前请求或已保留的 Agent 消息；不会执行任何操作",
    "Esc：关闭；F2：请求；F3：Agent；F4：任务；F11：活动",
    "Ctrl+C：中断根 Agent；Ctrl+Q：退出并清理",
    "接收顺序就是证据顺序，不是 RPC 到达顺序，也不是 journal 游标",
    "状态定格在事件发生时；当前状态见顶部",
    "仅元数据：不含提示词、保密内容、原始输出和历史对话",
    "被清出的书签只保留元数据，不能再打开对应内容",
    "书签只存在于本 TUI，不会持久化、导出或恢复",
    "实时存档：512 条 / 256 KiB；书签：64 条 / 64 KiB",
];

#[cfg(test)]
mod tests;
