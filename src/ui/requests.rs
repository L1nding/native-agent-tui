//! Request detail rendering from Core's bounded live projection.
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::lang::{self, tr};
use super::layout::wrap;
use super::theme::{self, Theme};
use super::{request_locked, selected_request, LocalState};
use crate::interactions::{InputOption, RequestKind, RequestView};
use crate::state::{display_text, CoreSnapshot};

fn locked_text() -> &'static str {
    tr(
        "Submitted or unavailable; answers disabled",
        "已提交或不可用，不能回答",
    )
}

/// An option's number and label; the number is what the user may type instead.
pub(super) fn option_line(index: usize, option: &InputOption) -> String {
    let colon = if option.description.is_empty() {
        ""
    } else {
        ":"
    };
    format!("{}. {}{colon}", index + 1, option.label)
}

fn unsupported_text() -> &'static str {
    tr(
        "No supported decision; Ctrl+C interrupt root",
        "没有受支持的决定；Ctrl+C 中断根 Agent",
    )
}

/// Core names approval fields in English from a fixed list; unknown names are
/// shown as received.
fn field_label(label: &'static str) -> &'static str {
    if !lang::zh() {
        return label;
    }
    match label {
        "Operation" => "操作",
        "Command" => "命令",
        "Command cwd" => "命令工作目录",
        "Reason" => "原因",
        "Approval callback" => "审批回调",
        "Environment" => "环境",
        "Proposed session write root (not applied)" => "提议的会话可写根目录（未应用）",
        "Server command actions" => "服务端命令动作",
        "Network approval context" => "网络审批上下文",
        "Additional permission proposal (not applied)" => "额外权限提议（未应用）",
        "Execution policy proposal (not applied)" => "执行策略提议（未应用）",
        "Network policy proposals (not applied)" => "网络策略提议（未应用）",
        other => other,
    }
}

/// A field name followed by the colon of the current language.
fn labelled(label: &str) -> String {
    if lang::zh() {
        format!("{label}：")
    } else {
        format!("{label}:")
    }
}
/// `theme::hints` redraws each ` | ` separator as five cells.
const HINT_GAP: usize = 5;

/// What the selected request lets the user do right now.
enum Actions {
    Locked,
    Hints(Vec<&'static str>),
    Unsupported,
}

fn available_actions(
    snapshot: &CoreSnapshot,
    local: &LocalState,
    request: &RequestView,
) -> Actions {
    if request_locked(snapshot, local, request) {
        return Actions::Locked;
    }
    if matches!(request.kind, RequestKind::UserInput { .. }) {
        return Actions::Hints(vec![
            tr("Enter answer", "Enter 回答"),
            tr("F2 next request", "F2 下一个请求"),
        ]);
    }
    let mut hints = Vec::new();
    if request.allow_accept {
        hints.push(tr("Ctrl+Y accept", "Ctrl+Y 接受"));
    }
    if request.allow_decline {
        hints.push(tr("Ctrl+N decline", "Ctrl+N 拒绝"));
    }
    if request.allow_cancel {
        hints.push(tr("Ctrl+B stop turn", "Ctrl+B 停止本轮"));
    }
    if hints.is_empty() {
        Actions::Unsupported
    } else {
        Actions::Hints(hints)
    }
}

pub(super) fn actions(
    snapshot: &CoreSnapshot,
    local: &LocalState,
    request: &RequestView,
) -> String {
    match available_actions(snapshot, local, request) {
        Actions::Locked => locked_text().into(),
        Actions::Hints(hints) => hints.join(" | "),
        Actions::Unsupported => unsupported_text().into(),
    }
}

/// Visual priority of a piece of text, decided where the text is generated.
/// Wrapped continuation rows keep the kind of the row they came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    /// Panel title and question headings.
    Title,
    /// State that still needs the user.
    Waiting,
    /// State that no longer accepts an answer.
    Locked,
    /// Field names.
    Label,
    /// Content the user decides on: commands, questions, options, previews.
    Decision,
    /// Identity, policy, hints and missing facts.
    Meta,
    /// Explanations of what a decision does.
    Note,
}

fn style(theme: Theme, kind: RowKind) -> Style {
    match kind {
        RowKind::Title => theme.heading(),
        RowKind::Waiting => Style::default()
            .fg(theme.warning)
            .add_modifier(Modifier::BOLD),
        RowKind::Label => theme.label(),
        RowKind::Decision => Style::default().fg(theme.foreground),
        RowKind::Locked | RowKind::Meta => theme.secondary(),
        RowKind::Note => theme.secondary().add_modifier(Modifier::ITALIC),
    }
}

/// One generated row. `lead` is drawn before the first wrapped line and is
/// replaced by blanks of the same width on continuation lines.
struct Row {
    lead: &'static str,
    lead_kind: RowKind,
    parts: Vec<(RowKind, String)>,
}

const INDENT: &str = "  ";
const DEEP_INDENT: &str = "    ";
const MARKER: &str = "● ";

impl Row {
    fn new(kind: RowKind, text: impl Into<String>) -> Self {
        Self {
            lead: "",
            lead_kind: kind,
            parts: vec![(kind, text.into())],
        }
    }

    fn blank() -> Self {
        Self::new(RowKind::Meta, "")
    }

    fn lead(mut self, lead: &'static str, kind: RowKind) -> Self {
        self.lead = lead;
        self.lead_kind = kind;
        self
    }

    fn then(mut self, kind: RowKind, text: impl Into<String>) -> Self {
        self.parts.push((kind, text.into()));
        self
    }
}

fn details(request: &RequestView, snapshot: &CoreSnapshot, local: &LocalState) -> Vec<Row> {
    // 先显示要决定的内容（命令、文件、问题），身份和策略元数据放在后面。
    let locked = request_locked(snapshot, local, request);
    let state = if locked {
        RowKind::Locked
    } else {
        RowKind::Waiting
    };
    let zh = lang::zh();
    let missing = tr("unavailable", "不可用");
    // 全角冒号自带间隔；英文冒号后补一个空格。
    let gap = if zh { "" } else { " " };
    let mut rows = vec![Row::new(
        RowKind::Title,
        match request.kind {
            RequestKind::CommandApproval => tr("Command approval", "命令审批"),
            RequestKind::FileApproval => tr("File approval", "文件审批"),
            RequestKind::UserInput { .. } => tr("User input", "用户输入"),
        },
    )
    .lead(MARKER, state)
    .then(RowKind::Meta, " · ")
    .then(
        state,
        if locked {
            tr("submitted or unavailable", "已提交或不可用")
        } else {
            tr("awaiting your answer", "等待你回答")
        },
    )];
    let mut meta = Vec::new();
    match &request.kind {
        RequestKind::UserInput { questions } => {
            for (index, question) in questions.iter().enumerate() {
                rows.push(Row::blank());
                rows.push(
                    Row::new(
                        RowKind::Title,
                        format!(
                            "{} {}/{} · {}",
                            tr("Question", "问题"),
                            index + 1,
                            questions.len(),
                            question.header
                        ),
                    )
                    .then(RowKind::Meta, format!(" · id={}", question.id))
                    .then(
                        state,
                        if index == local.question_index {
                            tr(" [current]", " [当前]")
                        } else {
                            ""
                        },
                    )
                    .then(
                        RowKind::Meta,
                        if question.is_secret {
                            tr(" [secret answer masked]", " [保密回答已隐藏]")
                        } else {
                            ""
                        },
                    ),
                );
                rows.push(
                    Row::new(RowKind::Decision, question.question.clone())
                        .lead(INDENT, RowKind::Decision),
                );
                if let Some(options) = &question.options {
                    for (index, option) in options.iter().enumerate() {
                        rows.push(
                            Row::new(RowKind::Label, option_line(index, option))
                                .lead(DEEP_INDENT, RowKind::Decision)
                                .then(RowKind::Decision, format!(" {}", option.description)),
                        );
                    }
                    if question.multi_select {
                        rows.push(
                            Row::new(
                                RowKind::Meta,
                                tr(
                                    "Several may be chosen, separated by commas.",
                                    "可以选多项，用逗号分隔。",
                                ),
                            )
                            .lead(DEEP_INDENT, RowKind::Meta),
                        );
                    }
                } else {
                    rows.push(
                        Row::new(RowKind::Label, tr("Options:", "选项："))
                            .lead(INDENT, RowKind::Meta)
                            .then(RowKind::Meta, tr(" free text", "自由文本")),
                    );
                }
            }
            if zh {
                meta.push(format!(
                    "服务端阻塞提示：{}",
                    match request.details.is_blocking {
                        Some(true) => "是",
                        Some(false) => "否",
                        None => missing,
                    }
                ));
                meta.push(format!(
                    "旧版自动处理提示：{}",
                    request
                        .details
                        .auto_resolution_ms
                        .map_or_else(|| missing.into(), |ms| format!("{ms} ms（仅供参考）"))
                ));
            } else {
                meta.push(format!(
                    "Server blocking hint: {}",
                    request
                        .details
                        .is_blocking
                        .map_or_else(|| "unavailable".into(), |blocking| blocking.to_string())
                ));
                meta.push(format!(
                    "Legacy auto-resolution hint: {}",
                    request.details.auto_resolution_ms.map_or_else(
                        || "unavailable".into(),
                        |ms| format!("{ms} ms (informational)")
                    )
                ));
            }
        }
        _ => {
            if matches!(request.kind, RequestKind::CommandApproval) {
                for label in ["Command", "Command cwd"] {
                    if !request
                        .details
                        .fields
                        .iter()
                        .any(|field| field.label == label)
                    {
                        rows.push(
                            Row::new(RowKind::Label, labelled(field_label(label)))
                                .then(RowKind::Meta, format!("{gap}{missing}")),
                        );
                    }
                }
            }
            for field in &request.details.fields {
                rows.push(Row::blank());
                rows.push(Row::new(RowKind::Label, labelled(field_label(field.label))));
                rows.push(
                    Row::new(RowKind::Decision, field.text.clone()).lead(INDENT, RowKind::Decision),
                );
            }
            if matches!(request.kind, RequestKind::FileApproval) {
                let label = Row::new(RowKind::Label, tr("File preview:", "文件预览："));
                match &request.details.file_preview {
                    Some(preview) if !preview.unavailable => {
                        rows.push(Row::blank());
                        rows.push(
                            label
                                .then(
                                    RowKind::Meta,
                                    if zh {
                                        format!(
                                            "服务端 fileChange 条目，序号 {}",
                                            preview.source_seq
                                        )
                                    } else {
                                        format!(
                                            " server fileChange item, sequence {}",
                                            preview.source_seq
                                        )
                                    },
                                )
                                .then(
                                    RowKind::Waiting,
                                    if preview.truncated {
                                        tr(" [truncated at 32 KiB]", " [已在 32 KiB 处截断]")
                                    } else {
                                        ""
                                    },
                                ),
                        );
                        rows.push(
                            Row::new(RowKind::Decision, preview.text.clone())
                                .lead(INDENT, RowKind::Decision),
                        );
                    }
                    Some(_) => rows.push(label.then(
                        RowKind::Meta,
                        tr(
                            " unavailable (missing or invalid server changes)",
                            "不可用（服务端变更缺失或无效）",
                        ),
                    )),
                    None => rows.push(label.then(
                        RowKind::Meta,
                        tr(
                            " unavailable (no matching retained server item)",
                            "不可用（没有匹配的已保留服务端条目）",
                        ),
                    )),
                }
            }
            if request.allow_cancel {
                rows.push(Row::blank());
                rows.push(Row::new(
                    RowKind::Note,
                    tr(
                        "Cancel rejects this approval and interrupts its owning turn. Decline rejects it and lets the agent continue.",
                        "停止本轮（取消）会拒绝这次审批并中断它所属的轮次；拒绝只驳回这次审批，Agent 继续运行。",
                    ),
                ));
            }
            // 决定名是协议值，按服务端原样显示。
            meta.push(format!(
                "{}\n{}",
                tr("Server decisions:", "服务端给出的决定："),
                request.details.available_decisions.as_ref().map_or_else(
                    || tr(
                        "unavailable; client choices: accept / decline / cancel",
                        "不可用；客户端可选：接受 / 拒绝 / 取消",
                    )
                    .into(),
                    |decisions| if decisions.is_empty() {
                        tr("none", "无").into()
                    } else {
                        decisions.join("\n")
                    }
                )
            ));
            meta.push(
                tr(
                    "UI supports accept / decline / cancel. Session grants and policy amendments require a supported decision form.",
                    "界面支持接受 / 拒绝 / 取消。会话授权和策略修改需要受支持的决定形式。",
                )
                .into(),
            );
            meta.push(
                tr(
                    "Risk assessment: unavailable (review the request context)",
                    "风险评估：不可用（请自行查看请求上下文）",
                )
                .into(),
            );
        }
    }
    let item = request.details.item_id.as_deref().unwrap_or(missing);
    let started = request
        .details
        .started_at_ms
        .map_or_else(|| missing.into(), |ms| ms.to_string());
    let own_thread = snapshot.thread_id.as_deref() == Some(&request.thread_id);
    let mut context = if zh {
        let mut context = vec![
            "\n── 详情 ──".to_owned(),
            format!("请求：{:?} | 接收序号 {}", request.id, request.received_seq),
            format!("Agent/线程：{}", request.thread_id),
            format!("轮次：{}", request.turn_id),
            format!("条目：{item}"),
            format!("服务端开始时间（ms）：{started}"),
        ];
        let preset = match snapshot.permission.current.as_str() {
            "" => "未读取（在 /setting 查看）",
            preset => preset,
        };
        if own_thread {
            context.push(format!("dsh 权限预设：\n{preset}"));
        } else {
            context.push("子 Agent 的权限预设：不可用".into());
            context.push(format!("根 Agent 的 dsh 权限预设：{preset}"));
        }
        context.push(format!("会话工作目录：{}", snapshot.cwd));
        context.push("上下文来源：服务端请求".into());
        context.push("提议：未应用".into());
        context
    } else {
        let mut context = vec![
            "\n── Details ──".to_owned(),
            format!(
                "Request: {:?} | received sequence {}",
                request.id, request.received_seq
            ),
            format!("Agent/thread: {}", request.thread_id),
            format!("Turn: {}", request.turn_id),
            format!("Item: {item}"),
            format!("Server start ms: {started}"),
        ];
        let preset = match snapshot.permission.current.as_str() {
            "" => "not read (see /setting)",
            preset => preset,
        };
        if own_thread {
            context.push(format!("dsh permission preset:\n{preset}"));
        } else {
            context.push("Child permission preset: unavailable".into());
            context.push(format!("Root dsh permission preset: {preset}"));
        }
        context.push(format!("Session cwd: {}", snapshot.cwd));
        context.push("Context source: server request".into());
        context.push("Proposals: not applied".into());
        context
    };
    context.extend(meta);
    rows.extend(
        context
            .into_iter()
            .map(|text| Row::new(RowKind::Meta, text)),
    );
    rows
}

fn panel_rows(snapshot: &CoreSnapshot, local: &LocalState) -> Vec<Row> {
    if let Some(request) = selected_request(snapshot, local) {
        details(request, snapshot, local)
    } else if let Some(reference) = &local.request_selection {
        let zh = lang::zh();
        vec![
            Row::new(
                RowKind::Title,
                if zh {
                    format!("所选请求 {:?} 已过期或已处理。", reference.id)
                } else {
                    format!("Selected request {:?} expired or resolved.", reference.id)
                },
            )
            .lead(MARKER, RowKind::Locked),
            Row::new(
                RowKind::Meta,
                format!("{}{}", tr("Thread: ", "线程："), reference.thread_id),
            ),
            Row::new(
                RowKind::Meta,
                format!("{}{}", tr("Turn: ", "轮次："), reference.turn_id),
            ),
            Row::new(
                RowKind::Meta,
                format!(
                    "{}{}",
                    tr("Received sequence: ", "接收序号："),
                    reference.received_seq
                ),
            ),
            Row::new(
                RowKind::Note,
                tr(
                    "Answers disabled. F2 explicitly selects a current request.",
                    "已不能回答。按 F2 选择一个当前请求。",
                ),
            ),
        ]
    } else {
        vec![Row::new(
            RowKind::Title,
            tr("No pending requests.", "没有待处理的请求。"),
        )
        .lead(MARKER, RowKind::Locked)]
    }
}

/// Tabs have no terminal cell width; expand them so indentation survives.
fn clean(text: &str) -> String {
    display_text(text).replace('\t', "    ")
}

/// Wrap rows to `width` cells. The lead is reserved before wrapping, so no
/// content is pushed past the right edge.
fn flatten(theme: Theme, rows: &[Row], width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for row in rows {
        let lead_width = row.lead.width();
        let room = width.saturating_sub(lead_width).max(1);
        let mut logical: Vec<Vec<(RowKind, String)>> = vec![Vec::new()];
        for (kind, text) in &row.parts {
            for (index, piece) in clean(text).split('\n').enumerate() {
                if index > 0 {
                    logical.push(Vec::new());
                }
                if !piece.is_empty() {
                    if let Some(line) = logical.last_mut() {
                        line.push((*kind, piece.to_owned()));
                    }
                }
            }
        }
        let mut first = true;
        for parts in logical {
            let joined: String = parts.iter().map(|(_, text)| text.as_str()).collect();
            let mut pending = parts.iter();
            let mut current = pending.next();
            let mut used = 0;
            for piece in wrap(&joined, room) {
                let mut spans = Vec::new();
                if lead_width > 0 {
                    spans.push(if first {
                        Span::styled(row.lead, style(theme, row.lead_kind))
                    } else {
                        Span::raw(" ".repeat(lead_width))
                    });
                }
                first = false;
                // 换行只切分文本，不改写；按字节把每段还原到它所属的语义片段。
                let mut rest = piece.len();
                while rest > 0 {
                    let Some((kind, text)) = current else { break };
                    let take = rest.min(text.len() - used);
                    spans.push(Span::styled(
                        text[used..used + take].to_owned(),
                        style(theme, *kind),
                    ));
                    used += take;
                    rest -= take;
                    if used == text.len() {
                        current = pending.next();
                        used = 0;
                    }
                }
                lines.push(Line::from(spans));
            }
        }
    }
    lines
}

/// Action rows under the panel. Shortcuts use the shared hint style; state
/// text keeps its own words so it never depends on color.
fn action_lines(
    theme: Theme,
    actions: Option<Actions>,
    notice: Option<&str>,
    width: usize,
) -> Vec<Line<'static>> {
    let room = width.saturating_sub(2).max(1);
    let text = |kind, text: &str| {
        let lines = wrap(&clean(text), room)
            .into_iter()
            .map(|line| Line::from(Span::styled(line, style(theme, kind))))
            .collect::<Vec<_>>();
        (kind, lines)
    };
    let (kind, bodies) = match (notice, actions) {
        (Some(notice), _) => text(RowKind::Waiting, notice),
        (None, None) => text(RowKind::Locked, tr("Answers disabled", "已不能回答")),
        (None, Some(Actions::Locked)) => text(RowKind::Locked, locked_text()),
        (None, Some(Actions::Unsupported)) => text(RowKind::Waiting, unsupported_text()),
        (None, Some(Actions::Hints(hints))) => {
            // 尽量把快捷键放在同一行；放不下时整项换行，不从中间截断。
            let mut groups = Vec::new();
            let mut group = String::new();
            let mut cells = 0;
            for piece in hints.iter().flat_map(|hint| wrap(hint, room)) {
                let piece_cells = piece.width();
                if !group.is_empty() && cells + HINT_GAP + piece_cells > room {
                    groups.push(std::mem::take(&mut group));
                    cells = 0;
                }
                if !group.is_empty() {
                    group.push_str(" | ");
                    cells += HINT_GAP;
                }
                group.push_str(&piece);
                cells += piece_cells;
            }
            groups.push(group);
            (
                RowKind::Waiting,
                groups.iter().map(|group| theme::hints(group)).collect(),
            )
        }
    };
    bodies
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let mut spans = vec![Span::styled(
                if index == 0 { "→ " } else { "  " },
                style(theme, kind),
            )];
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect()
}

