//! Read-only modeling business view. Natural language uses the platform editor.
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::layout::wrap_text;
use super::LocalState;
use crate::state::{display_text, CoreSnapshot};

// Reply text is conversation content, never an approval or execution fact.
fn current_reply(snapshot: &CoreSnapshot) -> Option<&str> {
    let thread = snapshot.thread_id.as_deref()?;
    let turn = snapshot.turn_id.as_deref()?;
    snapshot
        .messages
        .iter()
        .rev()
        .find(|item| {
            item.thread_id == thread
                && item.turn_id == turn
                && item.role == "Agent"
                && !item.text.trim().is_empty()
        })
        .map(|item| item.text.as_str())
}

pub(super) fn draw(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &CoreSnapshot,
    local: &LocalState,
) {
    let reply = current_reply(snapshot);
    let reply_height = if reply.is_some() && area.height >= 20 {
        (area.height / 3).clamp(6, 10)
    } else {
        0
    };
    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(reply_height),
        Constraint::Length(4),
        Constraint::Length(2),
    ])
    .split(area);
    let domain = &snapshot.domain;
    frame.render_widget(
        Paragraph::new(format!(
            "建模 · {} · {:?} · 平台模型 {}",
            domain.floor_id,
            snapshot.phase,
            snapshot.model.as_deref().unwrap_or("default")
        )),
        chunks[0],
    );
    let mut lines = Vec::new();
    if let Some(ledger) = &domain.ledger {
        lines.push(Line::from(format!(
            "来源实例对账 · 扫描 {}",
            ledger.scan_id
        )));
        lines.push(Line::from(format!("对账场景 {}", ledger.scene_sha256)));
        lines.push(Line::from(
            ledger
                .counts
                .iter()
                .map(|(state, count)| format!("{state} {count}"))
                .collect::<Vec<_>>()
                .join(" · "),
        ));
        lines.extend(ledger.rows.iter().map(|row| Line::from(display_text(row))));
        lines.push(Line::from(
            "跳过、待确认及缺能力对象保留在台账中。可输入类别或实例的采纳意见。",
        ));
    }
    if let Some(p) = &domain.proposal {
        lines.push(Line::from(format!(
            "方案 {} · 修订 {} · {}",
            p.binding.proposal_id, p.binding.revision, p.state
        )));
        lines.push(Line::from(display_text(&p.summary)));
        lines.push(Line::from(format!("场景基线 {}", p.binding.base_sha256)));
        lines.push(Line::from(format!("候选 {}", p.binding.candidate_sha256)));
        lines.push(Line::from("构件位置与依据"));
        lines.extend(p.objects.iter().map(|s| Line::from(display_text(s))));
        lines.push(Line::from("假设：批准后仍保留假设标签"));
        lines.extend(p.assumptions.iter().map(|s| Line::from(display_text(s))));
        lines.extend(p.diagnostics.iter().map(|s| Line::from(display_text(s))));
        lines.push(Line::from(format!(
            "领域校验 {} · 场景应用 {}",
            if p.diagnostics.is_empty()
                && matches!(p.state.as_str(), "prepared" | "applied" | "built")
            {
                "passed"
            } else {
                p.state.as_str()
            },
            domain.application.as_deref().unwrap_or("not_run")
        )));
    } else {
        lines.push(Line::from(
            "等待平台 Agent 读取楼层并准备方案。可直接输入目标和修改意见。",
        ));
    }
    lines.push(Line::from(format!(
        "Blender 产物 {}",
        domain.artifact.as_deref().unwrap_or("not_run")
    )));
    lines.push(Line::from(format!(
        "产物验证 {}",
        domain.artifact_validation.as_deref().unwrap_or("not_run")
    )));
    if domain.authorizing {
        lines.push(Line::from("正在记录具体版本批准…"));
    }
    for notice in [domain.notice.as_deref(), local.notice.as_deref()]
        .into_iter()
        .flatten()
    {
        lines.push(Line::from(display_text(notice)));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("方案与执行事实"),
            )
            .wrap(Wrap { trim: false })
            .scroll((local.domain_scroll, 0)),
        chunks[1],
    );
    if let Some(reply) = reply.filter(|_| reply_height > 0) {
        let reply_lines = wrap_text(reply, usize::from(chunks[2].width.saturating_sub(2)));
        let max_scroll = reply_lines
            .len()
            .saturating_sub(usize::from(reply_height - 2));
        let scroll = max_scroll.saturating_sub(usize::from(local.domain_reply_scroll_from_bottom));
        frame.render_widget(
            Paragraph::new(reply_lines.into_iter().map(Line::from).collect::<Vec<_>>())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Agent 回复 · Shift+PgUp/PgDn 滚动 · F6 对话"),
                )
                .scroll((scroll.min(usize::from(u16::MAX)) as u16, 0)),
            chunks[2],
        );
    }
    frame.render_widget(
        Paragraph::new(display_text(&local.editor.text))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("目标 / 自然语言反馈"),
            )
            .wrap(Wrap { trim: false }),
        chunks[3],
    );
    frame.render_widget(Paragraph::new("Enter 发送反馈 · Ctrl+Y 批准已展示版本 · Ctrl+C 中断 · F6/Esc 返回会话 · PgUp/PgDn 滚动"), chunks[4]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Command;
    use crate::domain::{DomainSnapshot, ProposalBinding, ProposalFact};
    use crate::state::SessionPhase;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn proposal(revision: u64) -> ProposalFact {
        ProposalFact {
            binding: ProposalBinding {
                run_id: "run".into(),
                context_sha256: "context".into(),
                proposal_id: format!("proposal-{revision}"),
                revision,
                prepare_token: format!("token-{revision}"),
                base_sha256: "base".into(),
                candidate_sha256: "candidate".into(),
            },
            state: "prepared".into(),
            summary: "两张示意床".into(),
            objects: vec!["bed.1.body · [6,2,0] · assumed".into()],
            assumptions: vec!["示意尺寸".into()],
            diagnostics: vec![],
            ready: true,
        }
    }

    fn message(thread: &str, turn: &str, text: &str) -> crate::state::ConversationItem {
        crate::state::ConversationItem {
            id: format!("{thread}-{turn}"),
            thread_id: thread.into(),
            turn_id: turn.into(),
            role: "Agent".into(),
            text: text.into(),
            complete: true,
            truncated: false,
        }
    }

    #[test]
    fn business_page_shows_current_root_clarification_and_keeps_natural_feedback() {
        use ratatui::{backend::TestBackend, Terminal};
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Completed,
            thread_id: Some("root".into()),
            turn_id: Some("current".into()),
            domain: DomainSnapshot {
                enabled: true,
                ..Default::default()
            },
            messages: vec![
                message("root", "old", "OLD_QUESTION"),
                message("root", "current", "请选择椅子 A、B 或 C；高度尚未确认。"),
                message("child", "current", "CHILD_QUESTION"),
            ],
            ..Default::default()
        };
        let mut local = LocalState {
            domain_visible: true,
            ..Default::default()
        };
        for (width, height) in [(120, 34), (60, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|f| draw(f, f.area(), &snapshot, &local))
                .unwrap();
            use unicode_width::UnicodeWidthStr;
            let buffer = terminal.backend().buffer();
            let mut screen = String::new();
            for y in 0..height {
                let mut x = 0;
                while x < width {
                    let symbol = buffer[(x, y)].symbol();
                    screen.push_str(symbol);
                    x += (symbol.width() as u16).max(1);
                }
                screen.push('\n');
            }
            assert!(screen.contains("请选择椅子 A、B 或 C"), "{screen}");
            assert!(screen.contains("目标 / 自然语言反馈"), "{screen}");
            assert!(!screen.contains("OLD_QUESTION") && !screen.contains("CHILD_QUESTION"));
        }
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        // A model reply alone must not create business write authority.
        super::super::handle_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        local.editor.insert("选 C，高度允许示意0.9米");
        super::super::handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::SubmitRootInput {
                text: "选 C，高度允许示意0.9米".into(),
            }
        );
    }

    #[test]
    fn long_reply_shows_the_final_question_and_scrolls_without_moving_domain_facts() {
        use ratatui::{backend::TestBackend, Terminal};
        use unicode_width::UnicodeWidthStr;
        let details = (0..20)
            .map(|i| format!("DETAIL_{i:02}\n"))
            .collect::<String>();
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Completed,
            thread_id: Some("root".into()),
            turn_id: Some("current".into()),
            domain: DomainSnapshot {
                enabled: true,
                ..Default::default()
            },
            messages: vec![message("root", "current", &(details + "请提供整体高度？"))],
            ..Default::default()
        };
        let mut local = LocalState {
            domain_visible: true,
            ..Default::default()
        };
        let render = |local: &LocalState| {
            let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
            terminal
                .draw(|f| draw(f, f.area(), &snapshot, local))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let mut screen = String::new();
            for y in 0..30 {
                let mut x = 0;
                while x < 80 {
                    let symbol = buffer[(x, y)].symbol();
                    screen.push_str(symbol);
                    x += (symbol.width() as u16).max(1);
                }
            }
            screen
        };
        assert!(render(&local).contains("请提供整体高度？"));
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        super::super::handle_key(
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.domain_scroll, 0);
        assert!(!render(&local).contains("请提供整体高度？"));
        assert!(render(&local).contains("DETAIL_12"));
        assert!(rx.try_recv().is_err());
        local.editor.insert("允许示意高度0.9米");
        super::super::handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(local.domain_reply_scroll_from_bottom, 0);
        assert!(matches!(
            rx.try_recv().unwrap(),
            Command::SubmitRootInput { .. }
        ));
    }

    #[test]
    fn a_new_turn_never_reuses_an_old_clarification_as_its_reply() {
        let mut snapshot = CoreSnapshot {
            thread_id: Some("root".into()),
            turn_id: Some("new".into()),
            messages: vec![message("root", "old", "请选择旧方案")],
            ..Default::default()
        };
        assert_eq!(current_reply(&snapshot), None);
        snapshot
            .messages
            .push(message("root", "new", "正在核对新意见"));
        assert_eq!(current_reply(&snapshot), Some("正在核对新意见"));
    }

    #[test]
    fn business_approval_submits_the_rendered_version_and_not_an_unseen_update() {
        let displayed = proposal(1);
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Completed,
            thread_id: Some("root".into()),
            domain: DomainSnapshot {
                enabled: true,
                proposal: Some(proposal(2)),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut local = LocalState {
            domain_visible: true,
            displayed_domain_binding: Some(displayed.binding.clone()),
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        super::super::handle_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::ApproveDomainChange {
                binding: displayed.binding
            }
        );
    }

    #[test]
    fn business_feedback_uses_the_platform_root_input_command() {
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Completed,
            thread_id: Some("root".into()),
            domain: DomainSnapshot {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut local = LocalState {
            domain_visible: true,
            ..Default::default()
        };
        local.editor.insert("移到右侧并保留墙体");
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        super::super::handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::SubmitRootInput {
                text: "移到右侧并保留墙体".into()
            }
        );
        assert!(local.editor.text.is_empty());
    }
}
