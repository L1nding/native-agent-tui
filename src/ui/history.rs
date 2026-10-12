//! Local history navigation; every operation goes through the read-only service.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::activity::{
    age, attention_word, execution_word, hint_row, label_mark, selected_line, shown_value, tone,
    word_mark, wrap_marked,
};
use super::lang::{self, tr};
use super::layout::wrap;
use super::theme;
use super::Editor;
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
    search_cursor: usize,
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
        self.search_cursor = self.search_query.len();
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
            self.notice = Some(
                tr(
                    "Wait for the current history operation.",
                    "请等待当前历史操作完成。",
                )
                .into(),
            );
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
                self.notice = Some(if lang::zh() {
                    format!("导出已保存：{bytes} 字节。原始身份已替换为稳定别名。")
                } else {
                    format!("Export saved: {bytes} bytes. Original identities were replaced by stable aliases.")
                });
                self.form = None;
                self.preview = None;
            }
            // 概览由会话页请求和处理。
            Ok(HistoryResult::Overview(_)) => {}
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
        let sessions = if let Some(view) = &self.view {
            vec![view.info.session_id.clone()]
        } else {
            self.sessions
                .iter()
                .map(|session| session.session_id.clone())
                .collect::<Vec<_>>()
        };
        if sessions.is_empty() {
            self.notice = Some(
                tr(
                    "No retained sessions are available to search.",
                    "没有可搜索的已保留会话。",
                )
                .into(),
            );
            return;
        }
        let query = Query {
            text: self.search_query.clone(),
            category: self.search_category,
            ..Default::default()
        };
        match service.search.submit_sessions(sessions, query) {
            Ok(id) => {
                self.search_pending = Some(id);
                self.search_results = None;
                self.search_selected = 0;
                self.notice = None;
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
    }

    fn search_insert(&mut self, character: char) {
        if self.search_query.len() + character.len_utf8() > FIELD_BYTES {
            self.notice = Some(if lang::zh() {
                format!("历史搜索内容最多 {FIELD_BYTES} 个 UTF-8 字节。")
            } else {
                format!("Historical search field limited to {FIELD_BYTES} UTF-8 bytes.")
            });
            return;
        }
        self.search_query.insert(self.search_cursor, character);
        self.search_cursor += character.len_utf8();
    }

    fn search_left(&mut self) {
        self.search_cursor = self.search_query[..self.search_cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index);
    }

    fn search_right(&mut self) {
        if let Some(grapheme) = self.search_query[self.search_cursor..]
            .graphemes(true)
            .next()
        {
            self.search_cursor += grapheme.len();
        }
    }

    fn search_backspace(&mut self) {
        let end = self.search_cursor;
        self.search_left();
        self.search_query.drain(self.search_cursor..end);
    }

    fn search_delete(&mut self) {
        if let Some(grapheme) = self.search_query[self.search_cursor..]
            .graphemes(true)
            .next()
        {
            let end = self.search_cursor + grapheme.len();
            self.search_query.drain(self.search_cursor..end);
        }
    }

    pub fn paste(&mut self, text: &str) {
        if self.search_editing {
            for character in text.chars().filter(|character| !character.is_control()) {
                self.search_insert(character);
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
            self.notice = Some(
                tr(
                    "Wait for the current operation; F12 returns to live execution.",
                    "请等待当前操作完成；F12 返回实时执行。",
                )
                .into(),
            );
            return false;
        }
        if key.code == KeyCode::Esc {
            if self.search_editing {
                self.search_editing = false;
                service.search.cancel();
                self.search_pending = None;
            } else if self.form.is_some() || self.preview.is_some() {
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

        if self.search_editing
            || key.code == KeyCode::Char('/')
            || (control && key.code == KeyCode::Char('f'))
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
                KeyCode::Char('u') if control => {
                    self.search_query.clear();
                    self.search_cursor = 0;
                }
                KeyCode::Left => self.search_left(),
                KeyCode::Right => self.search_right(),
                KeyCode::Home => self.search_cursor = 0,
                KeyCode::End => self.search_cursor = self.search_query.len(),
                KeyCode::Backspace => self.search_backspace(),
                KeyCode::Delete => self.search_delete(),
                KeyCode::Char(character)
                    if !control && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.search_insert(character);
                }
                _ => {}
            }
            return false;
        }

        if key.code == KeyCode::F(1) {
            self.notice = Some(
                tr(
                    "/ or Ctrl+F search metadata | F6 category | Up/Down hit | Enter locate | Esc closes",
                    "/ 或 Ctrl+F 搜索元数据 | F6 切换类别 | Up/Down 选择命中 | Enter 定位 | Esc 关闭",
                )
                .into(),
            );
            return false;
        }
        if key.code == KeyCode::F(6) {
            self.search_category = self.search_category.next();
            self.submit_search(service);
            return false;
        }
        if let Some(results) = &self.search_results {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.search_selected = self.search_selected.saturating_sub(1);
                    return false;
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('n') => {
                    self.search_selected =
                        (self.search_selected + 1).min(results.hits.len().saturating_sub(1));
                    return false;
                }
                KeyCode::Enter => {
                    if let Some(hit) = results.hits.get(self.search_selected) {
                        self.submit(
                            service,
                            HistoryRequest::Open {
                                session: hit.session_id.clone(),
                                sequence: Some(hit.event_seq),
                            },
                        );
                    } else {
                        self.notice = Some(
                            tr(
                                "No retained evidence hit is selected.",
                                "没有选中的证据命中。",
                            )
                            .into(),
                        );
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
                                self.notice = Some(
                                    tr(
                                        "Enter an unsigned event sequence.",
                                        "请输入非负整数的事件序号。",
                                    )
                                    .into(),
                                );
                                return false;
                            };
                            if sequence > view.info.committed_seq {
                                self.notice = Some(format!(
                                    "{}0..={}",
                                    tr("Available event range: ", "可用事件范围："),
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
                                self.notice = Some(
                                    tr("Choose a new output file.", "请指定一个新的输出文件。")
                                        .into(),
                                );
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
            KeyCode::Char('c' | 'y' | 'n') if control => self.notice = Some(tr("History is read-only. Return to live execution to control the current session.", "历史为只读。要控制当前会话，请返回实时执行。").into()),
            KeyCode::F(7) | KeyCode::F(8) | KeyCode::F(9) => self.notice = Some(tr("Historical tasks cannot resume. Their prompt was not retained; start a new task explicitly. Prior side effects may already have occurred.", "历史任务无法恢复执行。原始提示词没有保留；请明确新建任务。之前的副作用可能已经发生。").into()),
            _ => {}
        }
        false
    }

    pub fn draw(&self, frame: &mut ratatui::Frame<'_>, live: Option<&CoreSnapshot>) {
        let area = frame.area();
        let theme = theme::current();
        // 历史视图独占整屏，自己铺底色，避免残留上一屏的背景。
        frame.render_widget(Block::default().style(theme.base()), area);
        if area.height < 8 || area.width < 24 {
            frame.render_widget(
                Paragraph::new(tr(
                    "History: read-only\nResize terminal\nCtrl+Q quit",
                    "历史：只读\n请放大终端窗口\nCtrl+Q 退出",
                )),
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
        // 只读是这个视图最重要的事实：文字保留，用警示色加粗而不是只靠颜色。
        let mut header = vec![Line::from(vec![
            Span::styled(tr("HISTORY", "历史"), theme.heading()),
            Span::styled(" | ", theme.faint()),
            Span::styled(tr("read-only", "只读"), tone(theme.warning)),
        ])];
        header.extend(if let Some(live) = live {
            let phase = lang::phase(live.phase);
            wrap_marked(
                &format!(
                    "{}: {phase} | {}: {} | {}",
                    tr("Live", "实时"),
                    tr("requests", "请求"),
                    live.requests.len(),
                    tr("F12 return", "F12 返回")
                ),
                usize::MAX,
                Style::default(),
                |clean| {
                    label_mark(clean)
                        .into_iter()
                        .chain(word_mark(clean, &phase, theme.phase(live.phase)))
                        .collect()
                },
            )
        } else {
            vec![Line::from(Span::styled(
                tr("Offline | no Codex execution", "离线 | 没有 Codex 执行"),
                theme.secondary(),
            ))]
        });
        header.push(Line::from(Span::styled(
            tr(
                "Recorded ages are frozen. Missing prompts, text and answers are unavailable.",
                "记录的时长已冻结。缺失的提示词、文本和回答不可用。",
            ),
            theme.secondary(),
        )));
        frame.render_widget(
            Paragraph::new(header).block(theme::panel().borders(Borders::BOTTOM)),
            chunks[0],
        );
        let mut rows = Vec::new();
        // 行文字不变；下面这些下标只用来决定每行的样式。
        let mut session_rows = Vec::new();
        let mut excerpt_row = None;
        let notice_rows = usize::from(self.notice.is_some());
        if let Some(notice) = &self.notice {
            rows.push(notice.clone());
        }
        let zh = lang::zh();
        // 这几个词同时用于行文字和下面的着色匹配，两处必须是同一份。
        let preview_head = tr("EXPORT PREVIEW", "导出预览");
        let review = tr("REVIEW REQUIRED", "需要复核");
        let this_session = tr("this session", "当前会话");
        let never_searched = tr(
            "prompts, answers, secrets, commands and raw output are never searched.",
            "提示词、回答、密钥、命令和原始输出不会被搜索。",
        );
        let no_match = if zh {
            format!("没有匹配的已保留元数据。{never_searched}")
        } else {
            "No retained metadata matched this query. Prompts, answers, secrets, commands and raw output are never searched.".to_owned()
        };
        if let Some(preview) = &self.preview {
            rows.push(format!(
                "{preview_head} | {}",
                tr(
                    "stable identity aliases; no reverse mapping",
                    "身份使用稳定别名；不保留反向映射"
                )
            ));
            rows.push(format!(
                "{} {} {} {} | {}: {}",
                tr("Selected range: baseline", "所选范围: 从基线"),
                preview.manifest["since"],
                tr("through committed", "到已提交"),
                preview.manifest["high_watermark"],
                tr("historical result", "历史结果"),
                preview.manifest["execution_result"]
            ));
            rows.push(
                tr(
                    "Only retained state/evidence is exported. Existing files are never replaced.",
                    "只导出已保留的状态和证据。不会覆盖已有文件。",
                )
                .into(),
            );
            rows.push(tr("Preview excerpt (up to 8 KiB):", "预览片段（最多 8 KiB）:").into());
            excerpt_row = Some(rows.len());
            rows.push(preview.excerpt.clone());
        } else if let Some(view) = &self.view {
            let result = lang::phase(view.info.execution_result.unwrap_or(SessionPhase::Unknown));
            rows.push(format!(
                "{}: {result} | {}",
                tr("Result", "结果"),
                if view.info.needs_recovery {
                    review
                } else {
                    tr("closed history", "已关闭的历史")
                }
            ));
            rows.push(format!(
                "{} {} | {} {} / {} | schema {}",
                tr("Session", "会话"),
                view.info.session_id,
                tr("selected event", "当前事件"),
                view.selected.event_seq,
                view.info.committed_seq,
                view.info.schema_version
            ));
            rows.push(format!(
                "{}: {result} | {}: {} | {}: {} | {}: {}",
                tr("Latest recorded result", "最近记录的结果"),
                tr("closed", "已关闭"),
                flag(view.info.session_closed),
                tr("needs review", "待复核"),
                flag(view.info.needs_recovery),
                tr("uncommitted tail", "未提交尾部"),
                flag(view.uncommitted_tail)
            ));
            rows.push(self.search_row());
            if let Some(results) = &self.search_results {
                let retained = results.hits.len() as u64;
                let omitted = results.total.saturating_sub(retained);
                rows.push(format!(
                    "{} {} | {} | {} {}/{} | {} {} | {} {}",
                    tr(
                        "Search scope: committed journal prefix through event",
                        "搜索范围: 已提交日志前缀，截至事件"
                    ),
                    results.info.committed_seq,
                    tr("uncommitted tail excluded", "不含未提交尾部"),
                    tr("hits", "命中"),
                    retained,
                    results.total,
                    tr("omitted", "省略"),
                    omitted,
                    tr("deduplicated evidence omitted", "去重后省略的证据"),
                    results.omitted_evidence
                ));
                if let Some(hit) = results.hits.get(self.search_selected) {
                    rows.push(format!(
                        "{} {}/{}: {} {} | {}",
                        tr("Selected hit", "当前命中"),
                        self.search_selected + 1,
                        results.hits.len(),
                        tr("event", "事件"),
                        hit.event_seq,
                        hit.metadata()
                    ));
                } else if results.total == 0 {
                    rows.push(no_match.clone());
                }
            } else if self.search_pending.is_some() {
                rows.push(format!(
                    "{}{never_searched}",
                    tr(
                        "Searching retained metadata… uncommitted tail excluded; ",
                        "正在搜索已保留的元数据… 不含未提交尾部；"
                    )
                ));
            } else {
                rows.push(format!(
                    "{}{never_searched}",
                    tr(
                        "Search scope: fixed committed journal prefix; ",
                        "搜索范围: 固定的已提交日志前缀；"
                    )
                ));
            }
            if let Ok(state) = view.selected.state() {
                rows.push(format!(
                    "{}: {} | {}: {:?} | {}: {:?}",
                    tr("Selected recorded phase", "当前记录的阶段"),
                    lang::phase(state.phase),
                    tr("cleanup confirmed", "清理已确认"),
                    state.cleanup_confirmed,
                    tr("issue", "问题"),
                    state.issue
                ));
                if let Some(usage) = state.usage {
                    rows.push(format!(
                        "{} {:?} | {} {} | {} {} | {} {} | {} {} | {} {} | {} {}",
                        tr("Recorded usage: source", "记录的用量: 来源"),
                        usage.source,
                        tr("total", "总计"),
                        shown_value(usage.total_tokens),
                        tr("input", "输入"),
                        shown_value(usage.input_tokens),
                        tr("cached", "缓存"),
                        shown_value(usage.cached_input_tokens),
                        tr("output", "输出"),
                        shown_value(usage.output_tokens),
                        tr("reasoning", "推理"),
                        shown_value(usage.reasoning_tokens),
                        tr("context", "上下文"),
                        shown_value(usage.context_window),
                    ));
                }
                if let Some(budget) = state.token_budget {
                    rows.push(format!(
                        "{}: {} / {} | {}: {} | {}: {} | {}: {}",
                        tr("Recorded token budget", "记录的 token 预算"),
                        shown_value(budget.confirmed_total_tokens),
                        shown_value(budget.limit),
                        tr("complete", "完整"),
                        flag(budget.confirmed_complete),
                        tr("stop triggered", "已触发停止"),
                        flag(budget.stop_triggered),
                        tr("per-agent", "单 Agent"),
                        budget.per_agent_limit.map_or_else(
                            || tr("not set", "未设置").to_owned(),
                            |limit| {
                                if zh {
                                    format!(
                                        "上限 {limit}，已触发停止: {}",
                                        flag(budget.per_agent_stop_triggered)
                                    )
                                } else {
                                    format!(
                                        "limit {limit}, stop triggered: {}",
                                        budget.per_agent_stop_triggered
                                    )
                                }
                            },
                        ),
                    ));
                }
                if let Some(skills) = state.skills {
                    rows.push(format!(
                        "{}: {}",
                        tr("Recorded skills inventory", "记录的技能清单"),
                        skills.brief()
                    ));
                    rows.push(
                        tr(
                            "Directory metadata only; loaded/invoked/completed/failed status is unknown.",
                            "仅有目录元数据；是否已加载、调用、完成或失败均未知。",
                        )
                        .into(),
                    );
                } else {
                    rows.push(
                        tr(
                            "Recorded skills inventory: not queried or unavailable in this older journal.",
                            "记录的技能清单: 这份旧日志中未查询或不可用。",
                        )
                        .into(),
                    );
                }
                if view.info.needs_recovery {
                    rows.push(
                        tr(
                            "Unconfirmed external outcomes remain unknown. Inspect evidence before a new task; prior side effects may already have occurred.",
                            "未确认的外部结果仍然未知。新建任务前请先查看证据；之前的副作用可能已经发生。",
                        )
                        .into(),
                    );
                }
                let retained_compactions = state
                    .observation
                    .activities
                    .iter()
                    .filter(|activity| {
                        activity.tool_category == Some(crate::protocol::ToolCategory::Compaction)
                    })
                    .count();
                rows.push(if zh {
                    format!("保留的压缩记录: {retained_compactions} | 累计总数不可用")
                } else {
                    format!(
                        "Compactions retained: {retained_compactions} | lifetime total unavailable"
                    )
                });
                for task in &state.tasks {
                    rows.push(format!(
                        "{} {} / {} {} | {:?} | {} {:?} | {} {:?}",
                        tr("Task", "任务"),
                        task.id.0,
                        tr("attempt", "尝试"),
                        task.attempt,
                        task.kind,
                        tr("recorded", "记录状态"),
                        task.state,
                        tr("dependencies", "依赖"),
                        task.dependencies
                    ));
                }
                for activity in &state.observation.activities {
                    rows.push(format!(
                        "{} | {:?} / {:?} | {} | {} {} | {} {} | {} {:?}",
                        activity.identity.agent_id,
                        activity.scope,
                        activity.kind,
                        execution_word(activity.execution_state),
                        tr("historical quiet", "历史静默"),
                        age(activity.silence_ms),
                        tr("progress", "进度"),
                        activity.progress_seq,
                        tr("freshness", "新鲜度"),
                        activity.freshness
                    ));
                    if activity.tool_category == Some(crate::protocol::ToolCategory::Compaction) {
                        rows.push(format!(
                            "  {} {} | {} {:?} | {} {:?} | {}",
                            tr("Compaction", "压缩"),
                            execution_word(activity.execution_state),
                            tr("source", "来源"),
                            activity
                                .last_evidence
                                .as_ref()
                                .map(|evidence| evidence.source),
                            tr("item", "条目"),
                            activity.item_id,
                            tr(
                                "before/after usage, reason and summary unavailable",
                                "压缩前后用量、原因和摘要不可用"
                            )
                        ));
                    }
                    rows.push(format!(
                        "  {} {:?} {} {:?} {} {:?} {} {:?} {} {:?}",
                        tr("thread", "线程"),
                        activity.identity.thread_id,
                        tr("turn", "轮次"),
                        activity.identity.turn_id,
                        tr("generation", "代次"),
                        activity.identity.generation,
                        tr("item", "条目"),
                        activity.item_id,
                        tr("request", "请求"),
                        activity.request_id
                    ));
                    rows.push(format!(
                        "  {} {} / {:?} | {} {:?} | {} {:?}",
                        tr("attention", "提醒"),
                        attention_word(activity.attention.level),
                        activity.attention.reason,
                        tr("source", "来源"),
                        activity.attention.config_source,
                        tr("last evidence", "最近证据"),
                        activity.last_evidence
                    ));
                    if activity.request_id.is_some() {
                        rows.push(
                            tr(
                                "  Historical request: unavailable; answers cannot be sent.",
                                "  历史请求: 不可用；无法发送回答。",
                            )
                            .into(),
                        );
                    }
                    if !activity.wait_targets.is_empty() {
                        rows.push(format!(
                            "  {} {:?} -> {:?} | {} {:?}",
                            tr("Waiting", "等待"),
                            activity.wait_reason,
                            activity.wait_targets,
                            tr("resume", "恢复条件"),
                            activity.resume_condition
                        ));
                    }
                }
            }
        } else if self.sessions.is_empty() {
            rows.push(
                tr(
                    "No retained sessions in this workspace. r refresh | Esc return",
                    "这个工作区没有已保留的会话。r 刷新 | Esc 返回",
                )
                .into(),
            );
        } else {
            if self.search_editing || self.search_pending.is_some() || self.search_results.is_some()
            {
                rows.push(self.search_row());
                if let Some(results) = &self.search_results {
                    let retained = results.hits.len() as u64;
                    rows.push(format!(
                        "{} {} {} | {} | {} {}/{} | {} {} | {} {}",
                        tr("Search scope:", "搜索范围:"),
                        results.sessions.len(),
                        tr("retained sessions", "个已保留会话"),
                        tr("committed prefixes fixed", "已提交前缀固定"),
                        tr("hits", "命中"),
                        retained,
                        results.total,
                        tr("omitted", "省略"),
                        results.total.saturating_sub(retained),
                        tr("deduplicated evidence omitted", "去重后省略的证据"),
                        results.omitted_evidence
                    ));
                    if let Some(hit) = results.hits.get(self.search_selected) {
                        rows.push(format!(
                            "{} {}/{}: {} {} | {} {} | {}",
                            tr("Selected hit", "当前命中"),
                            self.search_selected + 1,
                            results.hits.len(),
                            tr("session", "会话"),
                            hit.session_id,
                            tr("event", "事件"),
                            hit.event_seq,
                            hit.metadata()
                        ));
                    } else if results.total == 0 {
                        rows.push(no_match.clone());
                    }
                } else if self.search_pending.is_some() {
                    rows.push(
                        tr(
                            "Searching retained metadata across sessions; uncommitted tails excluded.",
                            "正在跨会话搜索已保留的元数据；不含未提交尾部。",
                        )
                        .into(),
                    );
                }
            }
            rows.push(
                tr(
                    "Select a retained session. Enter opens its latest committed evidence.",
                    "选择一个已保留的会话。Enter 打开它最新的已提交证据。",
                )
                .into(),
            );
            // 当前会话尚未关闭，不能显示为需要复查；标为实时会话并给出实时阶段。
            let live_session = live
                .and_then(|live| live.journal.as_ref())
                .map(|journal| journal.session_id.as_str());
            for (index, session) in self.sessions.iter().enumerate() {
                let marker = if index == self.selected { ">" } else { " " };
                session_rows.push((rows.len(), index == self.selected));
                if live_session == Some(session.session_id.as_str()) {
                    let phase = lang::phase(live.map_or(SessionPhase::Unknown, |live| live.phase));
                    rows.push(if zh {
                        format!(
                            "{marker} {} | {this_session}（实时：{phase}） | 事件 {}",
                            session.session_id, session.committed_seq,
                        )
                    } else {
                        format!(
                            "{marker} {} | {this_session} (live: {phase}) | event {}",
                            session.session_id, session.committed_seq,
                        )
                    });
                    continue;
                }
                rows.push(format!(
                    "{marker} {} | {} | {} {} | {}",
                    session.session_id,
                    lang::phase(session.execution_result.unwrap_or(SessionPhase::Unknown)),
                    tr("event", "事件"),
                    session.committed_seq,
                    if session.needs_recovery {
                        review
                    } else {
                        tr("closed", "已关闭")
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
            .enumerate()
            .flat_map(|(index, row)| {
                if index < notice_rows {
                    return wrap_marked(row, width, tone(theme.warning), |_| Vec::new());
                }
                if excerpt_row == Some(index) {
                    return wrap_marked(row, width, Style::default(), |_| Vec::new());
                }
                if session_rows.contains(&(index, true)) {
                    return wrap(&display_text(row), width)
                        .into_iter()
                        .map(|piece| selected_line(vec![Span::raw(piece)], width))
                        .collect();
                }
                let session = session_rows.contains(&(index, false));
                let base = if row.starts_with(' ') && !session {
                    theme.secondary()
                } else {
                    Style::default()
                };
                wrap_marked(row, width, base, |clean| {
                    let mut marks = Vec::new();
                    if session {
                        marks.extend(word_mark(clean, this_session, tone(theme.accent)));
                    } else if clean.starts_with(preview_head) {
                        marks.push((0..preview_head.len(), theme.heading()));
                    } else if !clean.starts_with(' ') {
                        marks.extend(label_mark(clean));
                    }
                    marks.extend(word_mark(clean, review, tone(theme.warning)));
                    marks
                })
            })
            .skip(selected_offset.saturating_add(self.scroll))
            .take(body_height)
            .collect();
        // 焦点跟着输入走：有表单或在编辑搜索时焦点在下方面板，否则在正文。
        let editing = self.form.is_some() || self.search_editing;
        let frame_for = |focused: bool| {
            if focused {
                theme::focus_panel()
            } else {
                theme::panel()
            }
        };
        frame.render_widget(
            Paragraph::new(rows).block(
                frame_for(!editing)
                    .borders(Borders::ALL)
                    .title(tr(" Historical observation ", " 历史观察 ")),
            ),
            chunks[1],
        );
        let (title, editor) = match &self.form {
            Some(Form::Sequence(editor)) => (
                tr(" Go to event · Enter opens ", " 跳转到事件 · Enter 打开 "),
                Some(editor),
            ),
            Some(Form::Range(editor)) => (
                tr(
                    " Export baseline · 0 = all · Enter previews ",
                    " 导出基线 · 0 = 全部 · Enter 预览 ",
                ),
                Some(editor),
            ),
            Some(Form::Destination(editor)) => (
                tr(
                    " NEW export file · Enter saves preview ",
                    " 新导出文件 · Enter 保存预览内容 ",
                ),
                Some(editor),
            ),
            None => (tr(" Actions ", " 操作 "), None),
        };
        let mut prompt = tr(
            "Return to live execution or use --run with a fresh task. Prior side effects may have occurred.",
            "返回实时执行，或用 --run 新建任务。之前的副作用可能已经发生。",
        )
        .to_owned();
        let mut input_cursor = None;
        if self.search_editing {
            let left = self.search_query[..self.search_cursor].to_owned();
            input_cursor = Some(left.width());
            prompt = format!(
                "{}: {left}{} | {}",
                tr("Search metadata", "搜索元数据"),
                &self.search_query[self.search_cursor..],
                tr("Enter runs, Esc cancels", "Enter 搜索，Esc 取消")
            );
        }
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
            tr(
                "Loading history / export; live execution continues.",
                "正在加载历史 / 导出；实时执行不受影响。",
            )
        } else {
            self.notice.as_deref().unwrap_or("")
        };
        // 第一行是输入或说明，换行符之后是提示；提示用警示色，说明用次要色。
        let prompt_style = if editing {
            Style::default()
        } else {
            theme.secondary()
        };
        frame.render_widget(
            Paragraph::new(wrap_marked(
                &format!("{prompt}\n{notice}"),
                chunks[2].width.saturating_sub(2) as usize,
                prompt_style,
                |clean| {
                    clean
                        .find('\n')
                        .map(|split| (split + 1..clean.len(), tone(theme.warning)))
                        .into_iter()
                        .collect()
                },
            ))
            .block(frame_for(editing).borders(Borders::ALL).title(title)),
            chunks[2],
        );
        if let Some(cursor) = input_cursor {
            frame.set_cursor_position((chunks[2].x + 1 + cursor as u16, chunks[2].y + 1));
        }
        // 只列当前状态下真正可用的键；放不下时从末尾开始省略。
        let hints = if self.search_editing {
            tr(
                "Enter search | Ctrl+U clear | Esc cancel",
                "Enter 搜索 | Ctrl+U 清空 | Esc 取消",
            )
        } else if self.form.is_some() {
            tr(
                "Enter confirm | Ctrl+U clear | PgUp/PgDn scroll | Esc cancel",
                "Enter 确认 | Ctrl+U 清空 | PgUp/PgDn 滚动 | Esc 取消",
            )
        } else if self.view.is_some() {
            tr(
                "Left/Right event | g sequence | e export | / search | PgUp/PgDn scroll | Esc back | r refresh | Ctrl+Q quit",
                "Left/Right 事件 | g 序号 | e 导出 | / 搜索 | PgUp/PgDn 滚动 | Esc 返回 | r 刷新 | Ctrl+Q 退出",
            )
        } else {
            tr(
                "Up/Down select | Enter open | / search | r refresh | Esc back | Ctrl+Q quit",
                "Up/Down 选择 | Enter 打开 | / 搜索 | r 刷新 | Esc 返回 | Ctrl+Q 退出",
            )
        };
        frame.render_widget(
            Paragraph::new(hint_row(hints, chunks[3].width as usize)),
            chunks[3],
        );
    }
}

impl HistoryPanel {
    /// 搜索条件行；会话列表和单个会话视图共用。
    fn search_row(&self) -> String {
        format!(
            "{}: {:?} | {} {} | {}",
            tr("Search metadata", "搜索元数据"),
            self.search_query,
            tr("category", "类别"),
            category_word(self.search_category),
            if self.search_editing {
                tr("editing; Enter runs search", "编辑中；Enter 开始搜索")
            } else {
                tr(
                    "/ or Ctrl+F edit; F6 changes category",
                    "/ 或 Ctrl+F 编辑；F6 切换类别",
                )
            }
        )
    }
}

/// 搜索类别；英文沿用历史搜索自己的标签。
fn category_word(category: Category) -> &'static str {
    if !lang::zh() {
        return category.label();
    }
    match category {
        Category::All => "全部",
        Category::Lifecycle => "生命周期",
        Category::Output => "输出",
        Category::Tool => "工具",
        Category::Compaction => "压缩",
        Category::Request => "请求",
        Category::Waiting => "等待",
    }
}

/// 布尔事实；英文保持 `true`/`false`。
fn flag(value: bool) -> String {
    if lang::zh() {
        if value { "是" } else { "否" }.to_owned()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests;
