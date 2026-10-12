//! Read-only modeling business view. Natural language uses the platform editor.
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::LocalState;
use crate::state::{display_text, CoreSnapshot};

pub(super) fn draw(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &CoreSnapshot,
    local: &LocalState,
) {
    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
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
    if let Some(path) = &domain.preview_connection {
        lines.push(Line::from(format!(
            "Blender 连接文件：{}",
            display_text(path)
        )));
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
    frame.render_widget(
        Paragraph::new(display_text(&local.editor.text))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("目标 / 自然语言反馈"),
            )
            .wrap(Wrap { trim: false }),
        chunks[2],
    );
    frame.render_widget(Paragraph::new("Enter 发送反馈 · Ctrl+Y 批准已展示版本 · Ctrl+C 中断 · F6/Esc 返回会话 · PgUp/PgDn 滚动"), chunks[3]);
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