/// Largest useful scroll offset for this viewport. Key handling clamps to it, so
/// PageUp responds on the first press after scrolling past the end.
pub(super) fn max_scroll(snapshot: &CoreSnapshot, local: &LocalState, area: Rect) -> usize {
    let theme = theme::current();
    let input_rows =
        super::render::editor_view(snapshot, local, area.width).visible_rows(area.height);
    let rows = flatten(
        theme,
        &panel_rows(snapshot, local),
        area.width.saturating_sub(2) as usize,
    )
    .len();
    let actions = action_lines(
        theme,
        selected_request(snapshot, local)
            .map(|request| available_actions(snapshot, local, request)),
        local.notice.as_deref(),
        area.width as usize,
    )
    .len()
    .min(3);
    let body = (area.height.saturating_sub(input_rows + 3) as usize).saturating_sub(actions);
    rows.saturating_sub(body.saturating_sub(2))
}

pub(super) fn draw(frame: &mut ratatui::Frame<'_>, snapshot: &CoreSnapshot, local: &LocalState) {
    draw_themed(frame, snapshot, local, theme::current());
}

/// Wide-screen column beside the conversation: the selected request's details
/// without opening the full panel. It shows the top of the same rows and never
/// scrolls; F2 opens the scrollable panel.
pub(super) fn draw_inspector(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &CoreSnapshot,
    local: &LocalState,
) {
    let theme = theme::current();
    frame.render_widget(Clear, area);
    let block = theme::panel()
        .borders(Borders::TOP | Borders::LEFT)
        .padding(ratatui::widgets::Padding::horizontal(1))
        .border_style(Style::default().fg(theme.warning))
        .title_style(
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )
        .title(tr(" Request details ", " 请求详情 "));
    let inner = block.inner(area);
    let lines = flatten(theme, &panel_rows(snapshot, local), inner.width as usize);
    let height = inner.height as usize;
    let hidden = lines.len().saturating_sub(height);
    let block = if hidden > 0 {
        // 放不下时说明还有多少行，并指向可滚动的完整面板。
        block.title_bottom(
            Line::from(vec![
                Span::styled(
                    if super::lang::zh() {
                        format!(" 还有 {hidden} 行 · ")
                    } else {
                        format!(" {hidden} more · ")
                    },
                    theme.secondary(),
                ),
                Span::styled("F2", theme.key()),
                Span::styled(tr(" full details ", " 完整详情 "), theme.secondary()),
            ])
            .right_aligned(),
        )
    } else {
        block
    };
    // 底边标题占用最后一行，正文相应少画一行。
    let shown = if hidden > 0 {
        height.saturating_sub(1)
    } else {
        height
    };
    frame.render_widget(
        Paragraph::new(lines.into_iter().take(shown).collect::<Vec<_>>()).block(block),
        area,
    );
}

