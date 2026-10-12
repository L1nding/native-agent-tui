use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::activity::{clip_spans, footer, selected_line, tone, truncate_display_label};
use super::lang::{tr, zh};
use super::theme;

const QUERY_BYTES: usize = 32 * 1024;
/// Narrower palettes drop the description column and keep label and key.
const DETAIL_MIN_WIDTH: usize = 56;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaletteAction {
    Search,
    Timeline,
    Context,
    Skills,
    Requests,
    Workflow,
    Evidence,
    Attention,
    Help,
    NextAgent,
    Model,
}

#[derive(Clone, Copy)]
struct CommandItem {
    /// Heading shown above the first item of a run; display only.
    group: &'static str,
    group_zh: &'static str,
    label: &'static str,
    label_zh: &'static str,
    /// One-line description; display only in English. The Chinese interface
    /// also filters on `detail_zh`, so a word from the description finds the command.
    detail: &'static str,
    detail_zh: &'static str,
    hint: &'static str,
    keywords: &'static str,
    /// Extra Chinese words people may type; filtering only.
    keywords_zh: &'static str,
    action: PaletteAction,
}

impl CommandItem {
    fn shown_group(&self) -> &'static str {
        tr(self.group, self.group_zh)
    }

    fn shown_label(&self) -> &'static str {
        tr(self.label, self.label_zh)
    }

    fn shown_detail(&self) -> &'static str {
        tr(self.detail, self.detail_zh)
    }

    /// Whether the lowercased, trimmed `query` selects this command.
    /// English filtering is the English name, key and keywords only.
    fn matches(&self, query: &str) -> bool {
        let english = format!("{} {} {}", self.label, self.hint, self.keywords).to_lowercase();
        if english.contains(query) {
            return true;
        }
        if !zh() {
            return false;
        }
        let chinese =
            format!("{} {} {}", self.label_zh, self.detail_zh, self.keywords_zh).to_lowercase();
        // 中文里的空格只是排版；“下一个agent”和“下一个 Agent”应当等价。
        let squeeze = |text: &str| text.split_whitespace().collect::<String>();
        let query = squeeze(query);
        chinese.contains(query.as_str())
            || squeeze(&chinese).contains(query.as_str())
            || squeeze(&english).contains(query.as_str())
    }
}

