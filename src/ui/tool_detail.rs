use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::layout::wrap;
use super::tool_trace;
use crate::state::CoreSnapshot;
use crate::tool_details::ToolDetailLocator;

pub(super) fn draw(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    current: &CoreSnapshot,
    locator: &ToolDetailLocator,
    scroll: usize,
    notice: Option<&str>,
) -> usize {
    frame.render_widget(Clear, area);
    let mut lines = vec!["Tool detail · Esc back · PgUp/PgDn scroll".to_owned()];
    lines.push(format!(
        "Item: {} · thread: {} · turn: {}",
        locator.item_id,
        locator
            .identity
            .thread_id
            .as_deref()
            .unwrap_or("unavailable"),
        locator.identity.turn_id.as_deref().unwrap_or("unavailable")
    ));
    if let Some(notice) = notice {
        lines.push(notice.to_owned());
    }
    if let Some(detail) = current.tool_details.get(locator) {
        lines.push(format!(
            "Category: {:?} · lifecycle: {:?} · authoritative: {}",
            detail.category, detail.lifecycle, detail.authoritative
        ));
        lines.push(format!(
            "Exit code: {} · duration: {} ms",
            detail
                .exit_code
                .map_or_else(|| "unavailable".into(), |code| code.to_string()),
            detail
                .duration_ms
                .map_or_else(|| "unavailable".into(), |ms| ms.to_string())
        ));
        if let Some(name) = &detail.name {
            lines.push(format!("Tool: {name}"));
        }
        lines.push(format!(
            "Command: {}",
            detail.command.as_deref().unwrap_or("unavailable")
        ));
        lines.push(format!(
            "Cwd: {}",
            detail.cwd.as_deref().unwrap_or("unavailable")
        ));
        lines.push(format!(
            "Parameters: {}",
            detail.parameters.as_deref().unwrap_or("unavailable")
        ));
        lines.push(format!(
            "Result: {}",
            detail.result.as_deref().unwrap_or("unavailable")
        ));
        lines.push(format!(
            "Output source: {:?} · observed {} bytes · retained {} bytes · clipped {}",
            detail.output_source, detail.bytes_observed, detail.bytes_retained, detail.clipped
        ));
        lines.push(if detail.output.is_empty() {
            "Combined output: unavailable".into()
        } else {
            format!("Combined output: {}", detail.output)
        });
    } else {
        lines.push("Tool detail unavailable: exact detail was not retained.".into());
    }
    lines.push("Live tool trace · metadata only".into());
    lines.extend(tool_trace::lines(locator, current));
    let width = area.width.saturating_sub(2) as usize;
    let wrapped: Vec<_> = lines
        .iter()
        .flat_map(|line| wrap(line, width))
        .map(Line::from)
        .collect();
    let body_height = area.height.saturating_sub(2) as usize;
    let max = wrapped.len().saturating_sub(body_height);
    frame.render_widget(
        Paragraph::new(
            wrapped
                .into_iter()
                .skip(scroll.min(max))
                .take(body_height)
                .collect::<Vec<_>>(),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Tool detail "),
        ),
        area,
    );
    max
}
