//! Read-only modeling business view. Natural language uses the platform editor.
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::extensions::{ExtensionView, ViewContext};
use super::layout::wrap_text;
use crate::extensions::{ExtensionConfirmation, ExtensionId, ExtensionSnapshot};
use crate::state::display_text;

/// Business milestones from verified facts, independent of Agent turn completion.
pub(super) fn modeling_workflow_lines(domain: &crate::domain::DomainSnapshot) -> Vec<String> {
    use crate::domain::{ArtifactState, SelectionState};
    let selected = domain
        .selection
        .as_ref()
        .is_some_and(|selection| selection.state == SelectionState::Selected)
        || (domain.project.is_none() && !domain.floor_id.is_empty());
    let mut lines = vec!["建模工作流 · 各步骤按实际回执更新".into()];
    lines.push(format!(
        "1 选择楼层 · {}",
        if selected {
            "已绑定目标"
        } else {
            "等待定位或选择图纸"
        }
    ));
    lines.push(format!(
        "2 结构与方案 · {}",
        if domain.project_scope.is_some() {
            "已有楼层作用域；具体方案状态见第 5 步"
        } else {
            "尚无楼层准备回执"
        }
    ));
    lines.push(match &domain.drawing_audit {
        Some(audit) => format!(
            "3 图纸审查 · 已判断 {}/{} 项；未覆盖范围及人工复核仍须核对",
            audit.judged_count, audit.candidate_count
        ),
        None => "3 图纸审查 · 尚无审查回执".into(),
    });
    lines.push(match domain.component_progress() {
        Some(progress) => format!(
            "4 发现与识别构件 · 已发现 {} · 已建 {} · 待处理 {}",
            progress.total, progress.built, progress.pending
        ),
        None => "4 发现与识别构件 · 尚未扫描；结构之后继续家具、洁具和设备".into(),
    });
    if let Some(groups) = &domain.groups {
        lines.push(format!(
            "  类别识别 · 已评估 {}/{} 组；新类别由 Agent 提出定义",
            groups.assessed_groups, groups.total_groups
        ));
    }
    let proposal_status = match &domain.proposal {
        _ if domain
            .operation
            .as_ref()
            .is_some_and(|operation| operation.state == "applied") =>
        {
            "当前方案已有应用回执"
        }
        _ if domain.operation.as_ref().is_some_and(|operation| {
            matches!(operation.state.as_str(), "applying" | "unknown")
        }) =>
        {
            "应用结果未确认，先对账原操作"
        }
        Some(proposal) if !proposal.diagnostics.is_empty() => "方案存在诊断，需处理后重新准备",
        Some(proposal) if proposal.state == "applied" || proposal.state == "built" => {
            "当前方案已应用"
        }
        Some(proposal) if proposal.ready => "当前具体方案待批准；Ctrl+Y 查看并批准",
        Some(_) => "当前方案尚未就绪",
        None => "等待 Agent 准备具体方案",
    };
    lines.push(format!("5 批准、应用与构建 · {proposal_status}"));
    if let Some(artifact) = domain.artifacts.last() {
        let state = match artifact.state {
            ArtifactState::Queued => "排队中",
            ArtifactState::Building => "构建中",
            ArtifactState::Unknown => "结果未确认，需对账",
            ArtifactState::Cancelled => "已取消",
            ArtifactState::Failed => "失败，需处理诊断",
            ArtifactState::Built => "已生成；有效性与当前版本仍按验证回执核对",
        };
        lines.push(format!(
            "  Blender {} · {state}",
            if artifact.binding.candidate {
                "预览"
            } else {
                "正式产物"
            }
        ));
    }
    lines.push("6 最终对账与复核 · 仍须核对来源覆盖、未决项及实际看图；人工验收未执行".into());
    if domain.fact_sync_notice.is_some() {
        lines.push("事实同步存在提示；以上为已收到的回执，不能据此宣告整层完成".into());
    }
    lines
}

fn sections(area: Rect, has_reply: bool) -> [Rect; 5] {
    let reply_height = if has_reply && area.height >= 20 {
        (area.height / 3).clamp(6, 10)
    } else {
        0
    };
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(reply_height),
        Constraint::Length(4),
        Constraint::Length(2),
    ])
    .split(area);
    [chunks[0], chunks[1], chunks[2], chunks[3], chunks[4]]
}