const ITEMS: &[CommandItem] = &[
    CommandItem {
        group: "Inspect",
        group_zh: "查看",
        label: "Search conversation",
        label_zh: "搜索对话",
        detail: "Find text in retained messages",
        detail_zh: "在保留的消息里查找文字",
        hint: "Ctrl+F",
        keywords: "search conversation retained messages text",
        keywords_zh: "查找 消息 文字",
        action: PaletteAction::Search,
    },
    CommandItem {
        group: "Inspect",
        group_zh: "查看",
        label: "Timeline",
        label_zh: "时间线",
        detail: "Evidence events in order",
        detail_zh: "按顺序查看证据事件",
        hint: "Ctrl+T",
        keywords: "timeline evidence live events",
        keywords_zh: "证据 事件 实时",
        action: PaletteAction::Timeline,
    },
    CommandItem {
        group: "Inspect",
        group_zh: "查看",
        label: "Context",
        label_zh: "上下文",
        detail: "Usage, budget and compaction",
        detail_zh: "用量、预算与压缩",
        hint: "Ctrl+G",
        keywords: "context usage compaction budget",
        keywords_zh: "用量 压缩 预算 tokens",
        action: PaletteAction::Context,
    },
    CommandItem {
        group: "Inspect",
        group_zh: "查看",
        label: "Skills inventory",
        label_zh: "技能清单",
        detail: "Skills listed for this directory",
        detail_zh: "当前目录列出的技能",
        hint: "Ctrl+K",
        keywords: "skills inventory",
        keywords_zh: "技能 列表",
        action: PaletteAction::Skills,
    },
    CommandItem {
        group: "Tasks",
        group_zh: "任务",
        label: "Requests",
        label_zh: "请求",
        detail: "Approvals and questions waiting on you",
        detail_zh: "等你处理的审批和提问",
        hint: "F2",
        keywords: "requests approval input",
        keywords_zh: "审批 提问 输入",
        action: PaletteAction::Requests,
    },
    CommandItem {
        group: "Tasks",
        group_zh: "任务",
        label: "Workflow",
        label_zh: "工作流",
        detail: "Scheduler tasks and dependencies",
        detail_zh: "调度任务与依赖",
        hint: "F4",
        keywords: "workflow tasks scheduler",
        keywords_zh: "任务 调度 依赖",
        action: PaletteAction::Workflow,
    },
    CommandItem {
        group: "Activity",
        group_zh: "活动",
        label: "Evidence",
        label_zh: "证据",
        detail: "Activity and attention evidence",
        detail_zh: "活动与提醒的证据",
        hint: "F11",
        keywords: "evidence activity attention",
        keywords_zh: "活动 提醒",
        action: PaletteAction::Evidence,
    },
    CommandItem {
        group: "Activity",
        group_zh: "活动",
        label: "Attention thresholds",
        label_zh: "提醒阈值",
        detail: "Quiet and attention reminder timing",
        detail_zh: "静默与提醒的触发时间",
        hint: "F10",
        keywords: "attention thresholds silence reminders",
        keywords_zh: "静默 提醒 阈值",
        action: PaletteAction::Attention,
    },
    CommandItem {
        group: "Session",
        group_zh: "会话",
        label: "Help",
        label_zh: "帮助",
        detail: "Keyboard shortcuts",
        detail_zh: "快捷键一览",
        hint: "F1",
        keywords: "help shortcuts keyboard",
        keywords_zh: "快捷键 键盘 按键",
        action: PaletteAction::Help,
    },
    CommandItem {
        group: "Session",
        group_zh: "会话",
        label: "Switch next agent",
        label_zh: "切换到下一个 Agent",
        detail: "Show the next agent's conversation",
        detail_zh: "查看下一个 Agent 的对话",
        hint: "F3",
        keywords: "agent child thread switch",
        keywords_zh: "子 Agent 线程 切换",
        action: PaletteAction::NextAgent,
    },
    CommandItem {
        group: "Session",
        group_zh: "会话",
        label: "Switch model",
        label_zh: "切换模型",
        detail: "Pick the model for the next turn",
        detail_zh: "选择下一轮使用的模型",
        hint: "/model",
        keywords: "model switch select provider",
        keywords_zh: "模型 选择 切换",
        action: PaletteAction::Model,
    },
];

/// A `/name` line typed in the task editor; it is never sent to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Slash<'a> {
    Action(PaletteAction),
    /// `/model` opens the picker; `/model NAME` selects `NAME` directly.
    Model(Option<&'a str>),
    Tools,
    Settings,
    Quit,
    Unknown,
}

const SLASH_ACTIONS: &[(&str, PaletteAction)] = &[
    ("search", PaletteAction::Search),
    ("timeline", PaletteAction::Timeline),
    ("context", PaletteAction::Context),
    ("skills", PaletteAction::Skills),
    ("requests", PaletteAction::Requests),
    ("workflow", PaletteAction::Workflow),
    ("evidence", PaletteAction::Evidence),
    ("attention", PaletteAction::Attention),
    ("help", PaletteAction::Help),
    ("agent", PaletteAction::NextAgent),
];

/// One row of the hint list shown above the task input while a `/name` is typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SlashHint {
    pub name: &'static str,
    pub detail: &'static str,
}

/// Commands whose name starts with what follows the `/`; empty unless `text`
/// is a single `/word` still being typed.
pub(super) fn slash_hints(text: &str) -> Vec<SlashHint> {
    let Some(typed) = text.strip_prefix('/') else {
        return Vec::new();
    };
    if !typed.chars().all(|c| c.is_ascii_alphabetic()) {
        return Vec::new();
    }
    let typed = typed.to_ascii_lowercase();
    let detail = |action: PaletteAction| {
        ITEMS
            .iter()
            .find(|item| item.action == action)
            .map_or("", CommandItem::shown_detail)
    };
    SLASH_ACTIONS
        .iter()
        .map(|(name, action)| SlashHint {
            name,
            detail: detail(*action),
        })
        .chain([
            SlashHint {
                name: "model",
                detail: detail(PaletteAction::Model),
            },
            SlashHint {
                name: "setting",
                detail: tr(
                    "Language, theme, sandbox, approval, reminders",
                    "语言、主题、沙箱、审批、提醒阈值",
                ),
            },
            SlashHint {
                name: "tools",
                detail: tr("Fold or unfold tool groups", "折叠或展开工具组"),
            },
            SlashHint {
                name: "quit",
                detail: tr("Quit", "退出"),
            },
        ])
        .filter(|hint| hint.name.starts_with(typed.as_str()))
        .collect()
}