fn draw_themed(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &CoreSnapshot,
    local: &LocalState,
    theme: Theme,
) {
    let area = frame.area();
    // 主屏输入框（1–5 行加边框）和最底部提示行留在面板下方，回答时始终可见。
    let input_rows =
        super::render::editor_view(snapshot, local, area.width).visible_rows(area.height);
    let panel = Rect {
        height: area.height.saturating_sub(input_rows + 3),
        ..area
    };
    frame.render_widget(Clear, panel);
    let lines = flatten(
        theme,
        &panel_rows(snapshot, local),
        area.width.saturating_sub(2) as usize,
    );
    let mut action_lines = action_lines(
        theme,
        selected_request(snapshot, local)
            .map(|request| available_actions(snapshot, local, request)),
        local.notice.as_deref(),
        area.width as usize,
    );
    action_lines.truncate(3);
    let action_height = action_lines.len() as u16;
    let body = Rect {
        height: panel.height.saturating_sub(action_height),
        ..panel
    };
    let height = body.height.saturating_sub(2) as usize;
    let start = local.request_scroll.min(lines.len().saturating_sub(height));
    let below = lines.len().saturating_sub(start + height);
    let visible: Vec<_> = lines.into_iter().skip(start).take(height).collect();
    let phase = lang::phase(snapshot.phase);
    let mut block = theme::panel().borders(Borders::ALL).title(if lang::zh() {
        format!(" 请求：{} | {phase} ", snapshot.requests.len())
    } else {
        format!(" Requests: {} | {phase} ", snapshot.requests.len())
    });
    if below > 0 {
        block = block.title_bottom(
            Line::from(Span::styled(
                if lang::zh() {
                    format!(" 下方还有 {below} 行 ")
                } else {
                    format!(" {below} more below ")
                },
                style(theme, RowKind::Meta),
            ))
            .right_aligned(),
        );
    }
    frame.render_widget(Paragraph::new(visible).block(block), body);
    frame.render_widget(
        Paragraph::new(action_lines).style(theme.base()),
        Rect {
            y: panel.y + panel.height.saturating_sub(action_height),
            height: action_height,
            ..area
        },
    );
    let hint = Rect {
        y: area.y + area.height.saturating_sub(1),
        height: 1,
        ..area
    };
    // 主界面提示更长，先清空这一行，避免残留字符。
    frame.render_widget(Clear, hint);
    let mut hints = theme::hints(tr("F2 next", "F2 下一个")).spans;
    hints.push(Span::raw("  "));
    hints.push(Span::styled("PgUp/Dn", theme.key()));
    hints.push(Span::raw("  "));
    hints.extend(theme::hints(tr("Esc close", "Esc 关闭")).spans);
    frame.render_widget(Paragraph::new(Line::from(hints)).style(theme.base()), hint);
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Modifier};
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;
    use ratatui::Terminal;

    use super::super::{sync_local_requests, LocalState};
    use super::{draw_themed, Theme};
    use crate::interactions::RequestView;
    use crate::protocol::RpcId;
    use crate::state::{CoreSnapshot, SessionPhase};

    fn dark() -> Theme {
        Theme {
            background: Color::Rgb(21, 25, 31),
            surface: Color::Rgb(30, 36, 45),
            foreground: Color::Rgb(218, 225, 235),
            muted: Color::Rgb(143, 157, 177),
            border: Color::Rgb(61, 74, 92),
            accent: Color::Rgb(120, 199, 214),
            agent: Color::Rgb(184, 166, 232),
            success: Color::Rgb(141, 196, 160),
            warning: Color::Rgb(224, 186, 119),
            error: Color::Rgb(231, 143, 149),
            selection: Color::Rgb(36, 53, 67),
            monochrome: false,
        }
    }

    fn mono() -> Theme {
        Theme {
            background: Color::Reset,
            surface: Color::Reset,
            foreground: Color::Reset,
            muted: Color::Reset,
            border: Color::Reset,
            accent: Color::Reset,
            agent: Color::Reset,
            success: Color::Reset,
            warning: Color::Reset,
            error: Color::Reset,
            selection: Color::Reset,
            monochrome: true,
        }
    }

    fn approval(command: &str) -> (CoreSnapshot, LocalState) {
        let request = RequestView::decode(
            RpcId::Number(7),
            "item/commandExecution/requestApproval",
            &serde_json::json!({"threadId":"root","turnId":"one","itemId":"shell",
                "command":command,"cwd":"COMMAND_DIRECTORY",
                "availableDecisions":["accept","decline","cancel"]}),
        )
        .unwrap();
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Running,
            thread_id: Some("root".into()),
            cwd: "SESSION_DIRECTORY".into(),
            requests: vec![request],
            ..CoreSnapshot::default()
        };
        let mut local = LocalState {
            request_panel: true,
            ..Default::default()
        };
        sync_local_requests(&mut local, &snapshot);
        (snapshot, local)
    }

    /// Paints `X` everywhere first, so untouched cells stay recognizable.
    fn render(
        snapshot: &CoreSnapshot,
        local: &LocalState,
        theme: Theme,
        width: u16,
        height: u16,
    ) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::from("X".repeat(width as usize));
                        height as usize
                    ]),
                    frame.area(),
                );
                draw_themed(frame, snapshot, local, theme);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn row(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    fn screen(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| row(buffer, y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// First cell where an ASCII needle starts, searching top to bottom.
    fn find(buffer: &Buffer, needle: &str) -> (u16, u16) {
        let cells: Vec<String> = needle.chars().map(String::from).collect();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width.saturating_sub(cells.len() as u16 - 1) {
                if cells
                    .iter()
                    .enumerate()
                    .all(|(offset, cell)| buffer[(x + offset as u16, y)].symbol() == cell)
                {
                    return (x, y);
                }
            }
        }
        panic!("{needle:?} is not on screen:\n{}", screen(buffer));
    }

    /// Text inside the bordered body, with every blank removed.
    fn body_text(buffer: &Buffer) -> String {
        (1..buffer.area.height)
            .filter(|y| buffer[(0, *y)].symbol() == "│")
            .flat_map(|y| (1..buffer.area.width - 1).map(move |x| (x, y)))
            .map(|position| buffer[position].symbol())
            .filter(|symbol| !symbol.trim().is_empty())
            .collect()
    }

    #[test]
    fn only_the_title_row_carries_the_marker_after_scrolling() {
        let (snapshot, mut local) = approval("cargo test");
        let top = render(&snapshot, &local, dark(), 80, 24);
        assert!(
            row(&top, 1).starts_with("│● Command approval · awaiting your answer"),
            "{}",
            screen(&top)
        );
        for scroll in 1..12 {
            local.request_scroll = scroll;
            let scrolled = render(&snapshot, &local, dark(), 80, 24);
            assert!(
                !screen(&scrolled).contains('●'),
                "scroll {scroll}:\n{}",
                screen(&scrolled)
            );
        }
    }

    #[test]
    fn narrow_panels_wrap_inside_the_border_without_losing_text() {
        let command = "printf 中文字符👋 --flag=VALUE_AT_THE_RIGHT_EDGE_0123456789";
        let (snapshot, local) = approval(command);
        for width in [40, 24] {
            let buffer = render(&snapshot, &local, dark(), width, 200);
            let text = body_text(&buffer);
            for expected in [
                "●Commandapproval·awaitingyouranswer",
                &command.replace(' ', ""),
                "COMMAND_DIRECTORY",
                "Contextsource:serverrequest",
            ] {
                assert!(
                    text.contains(expected),
                    "{expected} lost at width {width}:\n{}",
                    screen(&buffer)
                );
            }
            for action in ["Ctrl+Y accept", "Ctrl+N decline", "Ctrl+B stop turn"] {
                let (x, y) = find(&buffer, action);
                assert!(x >= 2, "{action} overlaps the arrow at width {width}");
                assert_ne!(buffer[(0, y)].symbol(), "│");
            }
        }
    }

    #[test]
    fn panel_leaves_the_whole_answer_editor_uncovered() {
        let (snapshot, mut local) = approval("cargo test");
        for (lines, input_rows) in [(1, 1), (3, 3), (9, 5)] {
            local.editor.clear();
            local.editor.insert(&vec!["answer"; lines].join("\n"));
            let buffer = render(&snapshot, &local, dark(), 80, 24);
            // 底部一行是提示，其上是带边框的输入框。
            let input_top = 24 - 1 - (input_rows + 2);
            for y in input_top..23 {
                assert_eq!(row(&buffer, y), "X".repeat(80), "row {y}, {lines} lines");
            }
            assert!(
                row(&buffer, input_top - 1).starts_with("→ Ctrl+Y accept"),
                "{}",
                screen(&buffer)
            );
            assert_eq!(row(&buffer, 23).trim_end(), "F2 next  PgUp/Dn  Esc close");
        }
    }

    #[test]
    fn rows_are_styled_by_their_kind_and_wrapped_rows_inherit_it() {
        // 续行以 "Command: " 开头：样式必须来自所属的命令正文，而不是这段文字。
        let command = format!("{}Command: fake", "x".repeat(56));
        let (mut snapshot, local) = approval(&command);
        let theme = dark();
        let buffer = render(&snapshot, &local, theme, 60, 60);
        let bold = |position: (u16, u16)| buffer[position].modifier.contains(Modifier::BOLD);

        let label = find(&buffer, "Command:");
        assert_eq!(label.0, 1);
        assert_eq!(buffer[label].fg, theme.foreground);
        assert!(bold(label));

        let wrapped = find(&buffer, "Command: fake");
        assert_eq!(wrapped, (3, label.1 + 2), "{}", screen(&buffer));
        assert_eq!(buffer[wrapped].fg, theme.foreground);
        assert!(!bold(wrapped));

        let waiting = find(&buffer, "awaiting your answer");
        assert_eq!(buffer[waiting].fg, theme.warning);
        assert!(bold(waiting));
        assert_eq!(buffer[find(&buffer, "Command approval")].fg, theme.accent);

        for meta in [
            "── Details ──",
            "Session cwd:",
            "Risk assessment: unavailable",
        ] {
            let position = find(&buffer, meta);
            assert_eq!(buffer[position].fg, theme.muted, "{meta}");
            assert!(!bold(position), "{meta}");
        }

        snapshot.requests[0].responding = true;
        let buffer = render(&snapshot, &local, theme, 60, 60);
        let locked = find(&buffer, "submitted or unavailable");
        assert_eq!(buffer[locked].fg, theme.muted);
        assert!(!buffer[locked].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(1, locked.1)].fg, theme.muted);
    }

    #[test]
    fn monochrome_theme_separates_levels_by_weight_indent_and_words() {
        let (mut snapshot, local) = approval("cargo test");
        let buffer = render(&snapshot, &local, mono(), 60, 60);
        let bold = |needle: &str| {
            buffer[find(&buffer, needle)]
                .modifier
                .contains(Modifier::BOLD)
        };
        assert!(bold("Command approval"));
        assert!(bold("awaiting your answer"));
        assert!(bold("Command:"));
        assert!(!bold("cargo test"));
        assert!(!bold("Session cwd:"));
        assert_eq!(find(&buffer, "cargo test").0, 3);

        snapshot.requests[0].responding = true;
        let buffer = render(&snapshot, &local, mono(), 60, 60);
        let locked = find(&buffer, "submitted or unavailable");
        assert!(!buffer[locked].modifier.contains(Modifier::BOLD));
        assert!(!screen(&buffer).contains("awaiting your answer"));
    }

    #[test]
    fn approval_actions_and_navigation_share_the_hint_style() {
        let (mut snapshot, local) = approval("cargo test");
        let buffer = render(&snapshot, &local, dark(), 80, 24);
        let action = find(&buffer, "→");
        assert_eq!(
            row(&buffer, action.1).trim_end(),
            "→ Ctrl+Y accept  ·  Ctrl+N decline  ·  Ctrl+B stop turn"
        );
        let bold = |buffer: &Buffer, needle: &str| {
            buffer[find(buffer, needle)]
                .modifier
                .contains(Modifier::BOLD)
        };
        for key in ["Ctrl+Y", "Ctrl+N", "Ctrl+B", "F2", "PgUp/Dn", "Esc"] {
            assert!(bold(&buffer, key), "{key}");
        }
        for label in ["accept", "decline", "stop turn", "next", "close"] {
            assert!(!bold(&buffer, label), "{label}");
        }
        assert_eq!(row(&buffer, 23).trim_end(), "F2 next  PgUp/Dn  Esc close");

        snapshot.requests[0].responding = true;
        let buffer = render(&snapshot, &local, dark(), 80, 24);
        let locked = find(&buffer, "Submitted or unavailable; answers disabled");
        assert_eq!(locked.0, 2);
        assert_eq!(buffer[locked].fg, dark().muted);
        assert!(!buffer[locked].modifier.contains(Modifier::BOLD));
        assert!(!screen(&buffer).contains("Ctrl+Y"));
    }
}
