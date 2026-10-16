//! Read-only modeling business view. Natural language uses the platform editor.
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::extensions::{ExtensionView, ViewContext};
use super::layout::wrap_text;
use crate::extensions::{ExtensionConfirmation, ExtensionId, ExtensionSnapshot};
use crate::state::display_text;

pub(super) fn floor_progress_lines(domain: &crate::domain::DomainSnapshot) -> Vec<Line<'static>> {
    let Some(progress) = domain.current_floor_progress() else {
        return vec![Line::from(
            "整层尚未完成：全层进度尚未对账 · 已建 / 待建 / 卡住 / 未识别 · 图纸覆盖率未知",
        )];
    };
    let mut lines = vec![Line::from(
        "类别               已建    待建    卡住  未识别",
    )];
    for row in &progress.categories {
        let name = if row.category == "unidentified" {
            "未识别来源"
        } else {
            crate::domain::review::category_label(&row.category)
        };
        let padding = " ".repeat(18usize.saturating_sub(name.width()));
        lines.push(Line::from(format!(
            "{}{padding}{:>6}  {:>6}  {:>6}  {:>6}",
            display_text(name),
            row.built,
            row.pending,
            row.blocked,
            row.unidentified
        )));
    }
    let percentage = if progress.total == 0 {
        "未知".to_owned()
    } else {
        format!(
            "{:.1}%",
            progress.covered as f64 * 100.0 / progress.total as f64
        )
    };
    lines.push(Line::from(format!(
        "图纸覆盖率 {percentage}（{}/{} · {}）· 未判读散线区域 {} · F6 查看详情",
        progress.covered,
        progress.total,
        display_text(&progress.coverage_unit),
        progress.unreviewed_regions
    )));
    lines.push(Line::from(
        if progress
            .categories
            .iter()
            .any(|row| row.pending > 0 || row.blocked > 0 || row.unidentified > 0)
            || progress.total == 0
            || progress.covered < progress.total
            || progress.unreviewed_regions > 0
        {
            "整层尚未完成：仍有待建、卡住、未识别或未完成图纸覆盖的范围。"
        } else {
            "本表来源状态已对账；整层完成仍需结构、门窗及连接验收。"
        },
    ));
    lines
}

