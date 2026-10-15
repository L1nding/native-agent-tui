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
                    Interpret => match (&self.groups, progress) {
                        (Some(groups), _) => (
                            Recorded,
                            format!(
                                "已评估 {}/{} 组",
                                groups.assessed_groups, groups.total_groups
                            ),
                        ),
                        (_, Some(progress)) => (
                            Recorded,
                            format!("已发现 {}；分类证据另行核对", progress.total),
                        ),
                        _ => (Unrecorded, "尚无扫描或识别回执".into()),
                    },
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
}
