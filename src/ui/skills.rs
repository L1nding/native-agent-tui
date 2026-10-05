use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::state::{display_text, CoreSnapshot};
const SKILLS_PANEL_FULL_HEADER_LINES: u16 = 7;
const SKILLS_PANEL_COMPACT_HEADER_LINES: u16 = 5;

fn skill_refresh_source_label(source: Option<crate::skills::SkillRefreshSource>) -> &'static str {
    match source {
        Some(crate::skills::SkillRefreshSource::Initial) => "Initial",
        Some(crate::skills::SkillRefreshSource::Changed) => "Changed",
        Some(crate::skills::SkillRefreshSource::Manual) => "Manual",
        None => "unavailable",
    }
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
    let inner = Block::default()
        .borders(Borders::ALL)
        .inner(skills_panel_rect(area));
    let header = if inner.height < 10 {
        SKILLS_PANEL_COMPACT_HEADER_LINES
    } else {
        SKILLS_PANEL_FULL_HEADER_LINES
    };
    inner.height.saturating_sub(header) as usize
}

pub(super) fn draw_skills(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &CoreSnapshot,
    scroll: usize,
) {
    let rect = skills_panel_rect(area);
    let inner = Block::default().borders(Borders::ALL).inner(rect);
    let compact = inner.height < 10;
    frame.render_widget(Clear, rect);
    let skills = &snapshot.skills;
    let directory = Line::from(format!("Directory: {}", display_text(&snapshot.cwd)));
    let status = Line::from(
        if matches!(
            skills.availability,
            crate::skills::SkillAvailability::Available | crate::skills::SkillAvailability::Partial
        ) {
            format!(
                "Status: {:?} / {:?} | {} skills observed, {} enabled | scan errors: {}{}",
                skills.availability,
                skills.freshness,
                skills.skill_count,
                skills.enabled_count,
                skills.scan_error_count,
                if skills.truncated { " | truncated" } else { "" },
            )
        } else {
            format!(
                "Status: {:?} / {:?} | skill counts unavailable",
                skills.availability, skills.freshness
            )
        },
    );
    let session = Line::from(format!(
        "Session: {:?} · pending requests: {}",
        snapshot.phase,
        snapshot.requests.len()
    ));
    let refresh_source = if compact {
        Line::from(format!(
            "Refresh: {}",
            skill_refresh_source_label(skills.refresh_source)
        ))
    } else {
        Line::from(format!(
            "Refresh source: {}",
            skill_refresh_source_label(skills.refresh_source)
        ))
    };
    let source = if matches!(
        skills.availability,
        crate::skills::SkillAvailability::Available | crate::skills::SkillAvailability::Partial
    ) {
        Line::from("Source: server-confirmed skills/list directory scan")
    } else {
        Line::from(format!(
            "Inventory source unavailable: {:?}",
            skills.availability
        ))
    };
    let mut lines = if compact {
        let compact_status = Line::from(format!(
            "{:?} / {:?}",
            skills.availability, skills.freshness
        ));
        let compact_source = if matches!(
            skills.availability,
            crate::skills::SkillAvailability::Available | crate::skills::SkillAvailability::Partial
        ) {
            Line::from("Source: AppServer")
        } else {
            Line::from(format!("Unavailable: {:?}", skills.availability))
        };
        vec![
            directory,
            compact_status,
            compact_source,
            refresh_source,
            Line::from(format!(
                "{:?} req {} · Enter/F2",
                snapshot.phase,
                snapshot.requests.len()
            )),
        ]
    } else {
        vec![
            directory,
            status,
            session,
            source,
            refresh_source,
            Line::from("Listed entries do not confirm loaded, invoked, completed, or failed."),
            Line::from("Enter refresh · Esc/Ctrl+K close · ↑/↓ scroll · F2 requests"),
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
                Line::from(display_text(&format!(
                    "{} [{}] {} · {}",
                    if entry.enabled { "on" } else { "off" },
                    entry.scope,
                    entry.name,
                    entry.path,
                )))
            }),
    );
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Skills inventory · Ctrl+K "),
        ),
        rect,
    );
}
