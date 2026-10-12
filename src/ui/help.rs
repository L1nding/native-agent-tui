//! Static key reference shown as a modal overlay; it never reads or changes execution state.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::activity::{clip_spans, footer};
use super::lang::{tr, zh};
use super::theme;

type Section = (&'static str, &'static [(&'static str, &'static str)]);

const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Basics",
        &[
            ("Enter", "send task / answer"),
            ("Ctrl+O", "new line (Shift+Enter)"),
            ("Ctrl+S", "queue task while running"),
            ("/model", "choose the model"),
            ("/new", "start a new conversation"),
            ("Ctrl+U", "clear input"),
            ("Ctrl+C", "interrupt root turn"),
            ("Esc", "close panel, keep draft"),
            ("Ctrl+Q", "quit"),
        ],
    ),
    (
        "Requests",
        &[
            ("F2", "next request / details"),
            ("Ctrl+Y", "accept approval"),
            ("Ctrl+N", "decline approval"),
            ("Ctrl+B", "cancel approval"),
        ],
    ),
    (
        "Views",
        &[
            ("Ctrl+P", "command palette"),
            ("Ctrl+F", "search conversation"),
            ("Ctrl+T", "evidence timeline"),
            ("Ctrl+G", "context and usage"),
            ("Ctrl+K", "skills inventory"),
            ("Ctrl+E", "fold / unfold tool groups"),
            ("F3", "next agent"),
            ("F11", "activity evidence"),
            ("F12", "history and export"),
            ("PgUp/PgDn", "scroll"),
        ],
    ),
    (
        "Workflow",
        &[
            ("F4", "tasks panel"),
            ("F5 / F6", "pause dispatch / task"),
            ("F7 / F8", "cancel / retry (repeats)"),
            ("+ / -", "queued priority"),
            ("F9 twice", "stop workflow"),
            ("F10", "attention thresholds"),
            ("Ctrl+W", "silence reminder"),
        ],
    ),
];

/// Chinese wording of `SECTIONS`: same sections, same keys, same order.
const SECTIONS_ZH: &[(&str, &[(&str, &str)])] = &[
    (
        "基础",
        &[
            ("Enter", "发送任务 / 回答"),
            ("Ctrl+O", "换行（Shift+Enter）"),
            ("Ctrl+S", "运行中把任务排队"),
            ("/model", "选择模型"),
            ("/new", "新建会话"),
            ("Ctrl+U", "清空输入"),
            ("Ctrl+C", "中断根轮次"),
            ("Esc", "关闭面板，保留草稿"),
            ("Ctrl+Q", "退出"),
        ],
    ),
    (
        "请求",
        &[
            ("F2", "下一个请求 / 详情"),
            ("Ctrl+Y", "接受审批"),
            ("Ctrl+N", "拒绝审批"),
            ("Ctrl+B", "取消审批"),
        ],
    ),
    (
        "视图",
        &[
            ("Ctrl+P", "命令面板"),
            ("Ctrl+F", "搜索对话"),
            ("Ctrl+T", "证据时间线"),
            ("Ctrl+G", "上下文与用量"),
            ("Ctrl+K", "技能清单"),
            ("Ctrl+E", "折叠 / 展开工具组"),
            ("F3", "下一个 Agent"),
            ("F11", "活动证据"),
            ("F12", "历史与导出"),
            ("PgUp/PgDn", "滚动"),
        ],
    ),
    (
        "工作流",
        &[
            ("F4", "任务面板"),
            ("F5 / F6", "暂停派发 / 暂停任务"),
            ("F7 / F8", "取消 / 重试（可重复）"),
            ("+ / -", "调整排队优先级"),
            ("F9 两次", "停止工作流"),
            ("F10", "提醒阈值"),
            ("Ctrl+W", "静音提醒"),
        ],
    ),
];

fn active_sections() -> &'static [Section] {
    if zh() {
        SECTIONS_ZH
    } else {
        SECTIONS
    }
}

fn section_lines(sections: &[(&str, &[(&str, &str)])], width: usize) -> Vec<Line<'static>> {
    let theme = theme::current();
    // 键名列按全部分组对齐，两栏之间也保持同一列宽。
    let key_column = active_sections()
        .iter()
        .flat_map(|(_, keys)| keys.iter().map(|(key, _)| key.width()))
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (title, keys) in sections {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(clip_spans(
            vec![Span::styled(title.to_string(), theme.heading())],
            width,
        )));
        lines.extend(keys.iter().map(|(key, action)| {
            let pad = key_column.saturating_sub(key.width()) + 2;
            Line::from(clip_spans(
                vec![
                    Span::styled(key.to_string(), theme.key()),
                    Span::raw(format!("{}{action}", " ".repeat(pad))),
                ],
                width,
            ))
        }));
    }
    lines
}