fn sections(area: Rect, reply: Option<&str>) -> [Rect; 5] {
    let reply_height = reply.filter(|_| area.height >= 20).map_or(0, |reply| {
        let rows = wrap_text(reply, usize::from(area.width.saturating_sub(2).max(1))).len();
        rows.saturating_add(2)
            .min(usize::from((area.height / 3).clamp(3, 10))) as u16
    });
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
    if let Some(review) = &domain.structure_review {
        let problems: Vec<_> = review.checks.iter().filter(|c| c.attention).collect();
        if problems.is_empty() {
            lines.push(Line::from(
                "结构自检：楼板覆盖、墙线与柱线识别、墙体连通均正常",
            ));
        } else {
            lines.push(Line::from(format!(
                "结构自检：发现 {} 个问题",
                problems.len()
            )));
            for check in problems {
                lines.push(Line::from(format!("  ! {}", check.message)));
                if !check.suggestion.is_empty() {
                    lines.push(Line::from(format!("    {}", check.suggestion)));
                }
            }
        }
    }
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
    if let Some(p) = &domain.proposal {
        lines.push(Line::from(format!(
            "方案 · 修订 {} · {}",
            p.binding.revision,
            match p.state.as_str() {
                "prepared" => "已准备，待批准",
                "blocked" => "有问题待处理",
                "applied" => "已应用",
                "built" => "已应用并构建",
                "superseded" => "已被新版本取代",
                other => other,
            }
        )));
        lines.push(Line::from(display_text(&p.summary)));
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
    if let Some(selection) = &domain.preview_selection {
        lines.extend(
            selection
                .details()
                .into_iter()
                .map(|line| Line::from(display_text(&line))),
        );
        lines.push(Line::from(
            "输入意见会携带该选中对象及版本上下文；选择不会启动 Agent。",
        ));
    }
    lines.push(Line::from(if domain.preview_connection.is_some() {
        "Blender 预览已开启：打开 Blender 的 Agent 预览页签即可自动连接"
    } else {
        "Blender 预览未开启"
    }));
    lines.extend(floor_progress_lines(domain));
    if let Some(review) = &domain.first_use_review {
        let opened = domain
            .review_images
            .iter()
            .filter(|image| image.required && image.externally_opened)
            .count();
        let total = domain
            .review_images
            .iter()
            .filter(|image| image.required)
            .count();
        lines.push(Line::from(format!(
            "首用三维/CAD对照：{} · 需 {} 组 · 必需图已外部打开 {opened}/{total}",
            match review.status.as_str() {
                "ready" => "可复核",
                "blocked" => "尚未满足",
                "not_required" => "无需首用对照",
                _ => "未知",
            },
            review.required_count
        )));
        if !domain.first_use_satisfied() {
            lines.push(Line::from(
                "批准前需本版真实Blender对照图，并成功外部打开所有必需图；附件交付不等于AI观察。",
            ));
        }
    }
    if domain.first_use_review.is_some() || !domain.review_images.is_empty() {
        let count = domain.review_images.len();
        lines.push(Line::from(format!(
            "本版 {count} 张图片 · Alt+←/→ 选图 · Ctrl+O 外部打开"
        )));
        if let Some(image) = domain
            .review_images
            .get(context.panel.selected_image.min(count.saturating_sub(1)))
        {
            lines.push(Line::from(format!(
                "[{}/{}] {} · {} · {}",
                context.panel.selected_image.min(count - 1) + 1,
                count,
                display_text(&image.label),
                match image.file_state {
                    crate::domain::ImageFileState::Available => "文件SHA已核对",
                    crate::domain::ImageFileState::Unavailable => "图片不可用",
                    _ => "正在校验图片",
                },
                if image.externally_opened {
                    "已外部打开"
                } else {
                    "尚未外部打开"
                }
            )));
            if let Some(path) = image.verified_path() {
                lines.push(Line::from(display_text(&path.to_string_lossy())));
            }
            if let Some(detail) = &image.detail {
                lines.extend(
                    wrap_text(&display_text(detail), usize::from(width.max(1)))
                        .into_iter()
                        .map(Line::from),
                );
            }
            if let Some(notice) = &image.notice {
                lines.push(Line::from(display_text(notice)));
            }
        }
    }
    if let Some(proposal) = domain.proposal.as_ref().filter(|proposal| proposal.ready) {
        if !domain.candidate_review_delivered(&proposal.binding) {
            lines.push(Line::from(
                "等待当前已验证的 Blender 候选复核图交付 Agent，之后才能批准。",
            ));
        } else if !domain.first_use_satisfied() {
            lines.push(Line::from("首用图复核尚未满足，暂不能批准。"));
        } else {
            lines.push(Line::from(if context.phase.can_submit() {
                "等待批准当前具体方案：按 Ctrl+Y 批准并继续。普通问答选项不生成场景授权。"
            } else {
                "方案已准备；Agent 当前仍在执行，结束本轮后才能批准。请勿在普通问答中重复批准。"
            }));
        }
    }
    lines.push(Line::from(
        "散线流程：聚区域 → 裁图/文字/尺寸 → AI 开放判读/拆合 → 与图块共用定义、批准及建模 · F4 查看流程",
    ));
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
            if !selection.selected_keys.is_empty() {
                lines.push(Line::from(format!(
                    "已选择 {}",
                    selection
                        .selected_keys
                        .iter()
                        .take(100)
                        .map(|key| project.floor_titles.get(key).unwrap_or(key).clone())
                        .collect::<Vec<_>>()
                        .join("、")
                )));
                if selection.selected_keys.len() > 100 {
                    lines.push(Line::from("其余已选楼层见完整事实"));
                }
            }
            // Drawing locations matter only while the user picks a duplicate drawing.
            if selection.state == crate::domain::SelectionState::NeedsSelection {
                for row in &selection.rows {
                    lines.push(Line::from(row.clone()));
                }
            }
        }
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
            lines.push(Line::from(format!(
                "散线判读 {} · 结构已表示 {} · 未判读 {} · 待确认 {} · 过期 {} · 失去对应 {} · 缺汇总楼层 {}",
                report.source_discovery.state,
                report.source_discovery.structure_represented_region_count,
                report.source_discovery.unreviewed_region_count,
                report.source_discovery.uncertain_region_count,
                report.source_discovery.stale_review_count,
                report.source_discovery.orphaned_stale_review_count,
                report.source_discovery.missing_floors.len(),
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
        if location.status == "needs_wall_layers" {
            lines.extend(location.rows.iter().cloned().map(Line::from));
        }
    }
    if let Some(scope) = &domain.project_scope {
        lines.push(Line::from(format!("当前楼层 {}", domain.floor_title())));
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
    lines.push(Line::from("来源详情（候选、区域与部件不等于物体）"));
    if let Some(groups) = &domain.groups {
        lines.push(Line::from(format!(
            "对象AI识别 · 已评估 {}/{} 组 · 未定类别 {} 个候选",
            groups.assessed_groups,
            groups.total_groups,
            groups
                .category_counts
                .get("unresolved")
                .copied()
                .unwrap_or(0)
        )));
        let labels = groups
            .category_counts
            .iter()
            .filter(|(category, count)| category.as_str() != "unresolved" && **count > 0)
            .map(|(category, count)| {
                format!(
                    "{} {count}",
                    display_text(crate::domain::review::category_label(category))
                )
            })
            .collect::<Vec<_>>();
        lines.push(Line::from(if labels.is_empty() {
            "尚无已标注类别；来源扫描数量不代表已识别构件".into()
        } else {
            format!(
                "已标注类别：{} · 分类仍保留实际依据与假设",
                labels.join("、")
            )
        }));
    }
    lines.push(Line::from(domain.opening_inventory_detail()));
    match domain.current_linework() {
        Some(coverage) => {
            lines.push(Line::from(format!(
                "通用纯线稿覆盖：{} 个来源成员 · 已选 {} · 未选 {}",
                coverage.member_count,
                coverage.selected_member_count,
                coverage.unselected_member_count
            )));
            lines.push(Line::from(format!(
                "对象候选 {} · 未分类 {} · 区域 {} · 注释 {}；区域和注释数不等于物体数",
                coverage.candidate_count,
                coverage.unclassified_candidate_count,
                coverage.region_count,
                coverage.annotation_count
            )));
            lines.push(Line::from(format!(
                "未覆盖 {} · 有诊断成员 {} · 过期选择 {}；覆盖记录不代表已建三维对象",
                coverage.uncovered_count,
                coverage.diagnostic_member_count,
                coverage.stale_selection_count
            )));
        }
        None if domain.linework.is_some() => lines.push(Line::from(
            "通用纯线稿覆盖已过期或未与当前来源绑定，需重新排查；旧计数不代表当前覆盖",
        )),
        None => lines.push(Line::from("通用纯线稿覆盖未知，尚未排查；所有类别均需核对")),
    }
    lines.push(Line::from(domain.existing_structure_detail()));
    lines.push(Line::from(domain.source_region_review_detail()));
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
    let business = sections(area, context.reply)[1];
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
    let chunks = sections(area, reply);
    let reply_height = chunks[2].height;
    frame.render_widget(
        Paragraph::new(format!(
            "建模 · {} · {} · 平台模型 {}\n{}",
            domain.floor_title(),
            super::lang::phase(context.phase),
            context.model.unwrap_or("default"),
            domain
                .fact_sync_notice
                .as_ref()
                .map(|notice| display_text(notice))
                .unwrap_or_default()
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
    let approval_hint = if let Some(proposal) = domain.proposal.as_ref().filter(|p| p.ready) {
        if !domain.candidate_review_delivered(&proposal.binding) {
            "等待已验证候选的复核图交付"
        } else if !domain.first_use_satisfied() {
            "等待首用必需图外部打开"
        } else if context.phase.can_submit() {
            "Ctrl+Y 批准已展示版本"
        } else {
            "等待本轮结束后批准"
        }
    } else {
        "等待本轮结束后批准"
    };
    frame.render_widget(Paragraph::new(format!("Ctrl+O 外部打开 · Alt+←/→ 选图 · {approval_hint} · Enter 反馈 · F4 工作流 · F6/Esc 会话 · PgUp/PgDn 滚动")), chunks[4]);
}

pub(super) struct ModelingView;

pub(super) fn approval_card_lines(
    domain: &crate::domain::DomainSnapshot,
    selected_image: usize,
) -> Vec<String> {
    let Some(proposal) = &domain.proposal else {
        return Vec::new();
    };
    let mut lines = vec![format!(
        "建模审批 · {} · 修订 {} · {}",
        display_text(&proposal.binding.proposal_id),
        proposal.binding.revision,
        display_text(&proposal.summary)
    )];
    lines.push(if domain.proposal_categories.is_empty() {
        "本版类别数量未知；详见选中对象清单".into()
    } else {
        format!(
            "本版选中对象：{}",
            domain
                .proposal_categories
                .iter()
                .map(|(category, count)| format!(
                    "{} {count}",
                    display_text(crate::domain::review::category_label(category))
                ))
                .collect::<Vec<_>>()
                .join("、")
        )
    });
    let mut assumptions = std::collections::BTreeSet::new();
    let mut grouped = std::collections::BTreeMap::<(String, String), usize>::new();
    for text in &proposal.assumptions {
        let structured = serde_json::from_str::<serde_json::Value>(text).ok();
        let item = structured.as_ref().and_then(|value| {
            let object = value.as_object()?;
            if object.len() != 3 || value["entity_id"].as_str()?.is_empty() {
                return None;
            }
            Some((value["code"].as_str()?, value["message"].as_str()?))
        });
        if let Some((code, message)) = item {
            *grouped.entry((code.into(), message.into())).or_default() += 1;
        } else {
            assumptions.insert(format!(
                "示意假设：{}",
                display_text(&crate::domain::review::evidence_labels(text))
            ));
        }
    }
    // Only entity identity differs within a group; complete per-entity evidence stays in F6.
    for ((code, message), count) in grouped {
        let code = if code == "assumed_value" {
            "示意参数"
        } else {
            &code
        };
        let message = match message.as_str() {
            "walls: CAD-derived structure; parameter basis retained as assumptions" => {
                "墙体：依据CAD重建；参数依据仍保留为假设"
            }
            "openings: CAD-derived structure; parameter basis retained as assumptions" => {
                "门窗洞口：依据CAD重建；参数依据仍保留为假设"
            }
            "slabs: CAD-derived structure; parameter basis retained as assumptions" => {
                "楼板：依据CAD重建；参数依据仍保留为假设"
            }
            "elements: CAD-derived structure; parameter basis retained as assumptions" => {
                "其他结构实体：依据CAD重建；参数依据仍保留为假设"
            }
            "constraints: CAD-derived structure; parameter basis retained as assumptions" => {
                "约束：依据CAD重建；参数依据仍保留为假设"
            }
            other => other,
        };
        assumptions.insert(format!(
            "示意假设（{count}项 · {}）：{}",
            display_text(code),
            display_text(message)
        ));
    }
    if let Some(review) = &domain.first_use_review {
        if !review.dimensions.is_empty() {
            assumptions.insert(format!(
                "首用对照图参数：{}项（详细尺寸与判读说明见 F6）",
                review.dimensions.len()
            ));
        }
    }
    if assumptions.is_empty() {
        lines.push("示意假设：本版未列出".into());
    }
    lines.extend(assumptions);
    let artifacts: Vec<_> = domain
        .artifacts
        .iter()
        .filter(|artifact| artifact.binding.candidate)
        .collect();
    if artifacts.is_empty() {
        lines.push("Blender构建：尚无本版产物".into());
    }
    for artifact in artifacts {
        lines.push(format!(
            "Blender构建 {} · {} · 验证：{} · 版本：{}",
            display_text(&artifact.binding.build_id),
            match artifact.state {
                crate::domain::ArtifactState::Queued => "排队中",
                crate::domain::ArtifactState::Building => "构建中",
                crate::domain::ArtifactState::Unknown => "结果未知",
                crate::domain::ArtifactState::Cancelled => "已取消",
                crate::domain::ArtifactState::Failed => "构建失败",
                crate::domain::ArtifactState::Built => "已构建",
            },
            match artifact.valid {
                Some(true) => "通过",
                Some(false) => "未通过",
                None => "未知",
            },
            match artifact.current {
                Some(true) => "当前",
                Some(false) => "已过期",
                None => "未知",
            }
        ));
    }
    lines.push("GLB：未知（当前投影未提供独立GLB校验事实）".into());
    let images = &domain.review_images;
    let required = images.iter().filter(|image| image.required).count();
    let opened = images
        .iter()
        .filter(|image| image.required && image.externally_opened)
        .count();
    lines.push(format!(
        "本版 {} 张图（对照 {} · 总览 {} · 局部 {}）· 必需图外部打开 {opened}/{required} · Ctrl+O 打开 · Alt+←/→ 选图",
        images.len(),
        images.iter().filter(|image| image.view == crate::domain::ReviewImageView::Comparison).count(),
        images.iter().filter(|image| image.view == crate::domain::ReviewImageView::Overview).count(),
        images.iter().filter(|image| image.view == crate::domain::ReviewImageView::Detail).count()
    ));
    if let Some(image) = images.get(selected_image.min(images.len().saturating_sub(1))) {
        lines.push(format!(
            "[{}/{}] {} · {} · {}",
            selected_image.min(images.len() - 1) + 1,
            images.len(),
            display_text(&image.label),
            if image.verified_path().is_some() {
                "SHA已核对"
            } else {
                "图片不可用/校验中"
            },
            if image.externally_opened {
                "已外部打开"
            } else {
                "未外部打开"
            }
        ));
        if let Some(notice) = &image.notice {
            lines.push(display_text(notice));
        }
    }
    lines.push("未纳入项：本提案仅包含上述选中对象；其他来源范围/剩余对象尚未完整核对，不能据此宣称整层完成。".into());
    for diagnostic in &proposal.diagnostics {
        lines.push(display_text(diagnostic));
    }
    if let Some(notice) = &domain.fact_sync_notice {
        lines.push(display_text(notice));
    }
    lines.push(
        if domain.candidate_review_delivered(&proposal.binding) && domain.first_use_satisfied() {
            "Ctrl+Y 批准本版 · F6 查看建模详情（外部打开不代表已经看见或完成AI观察）"
        } else {
            "尚未满足批准条件：需有效候选复核图及本版首用图外部打开 · F6 详情"
        }
        .into(),
    );
    lines
}

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
            .filter(|proposal| domain.candidate_review_delivered(&proposal.binding))
            .filter(|_| domain.first_use_satisfied())
            .map(|_| ExtensionConfirmation::Modeling(domain.confirmation()))
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
    fn floor_progress_domain() -> DomainSnapshot {
        DomainSnapshot {
            enabled: true,
            ledger: Some(crate::domain::SourceLedgerFact {
                scan_id: "scan".into(),
                scene_sha256: "scene".into(),
                rows: vec![],
                counts: [("built".into(), 999)].into(),
            }),
            linework: Some(crate::domain::LineworkCoverageFact {
                scan_id: "scan".into(),
                scene_sha256: "scene".into(),
                member_count: 100,
                selected_member_count: 90,
                unselected_member_count: 10,
                candidate_count: 0,
                unclassified_candidate_count: 0,
                region_count: 2,
                annotation_count: 0,
                uncovered_count: 10,
                diagnostic_member_count: 0,
                stale_selection_count: 0,
            }),
            floor_progress: Some(crate::domain::FloorProgress {
                scan_id: "scan".into(),
                scene_sha256: "scene".into(),
                categories: vec![
                    crate::domain::CategoryProgress {
                        category: "armchair".into(),
                        built: 322,
                        pending: 495,
                        blocked: 7,
                        unidentified: 0,
                    },
                    crate::domain::CategoryProgress {
                        category: "stair".into(),
                        built: 0,
                        pending: 0,
                        blocked: 13,
                        unidentified: 0,
                    },
                    crate::domain::CategoryProgress {
                        category: "unidentified".into(),
                        built: 0,
                        pending: 0,
                        blocked: 0,
                        unidentified: 2,
                    },
                ],
                covered: 90,
                total: 100,
                coverage_unit: "来源成员".into(),
                unreviewed_regions: 2,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn floor_progress_uses_complete_fact_counts_without_recounting_source_pages() {
        let domain = floor_progress_domain();
        let lines: Vec<_> = floor_progress_lines(&domain)
            .iter()
            .map(|line| {
                line.to_string()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        assert_eq!(lines[0], "类别 已建 待建 卡住 未识别");
        assert_eq!(lines[1], "扶手椅 322 495 7 0");
        assert_eq!(lines[2], "示意楼梯 0 0 13 0");
        assert_eq!(lines[3], "未识别来源 0 0 0 2");
        assert!(lines[4].contains("图纸覆盖率 90.0%（90/100 · 来源成员）"));
        assert!(lines[4].contains("未判读散线区域 2"));
        assert!(lines[5].contains("整层尚未完成"));
        assert!(!lines.join("\n").contains("999"));
    }

    #[test]
    fn floor_progress_requires_current_source_binding_and_does_not_invent_counts() {
        let mut domain = floor_progress_domain();
        domain.floor_progress.as_mut().unwrap().scan_id = "stale".into();
        let text = floor_progress_lines(&domain)[0].to_string();
        assert!(text.contains("尚未对账"));
        assert!(text.contains("图纸覆盖率未知"));
        assert!(!text.contains("322"));
        domain.floor_progress.as_mut().unwrap().scan_id = "scan".into();
        domain.operation = Some(crate::domain::OperationFact {
            operation_id: "new".into(),
            state: "applied".into(),
            recovery: "not_required".into(),
            current_sha256: "new-scene".into(),
        });
        assert!(floor_progress_lines(&domain)[0]
            .to_string()
            .contains("尚未对账"));
        domain.operation = None;
        domain.floor_progress = None;
        assert!(floor_progress_lines(&domain)[0]
            .to_string()
            .contains("尚未对账"));
    }

    #[test]
    fn reconciled_progress_does_not_claim_whole_floor_acceptance() {
        let mut domain = floor_progress_domain();
        let progress = domain.floor_progress.as_mut().unwrap();
        progress.categories.truncate(1);
        progress.categories[0].pending = 0;
        progress.categories[0].blocked = 0;
        progress.covered = progress.total;
        progress.unreviewed_regions = 0;
        let text = floor_progress_lines(&domain)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("图纸覆盖率 100.0%"));
        assert!(text.contains("整层完成仍需结构、门窗及连接验收"));
    }

    #[test]
    fn conversation_and_modeling_views_share_the_floor_progress_table() {
        let snapshot = CoreSnapshot {
            extensions: vec![ExtensionSnapshot::Modeling(floor_progress_domain())],
            ..Default::default()
        };
        for business in [false, true] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 75)).unwrap();
            let local = LocalState {
                extension: ExtensionPanel::new(business.then_some(ExtensionId::Modeling)),
                ..Default::default()
            };
            terminal
                .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let screen = (0..75)
                .map(|y| {
                    (0..160)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            let compact: String = screen.chars().filter(|c| !c.is_whitespace()).collect();
            for expected in [
                "类别已建待建卡住未识别",
                "扶手椅32249570",
                "示意楼梯00130",
                "未识别来源0002",
                "图纸覆盖率90.0%",
                "整层尚未完成",
            ] {
                assert!(
                    compact.contains(expected),
                    "{business}: missing {expected}: {screen}"
                );
            }
        }
    }

    #[test]
    fn structure_problems_and_the_floor_title_lead_the_page() {
        use crate::domain::{
            ProjectCatalogFact, ProjectScopeFact, StructureCheck, StructureReview,
        };
        let domain = DomainSnapshot {
            enabled: true,
            floor_id: "floor.f02.trial".into(),
            project: Some(ProjectCatalogFact {
                catalog_id: "c".repeat(64),
                project_path: "projects/h".into(),
                source_sha256: "s".repeat(64),
                total_floors: 1,
                floor_keys: vec!["f02".into()],
                floor_titles: [("f02".to_owned(), "二层平面".to_owned())].into(),
                component_source_layers: None,
                rows: vec!["f02 · 二层平面 · CAD (1,2)".into()],
            }),
            selection: Some(crate::domain::ProjectSelectionFact {
                selection_id: "selection".into(),
                catalog_id: "c".repeat(64),
                source_sha256: "s".repeat(64),
                target: "二层".into(),
                state: crate::domain::SelectionState::Selected,
                selected_keys: vec!["f02".into()],
                candidate_keys: vec!["f02".into()],
                rows: vec![],
            }),
            project_scope: Some(ProjectScopeFact {
                activation_revision: Some(1),
                assembly_binding: None,
                opening_completion_binding: None,
                wall_layer_choice: None,
                component_discovery: None,
                scope_id: "a".repeat(64),
                floor_key: "f02".into(),
                floor_id: "floor.f02.trial".into(),
                selection_id: "b".repeat(64),
                source_sha256: "s".repeat(64),
            }),
            structure_review: Some(StructureReview {
                attention: true,
                checks: vec![
                    StructureCheck {
                        attention: true,
                        message: "楼板覆盖了 50% 的墙体".into(),
                        suggestion: "向用户说明缺楼板的区域".into(),
                    },
                    StructureCheck {
                        attention: false,
                        message: "图纸墙线识别出 94%".into(),
                        suggestion: String::new(),
                    },
                ],
            }),
            ..Default::default()
        };
        assert_eq!(domain.floor_title(), "二层");
        let snapshot = CoreSnapshot {
            extensions: vec![ExtensionSnapshot::Modeling(domain)],
            ..Default::default()
        };
        let local = LocalState::default();
        let context = ViewContext::new(&snapshot, &local.extension, "", None);
        let text = business_lines(snapshot.extensions[0].modeling().unwrap(), &context, 120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        assert!(text.starts_with("结构自检：发现 1 个问题"), "{text}");
        assert!(text.contains("! 楼板覆盖了 50% 的墙体"), "{text}");
        assert!(!text.contains("94%"), "passing checks stay quiet: {text}");
        assert!(text.contains("已选择 二层平面"), "{text}");
        assert!(text.contains("当前楼层 二层"), "{text}");
        for hidden in [
            "CAD (1,2)",
            "floor.f02.trial",
            &"a".repeat(64),
            &"c".repeat(64),
            &"s".repeat(64),
        ] {
            assert!(!text.contains(hidden), "{hidden}: {text}");
        }
    }
    #[test]
    fn short_agent_reply_leaves_room_for_project_choices() {
        let area = Rect::new(0, 0, 120, 36);
        let without = sections(area, None);
        let short = sections(area, Some("请选择重复图纸"));
        assert_eq!(short[2].height, 3);
        assert_eq!(without[1].height - short[1].height, 3);
        let long = sections(area, Some(&"多行回复\n".repeat(40)));
        assert_eq!(long[2].height, 10);
        assert!(short[1].height > long[1].height);
    }
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

    #[test]
    fn selected_object_information_is_visible_in_conversation_and_business_views() {
        let selection:crate::preview::Selection = serde_json::from_value(serde_json::json!({
            "client_id":"gui", "sequence":1, "binding":proposal(1).binding,
            "snapshot_id":"a".repeat(64), "active_part_id":"bed.1.mattress", "active_layer":"candidate",
            "objects":[{"part_id":"bed.1.mattress", "entity_id":"bed.1.mattress", "instance_id":"bed.1",
            "category":"bed", "floor_id":"floor.ground", "source_ids":[], "feedback_targets":["bed.1.mattress"],
            "layer":"candidate", "diff":"added", "center":["1.5","1.7","0.5"], "dimensions":["0.9","2","0.2"], "diagnostics":[]}]})).unwrap();
        let snapshot = CoreSnapshot {
            phase: SessionPhase::Ready,
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                proposal: Some(proposal(1)),
                preview_selection: Some(selection),
                ..Default::default()
            })],
            ..Default::default()
        };
        for business in [false, true] {
            let backend = ratatui::backend::TestBackend::new(100, 40);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            let local = LocalState {
                extension: ExtensionPanel::new(business.then_some(ExtensionId::Modeling)),
                ..Default::default()
            };
            terminal
                .draw(|frame| super::super::render::draw(frame, &snapshot, &local))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(
                text.replace(' ', "").contains("Blender选中bed.1"),
                "{business}: {text}"
            );
            assert!(text.contains("bed.1.mattress"));
            if business {
                assert!(text.replace(' ', "").contains("尺寸0.9/2/0.2m"));
            }
        }
    }

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
    fn classified_components_are_visible_separately_from_unresolved_symbols_and_builds() {
        let snapshot = CoreSnapshot {
            extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                enabled: true,
                groups: Some(crate::domain::SourceGroupsFact {
                    scan_id: "scan".into(),
                    scene_sha256: "scene".into(),
                    total_objects: 10,
                    total_groups: 2,
                    assessed_groups: 1,
                    category_counts: [("候诊座椅".into(), 8), ("unresolved".into(), 2)].into(),
                    counts: [
                        ("pending_addition".into(), 8),
                        ("pending_confirmation".into(), 2),
                    ]
                    .into(),
                    rows: vec![],
                }),
                ..floor_progress_domain()
            })],
            ..Default::default()
        };
        let local = LocalState::default();
        let context = ViewContext::new(&snapshot, &local.extension, "", None);
        let text = business_lines(snapshot.extensions[0].modeling().unwrap(), &context, 120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("已评估 1/2 组"), "{text}");
        assert!(text.contains("候诊座椅 8"), "{text}");
        assert!(text.contains("未定类别 2 个候选"), "{text}");
        let table = floor_progress_lines(snapshot.extensions[0].modeling().unwrap());
        assert_eq!(
            table[1].to_string().split_whitespace().collect::<Vec<_>>(),
            ["扶手椅", "322", "495", "7", "0"]
        );
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
                        floor_titles: Default::default(),
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
            ] {
                assert!(screen.contains(text), "{width}: {text}");
            }
            // Internal action states stay out of the user's page.
            assert!(!screen.contains("not_granted"), "{width}");
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
                opening_completion_binding: None,
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
            for hidden in ["定位证据", "作用域", &"d".repeat(64), &"e".repeat(64)] {
                assert!(!screen.contains(hidden), "{hidden}: {screen}");
            }
        }
    }

    #[test]
    fn configured_and_derived_locations_do_not_show_internal_layer_evidence() {
        for status in ["configured", "derived"] {
            let snapshot = CoreSnapshot {
                extensions: vec![ExtensionSnapshot::Modeling(DomainSnapshot {
                    enabled: true,
                    project_location: Some(crate::domain::ProjectLocationFact {
                        location_id: "e".repeat(64),
                        catalog_id: "a".repeat(64),
                        selection_id: "c".repeat(64),
                        source_sha256: "b".repeat(64),
                        floor_key: "f02".into(),
                        status: status.into(),
                        layers: vec!["WALLS".into()],
                        rows: vec!["WALLS · 211 线实体".into()],
                    }),
                    ..Default::default()
                })],
                ..Default::default()
            };
            let panel = ExtensionPanel::default();
            let text = business_lines(
                snapshot.extensions[0].modeling().unwrap(),
                &ViewContext::new(&snapshot, &panel, "", None),
                120,
            )
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
            assert!(text.contains("楼层定位"), "{text}");
            for hidden in ["线实体", "WALLS", "定位证据", &"e".repeat(64)] {
                assert!(!text.contains(hidden), "{hidden}: {text}");
            }
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
            assert!(render(&local).contains("方案 · 修订"));
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
            let local = LocalState {
                extension: ExtensionPanel::new(Some(ExtensionId::Modeling)),
                ..Default::default()
            };
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
                        reviewed: true,
                    },
                    ArtifactFact {
                        binding: binding("build-failed"),
                        state: ArtifactState::Failed,
                        current: Some(true),
                        valid: Some(false),
                        reviewed: true,
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
    fn business_approval_rejects_a_rendered_version_after_an_unseen_update() {
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
        local.extension.displayed(ExtensionConfirmation::Modeling(
            displayed.binding.clone().into(),
        ));
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        super::super::handle_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &snapshot,
            &mut local,
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert!(local.notice.is_some());
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
    fn header_names_the_floor_and_phase_without_fact_digests() {
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
        // The header names the floor and phase; fact digests stay in the journal.
        assert!(header.replace(' ', "").contains("建模"), "{header}");
        assert!(!header.contains(&digest), "{header}");
        assert!(!header.contains("artifact_validation"), "{header}");
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
                    category_counts: Default::default(),
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
                    reviewed: true,
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
    fn object_identification_displays_open_categories_and_keeps_old_stair_evidence_unconfirmed() {
        let mut domain = DomainSnapshot {
            enabled: true,
            groups: Some(crate::domain::SourceGroupsFact {
                scan_id: "scan".into(),
                scene_sha256: "current".into(),
                total_objects: 3,
                total_groups: 1,
                assessed_groups: 1,
                category_counts: [("air_handling_unit".into(), 3)].into(),
                rows: vec![],
                counts: [("built".into(), 3)].into(),
            }),
            drawing_audit: Some(crate::domain::DrawingAuditFact {
                audit_id: "audit".into(),
                scene_sha256: "current".into(),
                candidate_count: 1,
                judged_count: 1,
                ai_review: "recorded".into(),
                offset: 0,
                next_offset: None,
                stair_linework: Some(crate::domain::StairLineworkFact {
                    line_count: 10,
                    region_count: 1,
                    reviewed_region_count: 1,
                }),
                rows: vec![],
            }),
            ..Default::default()
        };
        let snapshot = CoreSnapshot::default();
        let panel = ExtensionPanel::default();
        let context = ViewContext::new(&snapshot, &panel, "", None);
        let text = business_lines(&domain, &context, 120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("对象AI识别"));
        assert!(text.contains("air_handling_unit 3"));
        assert!(!text.contains("楼梯专层排查"));
        assert!(text.contains("通用纯线稿覆盖未知，尚未排查"));
        assert!(text.contains("既有结构覆盖未知"));
        assert!(text.contains("通用区域判读未知"));
        assert!(text.contains("整层尚未完成"));
        assert!(text.contains("全层进度尚未对账"));
        assert!(!text.contains("已建 3 · 待处理 0"));
        domain.linework = Some(crate::domain::LineworkCoverageFact {
            scan_id: "scan".into(),
            scene_sha256: "current".into(),
            member_count: 90,
            selected_member_count: 50,
            unselected_member_count: 40,
            candidate_count: 3,
            unclassified_candidate_count: 2,
            region_count: 5,
            annotation_count: 7,
            uncovered_count: 11,
            diagnostic_member_count: 13,
            stale_selection_count: 17,
        });
        domain.existing_structure = Some(crate::domain::ExistingStructureCoverageFact {
            scan_id: "scan".into(),
            scene_sha256: "current".into(),
            member_count: 90,
            complete_member_count: 40,
            partial_member_count: 10,
            reference_only_member_count: 20,
            unproven_member_count: 20,
            unselected_complete_member_count: 40,
        });
        domain.source_regions = Some(crate::domain::SourceRegionReviewFact {
            scan_id: "scan".into(),
            scene_sha256: "current".into(),
            region_count: 5,
            reviewed_region_count: 2,
            unreviewed_region_count: 2,
            structure_represented_region_count: 1,
            uncertain_region_count: 1,
            stale_review_count: 1,
            orphaned_stale_review_count: 2,
        });
        let text = business_lines(&domain, &context, 120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "90 个来源成员 · 已选 50 · 未选 40",
            "对象候选 3 · 未分类 2 · 区域 5 · 注释 7",
            "区域和注释数不等于物体数",
            "未覆盖 11 · 有诊断成员 13 · 过期选择 17",
            "覆盖记录不代表已建三维对象",
            "既有结构覆盖：完整 40、部分 10、仅参考 20、无证据 20",
            "未选且已完整覆盖 40",
            "不改写来源选择及对象建模状态",
            "通用区域判读：5 个区域 · 已判读 2 · 未判读 2 · 不确定 1 · 过期 1",
            "失去源成员的判读 2",
            "结构已表示 1",
            "结构表示不等于 AI 已判读",
            "区域不等于物体，判读不代表已建模",
        ] {
            assert!(
                text.replace('\n', "").contains(expected),
                "missing {expected}: {text}"
            );
        }
        assert!(text.contains("整层尚未完成"));
        domain.linework.as_mut().unwrap().scan_id = "old-scan".into();
        let text = business_lines(&domain, &context, 120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("通用纯线稿覆盖已过期"));
        assert!(text.contains("既有结构覆盖已过期"));
        assert!(text.contains("通用区域判读已过期"));
        assert!(!text.contains("既有结构覆盖：完整 40"));
        assert!(!text.contains("90 个来源成员"));
        domain.linework.as_mut().unwrap().scan_id = "scan".into();
        domain.groups.as_mut().unwrap().scene_sha256 = "new".into();
        let text = business_lines(&domain, &context, 120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("通用区域判读已过期"));
        assert!(text.contains("通用纯线稿覆盖已过期"));
        domain.groups.as_mut().unwrap().scene_sha256 = "current".into();
        domain.operation = Some(crate::domain::OperationFact {
            operation_id: "latest".into(),
            state: "applied".into(),
            recovery: "not_required".into(),
            current_sha256: "new".into(),
        });
        let text = business_lines(&domain, &context, 120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("通用区域判读已过期"));
        assert!(!text.contains("楼梯专层排查：10"));
        assert!(text.contains("通用纯线稿覆盖已过期"));
    }

    #[test]
    fn approved_apertures_do_not_hide_missing_or_unverified_door_and_window_entities() {
        let mut domain = DomainSnapshot {
            enabled: true,
            groups: Some(crate::domain::SourceGroupsFact {
                scan_id: "scan".into(),
                scene_sha256: "scene".into(),
                total_objects: 2,
                total_groups: 1,
                assessed_groups: 1,
                category_counts: [("chair".into(), 2)].into(),
                rows: vec![],
                counts: [("built".into(), 2)].into(),
            }),
            openings: Some(crate::domain::OpeningInventoryFact {
                scan_id: Some("scan".into()),
                scene_sha256: "scene".into(),
                inventory_verified: true,
                total_count: 4,
                verified_aperture_count: 3,
                missing_entity_count: 2,
                complete_entity_count: 1,
                unverified_count: 2,
            }),
            ..Default::default()
        };
        let snapshot = CoreSnapshot::default();
        let panel = ExtensionPanel::default();
        let context = ViewContext::new(&snapshot, &panel, "", None);
        let text = business_lines(&domain, &context, 120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("洞口已核验 3、待核验 1"));
        assert!(text.contains("门窗实体已建 1、缺失 2、待核验 1"));
        assert!(text.contains("洞口或实体未核验项 2"));
        assert!(text.contains("整层尚未完成"));
        domain.openings.as_mut().unwrap().scene_sha256 = "old".into();
        let text = business_lines(&domain, &context, 120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("清单已过期"));
        assert!(!text.contains("门窗实体已建 1"));
        domain.openings = None;
        let text = business_lines(&domain, &context, 120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("清单未知"));
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
                source_discovery: crate::domain::BuildingSourceDiscoveryFact {
                    state: "unknown".into(),
                    missing_floors: vec!["f08".into(), "f09".into()],
                    unresolved_member_count: 0,
                    unclassified_candidate_count: 0,
                    uncovered_count: 0,
                    diagnostic_member_count: 0,
                    stale_selection_count: 0,
                    region_count: 0,
                    unreviewed_region_count: 0,
                    structure_represented_region_count: 0,
                    uncertain_region_count: 0,
                    stale_review_count: 0,
                    orphaned_stale_review_count: 0,
                },
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
                    source_discovery: crate::domain::BuildingSourceDiscoveryFact {
                        state: "unknown".into(),
                        missing_floors: vec!["f08".into(), "f09".into()],
                        unresolved_member_count: 0,
                        unclassified_candidate_count: 0,
                        uncovered_count: 0,
                        diagnostic_member_count: 0,
                        stale_selection_count: 0,
                        region_count: 0,
                        unreviewed_region_count: 0,
                        structure_represented_region_count: 0,
                        uncertain_region_count: 0,
                        stale_review_count: 0,
                        orphaned_stale_review_count: 0,
                    },
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
                stair_linework: None,
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
            assert!(text.contains("通用区域判读未知"));
        }
    }
}