fn business_lines(
    domain: &crate::domain::DomainSnapshot,
    context: &ViewContext<'_>,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.extend(modeling_workflow_lines(domain).into_iter().map(Line::from));
    for notice in [
        domain.notice.as_deref(),
        domain.fact_sync_notice.as_deref(),
        context.notice,
    ]
    .into_iter()
    .flatten()
    {
        lines.push(Line::from(display_text(notice)));
    }
    if let Some(progress) = domain.component_progress() {
        lines.push(Line::from(format!(
            "{} · 已发现 {} · 已建 {} · 待处理 {} · 已跳过 {} · 已排除 {}",
            if progress.pending > 0 {
                "整层尚未完成"
            } else {
                "当前扫描范围的构件处理情况"
            },
            progress.total,
            progress.built,
            progress.pending,
            progress.skipped,
            progress.rejected
        )));
    } else if domain
        .project_scope
        .as_ref()
        .is_some_and(|scope| scope.assembly_binding.is_none())
    {
        lines.push(Line::from(
            "构件发现尚未扫描；结构完成后继续识别家具、洁具等构件",
        ));
    }
    if let Some(project) = &domain.project {
        lines.push(Line::from(format!(
            "项目 {} · {} 个楼层候选",
            project.project_path, project.total_floors
        )));
        lines.push(Line::from(match &project.component_source_layers {
            Some(layers) => format!("构件来源图层 {} · 只统计绑定图层", layers.join("、")),
            None => "项目未指定构件图层；下方显示本层实际读取范围".into(),
        }));
        if let Some(selection) = &domain.selection {
            lines.push(Line::from(format!(
                "目标 {} · {}",
                selection.target,
                match selection.state {
                    crate::domain::SelectionState::Selected => "目标已选择",
                    crate::domain::SelectionState::NeedsSelection => "重复图纸待选择",
                }
            )));
            lines.push(Line::from(
                "选层操作：场景应用 not_run · 写入授权 not_granted；实际建模进度见下方",
            ));
            if !selection.selected_keys.is_empty() {
                lines.push(Line::from(format!(
                    "已选择 {}",
                    selection
                        .selected_keys
                        .iter()
                        .take(100)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("、")
                )));
                if selection.selected_keys.len() > 100 {
                    lines.push(Line::from("其余已选楼层见完整事实"));
                }
            }
            for row in &selection.rows {
                lines.push(Line::from(row.clone()));
            }
        } else {
            for row in &project.rows {
                lines.push(Line::from(row.clone()));
            }
        }
        lines.push(Line::from(format!("目录 {}", project.catalog_id)));
        lines.push(Line::from(format!("来源 SHA256 {}", project.source_sha256)));
    }
    for building in &domain.buildings {
        lines.push(Line::from(format!(
            "楼栋 {} · {} · current={} · valid={}",
            building.build_id,
            building.state,
            building.current,
            building
                .valid
                .map_or_else(|| "not_run".into(), |valid| valid.to_string())
        )));
        lines.push(Line::from(format!(
            "楼层 {} · 来源 {} · 连接检查 {}",
            building.floor_keys.join("、"),
            building.source_reconciliation,
            building.connection_check
        )));
        if let Some(report) = &building.report {
            let historical =
                report.current != building.current || Some(report.artifact_valid) != building.valid;
            lines.push(Line::from(format!(
                "总报告 {} · 技术 {}{}",
                report.state,
                report.technical_status,
                if historical { " · 历史报告" } else { "" }
            )));
            lines.push(Line::from(format!(
                "来源未完成 {}/{} · 图纸未决 {} · 缺楼层审查 {} · 连接问题 {} · 人工 not_run",
                report.unresolved_sources,
                report.source_total,
                report.unresolved_drawing,
                report.missing_floor_reviews,
                report.connection_issues
            )));
        }
        lines.push(Line::from(format!(
            "楼栋产物 {}",
            display_text(&building.output)
        )));
        lines.push(Line::from("Agent 看图观察见回复 · 人工复核 not_run"));
    }
    if let Some(audit) = &domain.drawing_audit {
        lines.push(Line::from(format!(
            "图纸审查候选 {} 项 · 当前页起点 {} · 下一页 {}",
            audit.candidate_count,
            audit.offset,
            audit
                .next_offset
                .map_or_else(|| "无".into(), |offset| offset.to_string())
        )));
        lines.push(Line::from(format!("审查场景 {}", audit.scene_sha256)));
        lines.push(Line::from(format!(
            "确定性候选 · Agent 判断 {} · 已记录 {}/{} · 修正需具体方案批准",
            audit.ai_review, audit.judged_count, audit.candidate_count
        )));
        lines.extend(audit.rows.iter().cloned().map(Line::from));
    }
    if let Some(location) = &domain.project_location {
        lines.push(Line::from(format!(
            "楼层定位 {} · {} · 仅证据，未批准",
            location.floor_key, location.status
        )));
        lines.push(Line::from(format!("定位证据 {}", location.location_id)));
        lines.extend(location.rows.iter().cloned().map(Line::from));
    }
    if let Some(scope) = &domain.project_scope {
        lines.push(Line::from(format!(
            "当前楼层 {} · {}",
            scope.floor_key, scope.floor_id
        )));
        lines.push(Line::from(format!("作用域 {}", scope.scope_id)));
        lines.push(Line::from(match scope.component_discovery {
            Some(crate::domain::ComponentDiscovery::AllLayers) => {
                "构件发现：检查本层各图层的图纸符号，由 Agent 逐组识别用途"
            }
            Some(crate::domain::ComponentDiscovery::ConfiguredLayers) => {
                "构件发现：扫描项目指定图层，范围外对象尚未覆盖"
            }
            None => "构件发现：沿用较早的读取范围，其他图层是否遗漏仍待检查",
        }));
        if let Some(choice) = &scope.wall_layer_choice {
            lines.push(Line::from(format!(
                "结构提案绑定的墙体图层 {}",
                choice
                    .wall_layers
                    .iter()
                    .map(|layer| crate::state::display_text(layer))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
            lines.push(Line::from(format!("图层选择理由 {}", choice.reason)));
            lines.push(Line::from(format!(
                "所用定位证据 {}",
                choice.expected_location_id
            )));
        }
        if let Some(binding) = &scope.assembly_binding {
            lines.push(Line::from(format!(
                "楼栋连接作用域 · 基线 {} · 只读上层 {}",
                binding.build_id, binding.upper_floor_key
            )));
            lines.push(Line::from(format!("楼栋输入 {}", binding.input_binding)));
            lines.push(Line::from(
                "只增来源绑定连接 · 原楼层与楼板只读 · 不代表合规验收",
            ));
        }
    }
    if let Some(groups) = &domain.groups {
        lines.push(Line::from(format!(
            "全层来源分组 · {} 组 · {} 实例 · 已评估 {} 组",
            groups.total_groups, groups.total_objects, groups.assessed_groups
        )));
        lines.push(Line::from(format!(
            "分组扫描 {} · 场景 {}",
            groups.scan_id, groups.scene_sha256
        )));
        lines.push(Line::from(
            groups
                .counts
                .iter()
                .map(|(state, count)| {
                    format!("{} {count}", crate::domain::source_state_label(state))
                })
                .collect::<Vec<_>>()
                .join(" · "),
        ));
    }
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
                .map(|(state, count)| {
                    format!("{} {count}", crate::domain::source_state_label(state))
                })
                .collect::<Vec<_>>()
                .join(" · "),
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
    if let Some(operation) = &domain.operation {
        lines.push(Line::from(format!(
            "操作 {} · {} · 对账 {}",
            operation.operation_id, operation.state, operation.recovery
        )));
        lines.push(Line::from(format!("当前场景 {}", operation.current_sha256)));
        if operation.state == "applying" {
            lines.push(Line::from(
                "应用结果未确认；先查询操作与场景，再沿原操作恢复。",
            ));
        }
    }
    if domain.artifacts.is_empty() {
        lines.push(Line::from("Blender 产物 not_run"));
        lines.push(Line::from("产物验证 not_run"));
    }
    for artifact in domain.artifacts.iter().rev() {
        lines.push(Line::from(format!(
            "构建任务 {} · {}",
            artifact.binding.build_id,
            if artifact.binding.candidate {
                "候选预览"
            } else {
                "已应用场景"
            }
        )));
        let current = artifact
            .current
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".into());
        lines.push(Line::from(format!(
            "Blender 产物 {} · current={} · build_id={}",
            artifact.state.as_str(),
            current,
            artifact.binding.build_id
        )));
        let valid = artifact
            .valid
            .map(|v| format!("valid={v}"))
            .unwrap_or_else(|| "not_run".into());
        lines.push(Line::from(format!(
            "产物验证 {valid} · current={current} · build_id={}",
            artifact.binding.build_id
        )));
        lines.push(Line::from(format!(
            "产物位置 {}",
            display_text(&artifact.binding.output)
        )));
        lines.push(Line::from("Agent 看图观察见回复 · 人工复核 not_run"));
    }
    if domain.authorizing {
        lines.push(Line::from("正在记录具体版本批准…"));
    }
    if let Some(groups) = &domain.groups {
        lines.push(Line::from("来源分组明细"));
        lines.extend(groups.rows.iter().map(|row| Line::from(display_text(row))));
        lines.push(Line::from(
            "按相同来源几何分组；分类及参数依据由 Agent 评估，批准绑定具体方案。",
        ));
    }
    if let Some(ledger) = &domain.ledger {
        lines.push(Line::from("来源实例明细"));
        lines.extend(ledger.rows.iter().map(|row| Line::from(display_text(row))));
        lines.push(Line::from(
            "跳过、待确认及缺能力对象保留在台账中。可输入类别或实例的采纳意见。",
        ));
    }
    if let Some(p) = &domain.proposal {
        lines.push(Line::from("构件位置与依据"));
        lines.extend(p.objects.iter().map(|s| Line::from(display_text(s))));
        lines.push(Line::from("假设：批准后仍保留假设标签"));
        lines.extend(p.assumptions.iter().map(|s| Line::from(display_text(s))));
        lines.extend(p.diagnostics.iter().map(|s| Line::from(display_text(s))));
    }
    if let Some(fact) = &domain.verified_fact {
        lines.push(Line::from(format!(
            "已核验完整事实 · {} · 序号 {} · {} bytes",
            fact.kind, fact.sequence, fact.bytes
        )));
    }
    lines
        .into_iter()
        .flat_map(|line| wrap_text(&line.to_string(), usize::from(width.max(1))))
        .map(Line::from)
        .collect()
}

pub(super) fn max_scroll(
    snapshot: &ExtensionSnapshot,
    context: &ViewContext<'_>,
    area: Rect,
) -> u16 {
    let business = sections(area, context.reply.is_some())[1];
    business_lines(
        snapshot.modeling().expect("registered modeling projection"),
        context,
        business.width.saturating_sub(2),
    )
    .len()
    .saturating_sub(usize::from(business.height.saturating_sub(2)))
    .min(usize::from(u16::MAX)) as u16
}

pub(super) fn draw(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &ExtensionSnapshot,
    context: &ViewContext<'_>,
) {
    let domain = snapshot.modeling().expect("registered modeling projection");
    let reply = context.reply;
    let chunks = sections(area, reply.is_some());
    let reply_height = chunks[2].height;
    frame.render_widget(
        Paragraph::new(format!(
            "建模 · {} · {:?} · 平台模型 {}\n{}",
            domain.floor_id,
            context.phase,
            context.model.unwrap_or("default"),
            domain
                .fact_sync_notice
                .as_ref()
                .map(|notice| display_text(notice))
                .unwrap_or_else(|| {
                    domain
                        .verified_fact
                        .as_ref()
                        .map(|fact| {
                            format!(
                                "Core事实 {} · seq {} · {} B
SHA256 {}",
                                fact.kind, fact.sequence, fact.bytes, fact.sha256
                            )
                        })
                        .unwrap_or_default()
                })
        )),
        chunks[0],
    );
    let lines = business_lines(domain, context, chunks[1].width.saturating_sub(2));
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("方案与执行事实"),
            )
            .scroll((
                context
                    .panel
                    .scroll
                    .min(max_scroll(snapshot, context, area)),
                0,
            )),
        chunks[1],
    );
    if let Some(reply) = reply.filter(|_| reply_height > 0) {
        let reply_lines = wrap_text(reply, usize::from(chunks[2].width.saturating_sub(2)));
        let max_scroll = reply_lines
            .len()
            .saturating_sub(usize::from(reply_height - 2));
        let scroll = max_scroll.saturating_sub(usize::from(context.panel.reply_scroll_from_bottom));
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
        Paragraph::new(if let Some(label) = context.activity {
            format!(
                "{}{}",
                context.activity_marker,
                if context.draft.is_empty() {
                    label.to_owned()
                } else {
                    display_text(context.draft)
                }
            )
        } else {
            display_text(context.draft)
        })
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

pub(super) struct ModelingView;

impl ExtensionView for ModelingView {
    fn id(&self) -> ExtensionId {
        ExtensionId::Modeling
    }
    fn available(&self, snapshot: &ExtensionSnapshot) -> bool {
        snapshot.modeling().is_some_and(|domain| domain.enabled)
    }
    fn max_scroll(
        &self,
        snapshot: &ExtensionSnapshot,
        context: &ViewContext<'_>,
        area: Rect,
    ) -> u16 {
        max_scroll(snapshot, context, area)
    }
    fn draw(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        snapshot: &ExtensionSnapshot,
        context: &ViewContext<'_>,
    ) {
        draw(frame, area, snapshot, context);
    }
    fn confirmation(&self, snapshot: &ExtensionSnapshot) -> Option<ExtensionConfirmation> {
        let domain = snapshot.modeling()?;
        if domain.fact_sync_notice.is_some() || domain.authorizing {
            return None;
        }
        domain
            .proposal
            .as_ref()
            .filter(|proposal| proposal.ready)
            .map(|proposal| ExtensionConfirmation::Modeling(proposal.binding.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        extensions::{current_reply, ExtensionPanel},
        LocalState,
    };
    use super::*;
    use crate::state::CoreSnapshot;
    fn draw(
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        snapshot: &CoreSnapshot,
        local: &LocalState,
    ) {
        let context = ViewContext::new(
            snapshot,
            &local.extension,
            &local.editor.text,
            local.notice.as_deref(),
        );
        super::draw(frame, area, &snapshot.extensions[0], &context);
    }
    fn max_scroll(snapshot: &CoreSnapshot, local: &LocalState, area: Rect) -> u16 {
        let context = ViewContext::new(
            snapshot,
            &local.extension,
            &local.editor.text,
            local.notice.as_deref(),
        );
        super::max_scroll(&snapshot.extensions[0], &context, area)
    }
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
    fn modeling_workflow_keeps_pending_sources_after_structure_application() {
        let domain = DomainSnapshot {
            enabled: true,
            proposal: Some(proposal(1)),
            operation: Some(crate::domain::OperationFact {
                operation_id: "structure".into(),
                state: "applied".into(),
                recovery: "candidate_present".into(),
                current_sha256: "candidate".into(),
            }),
            ledger: Some(crate::domain::SourceLedgerFact {
                scan_id: "scan".into(),
                scene_sha256: "candidate".into(),
                rows: vec![],
                counts: [("pending_confirmation".into(), 323)].into(),
            }),
            ..Default::default()
        };
        let lines = modeling_workflow_lines(&domain).join("\n");
        assert!(lines.contains("待处理 323"), "{lines}");
        assert!(lines.contains("已有应用回执"), "{lines}");
        assert!(!lines.contains("待批准"), "{lines}");
        assert!(lines.contains("人工验收未执行"), "{lines}");
        assert!(!lines.contains("整层完成"), "{lines}");
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
            tools: Vec::new(),
        }
    }

    #[test]
    fn applied_structure_with_unresolved_components_still_shows_floor_work_remaining() {
        let domain = DomainSnapshot {
            enabled: true,
            floor_id: "floor.f08".into(),
            application: Some("applied".into()),
            ledger: Some(crate::domain::SourceLedgerFact {
                scan_id: "scan".into(),
                scene_sha256: "candidate".into(),
                rows: vec![],
                counts: [("pending_confirmation".into(), 323)].into(),
            }),
            ..Default::default()
        };
        let snapshot = CoreSnapshot::default();
        let local = LocalState::default();
        let context = ViewContext::new(&snapshot, &local.extension, "", None);
        let text = business_lines(&domain, &context, 120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("整层尚未完成"), "{text}");
        assert!(text.contains("待确认 323"), "{text}");
        assert!(!text.contains("pending_confirmation"), "{text}");
    }

    #[test]
    fn project_candidates_and_target_selection_are_visible_without_scene_approval() {
        use crate::domain::{ProjectCatalogFact, ProjectSelectionFact, SelectionState};
        for width in [40, 120] {
            let mut snapshot = CoreSnapshot {
                phase: SessionPhase::Ready,
                ..Default::default()
            };
            snapshot
                .extensions
                .push(ExtensionSnapshot::Modeling(DomainSnapshot {
                    enabled: true,
                    floor_id: "项目目标".into(),
                    project: Some(ProjectCatalogFact {
                        catalog_id: "a".repeat(64),
                        project_path: "projects/hospital".into(),
                        source_sha256: "b".repeat(64),
                        total_floors: 2,
                        component_source_layers: Some(vec![
                            "PL-家具".into(),
                            "USER-FIXTURES".into(),
                        ]),
                        floor_keys: vec!["f09.a".into(), "f09.b".into()],
                        rows: vec!["f09.a · 九层图纸A".into(), "f09.b · 九层图纸B".into()],
                    }),
                    selection: Some(ProjectSelectionFact {
                        selection_id: "c".repeat(64),
                        catalog_id: "a".repeat(64),
                        target: "整栋".into(),
                        state: SelectionState::NeedsSelection,
                        selected_keys: vec![],
                        candidate_keys: vec!["f09.a".into(), "f09.b".into()],
                        source_sha256: "b".repeat(64),
                        rows: vec![
                            "重复图纸：需用户选择".into(),
                            "f09.a · 九层图纸A".into(),
                            "f09.b · 九层图纸B".into(),
                        ],
                    }),
                    ..Default::default()
                }));
            let local = LocalState::default();
            let backend = ratatui::backend::TestBackend::new(width, 40);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| draw(frame, frame.area(), &snapshot, &local))
                .unwrap();
            use unicode_width::UnicodeWidthStr;
            let buffer = terminal.backend().buffer();
            let mut screen = String::new();
            for y in 0..40 {
                let mut x = 0;
                while x < width {
                    let symbol = buffer[(x, y)].symbol();
                    screen.push_str(symbol);
                    x += (symbol.width() as u16).max(1);
                }
                screen.push('\n');
            }
            for text in [
                "项目",
                "整栋",
                "重复图纸",
                "九层图纸A",
                "九层图纸B",
                "PL-家具",
                "not_run",
                "not_granted",
            ] {
                assert!(screen.contains(text), "{width}: {text}");
            }
            assert!(ModelingView.confirmation(&snapshot.extensions[0]).is_none());
        }
    }

    #[test]
    fn source_bound_location_and_layer_choice_are_visible_without_an_extra_approval_path() {
        use crate::domain::{ProjectLocationFact, ProjectScopeFact, ProjectWallLayerChoice};
        let domain = DomainSnapshot {
            enabled: true,
            project_location: Some(ProjectLocationFact {
                location_id: "e".repeat(64),
                catalog_id: "a".repeat(64),
                selection_id: "c".repeat(64),
                source_sha256: "b".repeat(64),
                floor_key: "f10".into(),
                status: "needs_wall_layers".into(),
                layers: vec!["AUTHORED-ENCLOSURE".into()],
                rows: vec!["AUTHORED-ENCLOSURE · 24 线实体".into()],
            }),
            project_scope: Some(ProjectScopeFact {
                component_discovery: None,
                activation_revision: Some(1),
                assembly_binding: None,
                scope_id: "d".repeat(64),
                floor_key: "f10".into(),
                floor_id: "floor.f10".into(),
                selection_id: "c".repeat(64),
                source_sha256: "b".repeat(64),
                wall_layer_choice: Some(ProjectWallLayerChoice {
                    expected_location_id: "e".repeat(64),
                    wall_layers: vec!["AUTHORED-ENCLOSURE".into()],
                    reason: "配对墙面证据".into(),
                }),
            }),
            ..Default::default()
        };
        for width in [40, 120] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 40)).unwrap();
            let snapshot = CoreSnapshot {
                extensions: vec![ExtensionSnapshot::Modeling(domain.clone())],
                ..Default::default()
            };
            terminal
                .draw(|frame| draw(frame, frame.area(), &snapshot, &LocalState::default()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let mut screen = String::new();
            for y in 0..40 {
                let mut x = 0;
                while x < width {
                    let text = buffer[(x, y)].symbol();
                    screen.push_str(text);
                    x += (unicode_width::UnicodeWidthStr::width(text) as u16).max(1);
                }
                screen.push('\n');
            }
            for expected in [
                "楼层定位",
                "未批准",
                "24 线实体",
                "配对墙面证据",
                "结构提案绑定",
            ] {
                assert!(screen.contains(expected), "{width}: {expected}\n{screen}");
            }
            assert!(ModelingView.confirmation(&snapshot.extensions[0]).is_none());
        }
    }

    #[test]
    fn business_page_shows_current_root_clarification_and_keeps_natural_feedback() {
        use ratatui::{backend::TestBackend, Terminal};
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Completed,
            thread_id: Some("root".into()),
            turn_id: Some("current".into()),
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                ..Default::default()
            })],
            messages: vec![
                message("root", "old", "OLD_QUESTION"),
                message("root", "current", "请选择椅子 A、B 或 C；高度尚未确认。"),
                message("child", "current", "CHILD_QUESTION"),
            ],
            ..Default::default()
        };
        let mut local = LocalState {
            extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
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
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                ..Default::default()
            })],
            messages: vec![message("root", "current", &(details + "请提供整体高度？"))],
            ..Default::default()
        };
        let mut local = LocalState {
            extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
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
        assert_eq!(local.extension.scroll, 0);
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
        assert_eq!(local.extension.reply_scroll_from_bottom, 0);
        assert!(matches!(
            rx.try_recv().unwrap(),
            Command::SubmitRootInput { .. }
        ));
    }

    #[test]
    fn business_scroll_stays_readable_at_the_bottom_and_returns_with_one_page_up() {
        use ratatui::{backend::TestBackend, Terminal};
        use unicode_width::UnicodeWidthStr;
        let mut prepared = proposal(3);
        prepared.objects = (0..24)
            .map(|i| {
                format!(
                    "OBJECT_{i:02} 来源尺寸与位置依据 {}",
                    "confirmed ".repeat(8)
                )
            })
            .collect();
        prepared.diagnostics.push("BOTTOM_FACT".into());
        let snapshot = CoreSnapshot {
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                proposal: Some(prepared),
                ..Default::default()
            })],
            ..Default::default()
        };
        let render = |local: &LocalState| {
            let area = local.viewport;
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            terminal
                .draw(|f| draw(f, f.area(), &snapshot, local))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let mut screen = String::new();
            for y in 0..area.height {
                let mut x = 0;
                while x < area.width {
                    let symbol = buffer[(x, y)].symbol();
                    screen.push_str(symbol);
                    x += (symbol.width() as u16).max(1);
                }
                screen.push('\n');
            }
            screen
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let mut local = LocalState {
            extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
            ..Default::default()
        };
        for (width, height) in [(120, 34), (40, 24)] {
            local.viewport = Rect::new(0, 0, width, height);
            local.extension.scroll = 0;
            for _ in 0..200 {
                super::super::handle_key(
                    KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
                    &snapshot,
                    &mut local,
                    &tx,
                );
            }
            let bottom = max_scroll(&snapshot, &local, local.viewport);
            assert!(bottom > 8);
            assert_eq!(local.extension.scroll, bottom);
            assert!(render(&local).contains("BOTTOM_FACT"));
            // A restored offset or a resize must also clamp during rendering.
            local.extension.scroll = u16::MAX;
            assert!(render(&local).contains("BOTTOM_FACT"));
            super::super::handle_key(
                KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
                &snapshot,
                &mut local,
                &tx,
            );
            assert_eq!(local.extension.scroll, bottom - 8);
            assert!(render(&local).contains("OBJECT_"));
            for _ in 0..200 {
                super::super::handle_key(
                    KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
                    &snapshot,
                    &mut local,
                    &tx,
                );
            }
            assert_eq!(local.extension.scroll, 0);
            assert!(render(&local).contains("方案 proposal-3"));
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn interrupted_operation_displays_its_receipt_and_actual_authority() {
        use ratatui::{backend::TestBackend, Terminal};
        use unicode_width::UnicodeWidthStr;
        let snapshot = CoreSnapshot {
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                proposal: Some(proposal(3)),
                application: Some("applying".into()),
                operation: Some(crate::domain::OperationFact {
                    operation_id: "apply-one".into(),
                    state: "applying".into(),
                    recovery: "candidate_present".into(),
                    current_sha256: "b".repeat(64),
                }),
                ..Default::default()
            })],
            ..Default::default()
        };
        for (width, height) in [(120, 34), (60, 24)] {
            let mut local = LocalState {
                extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
                ..Default::default()
            };
            local.extension.scroll = max_scroll(&snapshot, &local, Rect::new(0, 0, width, height));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|f| draw(f, f.area(), &snapshot, &local))
                .unwrap();
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
            assert!(screen.contains("操作 apply-one"), "{screen}");
            assert!(screen.contains("candidate_present"), "{screen}");
            assert!(screen.contains("应用结果未确认"), "{screen}");
            assert!(screen.contains("当前场景"), "{screen}");
        }
    }

    #[test]
    fn business_artifacts_show_success_and_failure_under_their_own_build_ids() {
        use crate::domain::{ArtifactBinding, ArtifactFact, ArtifactState};
        let binding = |id: &str| ArtifactBinding {
            build_id: id.into(),
            proposal_id: "proposal".into(),
            revision: 3,
            prepare_token: "token".into(),
            candidate: true,
            authority_sha256: "base".into(),
            input_sha256: "candidate".into(),
            output: format!("{id}.glb"),
        };
        let snapshot = CoreSnapshot {
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                proposal: Some(proposal(3)),
                artifacts: vec![
                    ArtifactFact {
                        binding: binding("preview-valid"),
                        state: ArtifactState::Built,
                        current: Some(false),
                        valid: Some(true),
                    },
                    ArtifactFact {
                        binding: binding("build-failed"),
                        state: ArtifactState::Failed,
                        current: Some(true),
                        valid: Some(false),
                    },
                ],
                ..Default::default()
            })],
            ..Default::default()
        };
        let lines = business_lines(
            snapshot.extensions[0].modeling().unwrap(),
            &ViewContext::new(&snapshot, &ExtensionPanel::default(), "", None),
            180,
        );
        let screen = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let failed = screen
            .split("构建任务 build-failed")
            .nth(1)
            .unwrap()
            .split("构建任务 preview-valid")
            .next()
            .unwrap();
        assert!(failed.contains("Blender 产物 failed"), "{screen}");
        assert!(failed.contains("valid=false"), "{screen}");
        assert!(!failed.contains("valid=true"), "{screen}");
        let previous = screen.split("构建任务 preview-valid").nth(1).unwrap();
        assert!(previous.contains("valid=true · current=false"), "{screen}");
        // Exercise the real business renderer at both wide and narrow widths.
        use ratatui::{backend::TestBackend, Terminal};
        use unicode_width::UnicodeWidthStr;
        for (width, height) in [(120, 45), (60, 45)] {
            let mut local = LocalState {
                extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
                ..Default::default()
            };
            local.extension.scroll = max_scroll(&snapshot, &local, Rect::new(0, 0, width, height));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|f| draw(f, f.area(), &snapshot, &local))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let mut rendered = String::new();
            for y in 0..height {
                let mut x = 0;
                while x < width {
                    let symbol = buffer[(x, y)].symbol();
                    rendered.push_str(symbol);
                    x += (symbol.width() as u16).max(1);
                }
                rendered.push('\n');
            }
            assert!(rendered.contains("构建任务 build-failed"), "{rendered}");
            assert!(rendered.contains("构建任务 preview-valid"), "{rendered}");
            let failed = rendered
                .split("构建任务 build-failed")
                .nth(1)
                .unwrap()
                .split("构建任务 preview-valid")
                .next()
                .unwrap();
            assert!(failed.contains("valid=false"), "{rendered}");
            assert!(!failed.contains("valid=true"), "{rendered}");
        }
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
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                proposal: Some(proposal(2)),
                ..Default::default()
            })],
            ..Default::default()
        };
        let mut local = LocalState {
            extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
            ..Default::default()
        };
        local
            .extension
            .displayed(ExtensionConfirmation::Modeling(displayed.binding.clone()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        super::super::handle_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Command::ConfirmExtension {
                confirmation: ExtensionConfirmation::Modeling(displayed.binding)
            }
        );
    }

    #[test]
    fn business_feedback_uses_the_platform_root_input_command() {
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Completed,
            thread_id: Some("root".into()),
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                ..Default::default()
            })],
            ..Default::default()
        };
        let mut local = LocalState {
            extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
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
    #[test]
    fn long_fact_kind_and_sequence_keep_entire_sha_visible_in_real_acceptance_width() {
        use crate::domain::VerifiedFact;
        use ratatui::{backend::TestBackend, Terminal};
        let digest = "ab".repeat(32);
        let snapshot = CoreSnapshot {
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                verified_fact: Some(VerifiedFact {
                    kind: "artifact_validation".into(),
                    sequence: 123456,
                    bytes: 8_388_608,
                    sha256: digest.clone(),
                }),
                ..Default::default()
            })],
            ..Default::default()
        };
        let local = LocalState {
            extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| draw(frame, frame.area(), &snapshot, &local))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let header: String = (0..3)
            .flat_map(|y| (0..120).map(move |x| buffer[(x, y)].symbol().to_owned()))
            .collect();
        assert!(header.contains("artifact_validation"));
        assert!(header.contains("123456"));
        assert!(header.contains(&digest), "{header}");
    }
    #[test]
    fn execution_state_precedes_large_group_ledger_and_component_detail_lists() {
        use crate::domain::{
            ArtifactBinding, ArtifactFact, ArtifactState, SourceGroupsFact, SourceLedgerFact,
        };
        let snapshot = CoreSnapshot {
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                proposal: Some(proposal(1)),
                application: Some("applied".into()),
                groups: Some(SourceGroupsFact {
                    scan_id: "scan".into(),
                    scene_sha256: "base".into(),
                    total_objects: 1000,
                    total_groups: 200,
                    assessed_groups: 200,
                    rows: (0..200).map(|i| format!("GROUP_ROW_{i}")).collect(),
                    counts: Default::default(),
                }),
                ledger: Some(SourceLedgerFact {
                    scan_id: "scan".into(),
                    scene_sha256: "base".into(),
                    rows: (0..1000).map(|i| format!("LEDGER_ROW_{i}")).collect(),
                    counts: Default::default(),
                }),
                artifacts: vec![ArtifactFact {
                    binding: ArtifactBinding {
                        build_id: "current-build".into(),
                        proposal_id: "proposal-1".into(),
                        revision: 1,
                        prepare_token: "token-1".into(),
                        candidate: false,
                        authority_sha256: "candidate".into(),
                        input_sha256: "candidate".into(),
                        output: "model.glb".into(),
                    },
                    state: ArtifactState::Built,
                    current: Some(true),
                    valid: Some(true),
                }],
                ..Default::default()
            })],
            ..Default::default()
        };
        let lines = business_lines(
            snapshot.extensions[0].modeling().unwrap(),
            &ViewContext::new(&snapshot, &ExtensionPanel::default(), "", None),
            120,
        );
        let rows: Vec<_> = lines.iter().map(|line| line.to_string()).collect();
        let validation = rows
            .iter()
            .position(|line| line.contains("valid=true · current=true"))
            .unwrap();
        assert!(
            validation < 30,
            "critical build result hidden after {validation} rows"
        );
        assert!(
            rows.iter()
                .position(|line| line.contains("GROUP_ROW_0"))
                .unwrap()
                > validation
        );
        assert!(
            rows.iter()
                .position(|line| line.contains("LEDGER_ROW_0"))
                .unwrap()
                > validation
        );
        assert!(rows.iter().any(|line| line.contains("GROUP_ROW_199")));
        assert!(rows.iter().any(|line| line.contains("LEDGER_ROW_999")));
        assert!(rows.iter().any(|line| line.contains("bed.1.body")));
    }

    #[test]
    fn control_failure_is_visible_before_large_proposal_details() {
        let mut p = proposal(1);
        p.objects = (0..2220).map(|i| format!("STRUCTURE_ENTITY_{i}")).collect();
        let snapshot = CoreSnapshot {
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                proposal: Some(p),
                notice: Some("批准结果超过大小上限".into()),
                ..Default::default()
            })],
            ..Default::default()
        };
        let panel = ExtensionPanel::default();
        let lines = business_lines(
            snapshot.extensions[0].modeling().unwrap(),
            &ViewContext::new(&snapshot, &panel, "", None),
            120,
        );
        assert!(lines
            .iter()
            .take(10)
            .any(|line| line.to_string().contains("批准结果超过大小上限")));
        assert!(lines
            .iter()
            .any(|line| line.to_string().contains("STRUCTURE_ENTITY_2219")));
    }

    #[test]
    fn building_success_keeps_source_attention_and_unchecked_connections_visible_without_approval()
    {
        let domain = DomainSnapshot {
            enabled: true,
            buildings: vec![crate::domain::ProjectBuildingFact {
                connection_authority: None,
                build_id: "building-job".into(),
                selection_id: "selection".into(),
                input_binding: "binding".into(),
                input_sha256: "sha".into(),
                state: "built".into(),
                current: true,
                valid: Some(true),
                floor_keys: vec!["f08".into(), "f09".into()],
                floor_authorities: Default::default(),
                source_reconciliation: "attention".into(),
                connection_check: "not_run".into(),
                report: Some(crate::domain::BuildingReportFact {
                    state: "attention".into(),
                    technical_status: "attention".into(),
                    current: true,
                    artifact_valid: true,
                    source_total: 487,
                    unresolved_sources: 222,
                    unresolved_drawing: 15,
                    missing_floor_reviews: 0,
                    connection_issues: 1,
                }),
                output: "building/model.glb".into(),
            }],
            drawing_audit: Some(crate::domain::DrawingAuditFact {
                audit_id: "audit".into(),
                scene_sha256: "scene-sha".into(),
                candidate_count: 41,
                judged_count: 0,
                ai_review: "not_run".into(),
                offset: 0,
                next_offset: Some(40),
                rows: vec!["gap · wall_gap · review required".into()],
            }),
            ..Default::default()
        };
        let extension = ExtensionSnapshot::Modeling(domain.clone());
        assert!(ModelingView.confirmation(&extension).is_none());
        let snapshot = CoreSnapshot {
            extensions: vec![extension],
            ..Default::default()
        };
        let panel = ExtensionPanel::default();
        for width in [60, 120] {
            let lines = business_lines(
                &domain,
                &ViewContext::new(&snapshot, &panel, "", None),
                width,
            );
            let text = lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("楼栋 building-job · built · current=true · valid=true"));
            assert!(text.contains("来源 attention · 连接检查 not_run"));
            assert!(text.contains("总报告 attention · 技术 attention"));
            assert!(text.contains("来源未完成 222/487"));
            assert!(text.contains("图纸未决 15"));
            assert!(text.contains("图纸审查候选 41 项"));
            assert!(text.contains("下一页 40"));
            assert!(text.contains("人工复核 not_run"));
        }
    }
}
