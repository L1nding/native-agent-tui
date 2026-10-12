use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Paragraph};

use super::activity::{footer, tone, word_mark, wrap_marked};
use super::lang::{self, tr, zh};
use super::theme;
use super::workflow_view::label_mark_l10n;
use crate::skills::{SkillAvailability, SkillFreshness};
use crate::state::{display_text, CoreSnapshot};
const SKILLS_PANEL_FULL_HEADER_LINES: u16 = 7;
const SKILLS_PANEL_COMPACT_HEADER_LINES: u16 = 5;

fn skill_refresh_source_label(source: Option<crate::skills::SkillRefreshSource>) -> &'static str {
    match source {
        Some(crate::skills::SkillRefreshSource::Initial) => tr("Initial", "首次加载"),
        Some(crate::skills::SkillRefreshSource::Changed) => tr("Changed", "检测到变更"),
        Some(crate::skills::SkillRefreshSource::Manual) => tr("Manual", "手动"),
        None => tr("unavailable", "不可用"),
    }
}

/// Inventory availability word. English keeps the variant name.
fn availability_label(availability: SkillAvailability) -> String {
    if !zh() {
        return format!("{availability:?}");
    }
    match availability {
        SkillAvailability::NotQueried => "未查询",
        SkillAvailability::Available => "可用",
        SkillAvailability::Partial => "部分可用",
        SkillAvailability::Unsupported => "不支持",
        SkillAvailability::TimedOut => "超时",
        SkillAvailability::RpcError => "RPC 错误",
        SkillAvailability::Malformed => "格式错误",
        SkillAvailability::TransportUnavailable => "连接不可用",
    }
    .to_owned()
}

/// Inventory freshness word. English keeps the variant name.
fn freshness_label(freshness: SkillFreshness) -> String {
    if !zh() {
        return format!("{freshness:?}");
    }
    match freshness {
        SkillFreshness::Unknown => "新旧未知",
        SkillFreshness::Current => "最新",
        SkillFreshness::Stale => "已过期",
        SkillFreshness::Queued => "刷新排队中",
    }
    .to_owned()
}

fn skills_panel_rect(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).clamp(1, 100);
    let height = area.height.saturating_sub(2).max(1);
    ratatui::layout::Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

pub(super) fn skills_panel_entries_capacity(area: Rect) -> usize {
    let inner = super::theme::panel()
        .borders(Borders::ALL)
        .inner(skills_panel_rect(area));
    let header = if inner.height < 10 {
        SKILLS_PANEL_COMPACT_HEADER_LINES
    } else {
        SKILLS_PANEL_FULL_HEADER_LINES
    };
    inner.height.saturating_sub(header) as usize
}

fn availability_tone(availability: SkillAvailability) -> Style {
    let theme = theme::current();
    tone(match availability {
        SkillAvailability::Available => theme.success,
        SkillAvailability::Partial => theme.warning,
        SkillAvailability::NotQueried | SkillAvailability::Unsupported => theme.muted,
        _ => theme.error,
    })
}

/// One header row: `Label:` in label style, an optional state word toned,
/// everything else in `base`. The text itself is passed through unchanged.
fn field(text: String, base: Style, state: Option<(&str, Style)>) -> Line<'static> {
    let clean = display_text(&text).replace('\n', " ");
    wrap_marked(&clean, usize::MAX, base, |clean| {
        label_mark_l10n(clean)
            .into_iter()
            .chain(state.and_then(|(word, style)| word_mark(clean, word, style)))
            .collect()
    })
    .into_iter()
    .next()
    .unwrap_or_default()
}

