//! Read-only evidence for the modeling lifecycle. dsh still owns its execution loop.
use super::{ArtifactState, DomainSnapshot, SelectionState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowNode {
    Target,
    Structure,
    Drawing,
    Interpret,
    Capability,
    Instance,
    Definition,
    Prepare,
    Feedback,
    Revision,
    Apply,
    Build,
    Review,
    Pending,
    Result,
    Reconcile,
}

impl WorkflowNode {
    pub const ALL: [Self; 16] = [
        Self::Target,
        Self::Structure,
        Self::Drawing,
        Self::Interpret,
        Self::Capability,
        Self::Instance,
        Self::Definition,
        Self::Prepare,
        Self::Feedback,
        Self::Revision,
        Self::Apply,
        Self::Build,
        Self::Review,
        Self::Pending,
        Self::Result,
        Self::Reconcile,
    ];

    pub fn number(self) -> usize {
        Self::ALL.iter().position(|node| *node == self).unwrap() + 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowEvidenceState {
    Unrecorded,
    Recorded,
    Waiting,
    Running,
    Attention,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowEvidence {
    pub node: WorkflowNode,
    pub state: WorkflowEvidenceState,
    pub detail: String,
}

impl DomainSnapshot {
    pub fn current_source_regions(&self) -> Option<&super::SourceRegionReviewFact> {
        self.source_regions.as_ref().filter(|regions| {
            self.current_linework().is_some_and(|linework| {
                linework.scan_id == regions.scan_id
                    && linework.scene_sha256 == regions.scene_sha256
                    && linework.region_count == regions.region_count
                    && (regions.structure_represented_region_count == 0
                        || self.current_existing_structure().is_some_and(|coverage| {
                            coverage.complete_member_count
                                >= regions.structure_represented_region_count
                        }))
            })
        })
    }

    pub fn source_region_review_detail(&self) -> String {
        match self.current_source_regions() {
            Some(regions) => format!(
                "通用区域判读：{} 个区域 · 已判读 {} · 未判读 {} · 不确定 {} · 过期 {} · 失去源成员的判读 {} · 结构已表示 {}；区域不等于物体，判读不代表已建模；结构表示不等于 AI 已判读",
                regions.region_count, regions.reviewed_region_count, regions.unreviewed_region_count,
                regions.uncertain_region_count, regions.stale_review_count, regions.orphaned_stale_review_count,
                regions.structure_represented_region_count
            ),
            None if self.source_regions.is_some() => {
                "通用区域判读已过期；需读取当前裁图、附近文字和尺寸重新判读".into()
            }
            None => "通用区域判读未知；需读取区域裁图、附近文字和尺寸，由 AI 判读所有类别".into(),
        }
    }

    pub fn current_existing_structure(&self) -> Option<&super::ExistingStructureCoverageFact> {
        self.existing_structure.as_ref().filter(|coverage| {
            self.current_linework().is_some_and(|linework| {
                linework.scan_id == coverage.scan_id
                    && linework.scene_sha256 == coverage.scene_sha256
                    && linework.member_count == coverage.member_count
            })
        })
    }

    pub fn existing_structure_detail(&self) -> String {
        match self.current_existing_structure() {
            Some(coverage) => format!(
                "既有结构覆盖：完整 {}、部分 {}、仅参考 {}、无证据 {}；未选且已完整覆盖 {}；不改写来源选择及对象建模状态",
                coverage.complete_member_count, coverage.partial_member_count,
                coverage.reference_only_member_count, coverage.unproven_member_count,
                coverage.unselected_complete_member_count
            ),
            None if self.existing_structure.is_some() => {
                "既有结构覆盖已过期；未选来源成员仍需逐项核对".into()
            }
            None => "既有结构覆盖未知；未选来源成员仍需逐项核对".into(),
        }
    }

    /// Approved openings remain separate from discovered source candidates.
    pub fn current_openings(&self) -> Option<&super::OpeningInventoryFact> {
        self.openings.as_ref().filter(|inventory| {
            self.groups.as_ref().is_none_or(|groups| {
                inventory
                    .scan_id
                    .as_ref()
                    .is_none_or(|scan| &groups.scan_id == scan)
                    && groups.scene_sha256 == inventory.scene_sha256
            }) && self.ledger.as_ref().is_none_or(|ledger| {
                inventory
                    .scan_id
                    .as_ref()
                    .is_none_or(|scan| &ledger.scan_id == scan)
                    && ledger.scene_sha256 == inventory.scene_sha256
            }) && self.operation.as_ref().is_none_or(|operation| {
                operation.state == "applied" && operation.current_sha256 == inventory.scene_sha256
            }) && self.proposal.as_ref().is_none_or(|proposal| {
                &inventory.scene_sha256
                    == if matches!(proposal.state.as_str(), "applied" | "built") {
                        &proposal.binding.candidate_sha256
                    } else {
                        &proposal.binding.base_sha256
                    }
            })
        })
    }

    pub fn opening_inventory_detail(&self) -> String {
        match self.current_openings() {
            Some(inventory) => format!(
                "{}：{} 个；洞口已核验 {}、待核验 {}；门窗实体已建 {}、缺失 {}、待核验 {}；洞口或实体未核验项 {}",
                if inventory.inventory_verified { "已批准洞口清单已核验" } else { "洞口清单未核验批准依据" },
                inventory.total_count,
                inventory.verified_aperture_count,
                inventory.total_count - inventory.verified_aperture_count,
                inventory.complete_entity_count,
                inventory.missing_entity_count,
                inventory.total_count - inventory.complete_entity_count - inventory.missing_entity_count,
                inventory.unverified_count
            ),
            None if self.openings.is_some() => {
                "门窗洞口与实体清单已过期，需重新读取当前场景；完整性未知".into()
            }
            None => "门窗洞口与实体清单未知，尚未核对已批准洞口及实体缺口".into(),
        }
    }

    /// Coverage is current only alongside the same source view and applied authority.
    pub fn current_linework(&self) -> Option<&super::LineworkCoverageFact> {
        self.linework.as_ref().filter(|coverage| {
            (self.groups.is_some() || self.ledger.is_some())
                && self.groups.as_ref().is_none_or(|groups| {
                    groups.scan_id == coverage.scan_id
                        && groups.scene_sha256 == coverage.scene_sha256
                })
                && self.ledger.as_ref().is_none_or(|ledger| {
                    ledger.scan_id == coverage.scan_id
                        && ledger.scene_sha256 == coverage.scene_sha256
                })
                && self.operation.as_ref().is_none_or(|operation| {
                    operation.state != "applied"
                        || operation.current_sha256 == coverage.scene_sha256
                })
        })
    }

    /// Derive node evidence from the current bound facts; no task or edge is dispatched here.
    pub fn workflow_evidence(&self) -> Vec<WorkflowEvidence> {
        use WorkflowEvidenceState::*;
        use WorkflowNode::*;
        let proposal = self.proposal.as_ref();
        let operation = self.operation.as_ref();
        let applied = operation.is_some_and(|operation| {
            operation.state == "applied"
                && proposal.is_some_and(|p| operation.current_sha256 == p.binding.candidate_sha256)
        }) || proposal
            .is_some_and(|p| matches!(p.state.as_str(), "applied" | "built"));
        let uncertain = operation
            .is_some_and(|operation| matches!(operation.state.as_str(), "unknown" | "applying"));
        let artifact = self.artifacts.iter().rev().find(|artifact| {
            !artifact.binding.candidate
                && proposal.is_some_and(|p| {
                    artifact.binding.proposal_id == p.binding.proposal_id
                        && artifact.binding.revision == p.binding.revision
                        && artifact.binding.prepare_token == p.binding.prepare_token
                        && artifact.binding.input_sha256 == p.binding.candidate_sha256
                        && artifact.binding.authority_sha256 == p.binding.candidate_sha256
                })
        });
        let progress = self.component_progress();
        WorkflowNode::ALL
            .into_iter()
            .map(|node| {
                let (state, detail) = match node {
                    Target
                        if self.project_scope.is_some()
                            || self
                                .selection
                                .as_ref()
                                .is_some_and(|s| s.state == SelectionState::Selected)
                            || (self.project.is_none()
                                && !self.floor_id.is_empty()
                                && self.floor_id != "项目目标") =>
                    {
                        (Recorded, "已有目标绑定".into())
                    }
                    Structure if self.project_scope.is_some() => {
                        (Recorded, "已有楼层作用域；不代表整层完成".into())
                    }
                    Drawing => match &self.drawing_audit {
                        Some(audit) => (
                            Recorded,
                            format!(
                                "已判断 {}/{} 项；人工复核另行核对",
                                audit.judged_count, audit.candidate_count
                            ),
                        ),
                        None => (Unrecorded, "尚无审查回执".into()),
                    },
                    Interpret => {
                        let linework = self.current_linework();
                        let objects = match (&self.groups, progress) {
                            (Some(groups), _) => format!(
                                "对象已评估 {}/{} 组；未定类别 {} 个候选",
                                groups.assessed_groups,
                                groups.total_groups,
                                groups
                                    .category_counts
                                    .get("unresolved")
                                    .copied()
                                    .unwrap_or(0)
                            ),
                            (_, Some(progress)) => {
                                format!("已发现 {} 个来源；AI 分类尚未核对", progress.total)
                            }
                            _ => "对象尚无分组识别回执".into(),
                        };
                        let coverage_detail = match linework {
                            Some(coverage) => format!(
                                "通用纯线稿 {} 个成员，已选 {}、未选 {}；对象候选 {}、未分类 {}；区域 {}、注释 {}（不等于物体数）；未覆盖 {}、诊断 {}、过期选择 {}",
                                coverage.member_count,
                                coverage.selected_member_count,
                                coverage.unselected_member_count,
                                coverage.candidate_count,
                                coverage.unclassified_candidate_count,
                                coverage.region_count,
                                coverage.annotation_count,
                                coverage.uncovered_count,
                                coverage.diagnostic_member_count,
                                coverage.stale_selection_count
                            ),
                            None if self.linework.is_some() => {
                                "通用纯线稿覆盖已过期或未与当前来源绑定，需重新排查".into()
                            }
                            None => "通用纯线稿覆盖未知，尚未排查".into(),
                        };
                        let assessed = self.groups.as_ref().is_some_and(|groups| {
                            groups.assessed_groups == groups.total_groups
                                && groups
                                    .category_counts
                                    .get("unresolved")
                                    .copied()
                                    .unwrap_or(0)
                                    == 0
                        }) && linework.is_some_and(|coverage| {
                            coverage.unselected_member_count
                                == self.current_existing_structure().map_or(0, |structure| structure.unselected_complete_member_count)
                                && coverage.unclassified_candidate_count == 0
                                && coverage.uncovered_count == 0
                                && coverage.diagnostic_member_count == 0
                                && coverage.stale_selection_count == 0
                        }) && self.current_source_regions().is_some_and(|regions| {
                            regions.unreviewed_region_count == 0
                                && regions.uncertain_region_count == 0
                                && regions.stale_review_count == 0
                                && regions.orphaned_stale_review_count == 0
                        });
                        let state = if assessed {
                            Recorded
                        } else if self.groups.is_some()
                            || progress.is_some()
                            || self.linework.is_some()
                            || self.source_regions.is_some()
                        {
                            Attention
                        } else {
                            Unrecorded
                        };
                        (
                            state,
                            format!("{objects}；{coverage_detail}；{}；{}；识别回执不代表整层已建", self.existing_structure_detail(), self.source_region_review_detail()),
                        )
                    }
                    Prepare => match proposal {
                        Some(p) if !p.diagnostics.is_empty() => (
                            Attention,
                            format!(
                                "r{} · {} 项诊断；修订后重新校验",
                                p.binding.revision,
                                p.diagnostics.len()
                            ),
                        ),
                        Some(p) => (
                            Recorded,
                            format!(
                                "r{} · {}",
                                p.binding.revision,
                                if p.ready || applied {
                                    "已有准备回执"
                                } else {
                                    "尚未就绪"
                                }
                            ),
                        ),
                        None => (Unrecorded, "尚无具体提案".into()),
                    },
                    Feedback if uncertain => (Unknown, "先对账本版应用结果，不重复批准".into()),
                    Feedback if applied => {
                        (Recorded, "本版已有应用回执；后续版本须重新批准".into())
                    }
                    Feedback if self.authorizing => (Running, "正在提交具体版本授权".into()),
                    Feedback if proposal.is_some_and(|p| p.ready) => {
                        (Waiting, "当前版本已就绪；返回建模页核对具体批准状态".into())
                    }
                    Revision if proposal.is_some_and(|p| p.binding.revision > 1) => (
                        Recorded,
                        format!(
                            "当前 r{}；不据此推算循环次数",
                            proposal.unwrap().binding.revision
                        ),
                    ),
                    Apply if uncertain => (Unknown, "应用结果未确认；只读对账原操作".into()),
                    Apply if applied => (Recorded, "已有应用回执".into()),
                    Apply if operation.is_some() => (Attention, "核对原操作的终态及诊断".into()),
                    Build => match artifact {
                        Some(a) => match a.state {
                            ArtifactState::Unknown => {
                                (Unknown, "结果未知；对账原构建，不自动重放".into())
                            }
                            ArtifactState::Failed | ArtifactState::Cancelled => {
                                (Attention, "失败或取消；保留原产物与诊断".into())
                            }
                            ArtifactState::Building => (Running, "当前版本构建中".into()),
                            ArtifactState::Queued => (Waiting, "当前版本排队中".into()),
                            ArtifactState::Built
                                if a.current == Some(true) && a.valid == Some(true) =>
                            {
                                (Recorded, "当前版本已构建并验证；看图复核另有回执".into())
                            }
                            ArtifactState::Built => {
                                (Attention, "已有产物；有效性或当前性尚未确认".into())
                            }
                        },
                        None => (Unrecorded, "尚无本版正式构建回执；预览或旧版不替代".into()),
                    },
                    Pending if self.current_openings().is_none_or(|inventory| !inventory.complete()) => (
                        Attention,
                        format!("{}；来源台账待处理 {}；保留门窗缺口", self.opening_inventory_detail(), progress.map_or_else(|| "未知".into(), |p| p.pending.to_string())),
                    ),
                    Pending => match progress {
                        Some(p) if p.pending > 0 => (
                            Attention,
                            format!(
                                "待处理 {} · 已建 {} · 已跳过 {} · 已排除 {}",
                                p.pending, p.built, p.skipped, p.rejected
                            ),
                        ),
                        Some(_) => (Recorded, "来源台账当前无待处理；不代替其他检查".into()),
                        None => (Unrecorded, "尚无完整来源对账".into()),
                    },
                    Result => (Waiting, "结果与未决项分别报告；人工验收未执行".into()),
                    Reconcile
                        if uncertain
                            || artifact.is_some_and(|a| a.state == ArtifactState::Unknown) =>
                    {
                        (
                            Unknown,
                            "核对原 operation/build；不自动重放写入或构建".into(),
                        )
                    }
                    Review => (
                        Unrecorded,
                        "实际图像准入及观察须另有回执；网格有效不等于看图完成".into(),
                    ),
                    _ => (Unrecorded, "未记录；不由模型文字或后续节点反推执行".into()),
                };
                WorkflowEvidence {
                    node,
                    state,
                    detail,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ArtifactBinding, ArtifactFact, OperationFact, ProposalBinding, ProposalFact,
        SourceLedgerFact,
    };

    fn domain() -> DomainSnapshot {
        DomainSnapshot {
            enabled: true,
            proposal: Some(ProposalFact {
                binding: ProposalBinding {
                    run_id: "run".into(),
                    context_sha256: "context".into(),
                    proposal_id: "proposal".into(),
                    revision: 2,
                    prepare_token: "token".into(),
                    base_sha256: "base".into(),
                    candidate_sha256: "candidate".into(),
                },
                state: "prepared".into(),
                summary: "".into(),
                objects: vec![],
                assumptions: vec![],
                diagnostics: vec![],
                ready: true,
            }),
            ..Default::default()
        }
    }

    fn state(domain: &DomainSnapshot, node: WorkflowNode) -> WorkflowEvidenceState {
        domain
            .workflow_evidence()
            .into_iter()
            .find(|entry| entry.node == node)
            .unwrap()
            .state
    }

    fn linework() -> crate::domain::LineworkCoverageFact {
        crate::domain::LineworkCoverageFact {
            scan_id: "scan".into(),
            scene_sha256: "scene".into(),
            member_count: 10,
            selected_member_count: 10,
            unselected_member_count: 0,
            candidate_count: 2,
            unclassified_candidate_count: 0,
            region_count: 3,
            annotation_count: 4,
            uncovered_count: 0,
            diagnostic_member_count: 0,
            stale_selection_count: 0,
        }
    }

    fn regions() -> crate::domain::SourceRegionReviewFact {
        crate::domain::SourceRegionReviewFact {
            scan_id: "scan".into(),
            scene_sha256: "scene".into(),
            region_count: 3,
            reviewed_region_count: 3,
            unreviewed_region_count: 0,
            structure_represented_region_count: 0,
            uncertain_region_count: 0,
            stale_review_count: 0,
            orphaned_stale_review_count: 0,
        }
    }

    #[test]
    fn applied_structure_does_not_close_component_or_review_loops() {
        let mut domain = domain();
        domain.operation = Some(OperationFact {
            operation_id: "op".into(),
            state: "applied".into(),
            recovery: "candidate_present".into(),
            current_sha256: "candidate".into(),
        });
        domain.ledger = Some(SourceLedgerFact {
            scan_id: "scan".into(),
            scene_sha256: "candidate".into(),
            rows: vec![],
            counts: [("pending_confirmation".into(), 323)].into(),
        });
        assert_eq!(
            state(&domain, WorkflowNode::Apply),
            WorkflowEvidenceState::Recorded
        );
        assert_eq!(
            state(&domain, WorkflowNode::Pending),
            WorkflowEvidenceState::Attention
        );
        assert_eq!(
            state(&domain, WorkflowNode::Review),
            WorkflowEvidenceState::Unrecorded
        );
        assert_eq!(
            state(&domain, WorkflowNode::Result),
            WorkflowEvidenceState::Waiting
        );
        assert_eq!(
            state(&domain, WorkflowNode::Capability),
            WorkflowEvidenceState::Unrecorded
        );
        assert!(domain
            .workflow_evidence()
            .iter()
            .any(|entry| entry.detail.contains("待处理 323")));
    }

    #[test]
    fn unknown_operation_prevents_inferred_approval_and_requests_read_only_reconciliation() {
        let mut domain = domain();
        domain.operation = Some(OperationFact {
            operation_id: "op".into(),
            state: "unknown".into(),
            recovery: "unknown".into(),
            current_sha256: "base".into(),
        });
        for node in [
            WorkflowNode::Apply,
            WorkflowNode::Feedback,
            WorkflowNode::Reconcile,
        ] {
            assert_eq!(state(&domain, node), WorkflowEvidenceState::Unknown);
        }
    }

    #[test]
    fn stair_assessment_cannot_complete_missing_or_stale_generic_linework_coverage() {
        let mut domain = domain();
        domain.groups = Some(crate::domain::SourceGroupsFact {
            scan_id: "scan".into(),
            scene_sha256: "scene".into(),
            total_objects: 20,
            total_groups: 2,
            assessed_groups: 2,
            category_counts: [("hospital_bed".into(), 20)].into(),
            rows: vec![],
            counts: Default::default(),
        });
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
        domain.drawing_audit = Some(crate::domain::DrawingAuditFact {
            audit_id: "audit".into(),
            scene_sha256: "scene".into(),
            candidate_count: 2,
            judged_count: 1,
            ai_review: "partial".into(),
            offset: 0,
            next_offset: None,
            stair_linework: Some(crate::domain::StairLineworkFact {
                line_count: 100,
                region_count: 2,
                reviewed_region_count: 1,
            }),
            rows: vec![],
        });
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
        domain
            .drawing_audit
            .as_mut()
            .unwrap()
            .stair_linework
            .as_mut()
            .unwrap()
            .reviewed_region_count = 2;
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
        domain.linework = Some(linework());
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
        domain.source_regions = Some(regions());
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Recorded
        );
        assert!(domain
            .workflow_evidence()
            .iter()
            .any(|entry| entry.node == WorkflowNode::Interpret
                && entry.detail.contains("判读不代表已建模")));
        domain.groups.as_mut().unwrap().assessed_groups = 1;
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
        domain.groups.as_mut().unwrap().assessed_groups = 2;
        domain.linework.as_mut().unwrap().scene_sha256 = "old".into();
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
        domain.linework.as_mut().unwrap().scene_sha256 = "scene".into();
        domain.groups.as_mut().unwrap().scene_sha256 = "old".into();
        domain.operation = Some(OperationFact {
            operation_id: "latest".into(),
            state: "applied".into(),
            recovery: "not_required".into(),
            current_sha256: "new".into(),
        });
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
    }

    #[test]
    fn generic_linework_gaps_and_foreign_source_views_never_record_identification() {
        let mut domain = domain();
        domain.groups = Some(crate::domain::SourceGroupsFact {
            scan_id: "scan".into(),
            scene_sha256: "scene".into(),
            total_objects: 2,
            total_groups: 1,
            assessed_groups: 1,
            category_counts: [("unknown_equipment_type".into(), 2)].into(),
            rows: vec![],
            counts: [("pending_addition".into(), 2)].into(),
        });
        domain.linework = Some(linework());
        domain.source_regions = Some(regions());
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Recorded
        );
        assert_eq!(
            state(&domain, WorkflowNode::Pending),
            WorkflowEvidenceState::Attention
        );
        let gaps: [fn(&mut crate::domain::LineworkCoverageFact); 5] = [
            |coverage| coverage.unselected_member_count = 1,
            |coverage| coverage.unclassified_candidate_count = 1,
            |coverage| coverage.uncovered_count = 1,
            |coverage| coverage.diagnostic_member_count = 1,
            |coverage| coverage.stale_selection_count = 1,
        ];
        for gap in gaps {
            domain.linework = Some(linework());
            gap(domain.linework.as_mut().unwrap());
            assert_eq!(
                state(&domain, WorkflowNode::Interpret),
                WorkflowEvidenceState::Attention
            );
        }
        domain.linework = Some(linework());
        domain.groups.as_mut().unwrap().scan_id = "older-scan".into();
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
        domain.groups.as_mut().unwrap().scan_id = "scan".into();
        domain.ledger = Some(SourceLedgerFact {
            scan_id: "another-scan".into(),
            scene_sha256: "scene".into(),
            rows: vec![],
            counts: Default::default(),
        });
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
        domain.ledger.as_mut().unwrap().scan_id = "scan".into();
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Recorded
        );
        domain.ledger.as_mut().unwrap().scene_sha256 = "older-scene".into();
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
        domain.ledger = None;
        domain.operation = Some(OperationFact {
            operation_id: "latest".into(),
            state: "applied".into(),
            recovery: "not_required".into(),
            current_sha256: "new-scene".into(),
        });
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
    }

    #[test]
    fn preview_and_stale_builds_never_mark_current_build_or_visual_review_recorded() {
        let mut domain = domain();
        domain.artifacts.push(ArtifactFact {
            binding: ArtifactBinding {
                build_id: "build".into(),
                proposal_id: "proposal".into(),
                revision: 2,
                prepare_token: "token".into(),
                candidate: false,
                authority_sha256: "candidate".into(),
                input_sha256: "candidate".into(),
                output: "out".into(),
            },
            state: ArtifactState::Built,
            current: Some(true),
            valid: Some(true),
            reviewed: true,
        });
        assert_eq!(
            state(&domain, WorkflowNode::Build),
            WorkflowEvidenceState::Recorded
        );
        assert_eq!(
            state(&domain, WorkflowNode::Review),
            WorkflowEvidenceState::Unrecorded
        );
        domain.artifacts[0].binding.candidate = true;
        assert_eq!(
            state(&domain, WorkflowNode::Build),
            WorkflowEvidenceState::Unrecorded
        );
        domain.artifacts[0].binding.candidate = false;
        domain.artifacts[0].binding.revision = 1;
        assert_eq!(
            state(&domain, WorkflowNode::Build),
            WorkflowEvidenceState::Unrecorded
        );
        domain.artifacts[0].binding.revision = 2;
        domain.artifacts[0].current = Some(false);
        assert_eq!(
            state(&domain, WorkflowNode::Build),
            WorkflowEvidenceState::Attention
        );
    }

    #[test]
    fn opening_inventory_keeps_missing_unverified_and_expired_work_pending() {
        let mut domain = domain();
        domain.proposal = None;
        domain.ledger = Some(SourceLedgerFact {
            scan_id: "scan".into(),
            scene_sha256: "scene".into(),
            rows: vec![],
            counts: [("built".into(), 2)].into(),
        });
        assert_eq!(
            state(&domain, WorkflowNode::Pending),
            WorkflowEvidenceState::Attention
        );
        domain.openings = Some(crate::domain::OpeningInventoryFact {
            scan_id: Some("scan".into()),
            scene_sha256: "scene".into(),
            inventory_verified: true,
            total_count: 2,
            verified_aperture_count: 2,
            missing_entity_count: 0,
            complete_entity_count: 2,
            unverified_count: 0,
        });
        assert_eq!(
            state(&domain, WorkflowNode::Pending),
            WorkflowEvidenceState::Recorded
        );
        for alter in [
            |fact: &mut crate::domain::OpeningInventoryFact| fact.inventory_verified = false,
            |fact: &mut crate::domain::OpeningInventoryFact| fact.verified_aperture_count = 1,
            |fact: &mut crate::domain::OpeningInventoryFact| {
                fact.missing_entity_count = 1;
                fact.complete_entity_count = 1;
            },
            |fact: &mut crate::domain::OpeningInventoryFact| fact.unverified_count = 1,
            |fact: &mut crate::domain::OpeningInventoryFact| fact.scene_sha256 = "old".into(),
            |fact: &mut crate::domain::OpeningInventoryFact| fact.scan_id = Some("old".into()),
        ] {
            let complete = domain.openings.clone();
            alter(domain.openings.as_mut().unwrap());
            assert_eq!(
                state(&domain, WorkflowNode::Pending),
                WorkflowEvidenceState::Attention
            );
            domain.openings = complete;
        }
        domain.openings.as_mut().unwrap().scan_id = None;
        assert!(domain.current_openings().is_some());
        let mut empty_unverified = domain.openings.clone().unwrap();
        empty_unverified.inventory_verified = false;
        empty_unverified.total_count = 0;
        empty_unverified.verified_aperture_count = 0;
        empty_unverified.complete_entity_count = 0;
        assert!(!empty_unverified.complete());
        domain.operation = Some(OperationFact {
            operation_id: "new".into(),
            state: "applied".into(),
            recovery: "not_required".into(),
            current_sha256: "new".into(),
        });
        assert!(domain.current_openings().is_none());
        assert!(domain.opening_inventory_detail().contains("已过期"));
    }

    #[test]
    fn only_current_complete_structure_can_resolve_unselected_linework() {
        let mut domain = domain();
        domain.groups = Some(crate::domain::SourceGroupsFact {
            scan_id: "scan".into(),
            scene_sha256: "scene".into(),
            total_objects: 2,
            total_groups: 1,
            assessed_groups: 1,
            category_counts: [("chair".into(), 2)].into(),
            rows: vec![],
            counts: [("built".into(), 2)].into(),
        });
        domain.linework = Some(linework());
        domain.source_regions = Some(regions());
        domain.linework.as_mut().unwrap().selected_member_count = 4;
        domain.linework.as_mut().unwrap().unselected_member_count = 6;
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
        domain.existing_structure = Some(crate::domain::ExistingStructureCoverageFact {
            scan_id: "scan".into(),
            scene_sha256: "scene".into(),
            member_count: 10,
            complete_member_count: 6,
            partial_member_count: 1,
            reference_only_member_count: 1,
            unproven_member_count: 2,
            unselected_complete_member_count: 6,
        });
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Recorded
        );
        for alter in [
            |c: &mut crate::domain::ExistingStructureCoverageFact| {
                c.unselected_complete_member_count = 5
            },
            |c: &mut crate::domain::ExistingStructureCoverageFact| {
                c.complete_member_count = 0;
                c.reference_only_member_count = 7;
                c.unselected_complete_member_count = 0;
            },
            |c: &mut crate::domain::ExistingStructureCoverageFact| {
                c.complete_member_count = 0;
                c.partial_member_count = 7;
                c.unselected_complete_member_count = 0;
            },
            |c: &mut crate::domain::ExistingStructureCoverageFact| c.scan_id = "old".into(),
            |c: &mut crate::domain::ExistingStructureCoverageFact| c.scene_sha256 = "old".into(),
        ] {
            let complete = domain.existing_structure.clone();
            alter(domain.existing_structure.as_mut().unwrap());
            assert_eq!(
                state(&domain, WorkflowNode::Interpret),
                WorkflowEvidenceState::Attention
            );
            domain.existing_structure = complete;
        }
        for alter in [
            |c: &mut crate::domain::LineworkCoverageFact| c.unclassified_candidate_count = 1,
            |c: &mut crate::domain::LineworkCoverageFact| c.uncovered_count = 1,
            |c: &mut crate::domain::LineworkCoverageFact| c.diagnostic_member_count = 1,
            |c: &mut crate::domain::LineworkCoverageFact| c.stale_selection_count = 1,
        ] {
            let complete = domain.linework.clone();
            alter(domain.linework.as_mut().unwrap());
            assert_eq!(
                state(&domain, WorkflowNode::Interpret),
                WorkflowEvidenceState::Attention
            );
            domain.linework = complete;
        }
        assert_eq!(domain.linework.as_ref().unwrap().unselected_member_count, 6);
        assert_eq!(domain.component_progress().unwrap().total, 2);
        assert_eq!(
            state(&domain, WorkflowNode::Pending),
            WorkflowEvidenceState::Attention
        );
    }

    #[test]
    fn generic_region_review_is_required_and_uncertainty_is_never_model_completion() {
        let mut domain = domain();
        domain.groups = Some(crate::domain::SourceGroupsFact {
            scan_id: "scan".into(),
            scene_sha256: "scene".into(),
            total_objects: 2,
            total_groups: 1,
            assessed_groups: 1,
            category_counts: [("chair".into(), 2)].into(),
            rows: vec![],
            counts: [("built".into(), 2)].into(),
        });
        domain.linework = Some(linework());
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
        domain.source_regions = Some(regions());
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Recorded
        );
        domain.existing_structure = Some(crate::domain::ExistingStructureCoverageFact {
            scan_id: "scan".into(),
            scene_sha256: "scene".into(),
            member_count: 10,
            complete_member_count: 1,
            partial_member_count: 0,
            reference_only_member_count: 0,
            unproven_member_count: 9,
            unselected_complete_member_count: 0,
        });
        let review = domain.source_regions.as_mut().unwrap();
        review.reviewed_region_count = 2;
        review.structure_represented_region_count = 1;
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Recorded
        );
        assert!(domain.source_region_review_detail().contains("已判读 2"));
        assert!(domain
            .source_region_review_detail()
            .contains("结构已表示 1"));
        assert!(domain
            .source_region_review_detail()
            .contains("结构表示不等于 AI 已判读"));
        domain.existing_structure.as_mut().unwrap().scene_sha256 = "stale".into();
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Attention
        );
        domain.existing_structure = None;
        domain.source_regions = Some(regions());
        for change in [
            |review: &mut crate::domain::SourceRegionReviewFact| {
                review.reviewed_region_count = 2;
                review.unreviewed_region_count = 1;
            },
            |review: &mut crate::domain::SourceRegionReviewFact| review.uncertain_region_count = 1,
            |review: &mut crate::domain::SourceRegionReviewFact| {
                review.orphaned_stale_review_count = 1;
            },
            |review: &mut crate::domain::SourceRegionReviewFact| {
                review.reviewed_region_count = 2;
                review.unreviewed_region_count = 1;
                review.stale_review_count = 1;
            },
            |review: &mut crate::domain::SourceRegionReviewFact| review.scan_id = "old".into(),
            |review: &mut crate::domain::SourceRegionReviewFact| review.scene_sha256 = "old".into(),
        ] {
            domain.source_regions = Some(regions());
            change(domain.source_regions.as_mut().unwrap());
            assert_eq!(
                state(&domain, WorkflowNode::Interpret),
                WorkflowEvidenceState::Attention
            );
        }
        domain.source_regions = Some(regions());
        assert_eq!(
            state(&domain, WorkflowNode::Interpret),
            WorkflowEvidenceState::Recorded
        );
        assert_eq!(
            state(&domain, WorkflowNode::Pending),
            WorkflowEvidenceState::Attention
        );
    }
}