pub(super) fn draw_help(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let two_columns = area.width >= 80;
    let width = area
        .width
        .saturating_sub(2)
        .min(if two_columns { 90 } else { 50 });
    let height = area.height.min(if two_columns { 22 } else { 38 });
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = theme::focus_panel()
        .borders(Borders::ALL)
        .title(tr(" Keys · F1 ", " 快捷键 · F1 "))
        .title_bottom(footer(
            tr(
                "F1/Esc close | Ctrl+P commands",
                "F1/Esc 关闭 | Ctrl+P 命令",
            ),
            rect.width,
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if two_columns {
        let half = inner.width / 2;
        let (left, right) = active_sections().split_at(2);
        for (column, x, w) in [
            (left, inner.x + 1, half.saturating_sub(1)),
            (right, inner.x + half + 1, inner.width - half - 1),
        ] {
            frame.render_widget(
                Paragraph::new(section_lines(column, w as usize)),
                Rect {
                    x,
                    width: w,
                    ..inner
                },
            );
        }
    } else {
        frame.render_widget(
            Paragraph::new(section_lines(active_sections(), inner.width as usize)),
            inner,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn rows(width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw_help(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn help_groups_keys_and_aligns_actions_in_one_column() {
        // 单栏：每个分组标题都在，所有动作说明从同一列开始。
        let single = rows(60, 40);
        let text = single.join("\n");
        for (title, _) in SECTIONS {
            assert!(text.contains(title), "{title}: {text}");
        }
        let column = |row: &str, action: &str| {
            let start = row.find(action).unwrap();
            row[..start].width()
        };
        let mut columns = Vec::new();
        for (_, keys) in SECTIONS {
            for (key, action) in keys.iter() {
                let row = single
                    .iter()
                    .find(|row| row.contains(key) && row.contains(action))
                    .unwrap_or_else(|| panic!("{key} missing: {text}"));
                columns.push(column(row, action));
            }
        }
        assert!(columns.windows(2).all(|pair| pair[0] == pair[1]), "{text}");
        assert!(text.contains("F1/Esc close"), "{text}");

        let wide = rows(100, 24).join("\n");
        for expected in ["Basics", "Workflow", "command palette", "silence reminder"] {
            assert!(wide.contains(expected), "{expected}: {wide}");
        }
    }

    #[test]
    fn help_does_not_panic_at_tiny_sizes() {
        for (width, height) in [(1, 1), (2, 2), (24, 8), (80, 16)] {
            rows(width, height);
        }
    }

    /// Screen rows as displayed: the placeholder cell after a wide character is skipped.
    fn zh_rows(width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw_help(frame, frame.area()))
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
    fn help_lists_the_tool_group_fold_key_under_views() {
        let (_, views) = SECTIONS
            .iter()
            .find(|(title, _)| *title == "Views")
            .unwrap();
        assert!(views.contains(&("Ctrl+E", "fold / unfold tool groups")));
        for (width, height) in [(60, 40), (100, 24)] {
            let text = rows(width, height).join("\n");
            assert!(text.contains("fold / unfold tool groups"), "{text}");
            // 加了一行之后，最后一条说明仍在面板内。
            assert!(text.contains("silence reminder"), "{text}");
        }
    }

    #[test]
    fn chinese_help_has_the_same_keys_and_aligns_actions() {
        // 两张表逐项对应：分组数、每组键数、键名（“两次”这类说明词除外）都一致。
        assert_eq!(SECTIONS.len(), SECTIONS_ZH.len());
        for ((_, english), (_, chinese)) in SECTIONS.iter().zip(SECTIONS_ZH) {
            assert_eq!(english.len(), chinese.len());
            for ((key, _), (key_zh, _)) in english.iter().zip(chinese.iter()) {
                assert_eq!(key.split(' ').next(), key_zh.split(' ').next());
            }
        }
        crate::ui::lang::with_zh(|| {
            let single = zh_rows(60, 40);
            let text = single.join("\n");
            assert!(text.contains("快捷键 · F1"), "{text}");
            assert!(text.contains("F1/Esc 关闭"), "{text}");
            assert!(text.contains("Ctrl+P 命令"), "{text}");
            let mut columns = Vec::new();
            for (title, keys) in SECTIONS_ZH {
                assert!(text.contains(title), "{title}: {text}");
                for (key, action) in keys.iter() {
                    let row = single
                        .iter()
                        .find(|row| row.contains(key) && row.contains(action))
                        .unwrap_or_else(|| panic!("{key} missing: {text}"));
                    columns.push(row[..row.find(action).unwrap()].width());
                }
            }
            assert!(columns.windows(2).all(|pair| pair[0] == pair[1]), "{text}");

            // 最窄的两栏布局：每条中文说明完整显示，行尾仍是右边框。
            let wide = zh_rows(80, 24);
            let text = wide.join("\n");
            assert!(text.contains("折叠 / 展开工具组"), "{text}");
            for (_, keys) in SECTIONS_ZH {
                for (key, action) in keys.iter() {
                    assert!(
                        wide.iter()
                            .any(|row| row.contains(key) && row.contains(action)),
                        "{key} {action}: {text}"
                    );
                }
            }
            for row in wide.iter().filter(|row| row.contains('│')) {
                assert!(!row.contains('…'), "{row}");
                assert!(row.trim_end().ends_with('│'), "{row}");
                assert_eq!(row.trim_end().width(), 79, "{row}");
            }
            // 单栏窄屏和极小尺寸不崩溃。
            for (width, height) in [(1, 1), (2, 2), (24, 8), (40, 40)] {
                zh_rows(width, height);
            }
        });
    }
}