pub(super) fn draw_skills(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &CoreSnapshot,
    scroll: usize,
) {
    let theme = theme::current();
    let rect = skills_panel_rect(area);
    let inner = theme::panel().borders(Borders::ALL).inner(rect);
    let compact = inner.height < 10;
    frame.render_widget(Clear, rect);
    let skills = &snapshot.skills;
    let listed = matches!(
        skills.availability,
        SkillAvailability::Available | SkillAvailability::Partial
    );
    let availability = availability_label(skills.availability);
    let freshness = freshness_label(skills.freshness);
    let state = Some((
        availability.as_str(),
        availability_tone(skills.availability),
    ));
    let plain = Style::default();
    let directory = field(
        format!(
            "{}{}",
            tr("Directory: ", "目录："),
            display_text(&snapshot.cwd)
        ),
        plain,
        None,
    );
    let status = field(
        match (listed, zh()) {
            (true, false) => format!(
                "Status: {availability} / {freshness} | {} skills observed, {} enabled | scan errors: {}{}",
                skills.skill_count,
                skills.enabled_count,
                skills.scan_error_count,
                if skills.truncated { " | truncated" } else { "" },
            ),
            (true, true) => format!(
                "状态：{availability} / {freshness} | 观察到 {} 个技能，已启用 {} 个 | 扫描错误：{}{}",
                skills.skill_count,
                skills.enabled_count,
                skills.scan_error_count,
                if skills.truncated { " | 已截断" } else { "" },
            ),
            (false, false) => {
                format!("Status: {availability} / {freshness} | skill counts unavailable")
            }
            (false, true) => format!("状态：{availability} / {freshness} | 技能数量不可用"),
        },
        plain,
        state,
    );
    let phase = lang::phase(snapshot.phase);
    let phase_state = Some((phase.as_str(), theme.phase(snapshot.phase)));
    let requests = snapshot.requests.len();
    let session = field(
        if zh() {
            format!("会话：{phase} · 待处理请求：{requests}")
        } else {
            format!("Session: {phase} · pending requests: {requests}")
        },
        theme.secondary(),
        phase_state,
    );
    let refresh_source = field(
        format!(
            "{}{}",
            if compact {
                tr("Refresh: ", "刷新：")
            } else {
                tr("Refresh source: ", "刷新来源：")
            },
            skill_refresh_source_label(skills.refresh_source)
        ),
        theme.secondary(),
        None,
    );
    let source = if listed {
        field(
            tr(
                "Source: server-confirmed skills/list directory scan",
                "来源：服务端确认的 skills/list 目录扫描",
            )
            .into(),
            theme.secondary(),
            None,
        )
    } else {
        field(
            format!(
                "{}{availability}",
                tr("Inventory source unavailable: ", "清单来源不可用：")
            ),
            theme.secondary(),
            state,
        )
    };
    let mut lines = if compact {
        let compact_status = field(format!("{availability} / {freshness}"), plain, state);
        let compact_source = if listed {
            field(
                tr("Source: AppServer", "来源：AppServer").into(),
                theme.secondary(),
                None,
            )
        } else {
            field(
                format!("{}{availability}", tr("Unavailable: ", "不可用：")),
                theme.secondary(),
                state,
            )
        };
        vec![
            directory,
            compact_status,
            compact_source,
            refresh_source,
            field(
                format!("{phase} {} {requests} · Enter/F2", tr("req", "请求")),
                theme.secondary(),
                phase_state,
            ),
        ]
    } else {
        vec![
            directory,
            status,
            session,
            source,
            refresh_source,
            Line::from(Span::styled(
                tr(
                    "Listed entries do not confirm loaded, invoked, completed, or failed.",
                    "列出的条目不代表已加载、已调用、已完成或失败。",
                ),
                theme.secondary(),
            )),
            // 只画一条细分隔线；操作提示统一放到底边。
            Line::from(Span::styled(
                "─".repeat(inner.width as usize),
                theme.faint(),
            )),
        ]
    };
    let visible = inner.height.saturating_sub(lines.len() as u16) as usize;
    let start = scroll.min(skills.entries.len().saturating_sub(visible));
    lines.extend(
        skills
            .entries
            .iter()
            .skip(start)
            .take(visible)
            .map(|entry| {
                let one_line = |text: &str| display_text(text).replace('\n', " ");
                Line::from(vec![
                    if entry.enabled {
                        Span::styled(tr("on", "启用"), tone(theme.success))
                    } else {
                        Span::styled(tr("off", "停用"), theme.secondary())
                    },
                    Span::styled(format!(" [{}] ", one_line(&entry.scope)), theme.secondary()),
                    Span::raw(one_line(&entry.name)),
                    Span::styled(format!(" · {}", one_line(&entry.path)), theme.secondary()),
                ])
            }),
    );
    if skills.entries.is_empty() && visible > 0 {
        lines.push(Line::from(Span::styled(
            if listed {
                tr(
                    "No skills listed for this directory. Enter refreshes.",
                    "当前目录没有列出技能。按 Enter 刷新。",
                )
            } else {
                tr(
                    "No skill entries to show. Enter refreshes the inventory.",
                    "没有可显示的技能条目。按 Enter 刷新清单。",
                )
            },
            theme.secondary(),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            theme::focus_panel()
                .borders(Borders::ALL)
                .title(tr(" Skills inventory · Ctrl+K ", " 技能清单 · Ctrl+K "))
                .title_bottom(footer(
                    tr(
                        "Esc/Ctrl+K close | Enter refresh | ↑/↓ scroll | F2 requests",
                        "Esc/Ctrl+K 关闭 | Enter 刷新 | ↑/↓ 滚动 | F2 请求",
                    ),
                    rect.width,
                )),
        ),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn screen(snapshot: &CoreSnapshot, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw_skills(frame, frame.area(), snapshot, 0))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn empty_inventory_states_the_fact_and_the_next_step() {
        let snapshot = CoreSnapshot::default();
        let text = screen(&snapshot, 100, 24);
        // 未查询时仍明确显示不可用，不把“没有条目”写成“没有 skill”。
        assert!(text.contains("skill counts unavailable"), "{text}");
        assert!(text.contains("Inventory source unavailable"), "{text}");
        assert!(text.contains("Enter refreshes the inventory."), "{text}");
        assert!(text.contains("Esc/Ctrl+K close"), "{text}");
        assert!(text.contains("Enter refresh"), "{text}");
    }

    #[test]
    fn header_height_matches_the_capacity_used_for_scrolling() {
        // `skills_panel_entries_capacity` 被按键处理用来翻页；表头行数变了它也必须跟着变。
        for (width, height) in [(100, 24), (80, 16), (40, 12), (24, 8)] {
            let area = Rect::new(0, 0, width, height);
            let inner = theme::panel()
                .borders(Borders::ALL)
                .inner(skills_panel_rect(area));
            let header = if inner.height < 10 {
                SKILLS_PANEL_COMPACT_HEADER_LINES
            } else {
                SKILLS_PANEL_FULL_HEADER_LINES
            };
            assert_eq!(
                skills_panel_entries_capacity(area),
                inner.height.saturating_sub(header) as usize
            );
            screen(&CoreSnapshot::default(), width, height);
        }
        for (width, height) in [(1, 1), (3, 2)] {
            screen(&CoreSnapshot::default(), width, height);
        }
    }

    /// Screen rows as displayed: the placeholder cell after a wide character is skipped.
    fn zh_rows(snapshot: &CoreSnapshot, width: u16, height: u16) -> Vec<String> {
        use unicode_width::UnicodeWidthStr;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw_skills(frame, frame.area(), snapshot, 0))
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
    fn chinese_inventory_translates_chrome_and_keeps_entries_as_written() {
        use crate::skills::{SkillEntry, SkillRefreshSource, SkillsSnapshot};
        use unicode_width::UnicodeWidthStr;

        let entry = |name: &str, enabled: bool| SkillEntry {
            name: name.into(),
            path: format!("/skills/{name}/SKILL.md"),
            scope: "repo".into(),
            enabled,
        };
        let listed = CoreSnapshot {
            cwd: "D:/project/demo".into(),
            skills: SkillsSnapshot {
                availability: SkillAvailability::Available,
                freshness: SkillFreshness::Current,
                skill_count: 2,
                enabled_count: 1,
                refresh_source: Some(SkillRefreshSource::Manual),
                entries: vec![entry("rust-development", true), entry("research", false)],
                ..Default::default()
            },
            ..Default::default()
        };
        lang::with_zh(|| {
            let rows = zh_rows(&listed, 100, 24);
            let text = rows.join("\n");
            for expected in [
                "技能清单 · Ctrl+K",
                "目录：D:/project/demo",
                "状态：可用 / 最新 | 观察到 2 个技能，已启用 1 个 | 扫描错误：0",
                "会话：已创建 · 待处理请求：0",
                "来源：服务端确认的 skills/list 目录扫描",
                "刷新来源：手动",
                "列出的条目不代表已加载、已调用、已完成或失败。",
                "Esc/Ctrl+K 关闭",
                "F2 请求",
            ] {
                assert!(text.contains(expected), "{expected}: {text}");
            }
            // “启用 / 停用”等宽，作用域列对齐；名称和路径是服务端内容，原样显示。
            let on = rows.iter().find(|row| row.contains("rust-development"));
            let off = rows.iter().find(|row| row.contains("research")).unwrap();
            let on = on.unwrap();
            assert!(
                on.contains("启用 [repo] rust-development · /skills/"),
                "{on}"
            );
            assert!(off.contains("停用 [repo] research · /skills/"), "{off}");
            assert_eq!(
                on[..on.find("[repo]").unwrap()].width(),
                off[..off.find("[repo]").unwrap()].width()
            );

            let empty = zh_rows(&CoreSnapshot::default(), 100, 24).join("\n");
            for expected in [
                "状态：未查询 / 新旧未知 | 技能数量不可用",
                "清单来源不可用：未查询",
                "刷新来源：不可用",
                "没有可显示的技能条目。按 Enter 刷新清单。",
            ] {
                assert!(empty.contains(expected), "{expected}: {empty}");
            }

            // 窄而矮的面板走紧凑表头；每一行都停在右边框之内。
            let compact = zh_rows(&listed, 44, 12);
            let text = compact.join("\n");
            for expected in [
                "可用 / 最新",
                "来源：AppServer",
                "刷新：手动",
                "已创建 请求 0",
            ] {
                assert!(text.contains(expected), "{expected}: {text}");
            }
            for row in compact.iter().filter(|row| row.contains('│')) {
                assert!(row.trim_end().ends_with('│'), "{row}");
                assert_eq!(row.trim_end().width(), 42, "{row}");
            }
            for (width, height) in [(1, 1), (3, 2), (24, 8)] {
                zh_rows(&listed, width, height);
            }
        });
    }
}
