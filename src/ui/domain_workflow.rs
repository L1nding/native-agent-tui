//! Read-only node graph. Details and scheduler tasks are separate views.
use super::theme;
use crate::domain::{DomainSnapshot, WorkflowEvidence, WorkflowEvidenceState, WorkflowNode};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};

pub(super) fn domain(snapshot: &crate::state::CoreSnapshot) -> Option<&DomainSnapshot> {
    snapshot
        .extensions
        .iter()
        .filter_map(|extension| extension.modeling())
        .find(|domain| domain.enabled)
}

fn label(node: WorkflowNode) -> &'static str {
    use WorkflowNode::*;
    match node {
        Target => "选择楼层",
        Structure => "结构方案",
        Drawing => "图纸审查",
        Interpret => "对象发现与AI识别",
        Capability => "能力与已有定义",
        Instance => "实例新增/修改",
        Definition => "新定义及实例",
        Prepare => "准备校验",
        Feedback => "预览与具体批准",
        Revision => "修订/补资料",
        Apply => "应用当前版本",
        Build => "构建与回读",
        Review => "看图复核",
        Pending => "保留待处理",
        Result => "结果与未决项",
        Reconcile => "对账原操作",
    }
}

/// Main-flow order the progress line reads; branches report through these nodes.
const MAIN_PATH: [WorkflowNode; 11] = [
    WorkflowNode::Target,
    WorkflowNode::Structure,
    WorkflowNode::Drawing,
    WorkflowNode::Interpret,
    WorkflowNode::Capability,
    WorkflowNode::Prepare,
    WorkflowNode::Feedback,
    WorkflowNode::Apply,
    WorkflowNode::Build,
    WorkflowNode::Review,
    WorkflowNode::Result,
];

/// One line for the conversation: where the modeling work stands, from Core's
/// verified facts only (never inferred from the agent's words).
pub(super) fn progress_line(domain: &DomainSnapshot, width: usize) -> Option<Line<'static>> {
    use WorkflowEvidenceState::*;
    let evidence = domain.workflow_evidence();
    let state = |node: WorkflowNode| {
        evidence
            .iter()
            .find(|item| item.node == node)
            .map(|item| item.state)
    };
    // Something needing the user wins over work in progress, then waiting.
    let current = [Attention, Unknown, Running, Waiting]
        .into_iter()
        .find_map(|wanted| {
            MAIN_PATH
                .into_iter()
                .chain([WorkflowNode::Reconcile, WorkflowNode::Revision])
                .find(|node| state(*node) == Some(wanted))
                .map(|node| (node, wanted))
        })
        .or_else(|| {
            MAIN_PATH
                .into_iter()
                .rev()
                .find(|node| state(*node) == Some(Recorded))
                .map(|node| (node, Recorded))
        })?;
    let (marker, style) = status(current.1);
    let floor = crate::state::display_text(&domain.floor_title());
    let palette = theme::current();
    let mut spans = vec![Span::styled("◆ ", Style::default().fg(palette.accent))];
    if !floor.is_empty() && floor != "项目目标" {
        spans.push(Span::styled(format!("{floor} · "), palette.heading()));
    }
    spans.push(Span::styled(label(current.0).to_owned(), palette.heading()));
    spans.push(Span::raw(" · "));
    spans.push(Span::styled(marker.to_owned(), style));
    if width >= 48 {
        spans.push(Span::styled("  · F4 查看流程", palette.secondary()));
    }
    Some(Line::from(spans))
}

fn status(state: WorkflowEvidenceState) -> (&'static str, Style) {
    use WorkflowEvidenceState::*;
    let palette = theme::current();
    match state {
        Unrecorded => ("○ 未记录", palette.secondary()),
        Recorded => ("● 有回执", Style::default().fg(palette.accent)),
        Waiting => ("… 等待", Style::default().fg(palette.warning)),
        Running => ("▶ 进行中", Style::default().fg(palette.accent)),
        Attention => ("! 需处理", Style::default().fg(palette.warning)),
        Unknown => ("? 未知", Style::default().fg(palette.warning)),
    }
}