/// Draws the hint list directly above `input`, growing upward.
pub(super) fn draw_slash_hints(
    frame: &mut ratatui::Frame<'_>,
    input: Rect,
    hints: &[SlashHint],
    selected: usize,
) {
    let rows = hints.len().min(input.y.saturating_sub(2) as usize);
    if rows == 0 || input.width < 12 {
        return;
    }
    let rect = Rect {
        x: input.x,
        y: input.y - rows as u16 - 2,
        width: input.width.min(72),
        height: rows as u16 + 2,
    };
    frame.render_widget(Clear, rect);
    let theme = theme::current();
    let block = theme::focus_panel()
        .borders(Borders::ALL)
        .title(tr(" Commands ", " 命令 "))
        .title_bottom(footer(
            tr(
                "↑/↓ select | Tab complete | Enter run",
                "↑/↓ 选择 | Tab 补全 | Enter 执行",
            ),
            rect.width,
        ));
    let width = block.inner(rect).width as usize;
    let selected = selected.min(hints.len() - 1);
    let start = (selected + 1).saturating_sub(rows);
    let name_width = hints
        .iter()
        .map(|hint| hint.name.width())
        .max()
        .unwrap_or(0)
        + 1;
    let lines: Vec<Line<'static>> = hints
        .iter()
        .enumerate()
        .skip(start)
        .take(rows)
        .map(|(index, hint)| {
            let marker = if index == selected { "> " } else { "  " };
            let name = format!("/{:<name_width$}", hint.name);
            let spans = vec![
                Span::raw(marker),
                Span::styled(name, theme.key()),
                Span::raw(" "),
                Span::styled(hint.detail, theme.secondary()),
            ];
            if index == selected {
                selected_line(spans, width)
            } else {
                Line::from(clip_spans(spans, width))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

/// Every command name, for the notice shown after an unknown one.
pub(super) fn slash_names() -> String {
    SLASH_ACTIONS
        .iter()
        .map(|(name, _)| *name)
        .chain(["model", "setting", "tools", "quit"])
        .map(|name| format!("/{name}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Only a single `/word` is a command, so a task that starts with a path is still a task.
pub(super) fn parse_slash(text: &str) -> Option<Slash<'_>> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, argument) = match rest.split_once(char::is_whitespace) {
        Some((name, argument)) => (name, argument.trim()),
        None => (rest, ""),
    };
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let name = name.to_ascii_lowercase();
    if name == "model" {
        return Some(Slash::Model((!argument.is_empty()).then_some(argument)));
    }
    if !argument.is_empty() {
        return None;
    }
    Some(match name.as_str() {
        "tools" => Slash::Tools,
        "setting" | "settings" => Slash::Settings,
        "quit" => Slash::Quit,
        name => SLASH_ACTIONS
            .iter()
            .find(|(known, _)| *known == name)
            .map_or(Slash::Unknown, |(_, action)| Slash::Action(*action)),
    })
}

enum PaletteRow {
    Group(&'static str),
    Item(usize),
}

/// One command row: marker, label, optional description, key column.
/// Every fixed part is subtracted from the width before the label is cut.
fn command_line(item: &CommandItem, selected: bool, width: usize) -> Line<'static> {
    let theme = theme::current();
    let marker = if selected { "> " } else { "  " };
    let key_column = ITEMS
        .iter()
        .map(|item| item.hint.width())
        .max()
        .unwrap_or(0);
    let label_width = ITEMS
        .iter()
        .map(|item| item.shown_label().width())
        .max()
        .unwrap_or(0);
    // 太窄时先放弃快捷键列，保住完整的命令名；所有行用同一判断，列才对得齐。
    let show_key = width >= marker.width() + label_width + key_column + 2;
    let reserved = marker.width() + if show_key { key_column + 2 } else { 0 };
    let label = truncate_display_label(item.shown_label(), width.saturating_sub(reserved).max(1));
    let mut used = marker.width() + label.width();
    let mut spans = vec![Span::raw(marker), Span::raw(label)];
    let pad = |spans: &mut Vec<Span<'static>>, used: &mut usize, column: usize| {
        if column > *used {
            spans.push(Span::raw(" ".repeat(column - *used)));
            *used = column;
        }
    };
    if show_key {
        // 右侧留一格，快捷键列不贴边框。
        let key_start = width - key_column - 1;
        if width >= DETAIL_MIN_WIDTH {
            let label_column = marker.width() + label_width + 2;
            pad(&mut spans, &mut used, label_column);
            let detail =
                truncate_display_label(item.shown_detail(), key_start.saturating_sub(used + 2));
            used += detail.width();
            spans.push(Span::styled(detail, theme.secondary()));
        }
        pad(&mut spans, &mut used, key_start);
        spans.push(Span::styled(item.hint, theme.key()));
    }
    if selected {
        selected_line(spans, width)
    } else {
        Line::from(clip_spans(spans, width))
    }
}

#[derive(Debug, Default)]
pub(super) struct PaletteState {
    pub visible: bool,
    query: String,
    cursor: usize,
    selected: usize,
    scroll: usize,
    visible_rows: Cell<usize>,
    notice: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaletteEvent {
    Close,
    Action(PaletteAction),
    Consumed,
}

impl PaletteState {
    pub fn toggle(&mut self) {
        if self.visible {
            self.close();
        } else {
            self.visible = true;
            self.query.clear();
            self.cursor = 0;
            self.selected = 0;
            self.scroll = 0;
            self.notice = None;
        }
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    pub fn paste(&mut self, text: &str) {
        if !self.visible {
            return;
        }
        self.insert(text);
    }

    pub fn reject_paste(&mut self) {
        if self.visible {
            self.notice = Some(tr("Paste rejected: too large", "粘贴内容过大，已拒绝").into());
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> PaletteEvent {
        if !self.visible {
            return PaletteEvent::Consumed;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if (control && key.code == KeyCode::Char('p')) || key.code == KeyCode::Esc {
            self.close();
            return PaletteEvent::Close;
        }

        let count = self.matches().len();
        match key.code {
            KeyCode::Up if key.modifiers.is_empty() => {
                self.move_selection(-1, count);
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.move_selection(1, count);
            }
            KeyCode::Char('k') if control && key.modifiers == KeyModifiers::CONTROL => {
                self.move_selection(-1, count);
            }
            KeyCode::Char('j') if control && key.modifiers == KeyModifiers::CONTROL => {
                self.move_selection(1, count);
            }
            KeyCode::Home if key.modifiers.is_empty() => {
                self.selected = 0;
                self.scroll = 0;
            }
            KeyCode::End if key.modifiers.is_empty() => {
                self.selected = count.saturating_sub(1);
                self.scroll = self.selected;
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                if let Some(item) = self.matches().get(self.selected) {
                    self.close();
                    return PaletteEvent::Action(item.action);
                }
            }
            KeyCode::Left if key.modifiers.is_empty() => self.left(),
            KeyCode::Right if key.modifiers.is_empty() => self.right(),
            KeyCode::Backspace if key.modifiers.is_empty() => self.backspace(),
            KeyCode::Delete if key.modifiers.is_empty() => self.delete(),
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&c.to_string());
            }
            _ => {}
        }
        PaletteEvent::Consumed
    }

    pub fn draw(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let width = if area.width == 0 {
            0
        } else {
            area.width.saturating_sub(4).clamp(1, 76).min(area.width)
        };
        let height = if area.height == 0 {
            0
        } else {
            area.height.saturating_sub(2).clamp(1, 19).min(area.height)
        };
        let rect = Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        };
        frame.render_widget(Clear, rect);
        let theme = theme::current();
        let block = theme::focus_panel()
            .borders(Borders::ALL)
            .title(tr(" Command palette · Ctrl+P ", " 命令面板 · Ctrl+P "))
            .title_bottom(footer(
                tr(
                    "Esc close | Enter run | ↑/↓ select",
                    "Esc 关闭 | Enter 执行 | ↑/↓ 选择",
                ),
                rect.width,
            ));
        let inner = block.inner(rect);
        let inner_width = inner.width as usize;
        let query = self.query.replace('\n', "↵");
        let cursor_query = self.query[..self.cursor].replace('\n', "↵");
        let mut input = vec![Span::styled("> ", theme.key())];
        if query.is_empty() {
            input.push(Span::styled(
                tr("Type to filter commands", "输入以筛选命令"),
                theme.faint(),
            ));
        } else {
            input.push(Span::raw(query));
        }
        let mut lines = vec![Line::from(input)];
        if let Some(notice) = &self.notice {
            lines.push(Line::from(Span::styled(
                notice.clone(),
                tone(theme.warning),
            )));
        }
        // 只有放得下全部命令和分组标题时才画分隔线与标题；矮终端把行数留给命令。
        let groups = ITEMS
            .iter()
            .enumerate()
            .filter(|(index, item)| *index == 0 || ITEMS[index - 1].group != item.group)
            .count();
        let roomy = inner.height as usize >= lines.len() + 1 + ITEMS.len() + groups;
        if roomy {
            lines.push(Line::from(Span::styled(
                "─".repeat(inner_width),
                theme.faint(),
            )));
        }
        let matches = self.matches();
        let capacity = (inner.height as usize).saturating_sub(lines.len());
        let visible_rows = capacity.max(1);
        self.visible_rows.set(visible_rows);
        if matches.is_empty() {
            lines.push(Line::from(Span::styled(
                tr("No matching commands.", "没有匹配的命令。"),
                tone(theme.warning),
            )));
            lines.push(Line::from(clip_spans(
                vec![Span::styled(
                    tr(
                        "Try a name or key, e.g. timeline or F4.",
                        "试试命令名或快捷键，例如“时间线”或 F4。",
                    ),
                    theme.secondary(),
                )],
                inner_width,
            )));
        } else {
            // 显示行 = 可选的分组标题 + 命令；选择与滚动状态仍只按命令下标计数。
            let mut rows = Vec::new();
            let mut item_rows = Vec::with_capacity(matches.len());
            for (index, item) in matches.iter().enumerate() {
                if roomy && (index == 0 || matches[index - 1].group != item.group) {
                    rows.push(PaletteRow::Group(item.shown_group()));
                }
                item_rows.push(rows.len());
                rows.push(PaletteRow::Item(index));
            }
            let selected = self.selected.min(matches.len() - 1);
            let anchor = self.scroll.min(matches.len() - 1);
            // 滚动锚点是命令；若它是分组第一项，把标题一起带上。
            let mut start = item_rows[anchor];
            if start > 0 && matches!(rows[start - 1], PaletteRow::Group(_)) {
                start -= 1;
            }
            start = start.min(rows.len().saturating_sub(visible_rows));
            if item_rows[selected] < start {
                start = item_rows[selected];
            } else if item_rows[selected] >= start + visible_rows {
                start = item_rows[selected] + 1 - visible_rows;
            }
            for row in rows.iter().skip(start).take(capacity) {
                lines.push(match row {
                    PaletteRow::Group(group) => Line::from(clip_spans(
                        vec![Span::styled(*group, theme.secondary())],
                        inner_width,
                    )),
                    PaletteRow::Item(index) => {
                        command_line(matches[*index], *index == selected, inner_width)
                    }
                });
            }
        }
        frame.render_widget(Paragraph::new(lines).block(block), rect);
        if rect.width > 2 && rect.height > 2 {
            let cursor = format!("> {cursor_query}").width();
            frame.set_cursor_position((
                rect.x + 1 + cursor.min(rect.width.saturating_sub(2) as usize) as u16,
                rect.y + 1,
            ));
        }
    }

    #[cfg(test)]
    pub fn query(&self) -> &str {
        &self.query
    }

    fn matches(&self) -> Vec<&'static CommandItem> {
        let query = self.query.to_lowercase();
        if query.trim().is_empty() {
            return ITEMS.iter().collect();
        }
        ITEMS
            .iter()
            .filter(|item| item.matches(query.trim()))
            .collect()
    }

    fn move_selection(&mut self, delta: i8, count: usize) {
        if count == 0 {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        self.selected = if delta < 0 {
            self.selected.saturating_sub(1)
        } else {
            (self.selected + 1).min(count - 1)
        };
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else {
            let visible_rows = self.visible_rows.get().max(1);
            if self.selected >= self.scroll + visible_rows {
                self.scroll = self.selected + 1 - visible_rows;
            }
        }
    }

    fn insert(&mut self, text: &str) {
        if self.query.len() + text.len() > QUERY_BYTES {
            return;
        }
        self.query.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.selected = 0;
        self.scroll = 0;
        self.notice = None;
    }

    fn left(&mut self) {
        self.cursor = self.query[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index);
    }

    fn right(&mut self) {
        if let Some(grapheme) = self.query[self.cursor..].graphemes(true).next() {
            self.cursor += grapheme.len();
        }
    }

    fn backspace(&mut self) {
        let end = self.cursor;
        self.left();
        self.query.drain(self.cursor..end);
        self.selected = 0;
        self.scroll = 0;
    }

    fn delete(&mut self) {
        let start = self.cursor;
        self.right();
        self.query.drain(start..self.cursor);
        self.cursor = start;
        self.selected = 0;
        self.scroll = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_lines_name_commands_and_leave_paths_and_sentences_as_tasks() {
        assert_eq!(
            parse_slash("/timeline"),
            Some(Slash::Action(PaletteAction::Timeline))
        );
        assert_eq!(
            parse_slash("  /Agent "),
            Some(Slash::Action(PaletteAction::NextAgent))
        );
        assert_eq!(parse_slash("/model"), Some(Slash::Model(None)));
        assert_eq!(
            parse_slash("/model hi/gpt-6-luna "),
            Some(Slash::Model(Some("hi/gpt-6-luna")))
        );
        assert_eq!(parse_slash("/tools"), Some(Slash::Tools));
        assert_eq!(parse_slash("/quit"), Some(Slash::Quit));
        assert_eq!(parse_slash("/nope"), Some(Slash::Unknown));
        for task in [
            "/",
            "/etc/hosts",
            "/help me fix this",
            "/usr/bin/env python",
            "explain /model",
            "model",
            "/v2",
        ] {
            assert_eq!(parse_slash(task), None, "{task}");
        }
        let names = slash_names();
        for (name, _) in SLASH_ACTIONS {
            assert!(names.contains(&format!("/{name}")), "{names}");
        }
        assert!(names.ends_with("/model /setting /tools /quit"), "{names}");
        assert_eq!(parse_slash("/setting"), Some(Slash::Settings));
        assert_eq!(parse_slash("/settings"), Some(Slash::Settings));
    }

    #[test]
    fn palette_filters_and_enters_exact_action() {
        let mut palette = PaletteState::default();
        palette.toggle();
        palette.paste("timeline");
        assert_eq!(palette.query(), "timeline");
        assert_eq!(
            palette.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            PaletteEvent::Action(PaletteAction::Timeline)
        );
        assert!(!palette.visible);
    }

    #[test]
    fn palette_keeps_j_and_k_available_for_filter_text() {
        let mut palette = PaletteState::default();
        palette.toggle();
        palette.key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT));
        assert_eq!(palette.query(), "J");
        palette.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        for character in "job".chars() {
            palette.key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        assert_eq!(palette.query(), "job");
    }

    #[test]
    fn palette_shows_rejected_paste_notice() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut palette = PaletteState::default();
        palette.toggle();
        palette.reject_paste();
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|frame| palette.draw(frame, frame.area()))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("Paste rejected: too large"), "{screen}");
    }

    #[test]
    fn palette_scrolls_selection_into_view_on_a_short_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut palette = PaletteState::default();
        palette.toggle();
        for _ in 0..9 {
            palette.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        terminal
            .draw(|frame| palette.draw(frame, frame.area()))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("Switch next agent"), "{screen}");
    }

    fn palette_rows(palette: &PaletteState, width: u16, height: u16) -> Vec<String> {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| palette.draw(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn palette_fills_the_selected_row_and_keeps_keys_in_one_column() {
        use ratatui::backend::TestBackend;
        use ratatui::style::Modifier;
        use ratatui::Terminal;

        let mut palette = PaletteState::default();
        palette.toggle();
        palette.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| palette.draw(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows = palette_rows(&palette, 100, 24);
        let row_of = |needle: &str| {
            rows.iter()
                .position(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("{needle} missing: {rows:#?}"))
        };
        let selected = row_of("> Timeline");
        let other = row_of("  Search conversation");
        let left = rows[selected].find('│').unwrap();
        let left = rows[selected][..left].width() as u16 + 1;
        let right = left + 74;
        assert_eq!(buffer[(right, selected as u16)].symbol(), "│", "{rows:#?}");
        // 快捷键列和右边框之间留一格，不贴边。
        assert_eq!(buffer[(right - 1, other as u16)].symbol(), " ", "{rows:#?}");
        let theme = theme::current();
        for x in left..right {
            let cell = &buffer[(x, selected as u16)];
            assert_eq!(cell.style().bg, theme.selected().bg, "column {x}");
            assert!(cell.modifier.contains(Modifier::BOLD), "column {x}");
        }
        // 未选中的命令名是正文样式；选中态靠 `>`、加粗和整行底色，而不是只靠颜色。
        let label = rows[other].find("Search conversation").unwrap();
        let label = rows[other][..label].width() as u16;
        assert!(!buffer[(label, other as u16)]
            .modifier
            .contains(Modifier::BOLD));
        let key_column = |row: usize, key: &str| {
            let start = rows[row].rfind(key).unwrap();
            rows[row][..start].width()
        };
        let column = key_column(selected, "Ctrl+T");
        for (label, key) in [
            ("│  Search conversation", "Ctrl+F"),
            ("│  Requests", "F2"),
            ("│  Evidence", "F11"),
            ("│  Switch next agent", "F3"),
        ] {
            assert_eq!(key_column(row_of(label), key), column, "{label}");
        }
        let text = rows.join("\n");
        for expected in [
            "Type to filter commands",
            "Inspect",
            "Tasks",
            "Activity",
            "Session",
            "Find text in retained messages",
            "Enter run",
            "Esc close",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
    }

    #[test]
    fn palette_without_matches_says_what_to_try_next() {
        let mut palette = PaletteState::default();
        palette.toggle();
        palette.paste("zzzz");
        let text = palette_rows(&palette, 80, 24).join("\n");
        assert!(text.contains("> zzzz"), "{text}");
        assert!(text.contains("No matching commands."), "{text}");
        assert!(text.contains("Try a name or key"), "{text}");
        assert!(!text.contains("Timeline"), "{text}");
        assert!(text.contains("Esc close"), "{text}");
    }

    #[test]
    fn palette_keeps_full_command_names_and_survives_tiny_sizes() {
        let mut palette = PaletteState::default();
        palette.toggle();
        // 窄面板先放弃说明和快捷键列，命令名保持完整。
        let narrow = palette_rows(&palette, 30, 16).join("\n");
        assert!(narrow.contains("Attention thresholds"), "{narrow}");
        assert!(narrow.contains("Switch next agent"), "{narrow}");
        assert!(!narrow.contains("Find text"), "{narrow}");
        palette.paste("中文🙂 e\u{301} 很长的查询需要被右边界裁掉而不是换行");
        for (width, height) in [(1, 1), (3, 3), (10, 4), (24, 8), (80, 16)] {
            palette_rows(&palette, width, height);
        }
    }

    #[test]
    fn palette_supports_unicode_multiline_paste_and_no_results() {
        let mut palette = PaletteState::default();
        palette.toggle();
        palette.paste("中文🙂\n无结果");
        assert_eq!(palette.query(), "中文🙂\n无结果");
        assert_eq!(
            palette.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            PaletteEvent::Consumed
        );
    }

    #[test]
    fn palette_navigation_and_escape_are_local() {
        let mut palette = PaletteState::default();
        palette.toggle();
        palette.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        palette.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        palette.key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(
            palette.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            PaletteEvent::Close
        );
        assert!(!palette.visible);
    }

    /// Screen rows as displayed: the placeholder cell after a wide character is skipped.
    fn zh_rows(palette: &PaletteState, width: u16, height: u16) -> Vec<String> {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| palette.draw(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < buffer.area.width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                row
            })
            .collect()
    }

    #[test]
    fn chinese_palette_aligns_three_columns_inside_the_border() {
        use crate::ui::lang::with_zh;

        let mut palette = PaletteState::default();
        palette.toggle();
        with_zh(|| {
            let rows = zh_rows(&palette, 100, 24);
            let text = rows.join("\n");
            for expected in [
                "命令面板 · Ctrl+P",
                "输入以筛选命令",
                "查看",
                "任务",
                "活动",
                "会话",
                "Esc 关闭",
                "Enter 执行",
            ] {
                assert!(text.contains(expected), "{expected}: {text}");
            }
            let column = |row: &str, needle: &str| row[..row.rfind(needle).unwrap()].width();
            let mut details = Vec::new();
            let mut keys = Vec::new();
            for item in ITEMS {
                let row = rows
                    .iter()
                    .find(|row| row.contains(&format!(" {}  ", item.label_zh)))
                    .unwrap_or_else(|| panic!("{} missing: {text}", item.label_zh));
                // 说明完整显示，三列各自对齐，快捷键列离右边框一格。
                assert!(!row.contains('…'), "{row}");
                details.push(column(row, item.detail_zh));
                keys.push(column(row, item.hint));
                assert!(row.trim_end().ends_with(" │"), "{row}");
                assert_eq!(row.width(), 100, "{row}");
            }
            assert!(details.windows(2).all(|pair| pair[0] == pair[1]), "{text}");
            assert!(keys.windows(2).all(|pair| pair[0] == pair[1]), "{text}");

            // 窄面板放弃说明和快捷键列，中文命令名保持完整且不越过右边框。
            let narrow = zh_rows(&palette, 30, 16);
            let text = narrow.join("\n");
            for item in ITEMS {
                assert!(text.contains(item.label_zh), "{}: {text}", item.label_zh);
            }
            assert!(!text.contains("按顺序"), "{text}");
            for row in narrow.iter().filter(|row| row.contains('│')) {
                assert_eq!(row.width(), 30, "{row}");
                assert!(row.trim_end().ends_with('│'), "{row}");
            }
            for (width, height) in [(1, 1), (3, 3), (10, 4), (24, 8), (57, 12)] {
                zh_rows(&palette, width, height);
            }
        });
    }

    #[test]
    fn chinese_palette_filters_by_chinese_name_english_name_and_key() {
        use crate::ui::lang::with_zh;

        let run = |query: &str| {
            let mut palette = PaletteState::default();
            palette.toggle();
            palette.paste(query);
            palette.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        };
        with_zh(|| {
            for (query, action) in [
                ("时间线", PaletteAction::Timeline),
                ("timeline", PaletteAction::Timeline),
                ("Ctrl+T", PaletteAction::Timeline),
                ("f4", PaletteAction::Workflow),
                ("工作流", PaletteAction::Workflow),
                ("提醒阈值", PaletteAction::Attention),
                ("下一个agent", PaletteAction::NextAgent),
                ("下一个 Agent", PaletteAction::NextAgent),
                // 中文说明里的词也能找到命令。
                ("压缩", PaletteAction::Context),
            ] {
                assert_eq!(run(query), PaletteEvent::Action(action), "{query}");
            }
            assert_eq!(run("zzzz"), PaletteEvent::Consumed);
            let mut palette = PaletteState::default();
            palette.toggle();
            palette.paste("时间线");
            let text = zh_rows(&palette, 80, 24).join("\n");
            assert!(text.contains("> 时间线"), "{text}");
            assert!(!text.contains("工作流"), "{text}");
            palette.paste("zz");
            let text = zh_rows(&palette, 80, 24).join("\n");
            assert!(text.contains("没有匹配的命令。"), "{text}");
            assert!(text.contains("试试命令名或快捷键"), "{text}");
        });
        // 英文模式的过滤集合不变：中文名不参与匹配。
        assert_eq!(run("时间线"), PaletteEvent::Consumed);
        assert_eq!(run("Find text"), PaletteEvent::Consumed);
    }
}