fn branches(node: WorkflowNode) -> &'static [&'static str] {
    use WorkflowNode::*;
    match node {
        Interpret => &[
            "全层图块、纯线稿与上下文；散线按图层、连通与邻近聚区域（区域不等于物体）",
            "区域裁图 + 附近文字 + 尺寸 → AI 开放类别判读",
            "AI 可拆分、合并、识别非构件或保留不确定",
            "家具、楼梯、电梯、设备等均为识别对象，类别不限于预设名称",
            "与图块共用选线/解释 → 复用或定义 → 准备校验 → 具体批准 → 建模",
            "已知类型复用 / 新类型定义 → 05；歧义澄清 → 10；缺能力 → 14",
            "通用纯线稿逐成员核对选择、分类及覆盖",
            "既有结构仅完整覆盖可抵扣未选待处理；部分覆盖、参考线和无证据仍需处理",
            "区域与注释数不是物体数；二维记录不等于已建对象",
            "缺通用判读汇总、未判读、不确定或过期区域 → 14 保留缺口",
            "旧判读完全失去当前源成员仍保留未决缺口，不能计为当前区域或整层完成",
            "缺覆盖汇总、未选、未分类、未覆盖、诊断或过期选择 → 14",
            "逐组保留依据、缺参和未覆盖范围；二维符号不等于已建三维对象",
        ],
        Capability => &[
            "复用已有定义 → 06 → 08",
            "组合现有能力 → 07 → 08",
            "缺能力 → 14 保留待处理",
        ],
        Prepare => &["校验就绪 → 09 预览与批准", "阻塞 → 10 修订/补资料 ↺04"],
        Feedback => &[
            "批准当前版本 → 11 应用",
            "修改意见/部分采纳 → 10 ↺04",
            "跳过/拒绝 → 14 保留记录",
        ],
        Revision => &["补充资料、解释与约束 ↺04", "修订生成新版本，重新校验及批准"],
        Apply => &[
            "有应用回执 → 12 构建",
            "结果未知 → 16 只读对账",
            "不自动重放写入",
        ],
        Build => &[
            "验证回执 → 13 看图复核",
            "失败可修正 → 10 ↺04",
            "结果未知 → 16 对账原构建",
        ],
        Review => &["问题/下一批且预算允许 ↺04", "复核到位 → 15 结果与未决项"],
        Pending => &[
            "保留来源问题、洞口待核验与门窗实体缺失 → 15 如实报告",
            "缺已批准洞口清单时完整性未知；洞口存在不等于门窗实体已建",
            "明确解决缺口后再提出新方案",
        ],
        Reconcile => &[
            "只读核对原 operation/build",
            "不自动重放；unknown 不视为完成",
        ],
        Instance | Definition => &["提出定义或实例 → 08 准备校验", "准备不代表已批准或已写入"],
        Result => &["未决项、人工及规范检查分别报告", "产物有效不代表整层完成"],
        _ => &["按实际回执进入下一节点"],
    }
}

fn card(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    entry: &WorkflowEvidence,
    selected: bool,
    route: Option<&str>,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let palette = theme::current();
    let (state, state_style) = status(entry.state);
    let border = if selected {
        palette.accent
    } else {
        palette.border
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border));
    if selected {
        block = block.title(" 当前选择 ");
    }
    let inner = if area.height >= 3 {
        block.inner(area)
    } else {
        area
    };
    if area.height >= 3 {
        frame.render_widget(block, area);
    }
    let title = Line::from(vec![
        Span::styled(format!("{:02} ", entry.node.number()), palette.secondary()),
        Span::styled(
            label(entry.node),
            if selected {
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        ),
        Span::raw("  "),
        Span::styled(
            if inner.width < 40 {
                state.split_whitespace().next().unwrap_or(state)
            } else {
                state
            },
            state_style,
        ),
    ]);
    let mut lines = vec![title];
    if let Some(route) = route {
        lines.push(Line::styled(route, palette.secondary()));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

pub(super) fn draw(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    domain: &DomainSnapshot,
    selected: usize,
) {
    use WorkflowNode::*;
    frame.render_widget(Clear, area);
    let outer = theme::focus_panel()
        .borders(Borders::ALL)
        .title(" 建模工作流 · F4 ")
        .title_bottom(" ↑↓ 选节点 · Tab 调度任务 · Enter 方案 · Esc 返回 ");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    if inner.height < 2 || inner.width < 8 {
        return;
    }
    let evidence = domain.workflow_evidence();
    let selected = selected.min(evidence.len() - 1);
    let selected_node = evidence[selected].node;
    let body = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(inner);
    frame.render_widget(
        Paragraph::new("主流程                       分支与循环 · 复核问题/下一批 ↺04（预算允许）")
            .style(theme::current().secondary()),
        body[0],
    );
    let (main, branch_area, detail) = if body[1].width >= 140 {
        let columns = Layout::horizontal([
            Constraint::Length(48),
            Constraint::Length(48),
            Constraint::Min(22),
        ])
        .spacing(3)
        .split(body[1]);
        (columns[0], Some(columns[1]), columns[2])
    } else if body[1].width >= 95 {
        let columns = Layout::horizontal([Constraint::Length(43), Constraint::Min(32)])
            .spacing(3)
            .split(body[1]);
        let right = Layout::vertical([Constraint::Length(25), Constraint::Min(4)])
            .spacing(1)
            .split(columns[1]);
        (columns[0], Some(right[0]), right[1])
    } else {
        let rows = Layout::vertical([Constraint::Min(5), Constraint::Length(7)])
            .spacing(1)
            .split(body[1]);
        (rows[0], None, rows[1])
    };
    let path = [
        Target, Structure, Drawing, Interpret, Capability, Prepare, Feedback, Apply, Build, Review,
        Result,
    ];
    let anchor = match selected_node {
        Instance | Definition => Capability,
        Revision => Prepare,
        Pending => Capability,
        Reconcile => Apply,
        node => node,
    };
    let index = path.iter().position(|node| *node == anchor).unwrap_or(0);
    let step = if main.height >= 43 {
        4
    } else if main.height >= 21 {
        2
    } else {
        1
    };
    let capacity = ((main.height + 1) / step).max(1) as usize;
    let first = index
        .saturating_sub(capacity.saturating_sub(1))
        .min(path.len().saturating_sub(capacity));
    for (row, node) in path.iter().skip(first).take(capacity).enumerate() {
        let y = main.y + row as u16 * step;
        let height = (step - 1).max(1).min(main.bottom().saturating_sub(y));
        card(
            frame,
            Rect::new(main.x + 3, y, main.width.saturating_sub(3), height),
            &evidence[node.number() - 1],
            *node == selected_node,
            None,
        );
        if step > 1 && y + height < main.bottom() && first + row + 1 < path.len() {
            frame.render_widget(
                Paragraph::new("↓").style(theme::current().secondary()),
                Rect::new(main.x + main.width / 2, y + height, 1, 1),
            );
        }
        // 回边使用独立左侧轨道，不穿过节点卡片。
        if (3..=9).contains(&(first + row)) {
            let corner = if first + row == 3 {
                "┌→"
            } else if first + row == 9 {
                "└↺"
            } else {
                "│"
            };
            frame.render_widget(
                Paragraph::new(corner).style(Style::default().fg(theme::current().accent)),
                Rect::new(main.x, y, 2, height),
            );
            if y + height < main.bottom() {
                frame.render_widget(
                    Paragraph::new("│").style(theme::current().secondary()),
                    Rect::new(main.x, y + height, 1, 1),
                );
            }
        }
    }
    if let Some(branch_area) = branch_area {
        let branch_step = (branch_area.height / 5).clamp(1, 5);
        for (row, (node, route)) in [
            (Instance, "05 复用 → 06 → 08"),
            (Definition, "05 组合 → 07 → 08"),
            (Revision, "08 阻塞 / 09 修改 → 10 ↺04"),
            (Pending, "缺能力 / 跳过 / 拒绝 → 14 → 15"),
            (Reconcile, "11/12 结果未知 → 16 · 不自动重放"),
        ]
        .into_iter()
        .enumerate()
        {
            let y = branch_area.y + row as u16 * branch_step;
            if y >= branch_area.bottom() {
                break;
            }
            card(
                frame,
                Rect::new(
                    branch_area.x,
                    y,
                    branch_area.width,
                    (branch_step - 1).max(1).min(branch_area.bottom() - y),
                ),
                &evidence[node.number() - 1],
                selected_node == node,
                Some(route),
            );
        }
        let y = branch_area.y + 26;
        if y + 4 <= branch_area.bottom() {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from("循环入口  ↺04 解释与识别"),
                    Line::from("修改 / 阻塞 / 可修正失败 → 10 → 04"),
                    Line::from("13 复核问题/下一批 → 04（预算允许）"),
                ])
                .style(theme::current().secondary())
                .wrap(Wrap { trim: false }),
                Rect::new(branch_area.x, y, branch_area.width, 4),
            );
        }
    }
    let entry = &evidence[selected];
    let (state, state_style) = status(entry.state);
    let mut lines = vec![
        Line::styled(
            format!("{:02}  {}", entry.node.number(), label(entry.node)),
            theme::current().heading(),
        ),
        Line::styled(state, state_style),
        Line::from(""),
    ];
    if detail.height < 12 {
        let compact = vec![
            Line::styled(
                format!("{:02} {}  {state}", entry.node.number(), label(entry.node)),
                theme::current().heading(),
            ),
            Line::from(branches(entry.node)[0]),
            Line::from(entry.detail.clone()),
        ];
        frame.render_widget(Paragraph::new(compact).wrap(Wrap { trim: false }), detail);
        return;
    }
    lines.push(Line::from(entry.detail.clone()));
    lines.push(Line::from(""));
    lines.push(Line::styled("下一步与回路", theme::current().heading()));
    for route in branches(entry.node) {
        lines.push(Line::from(*route));
    }
    if detail.height >= 15 {
        lines.extend([
            Line::from(""),
            Line::styled("节点图是路线；状态来自回执。", theme::current().secondary()),
            Line::styled("有回执不等于整层完成。", theme::current().secondary()),
        ]);
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), detail);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn screen(width: u16, height: u16, selected: usize) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    frame.area(),
                    &DomainSnapshot {
                        enabled: true,
                        ..Default::default()
                    },
                    selected,
                )
            })
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
    fn modeling_graph_separates_main_path_branches_and_selected_evidence() {
        let text = screen(160, 50, WorkflowNode::Feedback.number() - 1).replace(' ', "");
        for label in [
            "建模工作流",
            "分支与循环",
            "实例新增/修改",
            "新定义及实例",
            "修订/补资料",
            "对账原操作",
            "修改意见/部分采纳",
            "↺04",
            "下一步与回路",
        ] {
            assert!(text.contains(label), "missing {label}: {text}");
        }
        assert!(!text.contains("Agent调度明细"));
        assert!(!text.contains("Root slots"));
    }

    fn progress(domain: &DomainSnapshot, width: usize) -> Option<String> {
        progress_line(domain, width).map(|line| line.to_string())
    }

    #[test]
    fn progress_line_names_the_floor_and_the_stage_from_verified_facts() {
        let mut domain = DomainSnapshot {
            enabled: true,
            floor_id: "八层".into(),
            proposal: Some(crate::domain::ProposalFact {
                binding: crate::domain::ProposalBinding {
                    run_id: "run".into(),
                    context_sha256: "context".into(),
                    proposal_id: "proposal".into(),
                    revision: 1,
                    prepare_token: "token".into(),
                    base_sha256: "base".into(),
                    candidate_sha256: "candidate".into(),
                },
                state: "prepared".into(),
                summary: String::new(),
                objects: vec![],
                assumptions: vec![],
                diagnostics: vec![],
                ready: true,
            }),
            ..Default::default()
        };
        let line = progress(&domain, 100).unwrap();
        assert!(line.starts_with("◆ 八层 · "), "{line}");
        assert!(line.contains("F4 查看流程"), "{line}");
        assert!(
            !line.contains("proposal") && !line.contains("token"),
            "{line}"
        );
        // A project session without a chosen floor shows the stage only.
        domain.floor_id = "项目目标".into();
        assert!(!progress(&domain, 40).unwrap().contains("项目目标"));
        assert!(!progress(&domain, 40).unwrap().contains("F4"));
    }

    #[test]
    fn selected_loop_nodes_remain_reachable_at_small_terminal_sizes() {
        for (width, height) in [(30, 18), (60, 24), (120, 22), (120, 40)] {
            for node in [
                WorkflowNode::Revision,
                WorkflowNode::Review,
                WorkflowNode::Reconcile,
            ] {
                let text = screen(width, height, node.number() - 1).replace(' ', "");
                assert!(
                    text.contains(label(node)),
                    "{width}x{height}, {node:?}: {text}"
                );
                assert!(text.contains("Tab"), "{text}");
                assert!(!text.contains("已完成"));
            }
        }
    }

    #[test]
    fn identification_node_routes_arbitrary_object_types_through_ai_and_capability_checks() {
        let text = screen(160, 50, WorkflowNode::Interpret.number() - 1)
            .split_whitespace()
            .collect::<String>();
        for expected in [
            "对象发现与AI识别",
            "全层图块、纯线稿与上下文",
            "区域裁图+附近文字+尺寸→AI开放类别判读",
            "旧判读完全失去当前源成员仍保留未决缺口",
            "类别不限于预设名称",
            "已知类型复用/新类型定义",
            "歧义澄清",
            "缺能力",
            "通用纯线稿逐成员核对",
            "区域与注释数不是物体数",
            "缺覆盖汇总",
            "二维符号不等于",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
    }
}
