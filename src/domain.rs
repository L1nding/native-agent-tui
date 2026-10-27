//! Modeling business facts and human control, owned by platform Core.
//! No model client or workflow loop lives here. Only bound MCP facts are projected.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::config::{Config, McpServerConfig};
use crate::protocol::{ObservedTool, ObservedToolOutcome, ToolCategory};

mod component_edit;
pub(crate) mod review;
pub use review::{
    FirstUseReviewFact, ImageFileState, ReviewImageBinding, ReviewImageFact, ReviewImageView,
};
mod workflow;
pub use workflow::{WorkflowEvidence, WorkflowEvidenceState, WorkflowNode};
mod project;
pub(crate) mod receipt;
pub use project::{create_project, list_projects, NewProject, ProjectEntry, PROJECT_UNITS};
pub use project::{
    BuildingReportFact, BuildingSourceDiscoveryFact, ComponentDiscovery, DrawingAuditFact,
    ProjectAssemblyBinding, ProjectBuildingFact, ProjectCatalogFact, ProjectConnectionAuthority,
    ProjectLocationFact, ProjectOpeningCompletionBinding, ProjectScopeFact, ProjectSelectionFact,
    ProjectWallLayerChoice, SelectionState, StairLineworkFact,
};

#[derive(Clone, PartialEq, Eq)]
pub struct DomainFactData(pub(crate) Vec<u8>);

impl std::fmt::Debug for DomainFactData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DomainFactData")
            .field("bytes", &self.0.len())
            .finish()
    }
}

const CONTRACT: &str = "modeling-domain/v2";
const SERVER: &str = "modeling_domain";
const LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainMode {
    Floor,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainSetup {
    pub mode: DomainMode,
    pub context_path: PathBuf,
    pub repository: PathBuf,
    pub python: PathBuf,
    pub run_id: String,
    pub context_sha256: String,
    pub floor_id: String,
}

pub fn configure(mut config: Config) -> Result<Config, String> {
    if config.domain_preview && config.domain_context.is_none() && config.domain_project.is_none() {
        return Err("--domain-preview 需要 --domain-context 或 --domain-project".into());
    }
    if config.domain_context.is_none() && config.domain_project.is_none() {
        return Ok(config);
    }
    if config.domain_context.is_some() && config.domain_project.is_some() {
        return Err("不能同时指定领域上下文和项目".into());
    }
    if config
        .permission
        .as_deref()
        .is_some_and(|p| p != "read-only")
    {
        return Err("领域建模会话的通用工具必须使用 read-only；受控写入由领域工具执行".into());
    }
    config.permission = Some("read-only".into());
    let repository = normalized_path(&config.cwd)?;
    let python = project::modeler_python(&repository);
    if !python.is_file() {
        return Err("建模 Python 运行时不存在（py 或 .venv）".into());
    }
    if let Some(path) = config.domain_project.as_ref() {
        config.domain_context = Some(project::create_context(&repository, path)?);
    }
    let context_path = normalized_path(config.domain_context.as_ref().unwrap())?;
    let raw = std::fs::read(&context_path).map_err(|e| e.to_string())?;
    if raw.len() > 16 * 1024 {
        return Err("建模上下文超过 16 KiB".into());
    }
    let value: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let mode = match value["contract"].as_str() {
        Some("modeling-context/v2") => DomainMode::Floor,
        Some("modeling-project/v1") => DomainMode::Project,
        _ => return Err("不支持的建模上下文版本".into()),
    };
    let run_id = text(&value, "run_id")?;
    if run_id.is_empty()
        || run_id.len() > 80
        || !run_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("无效建模运行身份".into());
    }
    let context_root = normalized_path(Path::new(&text(&value, "project_root")?))?;
    if context_root != repository {
        return Err("建模上下文必须属于当前工作目录".into());
    }
    let floor_id = match mode {
        DomainMode::Floor => {
            let scene = normalized_path(&repository.join(text(&value, "scene_path")?))?;
            if !scene.starts_with(&repository) {
                return Err("场景超出项目范围".into());
            }
            text(&value["scope"], "floor_id")?
        }
        DomainMode::Project => {
            let path = PathBuf::from(text(&value, "project_path")?);
            let project = normalized_path(&repository.join(&path))?;
            if path.is_absolute()
                || !project.starts_with(&repository)
                || !project.join("project.yaml").is_file()
            {
                return Err("建模项目必须位于当前工作目录内并包含 project.yaml".into());
            }
            "项目目标".into()
        }
    };
    if config.mcp_servers.iter().any(|s| matches!(s, McpServerConfig::Stdio { name, .. } | McpServerConfig::Http { name, .. } if name == SERVER)) {
        return Err("建模领域 MCP 已被配置".into());
    }
    if mode == DomainMode::Floor {
        config.mcp_servers.push(McpServerConfig::Stdio {
            name: SERVER.into(),
            command: python.to_string_lossy().into_owned(),
            args: vec![
                "-m".into(),
                match mode {
                    DomainMode::Floor => "blender_agent.integrations.domain_mcp",
                    DomainMode::Project => "blender_agent.integrations.project_mcp",
                }
                .into(),
                "--context".into(),
                context_path.to_string_lossy().into_owned(),
            ],
            env: Vec::new(),
        });
    }
    config.domain_setup = Some(DomainSetup {
        mode,
        context_path,
        repository,
        python,
        run_id,
        context_sha256: format!("{:x}", Sha256::digest(&raw)),
        floor_id,
    });
    Ok(config)
}

pub(crate) struct HttpMcp {
    child: crate::owned_process::Child,
    drains: Vec<tokio::task::JoinHandle<()>>,
    patch: PathBuf,
}

pub(crate) fn http_mcp_patch(port: u16) -> String {
    format!("- insert:\n    - id: mcp-modeling-domain\n      name: '@deepseek-ai/dsh-mcp-client'\n      config:\n        serverName: modeling_domain\n        transport: streamable-http\n        url: http://127.0.0.1:{port}/mcp\n        toolCallTimeoutMs: 600000\n        failOnStartupError: true\n")
}

pub(crate) async fn write_http_mcp_patch(port: u16) -> std::io::Result<PathBuf> {
    let patch = std::env::temp_dir().join(format!(
        "native-agent-domain-{}-{port}-{}.yml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    tokio::fs::write(&patch, http_mcp_patch(port)).await?;
    Ok(patch)
}

pub(crate) async fn wait_domain_port(
    port: u16,
    timeout: std::time::Duration,
) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + timeout;
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "领域 MCP 端口就绪超时",
                ));
            }
            if std::net::TcpStream::connect_timeout(
                &address,
                remaining.min(std::time::Duration::from_millis(100)),
            )
            .is_ok()
            {
                return Ok(());
            }
            std::thread::sleep(remaining.min(std::time::Duration::from_millis(50)));
        }
    })
    .await
    .map_err(std::io::Error::other)?
}

impl HttpMcp {
    pub(crate) async fn start(config: &mut Config) -> std::io::Result<Option<Self>> {
        let Some(setup) = config
            .domain_setup
            .as_ref()
            .filter(|setup| setup.mode == DomainMode::Project)
        else {
            return Ok(None);
        };
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let mut command = crate::owned_process::Command::new(&setup.python);
        command
            .current_dir(&setup.repository)
            .args(["-m", "blender_agent.integrations.project_mcp", "--context"])
            .args([setup.context_path.as_os_str()])
            .args([
                "--transport",
                "streamable-http",
                "--port",
                &port.to_string(),
            ]);
        let child = crate::owned_process::spawn(command, crate::owned_process::Input::Null)?;
        let mut server = Self {
            child,
            drains: Vec::new(),
            patch: PathBuf::new(),
        };
        use tokio::io::AsyncReadExt;
        if let Some(mut stdout) = server.child.stdout.take() {
            server.drains.push(tokio::spawn(async move {
                let mut buffer = [0; 8192];
                while matches!(stdout.read(&mut buffer).await, Ok(n) if n > 0) {}
            }));
        }
        if let Some(mut stderr) = server.child.stderr.take() {
            server.drains.push(tokio::spawn(async move {
                let mut buffer = [0; 8192];
                while matches!(stderr.read(&mut buffer).await, Ok(n) if n > 0) {}
            }));
        }
        if let Err(error) = wait_domain_port(port, std::time::Duration::from_secs(30)).await {
            let _ = server.stop().await;
            return Err(error);
        }
        server.patch = write_http_mcp_patch(port).await?;
        config.domain_mcp_patch = Some(server.patch.clone());
        Ok(Some(server))
    }

    pub(crate) async fn stop(&mut self) -> std::io::Result<()> {
        #[cfg(windows)]
        self.child.shutdown_tree().await?;
        self.child.kill().await?;
        for drain in &self.drains {
            drain.abort();
        }
        Ok(())
    }
}

impl Drop for HttpMcp {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        for drain in &self.drains {
            drain.abort();
        }
        let _ = std::fs::remove_file(&self.patch);
    }
}

fn normalized_path(path: &Path) -> Result<PathBuf, String> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    let text = path.to_string_lossy();
    Ok(PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)))
}

fn text(value: &Value, key: &str) -> Result<String, String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(|| format!("缺少建模字段 {key}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalBinding {
    pub run_id: String,
    pub context_sha256: String,
    pub proposal_id: String,
    pub revision: u64,
    pub prepare_token: String,
    pub base_sha256: String,
    pub candidate_sha256: String,
}

fn proposal_operation_sequence_key(binding: &ProposalBinding) -> String {
    format!(
        "approval-operation:{}:{}:{}:{}:{}",
        binding.proposal_id,
        binding.revision,
        binding.prepare_token,
        binding.base_sha256,
        binding.candidate_sha256
    )
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalFact {
    pub binding: ProposalBinding,
    pub state: String,
    pub summary: String,
    pub objects: Vec<String>,
    pub assumptions: Vec<String>,
    pub diagnostics: Vec<String>,
    pub ready: bool,
    #[serde(default)]
    pub parameter_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLedgerFact {
    pub scan_id: String,
    pub scene_sha256: String,
    pub rows: Vec<String>,
    pub counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryProgress {
    pub category: String,
    pub built: usize,
    pub pending: usize,
    pub blocked: usize,
    pub unidentified: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloorProgress {
    pub scan_id: String,
    pub scene_sha256: String,
    pub categories: Vec<CategoryProgress>,
    pub covered: usize,
    pub total: usize,
    pub coverage_unit: String,
    pub unreviewed_regions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloorProgressV2 {
    pub completion: bool,
    pub completion_blockers: Vec<String>,
    pub disposition_counts: BTreeMap<String, usize>,
    pub cross_floor_unverified: usize,
    pub deferrals: usize,
    pub skips: usize,
    pub legacy_items: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloorAdvanceNeeds {
    pub needs_type: String,
    pub diagnostic_code: Option<String>,
    pub rejection_category: Option<String>,
    pub split_batch_allowed: Option<bool>,
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalCardRequest {
    pub binding: ProposalBinding,
    pub fact_sequence: u64,
    pub build_id: String,
    pub card_ref: String,
}

impl FloorAdvanceNeeds {
    pub fn blocks_auto_continue(&self) -> bool {
        self.needs_type == "approve_card"
            || self.needs_type == "blocked"
                && (matches!(
                    self.rejection_category.as_deref(),
                    Some("unclassified" | "platform_contract")
                ) || (self.split_batch_allowed == Some(false)
                    && self.next_action.as_deref().is_some_and(|action| {
                        action.starts_with("report") || action.starts_with("stop")
                    })))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceGroupsFact {
    pub scan_id: String,
    pub scene_sha256: String,
    pub total_objects: usize,
    pub total_groups: usize,
    pub assessed_groups: usize,
    pub category_counts: BTreeMap<String, usize>,
    pub rows: Vec<String>,
    pub counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineworkCoverageFact {
    pub scan_id: String,
    pub scene_sha256: String,
    pub member_count: usize,
    pub selected_member_count: usize,
    pub unselected_member_count: usize,
    pub candidate_count: usize,
    pub unclassified_candidate_count: usize,
    pub region_count: usize,
    pub annotation_count: usize,
    pub uncovered_count: usize,
    pub diagnostic_member_count: usize,
    pub stale_selection_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingStructureCoverageFact {
    pub scan_id: String,
    pub scene_sha256: String,
    pub member_count: usize,
    pub complete_member_count: usize,
    pub partial_member_count: usize,
    pub reference_only_member_count: usize,
    pub unproven_member_count: usize,
    pub unselected_complete_member_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRegionReviewFact {
    pub scan_id: String,
    pub scene_sha256: String,
    pub region_count: usize,
    pub reviewed_region_count: usize,
    pub unreviewed_region_count: usize,
    pub structure_represented_region_count: usize,
    pub uncertain_region_count: usize,
    pub stale_review_count: usize,
    pub orphaned_stale_review_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpeningInventoryFact {
    pub scan_id: Option<String>,
    pub scene_sha256: String,
    pub inventory_verified: bool,
    pub total_count: usize,
    pub verified_aperture_count: usize,
    pub missing_entity_count: usize,
    pub complete_entity_count: usize,
    pub unverified_count: usize,
}

impl OpeningInventoryFact {
    pub fn complete(&self) -> bool {
        self.inventory_verified
            && self.verified_aperture_count == self.total_count
            && self.complete_entity_count == self.total_count
            && self.missing_entity_count == 0
            && self.unverified_count == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationFact {
    pub operation_id: String,
    pub state: String,
    pub recovery: String,
    pub current_sha256: String,
}

/// One immutable build input; validation belongs to this identity only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactBinding {
    pub build_id: String,
    pub proposal_id: String,
    pub revision: u64,
    pub prepare_token: String,
    pub candidate: bool,
    pub authority_sha256: String,
    pub input_sha256: String,
    pub output: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactState {
    Queued,
    Building,
    Unknown,
    Cancelled,
    Failed,
    Built,
}

impl ArtifactState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Building => "building",
            Self::Unknown => "unknown",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Built => "built",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactFact {
    pub binding: ArtifactBinding,
    pub state: ArtifactState,
    /// Whether this job's authority is current at the last query; None is unknown.
    pub current: Option<bool>,
    pub valid: Option<bool>,
    /// A review image of this build was delivered by `read_artifact_review_image`;
    /// this proves delivery only, not successful visual analysis.
    pub reviewed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFact {
    pub kind: String,
    pub sequence: u64,
    pub sha256: String,
    pub bytes: usize,
}

impl std::fmt::Debug for ProposalFact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProposalFact")
            .field("revision", &self.binding.revision)
            .field("state", &self.state)
            .field("objects", &self.objects.len())
            .field("diagnostics", &self.diagnostics.len())
            .field("ready", &self.ready)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct DomainSnapshot {
    pub enabled: bool,
    pub floor_id: String,
    pub proposal: Option<ProposalFact>,
    pub approval_cards: Vec<ApprovalCard>,
    pub(crate) approval_card_restore: Option<ApprovalCardRestore>,
    pub approval_restore_blocked: bool,
    pub approval_restore_build_id: Option<String>,
    pub source_current: Option<bool>,
    pub proposal_not_approvable: bool,
    pub(crate) approval_card_tombstones: Vec<ProposalBinding>,
    pub proposal_categories: BTreeMap<String, usize>,
    pub project: Option<ProjectCatalogFact>,
    pub selection: Option<ProjectSelectionFact>,
    pub project_location: Option<ProjectLocationFact>,
    pub project_scope: Option<ProjectScopeFact>,
    /// Measured plausibility of the current floor structure, from the modeler.
    pub structure_review: Option<StructureReview>,
    pub structure_revision: Option<StructureRevisionFact>,
    pub buildings: Vec<ProjectBuildingFact>,
    pub drawing_audit: Option<DrawingAuditFact>,
    pub ledger: Option<SourceLedgerFact>,
    pub floor_progress: Option<FloorProgress>,
    pub floor_progress_v2: Option<FloorProgressV2>,
    pub floor_advance_needs: Option<FloorAdvanceNeeds>,
    pub approval_card_request: Option<ApprovalCardRequest>,
    pub floor_advance_auto_continue_paused: bool,
    pub groups: Option<SourceGroupsFact>,
    pub linework: Option<LineworkCoverageFact>,
    pub existing_structure: Option<ExistingStructureCoverageFact>,
    pub source_regions: Option<SourceRegionReviewFact>,
    pub openings: Option<OpeningInventoryFact>,
    pub application: Option<String>,
    pub operation: Option<OperationFact>,
    pub artifacts: Vec<ArtifactFact>,
    pub first_use_review: Option<FirstUseReviewFact>,
    pub review_images: Vec<ReviewImageFact>,
    pub authorizing: bool,
    pub notice: Option<String>,
    pub fact_sync_notice: Option<String>,
    pub rejected_facts: Vec<RejectedFact>,
    pub verified_fact: Option<VerifiedFact>,
    pub preview_connection: Option<String>,
    pub preview_selection: Option<crate::preview::Selection>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ApprovalCard {
    pub snapshot: Box<DomainSnapshot>,
    pub fact_sequence: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ApprovalCardRestore {
    pub proposal_id: String,
    pub build_id: Option<String>,
    pub fact_sequence: u64,
    pub ready: bool,
    pub awaiting_user: bool,
}

impl ApprovalCard {
    fn binding(&self) -> Option<&ProposalBinding> {
        self.snapshot
            .proposal
            .as_ref()
            .map(|proposal| &proposal.binding)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureRevisionFact {
    pub proposal: ProposalBinding,
    pub host_mapping_count: usize,
    pub changes: Vec<String>,
    pub void_decisions: Vec<String>,
    pub verification_questions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentProgress {
    pub total: usize,
    pub built: usize,
    pub pending: usize,
    pub skipped: usize,
    pub rejected: usize,
}

/// One measured check of a reconstructed floor: `ok` or needs attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureCheck {
    pub attention: bool,
    pub message: String,
    pub suggestion: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureReview {
    pub attention: bool,
    pub checks: Vec<StructureCheck>,
}

/// Bounded display text only; an unknown or malformed review is simply absent.
fn structure_review(value: &Value) -> Option<StructureReview> {
    if value["contract"] != "structure-review/v1" {
        return None;
    }
    let checks = value["checks"]
        .as_array()?
        .iter()
        .take(16)
        .map(|check| {
            let text = |key: &str| {
                check[key].as_str().map(|text| {
                    crate::state::display_text(&text.chars().take(240).collect::<String>())
                })
            };
            Some(StructureCheck {
                attention: check["status"] == "attention",
                message: text("message")?,
                suggestion: text("suggestion").unwrap_or_default(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(StructureReview {
        attention: checks.iter().any(|check| check.attention),
        checks,
    })
}

impl DomainSnapshot {
    pub fn approval_card_snapshot(&self) -> Box<Self> {
        let mut snapshot = self.clone();
        snapshot.approval_cards.clear();
        snapshot.approval_card_tombstones.clear();
        snapshot.approval_card_restore = None;
        Box::new(snapshot)
    }

    pub fn approval_card_snapshots(&self) -> Vec<&DomainSnapshot> {
        let mut cards = self
            .approval_cards
            .iter()
            .filter_map(|card| {
                let snapshot = if self
                    .proposal
                    .as_ref()
                    .is_some_and(|proposal| Some(&proposal.binding) == card.binding())
                {
                    self
                } else {
                    card.snapshot.as_ref()
                };
                let binding = snapshot.proposal.as_ref()?.binding.clone();
                (!self.approval_card_removed(&binding)
                    && snapshot.source_current != Some(false)
                    && !snapshot.proposal_not_approvable
                    && snapshot.application.is_none()
                    && snapshot.operation.is_none())
                .then_some((card.fact_sequence, snapshot))
            })
            .collect::<Vec<_>>();
        if let Some(proposal) = &self.proposal {
            if !self
                .approval_cards
                .iter()
                .any(|card| card.binding() == Some(&proposal.binding))
                && !self.approval_card_removed(&proposal.binding)
                && self.source_current != Some(false)
                && !self.proposal_not_approvable
                && self.application.is_none()
                && self.operation.is_none()
            {
                cards.push((u64::MAX, self));
            }
        }
        cards.sort_by(|(left_sequence, left), (right_sequence, right)| {
            self.approval_card_waiting_for(right)
                .cmp(&self.approval_card_waiting_for(left))
                .then_with(|| right_sequence.cmp(left_sequence))
        });
        cards.into_iter().map(|(_, card)| card).collect()
    }

    pub fn selected_approval_card(&self) -> Option<&DomainSnapshot> {
        let cards = self.approval_card_snapshots();
        self.restored_approval_selection()
            .and_then(|(_, binding)| {
                cards.iter().copied().find(|card| {
                    card.proposal
                        .as_ref()
                        .is_some_and(|p| p.binding == *binding)
                })
            })
            .or_else(|| cards.into_iter().next())
    }

    pub fn restored_approval_selection(&self) -> Option<(u64, &ProposalBinding)> {
        let restored = self.approval_card_restore.as_ref().filter(|r| r.ready)?;
        let card = self.restored_approval_card()?;
        let binding = &card.proposal.as_ref()?.binding;
        (self.approval_card_is_pending(binding)
            && restored.build_id.as_ref().is_none_or(|build_id| {
                card.artifacts
                    .iter()
                    .any(|artifact| artifact.binding.build_id == *build_id)
            }))
        .then_some((restored.fact_sequence, binding))
    }

    fn restored_approval_card(&self) -> Option<&DomainSnapshot> {
        let restored = self.approval_card_restore.as_ref()?;
        std::iter::once(self)
            .chain(
                self.approval_cards
                    .iter()
                    .map(|card| card.snapshot.as_ref()),
            )
            .find(|card| {
                card.proposal
                    .as_ref()
                    .is_some_and(|proposal| proposal.binding.proposal_id == restored.proposal_id)
            })
    }

    pub fn approval_restore_waiting(&self) -> bool {
        self.approval_card_restore.as_ref().is_some_and(|restored| {
            restored.awaiting_user
                && self.restored_approval_card().is_none_or(|card| {
                    // An older version's terminal state cannot settle a restore
                    // whose build and proposal facts have not arrived yet.
                    if restored.build_id.as_ref().is_some_and(|build_id| {
                        !card
                            .artifacts
                            .iter()
                            .any(|artifact| artifact.binding.build_id == *build_id)
                    }) {
                        return true;
                    }
                    card.proposal.as_ref().is_some_and(|proposal| {
                        !self.approval_card_removed(&proposal.binding)
                            && card.source_current != Some(false)
                            && !card.proposal_not_approvable
                            && card.operation.is_none()
                            && card.application.is_none()
                    })
                })
        })
    }

    pub fn approval_card(&self, binding: &ProposalBinding) -> Option<&DomainSnapshot> {
        if self
            .proposal
            .as_ref()
            .is_some_and(|proposal| proposal.binding == *binding)
        {
            return Some(self);
        }
        self.approval_cards
            .iter()
            .find(|card| card.binding() == Some(binding))
            .map(|card| card.snapshot.as_ref())
    }

    pub fn approval_card_mut(&mut self, binding: &ProposalBinding) -> Option<&mut DomainSnapshot> {
        if self
            .proposal
            .as_ref()
            .is_some_and(|proposal| proposal.binding == *binding)
        {
            return Some(self);
        }
        self.approval_cards
            .iter_mut()
            .find(|card| card.binding() == Some(binding))
            .map(|card| card.snapshot.as_mut())
    }

    fn proposal_binding_for_owner(&self, owner: &Value) -> Option<ProposalBinding> {
        let matches = |binding: &ProposalBinding| {
            let base_sha256 = owner["base_sha256"].as_str();
            let candidate_sha256 = owner["candidate_sha256"].as_str();
            owner["proposal_id"].as_str() == Some(binding.proposal_id.as_str())
                && owner["revision"].as_u64() == Some(binding.revision)
                && owner["prepare_token"].as_str() == Some(binding.prepare_token.as_str())
                && base_sha256.is_none_or(|sha256| sha256 == binding.base_sha256)
                && candidate_sha256.is_none_or(|sha256| sha256 == binding.candidate_sha256)
        };
        self.proposal
            .as_ref()
            .map(|proposal| &proposal.binding)
            .filter(|binding| matches(binding))
            .cloned()
            .or_else(|| {
                self.approval_cards
                    .iter()
                    .filter_map(ApprovalCard::binding)
                    .find(|binding| matches(binding))
                    .cloned()
            })
    }

    fn application_binding_for_owner(&self, owner: &Value) -> Option<ProposalBinding> {
        self.proposal_binding_for_owner(owner).or_else(|| {
            if !owner["revision"].is_null() {
                return None;
            }
            let proposal_id = owner["proposal_id"].as_str()?;
            let prepare_token = owner["prepare_token"].as_str()?;
            let base_sha256 = owner["base_sha256"].as_str();
            let candidate_sha256 = owner["candidate_sha256"].as_str();
            let matches = |binding: &ProposalBinding| {
                binding.proposal_id == proposal_id
                    && binding.prepare_token == prepare_token
                    && base_sha256.is_none_or(|sha256| sha256 == binding.base_sha256)
                    && candidate_sha256.is_none_or(|sha256| sha256 == binding.candidate_sha256)
            };
            let mut matching_bindings = Vec::new();
            if let Some(binding) = self
                .proposal
                .as_ref()
                .map(|proposal| &proposal.binding)
                .filter(|binding| matches(binding))
            {
                matching_bindings.push(binding.clone());
            }
            for binding in self
                .approval_cards
                .iter()
                .filter_map(ApprovalCard::binding)
                .filter(|binding| matches(binding))
            {
                if !matching_bindings.contains(binding) {
                    matching_bindings.push(binding.clone());
                }
            }
            (matching_bindings.len() == 1).then(|| matching_bindings.remove(0))
        })
    }

    fn replace_review_images(&mut self, mut images: Vec<ReviewImageFact>, required: bool) {
        for image in &mut images {
            if let Some(old) = self
                .review_images
                .iter()
                .find(|old| old.binding == image.binding)
            {
                image.file_state = old.file_state.clone();
                image.externally_opened = old.externally_opened;
                image.notice.clone_from(&old.notice);
            }
        }
        self.review_images.retain(|image| {
            if required {
                !image.required
            } else {
                !images.iter().any(|new| {
                    new.binding.proposal == image.binding.proposal
                        && new.binding.review_key == image.binding.review_key
                        && new.binding.image_sha256 == image.binding.image_sha256
                })
            }
        });
        if required {
            let remaining = review::MAX_IMAGES.saturating_sub(images.len());
            images.extend(self.review_images.drain(..).take(remaining));
            self.review_images = images;
        } else {
            let remaining = review::MAX_IMAGES.saturating_sub(self.review_images.len());
            self.review_images
                .extend(images.into_iter().take(remaining));
        }
    }

    fn invalidate_artifact_currency(&mut self) {
        if let Some(operation) = &self.operation {
            if self
                .drawing_audit
                .as_ref()
                .is_some_and(|audit| audit.scene_sha256 != operation.current_sha256)
            {
                self.drawing_audit = None;
            }
            for building in &mut self.buildings {
                let connection_scope = self
                    .project_scope
                    .as_ref()
                    .filter(|scope| scope.assembly_binding.is_some());
                let changed = if let Some(scope) = connection_scope {
                    building
                        .connection_authority
                        .as_ref()
                        .is_some_and(|authority| {
                            authority.scope_id == scope.scope_id
                                && authority.scene_sha256 != operation.current_sha256
                        })
                } else {
                    building
                        .floor_authorities
                        .get(&self.floor_id)
                        .is_some_and(|sha| sha != &operation.current_sha256)
                };
                if changed {
                    building.current = false;
                }
            }
        }
        for artifact in &mut self.artifacts {
            if self.operation.as_ref().is_some_and(|operation| {
                operation.current_sha256 != artifact.binding.authority_sha256
            }) {
                artifact.current = Some(false);
            } else if artifact.binding.candidate {
                // Applying without an inspected authority cannot prove preview currency.
                artifact.current = None;
            }
        }
    }

    fn approval_card_removed(&self, binding: &ProposalBinding) -> bool {
        self.approval_card_tombstones
            .iter()
            .any(|removed| removed == binding)
    }

    fn approval_card_is_pending(&self, binding: &ProposalBinding) -> bool {
        self.approval_card_snapshots().iter().any(|card| {
            card.proposal
                .as_ref()
                .is_some_and(|proposal| proposal.binding == *binding)
        })
    }

    pub fn approval_card_waiting_for(&self, card: &Self) -> bool {
        self.fact_sync_notice.is_none()
            && !self.authorizing
            && !card.approval_restore_blocked
            && card.operation.is_none()
            && card.proposal.as_ref().is_some_and(|proposal| {
                card.source_current != Some(false)
                    && !card.proposal_not_approvable
                    && !self.approval_card_removed(&proposal.binding)
                    && proposal.ready
                    && card.application.is_none()
                    && card.candidate_review_delivered(&proposal.binding)
                    && card.first_use_satisfied()
            })
    }

    pub fn current_floor_progress(&self) -> Option<&FloorProgress> {
        self.floor_progress.as_ref().filter(|progress| {
            self.current_linework().is_some_and(|linework| {
                progress.scan_id == linework.scan_id
                    && progress.scene_sha256 == linework.scene_sha256
            })
        })
    }

    pub fn first_use_satisfied(&self) -> bool {
        self.first_use_review
            .as_ref()
            .is_none_or(|review| review.satisfied(&self.review_images))
    }

    pub fn review_set_sha256(&self) -> Option<String> {
        self.first_use_review
            .as_ref()
            .and_then(|review| review.review_set_sha256.clone())
    }

    pub fn confirmation(&self) -> crate::extensions::ModelingConfirmation {
        crate::extensions::ModelingConfirmation {
            proposal: self
                .proposal
                .as_ref()
                .expect("current proposal")
                .binding
                .clone(),
            review_set_sha256: self.review_set_sha256(),
        }
    }

    /// Image delivery and validation must belong to the same current candidate build.
    pub fn candidate_review_delivered(&self, proposal: &ProposalBinding) -> bool {
        self.artifacts.iter().any(|artifact| {
            artifact.binding.candidate
                && self.approval_card_request.as_ref().is_none_or(|request| {
                    request.binding != *proposal || artifact.binding.build_id == request.build_id
                })
                && self
                    .approval_restore_build_id
                    .as_ref()
                    .is_none_or(|build_id| artifact.binding.build_id == *build_id)
                && artifact.binding.proposal_id == proposal.proposal_id
                && artifact.binding.revision == proposal.revision
                && artifact.binding.prepare_token == proposal.prepare_token
                && artifact.binding.authority_sha256 == proposal.base_sha256
                && artifact.binding.input_sha256 == proposal.candidate_sha256
                && artifact.state == ArtifactState::Built
                && artifact.current == Some(true)
                && artifact.valid == Some(true)
                && artifact.reviewed
                && self.review_images.iter().any(|image| {
                    !image.required
                        && image.binding.proposal == *proposal
                        && image.binding.review_key == artifact.binding.build_id
                        && image.file_state == ImageFileState::Available
                })
        })
    }

    /// The approval card is visible and waiting for the user's decision.
    pub fn approval_card_waiting(&self) -> bool {
        self.approval_card_snapshots()
            .into_iter()
            .any(|card| self.approval_card_waiting_for(card))
    }

    /// The floor as the drawing names it (`二层`), falling back to the scene id.
    pub fn floor_title(&self) -> String {
        let key = self
            .project_scope
            .as_ref()
            .map(|scope| scope.floor_key.as_str());
        let title = key.and_then(|key| {
            self.project
                .as_ref()
                .and_then(|project| project.floor_titles.get(key))
        });
        match title {
            Some(title) => title.trim_end_matches("平面").to_owned(),
            None => self.floor_id.clone(),
        }
    }

    /// Report scanned source work independently of a completed model turn or structure build.
    pub fn component_progress(&self) -> Option<ComponentProgress> {
        let counts = self
            .ledger
            .as_ref()
            .map(|ledger| &ledger.counts)
            .or_else(|| self.groups.as_ref().map(|groups| &groups.counts))?;
        let count = |key: &str| counts.get(key).copied().unwrap_or(0);
        Some(ComponentProgress {
            total: counts
                .values()
                .fold(0usize, |total, count| total.saturating_add(*count)),
            built: count("built"),
            skipped: count("skipped"),
            rejected: count("rejected"),
            pending: counts
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "built" | "skipped" | "rejected"))
                .fold(0usize, |total, (_, count)| total.saturating_add(*count)),
        })
    }
}

pub(crate) fn source_state_label(state: &str) -> &str {
    match state {
        "built" => "已建",
        "pending_addition" => "待建",
        "pending_confirmation" => "待确认",
        "suspected_duplicate" => "疑似重复",
        "skipped" => "已跳过",
        "rejected" => "已排除",
        "capability_missing" => "需要补充定义或验证能力",
        _ => state,
    }
}

impl std::fmt::Debug for DomainSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DomainSnapshot")
            .field("enabled", &self.enabled)
            .field(
                "revision",
                &self.proposal.as_ref().map(|p| p.binding.revision),
            )
            .field("state", &self.proposal.as_ref().map(|p| &p.state))
            .field("authorizing", &self.authorizing)
            .field("artifact_count", &self.artifacts.len())
            .finish()
    }
}

/// Safe projection diagnostics: no raw tool payloads are retained.
#[derive(Clone, PartialEq, Eq)]
pub struct RejectedFact {
    pub kind: String,
    pub sequence: u64,
    pub reason: String,
    scope_id: Option<String>,
    activation_revision: Option<u64>,
    proposal_id: Option<String>,
    revision: Option<u64>,
    prepare_token: Option<String>,
    build_id: Option<String>,
    pub blocking: bool,
    pub dependency_rejected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FactProjection {
    Applied,
    Superseded,
    Unrelated,
    Invalid,
    Deferred,
    DependencyRejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceViewIdentity {
    contract: String,
    sha256: String,
    scan_id: String,
    scene_sha256: String,
}

impl SourceViewIdentity {
    fn read(value: &Value) -> Result<Option<Self>, ()> {
        if value.get("source_view_contract_version").is_none()
            && value.get("source_view_sha256").is_none()
        {
            return Ok(None);
        }
        if !matches!(
            value["source_view_contract_version"].as_str(),
            Some("source-view/1" | "source-view/2")
        ) {
            return Err(());
        }
        let hash = |key: &str| {
            let raw = value[key].as_str().ok_or(())?;
            if raw.len() != 64
                || !raw
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(());
            }
            Ok(raw.to_owned())
        };
        Ok(Some(Self {
            contract: value["source_view_contract_version"]
                .as_str()
                .ok_or(())?
                .to_owned(),
            sha256: hash("source_view_sha256")?,
            scene_sha256: hash("scene_sha256")?,
            scan_id: text(value, "scan_id").map_err(|_| ())?,
        }))
    }
}

pub struct DomainFacts {
    setup: Option<DomainSetup>,
    pub snapshot: DomainSnapshot,
    control_messages: BTreeMap<String, String>,
    fact_sequences: BTreeMap<String, u64>,
    pub(crate) highest_sequence: u64,
    source_view: Option<SourceViewIdentity>,
    source_policy: Option<String>,
    source_view_policy: Option<String>,
    source_view_required: Option<String>,
    source_view_sequence: Option<u64>,
    auto_approval_binding: Option<ProposalBinding>,
    pub(crate) synchronizing: bool,
    synchronization_errors: BTreeMap<String, u64>,
    projection_failure: Option<(String, u64, String)>,
    polled_sequence: u64,
    activation_sequence: u64,
    projected_sequences: BTreeMap<String, u64>,
    deferred_sequences: BTreeMap<u64, String>,
}

impl DomainFacts {
    pub fn new(setup: Option<DomainSetup>) -> Self {
        let snapshot = DomainSnapshot {
            enabled: setup.is_some(),
            floor_id: setup
                .as_ref()
                .map(|s| s.floor_id.clone())
                .unwrap_or_default(),
            ..Default::default()
        };
        let recovered = setup.clone();
        let mut facts = Self {
            setup,
            snapshot,
            control_messages: BTreeMap::new(),
            fact_sequences: BTreeMap::new(),
            highest_sequence: 0,
            source_view: None,
            source_policy: None,
            source_view_policy: None,
            source_view_required: None,
            source_view_sequence: None,
            auto_approval_binding: None,
            synchronizing: false,
            synchronization_errors: BTreeMap::new(),
            projection_failure: None,
            polled_sequence: 0,
            activation_sequence: 0,
            projected_sequences: BTreeMap::new(),
            deferred_sequences: BTreeMap::new(),
        };
        if let Some(setup) = &recovered {
            // Replay one sealed fact at a time so memory stays bounded by a single fact.
            receipt::baseline(setup, |value| {
                facts.apply_verified_fact(&value);
            });
        }
        // Persisted eligibility is display evidence; policy approval needs a fresh read.
        facts.auto_approval_binding = None;
        if let Some(proposal) = &mut facts.snapshot.proposal {
            proposal.ready = false;
        }
        for card in &mut facts.snapshot.approval_cards {
            if let Some(proposal) = &mut card.snapshot.proposal {
                proposal.ready = false;
            }
        }
        facts
    }

    pub(crate) fn observe(&mut self, notice: &ObservedTool, item: &Value) {
        if notice.category != ToolCategory::Mcp
            || notice.outcome != Some(ObservedToolOutcome::Completed)
            || item["type"] != "mcpToolCall"
            || item["server"] != SERVER
            || item["result"]["isError"] == true
        {
            return;
        }
        if let Some(envelope) = unwrap_envelope(item["result"].clone(), 0) {
            if let Some(value) = serde_json::to_string(&envelope)
                .ok()
                .and_then(|raw| decode_envelope(&raw))
            {
                self.apply_verified_fact(&value);
            }
        }
    }

    /// Core calls this only after validating a bounded inline envelope or a sealed fact.
    pub(crate) fn apply_verified_fact(&mut self, value: &Value) -> FactProjection {
        let sequence = value["fact_sequence"].as_u64().unwrap_or(0);
        let before = self.snapshot.clone();
        let mut result = self.project_fact(value);
        if result == FactProjection::Deferred {
            if let Some(rejected) = self.snapshot.rejected_facts.iter().find(|fact| {
                fact.kind == "project_scope"
                    && !fact.dependency_rejected
                    && fact
                        .scope_id
                        .as_deref()
                        .is_some_and(|id| value["project_scope"]["scope_id"].as_str() == Some(id))
                    && fact.activation_revision
                        == value["project_scope"]["activation_revision"].as_u64()
                    && fact.sequence < sequence
            }) {
                let reason = format!(
                    "依赖的 {} (seq {}) 已被拒绝：{}",
                    rejected.kind, rejected.sequence, rejected.reason
                );
                self.remember_projection_failure(value, reason);
                result = FactProjection::DependencyRejected;
            }
        }
        let kind = value["kind"].as_str().unwrap_or("unknown");
        if self.setup.as_ref().is_none_or(|setup| {
            value["contract"] != CONTRACT
                || value["run_id"] != setup.run_id
                || value["context_sha256"] != setup.context_sha256
        }) {
            return result;
        }
        if matches!(
            result,
            FactProjection::Invalid | FactProjection::DependencyRejected
        ) {
            self.snapshot = before;
            let reason = self
                .projection_failure
                .as_ref()
                .map(|(_, _, reason)| reason.clone())
                .unwrap_or_else(|| "fact projection validation".into());
            let owner = match kind {
                "artifact" | "artifact_validation" => &value["artifact"],
                "component_first_use_review" => &value["component_first_use_review"],
                "application" | "operation" => &value["receipt"],
                _ => &value["proposal"],
            };
            // Keep a bounded diagnostic history. Eviction never clears an active blocker.
            if self.snapshot.rejected_facts.len() >= 256 {
                if let Some(index) = self
                    .snapshot
                    .rejected_facts
                    .iter()
                    .position(|fact| !fact.blocking)
                {
                    self.snapshot.rejected_facts.remove(index);
                }
            }
            self.snapshot
                .rejected_facts
                .retain(|fact| fact.sequence != sequence || fact.kind != kind);
            self.snapshot.rejected_facts.push(RejectedFact {
                kind: kind.into(),
                sequence,
                reason,
                scope_id: value["project_scope"]["scope_id"]
                    .as_str()
                    .map(str::to_owned),
                activation_revision: value["project_scope"]["activation_revision"].as_u64(),
                proposal_id: owner["proposal_id"].as_str().map(str::to_owned),
                revision: owner["revision"].as_u64(),
                prepare_token: owner["prepare_token"].as_str().map(str::to_owned),
                build_id: owner["build_id"].as_str().map(str::to_owned),
                blocking: true,
                dependency_rejected: result == FactProjection::DependencyRejected,
            });
            self.highest_sequence = self.highest_sequence.max(sequence);
            if self.synchronization_errors.get(kind) == Some(&sequence) {
                self.synchronization_errors.remove(kind);
            }
        }
        if result == FactProjection::Deferred {
            self.deferred_sequences.insert(sequence, kind.into());
        } else {
            self.deferred_sequences.remove(&sequence);
        }
        if result == FactProjection::Applied {
            let key = value["artifact"]["build_id"]
                .as_str()
                .map_or_else(|| kind.to_owned(), |id| format!("{kind}:{id}"));
            self.projected_sequences
                .entry(key)
                .and_modify(|last| *last = (*last).max(sequence))
                .or_insert(sequence);
            self.snapshot
                .rejected_facts
                .retain(|fact| fact.sequence != sequence || fact.kind != kind);
            if self
                .synchronization_errors
                .get(kind)
                .is_some_and(|failed| *failed <= sequence)
            {
                self.synchronization_errors.remove(kind);
            }
        }
        self.refresh_rejections();
        if result == FactProjection::Applied {
            self.capture_approval_card(sequence);
            // Restore receipts and their separately sealed replay facts can arrive
            // in either order. Ready text never replaces the evidence checks.
            if let Some(restored) = self.snapshot.approval_card_restore.clone() {
                let binding = self
                    .snapshot
                    .restored_approval_card()
                    .and_then(|card| card.proposal.as_ref())
                    .map(|proposal| proposal.binding.clone());
                if let Some(card) = binding
                    .as_ref()
                    .and_then(|binding| self.snapshot.approval_card_mut(binding))
                {
                    card.approval_restore_blocked = !restored.ready;
                    card.approval_restore_build_id = restored.build_id;
                }
            }
        }
        result
    }

    fn project_fact(&mut self, value: &Value) -> FactProjection {
        macro_rules! invalid {
            () => {
                invalid!("fact projection validation")
            };
            ($reason:expr) => {{
                if self.projection_failure.is_none() {
                    let reason = $reason;
                    let summary = match reason {
                        "review::requirements" => "首用复核需求缺失或无效",
                        "proposal_fact" => "方案内容或绑定不合法",
                        "project::scope_fact: opening completion requires activation_revision" => {
                            "开洞补全作用域缺少激活版本 activation_revision"
                        }
                        "project::scope_fact" => "作用域字段缺失或无效",
                        "selection identity or current project scope mismatch" => {
                            "事实与当前选择或作用域不一致"
                        }
                        _ => "事实字段或绑定不符合领域契约",
                    };
                    self.remember_projection_failure(
                        value,
                        format!(
                            "{} 校验失败：{}；{} ({}:{})",
                            value["kind"].as_str().unwrap_or("unknown"),
                            summary,
                            reason,
                            file!(),
                            line!()
                        ),
                    );
                }
                FactProjection::Invalid
            }};
        }
        self.projection_failure = None;
        let Some(setup) = self.setup.as_ref() else {
            return invalid!();
        };
        if value["contract"] != CONTRACT {
            return invalid!();
        }
        if value["run_id"] != setup.run_id || value["context_sha256"] != setup.context_sha256 {
            return invalid!();
        }
        let Some(kind) = value["kind"].as_str() else {
            return invalid!();
        };
        // Tool jobs report waiting/unknown outcomes, not floor execution facts;
        // completed inspection returns the separately scoped business result.
        // Envelope identity was checked above, and sealed reads verify its hash.
        if matches!(
            kind,
            "fact_page"
                | "diagnostic_log"
                | "project_floor_preparing"
                | "reference_evaluation"
                | "tool_operation"
        ) {
            return FactProjection::Unrelated;
        }
        // Scope identity is a dependency, not corruption in a delayed sealed read.
        // Only an already projected newer activation can make this historical.
        if setup.mode == DomainMode::Project
            && project::scope_fact(value).is_some_and(|scope| {
                self.snapshot.project_scope.as_ref().is_some_and(|current| {
                    scope != *current
                        && value["fact_sequence"].as_u64().is_some_and(|sequence| {
                            self.fact_sequences
                                .get("project_scope")
                                .is_some_and(|last| sequence < *last)
                        })
                })
            })
        {
            return FactProjection::Superseded;
        }
        let incoming_scope = if setup.mode == DomainMode::Project
            && !matches!(
                kind,
                "project_catalog"
                    | "project_selection"
                    | "project_location"
                    | "project_building"
                    | "diagnostic"
            ) {
            let Some(scope) = project::scope_fact(value) else {
                return invalid!(if value["project_scope"]
                    .get("opening_completion_binding")
                    .is_some()
                    && value["project_scope"].get("activation_revision").is_none()
                {
                    "project::scope_fact: opening completion requires activation_revision"
                } else {
                    "project::scope_fact"
                });
            };
            // A background job keeps sealing facts for the scope it started in after the
            // agent re-activates another one; those belong to an older activation and are
            // not current state, so they are skipped without a synchronization error.
            if kind != "project_scope"
                && self.snapshot.project_scope.as_ref().is_some_and(|current| {
                    current.activation_revision.is_some_and(|revision| {
                        scope
                            .activation_revision
                            .is_some_and(|incoming| incoming < revision)
                    })
                })
            {
                return FactProjection::Superseded;
            }
            if self.snapshot.selection.as_ref().is_none_or(|selection| {
                selection.state != SelectionState::Selected
                    || selection.selection_id != scope.selection_id
                    || selection.source_sha256 != scope.source_sha256
                    || !selection.selected_keys.contains(&scope.floor_key)
            }) || (kind != "project_scope"
                && self.snapshot.project_scope.as_ref() != Some(&scope))
            {
                let future_activation =
                    self.snapshot.project_scope.as_ref().is_none_or(|current| {
                        scope.activation_revision.is_some_and(|incoming| {
                            current
                                .activation_revision
                                .is_none_or(|revision| incoming > revision)
                        })
                    });
                if self.snapshot.selection.is_none()
                    || (future_activation
                        && self.snapshot.selection.as_ref().is_some_and(|selection| {
                            selection.selected_keys.contains(&scope.floor_key)
                        }))
                {
                    self.remember_projection_failure(value, "作用域或选择依赖尚未到达".into());
                    return FactProjection::Deferred;
                }
                return invalid!("selection identity or current project scope mismatch");
            }
            if let Some(binding) = &scope.opening_completion_binding {
                if value["project_structure"] != false {
                    return invalid!();
                }
                if matches!(kind, "project_scope" | "proposal") {
                    let proposal = &value["proposal"];
                    let proof = &proposal["opening_completion_source"];
                    let operations = proposal["draft"]["operations"].as_array();
                    if proposal["base_sha256"] != binding.base_sha256
                        || proposal["scope"]["floor_id"] != scope.floor_id
                        || proof["contract"] != "project-opening-completion/1"
                        || text(proof, "path").is_err()
                        || proof["sha256"].as_str().is_none_or(|sha| {
                            sha.len() != 64
                                || !sha.bytes().all(|byte| {
                                    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
                                })
                        })
                        || operations.is_none_or(|operations| {
                            operations.len() > binding.opening_ids.len()
                                || operations.iter().any(|op| {
                                    op["op"] != "set"
                                        || op["collection"] != "openings"
                                        || op["field"] != "assets"
                                        || op["target_id"].as_str().is_none_or(|id| {
                                            !binding.opening_ids.iter().any(|allowed| allowed == id)
                                        })
                                })
                        })
                    {
                        return invalid!();
                    }
                }
            }
            if kind == "project_scope" {
                if let Some(choice) = &scope.wall_layer_choice {
                    if self.snapshot.project_location.is_none() {
                        return FactProjection::Deferred;
                    }
                    if self
                        .snapshot
                        .project_location
                        .as_ref()
                        .is_none_or(|location| {
                            location.location_id != choice.expected_location_id
                                || location.floor_key != scope.floor_key
                                || location.selection_id != scope.selection_id
                                || location.source_sha256 != scope.source_sha256
                                || location.status == "configured"
                                || choice
                                    .wall_layers
                                    .iter()
                                    .any(|layer| !location.layers.contains(layer))
                        })
                    {
                        return invalid!();
                    }
                }
                if let Some(binding) = &scope.assembly_binding {
                    if value["project_structure"] == false
                        && !self.snapshot.buildings.iter().any(|building| {
                            building.build_id == binding.build_id
                                && building.input_binding == binding.input_binding
                                && building.current
                                && building.valid == Some(true)
                                && building.state == "built"
                        })
                    {
                        return FactProjection::Deferred;
                    }
                    if self.snapshot.selection.as_ref().is_none_or(|selection| {
                        !selection.selected_keys.contains(&binding.upper_floor_key)
                    }) || !self.snapshot.buildings.iter().any(|building| {
                        building.build_id == binding.build_id
                            && building.input_binding == binding.input_binding
                            && building.selection_id == scope.selection_id
                            && building.state == "built"
                            && building.current
                            && building.valid == Some(true)
                    }) || value["project_structure"] != false
                    {
                        return invalid!();
                    }
                }
                if let Some(current) = self.snapshot.project_scope.as_ref() {
                    if let Some(revision) = current.activation_revision {
                        if scope.activation_revision.is_none_or(|incoming| {
                            incoming < revision || (incoming == revision && scope != *current)
                        }) {
                            return invalid!();
                        }
                    }
                }
            }
            Some(scope)
        } else {
            if setup.mode == DomainMode::Floor
                && (matches!(
                    kind,
                    "project_catalog"
                        | "project_selection"
                        | "project_location"
                        | "project_scope"
                        | "project_building"
                ) || value["project_scope"]
                    .get("opening_completion_binding")
                    .is_some())
            {
                return invalid!();
            }
            None
        };
        let source_kind = matches!(
            kind,
            "source_ledger" | "source_groups" | "source_linework" | "source_linework_selection"
        );
        let incoming_source_view = if source_kind {
            match SourceViewIdentity::read(value) {
                Ok(view) => view,
                Err(()) => return invalid!(),
            }
        } else {
            None
        };
        let same_source_view = incoming_source_view
            .as_ref()
            .is_some_and(|view| self.source_view.as_ref() == Some(view));
        let recover_source_shape = same_source_view
            && match kind {
                "source_groups" => self.snapshot.groups.is_none(),
                "source_ledger" => self.snapshot.ledger.is_none(),
                "source_linework" | "source_linework_selection" => self.snapshot.linework.is_none(),
                _ => false,
            };
        let artifact_kind = matches!(kind, "artifact" | "artifact_validation");
        let receipt_binding = match kind {
            "operation" => self.snapshot.proposal_binding_for_owner(&value["receipt"]),
            "application" => self
                .snapshot
                .application_binding_for_owner(&value["receipt"]),
            _ => None,
        };
        let fact_key = if matches!(kind, "operation" | "application") {
            receipt_binding
                .as_ref()
                .map_or_else(|| kind.to_owned(), proposal_operation_sequence_key)
        } else if kind == "project_building" {
            let Some(id) = value["project_building"]["build_id"].as_str() else {
                return invalid!();
            };
            format!("building:{id}")
        } else if artifact_kind {
            let Some(id) = value["artifact"]["build_id"].as_str() else {
                return invalid!();
            };
            format!("artifact:{id}")
        } else if source_kind {
            // Both shapes describe the same mutable source state. Delayed pages
            // from either shape must not replace a newer verified source view.
            "sources".to_owned()
        } else {
            kind.to_owned()
        };
        let sequence = value["fact_sequence"].as_u64();
        if kind == "capabilities"
            && ["sources", "opening_inventory"].iter().any(|key| {
                self.fact_sequences
                    .get(*key)
                    .is_some_and(|last| sequence.is_none_or(|seq| seq < *last))
            })
        {
            return FactProjection::Superseded;
        }
        // Once the server declares sealed source views, unsealed pages are stale.
        if source_kind
            && self
                .source_view_required
                .as_deref()
                .is_some_and(|required| {
                    incoming_source_view
                        .as_ref()
                        .is_none_or(|view| view.contract != required)
                })
        {
            return FactProjection::Superseded;
        }
        if self
            .fact_sequences
            .get(&fact_key)
            .is_some_and(|last| sequence.is_none_or(|seq| seq < *last))
            && !recover_source_shape
        {
            return FactProjection::Superseded;
        }
        let linework = if source_kind {
            match linework_coverage_fact(value) {
                Ok(summary) => summary,
                Err(()) => return invalid!(),
            }
        } else {
            None
        };
        let inventory_kind = source_kind || kind == "capabilities";
        let existing_structure = if source_kind {
            match existing_structure_coverage_fact(value, linework.as_ref()) {
                Ok(summary) if summary.is_none() || incoming_source_view.is_some() => summary,
                _ => return invalid!(),
            }
        } else {
            None
        };
        let source_regions = if source_kind {
            match source_region_review_fact(value, linework.as_ref(), existing_structure.as_ref()) {
                Ok(summary) if summary.is_none() || incoming_source_view.is_some() => summary,
                _ => return invalid!(),
            }
        } else {
            None
        };
        let openings = if inventory_kind {
            match opening_inventory_fact(value) {
                Ok(summary) => summary,
                Err(()) => return invalid!(),
            }
        } else {
            None
        };
        if openings.is_some() && sequence.is_none() {
            return invalid!();
        }
        let floor_progress = if source_kind {
            match floor_progress_fact(value) {
                Ok(progress) => progress,
                Err(()) => return invalid!(),
            }
        } else {
            None
        };
        let mut accepted = false;
        match Some(kind) {
            Some("project_building") => {
                let Some(mut fact) = self
                    .snapshot
                    .selection
                    .as_ref()
                    .and_then(|selection| project::building_fact(value, selection))
                else {
                    return invalid!();
                };
                if let Some(old) = self
                    .snapshot
                    .buildings
                    .iter_mut()
                    .find(|old| old.build_id == fact.build_id)
                {
                    if old.selection_id != fact.selection_id
                        || old.input_binding != fact.input_binding
                        || old.input_sha256 != fact.input_sha256
                    {
                        return invalid!();
                    }
                    if fact.valid.is_none() && old.state == fact.state {
                        fact.valid = old.valid;
                    }
                    if fact.report.is_none()
                        && fact.connection_check == "not_run"
                        && old.state == fact.state
                        && fact.source_discovery == old.source_discovery
                        && fact.source_reconciliation == old.source_reconciliation
                    {
                        fact.report = old.report.clone();
                        fact.connection_check.clone_from(&old.connection_check);
                    }
                    *old = fact;
                } else {
                    if self.snapshot.buildings.len() >= 16 {
                        let old = self.snapshot.buildings.remove(0);
                        self.fact_sequences
                            .remove(&format!("building:{}", old.build_id));
                    }
                    self.snapshot.buildings.push(fact);
                }
                accepted = true;
            }
            Some("drawing_audit") => {
                let Some(fact) = project::drawing_audit_fact(value) else {
                    return invalid!();
                };
                if self.snapshot.operation.as_ref().is_some_and(|operation| {
                    operation.state == "applied" && operation.current_sha256 != fact.scene_sha256
                }) {
                    return FactProjection::Superseded;
                }
                self.snapshot.drawing_audit = Some(fact);
                accepted = true;
            }
            Some("project_scope") => {
                let Some(scope) = incoming_scope else {
                    return invalid!();
                };
                let Some(proposal) = proposal_fact(value) else {
                    return invalid!("proposal_fact");
                };
                let Some(mut first_use) =
                    review::requirements_for_scope(&value["proposal"], &scope)
                else {
                    return invalid!("review::requirements");
                };
                let mut images = Vec::new();
                if value.get("component_first_use_review").is_some() {
                    let Some((projected, projected_images)) =
                        review::project(value, &proposal.binding, &first_use)
                    else {
                        self.remember_projection_failure(
                            value,
                            review::project_failure_reason(value),
                        );
                        return invalid!();
                    };
                    first_use = projected;
                    images = projected_images;
                }
                if self.snapshot.project_scope.as_ref() != Some(&scope) {
                    self.activation_sequence = sequence.unwrap_or(0);
                    self.clear_project_scope();
                    // Facts that failed before this activation describe the previous
                    // scope; the new activation re-establishes current state, so their
                    // errors must not block approval cards forever.
                    if let Some(sequence) = value["fact_sequence"].as_u64() {
                        self.synchronization_errors
                            .retain(|_, failed| *failed > sequence);
                    }
                }
                self.snapshot.floor_id = scope.floor_id.clone();
                self.snapshot.project_scope = Some(scope);
                if self
                    .snapshot
                    .proposal
                    .as_ref()
                    .is_none_or(|old| old.binding != proposal.binding)
                {
                    self.snapshot.review_images.clear();
                    self.snapshot.artifacts.clear();
                }
                self.snapshot.first_use_review = Some(first_use);
                self.replace_review_images(images, true);
                self.snapshot.proposal_categories = proposal_category_counts(&value["proposal"]);
                self.snapshot.proposal = Some(proposal);
                self.snapshot.source_current = value["source_current"].as_bool();
                self.snapshot.proposal_not_approvable = value["approval_review"]["status"]
                    == "proposal_not_approvable"
                    || matches!(
                        value["proposal"]["state"].as_str(),
                        Some(
                            "superseded"
                                | "discarded"
                                | "invalidated"
                                | "cancelled"
                                | "approved"
                                | "authorized"
                                | "applied"
                                | "built"
                        )
                    );
                self.snapshot.structure_review = structure_review(&value["structure_review"]);
                accepted = true;
            }
            Some("project_location") => {
                if self.snapshot.selection.is_none() {
                    return FactProjection::Deferred;
                }
                let Some(location) = project::location_fact(value) else {
                    return invalid!();
                };
                if self.snapshot.selection.as_ref().is_none_or(|selection| {
                    selection.state != SelectionState::Selected
                        || selection.catalog_id != location.catalog_id
                        || selection.selection_id != location.selection_id
                        || selection.source_sha256 != location.source_sha256
                        || !selection.selected_keys.contains(&location.floor_key)
                }) {
                    return invalid!();
                }
                self.snapshot.project_location = Some(location);
                accepted = true;
            }
            Some("project_catalog") => {
                let Some(project) = project::catalog_fact(value) else {
                    return invalid!();
                };
                // A read-only catalog includes the original persisted target. Restore
                // this selection before accepting floor facts; it grants no write access.
                let recovered_selection = if value["recovery_index"]["catalog_current"] == true {
                    let Some(selection) = project::selection_fact(&json!({
                        "ok":true,"selection":value["recovery_index"]["selection"]
                    })) else {
                        return invalid!();
                    };
                    if selection.catalog_id != project.catalog_id
                        || selection.source_sha256 != project.source_sha256
                        || selection
                            .candidate_keys
                            .iter()
                            .any(|key| !project.floor_keys.contains(key))
                    {
                        return invalid!();
                    }
                    Some(selection)
                } else {
                    None
                };
                if self
                    .snapshot
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.catalog_id != project.catalog_id)
                {
                    self.snapshot.selection = None;
                    self.snapshot.project_location = None;
                    for building in &mut self.snapshot.buildings {
                        building.current = false;
                    }
                    self.clear_project_scope();
                }
                if self.snapshot.selection.is_none() {
                    self.snapshot.selection = recovered_selection;
                }
                self.snapshot.project = Some(project);
                accepted = true;
            }
            Some("project_selection") => {
                if self.snapshot.project.is_none() {
                    return FactProjection::Deferred;
                }
                let Some(selection) = project::selection_fact(value) else {
                    return invalid!();
                };
                if self.snapshot.project.as_ref().is_none_or(|project| {
                    project.catalog_id != selection.catalog_id
                        || project.source_sha256 != selection.source_sha256
                        || selection
                            .candidate_keys
                            .iter()
                            .any(|key| !project.floor_keys.contains(key))
                }) {
                    return invalid!();
                }
                if self
                    .snapshot
                    .selection
                    .as_ref()
                    .is_none_or(|previous| previous.selection_id != selection.selection_id)
                {
                    for building in &mut self.snapshot.buildings {
                        building.current = false;
                    }
                    self.clear_project_scope();
                    self.snapshot.project_location = None;
                }
                self.snapshot.selection = Some(selection);
                accepted = true;
            }
            Some("proposal") => {
                if let Some(proposal) = proposal_fact(value) {
                    let Some(mut first_use) = (if let Some(scope) = &self.snapshot.project_scope {
                        review::requirements_for_scope(&value["proposal"], scope)
                    } else {
                        review::requirements(&value["proposal"])
                    }) else {
                        return invalid!("review::requirements");
                    };
                    let mut images = Vec::new();
                    if value.get("component_first_use_review").is_some() {
                        let Some((projected, projected_images)) =
                            review::project(value, &proposal.binding, &first_use)
                        else {
                            self.remember_projection_failure(
                                value,
                                review::project_failure_reason(value),
                            );
                            return invalid!();
                        };
                        first_use = projected;
                        images = projected_images;
                    }
                    let old_binding = self
                        .snapshot
                        .proposal
                        .as_ref()
                        .map(|old| old.binding.clone());
                    let same = old_binding.as_ref() == Some(&proposal.binding);
                    if !same && value["current"] != true {
                        if let Some(card) = self
                            .snapshot
                            .approval_cards
                            .iter_mut()
                            .find(|card| card.binding() == Some(&proposal.binding))
                        {
                            card.snapshot.proposal = Some(proposal);
                            card.snapshot.source_current = value["source_current"].as_bool();
                            card.snapshot.proposal_not_approvable =
                                value["approval_review"]["status"] == "proposal_not_approvable"
                                    || matches!(
                                        value["proposal"]["state"].as_str(),
                                        Some(
                                            "superseded"
                                                | "discarded"
                                                | "invalidated"
                                                | "cancelled"
                                                | "approved"
                                                | "authorized"
                                                | "applied"
                                                | "built"
                                        )
                                    );
                            card.snapshot.first_use_review = Some(first_use);
                            card.snapshot.replace_review_images(images, true);
                            card.snapshot.proposal_categories =
                                proposal_category_counts(&value["proposal"]);
                            if let Some(sequence) = sequence {
                                card.fact_sequence = sequence;
                                self.highest_sequence = self.highest_sequence.max(sequence);
                                self.fact_sequences.insert(fact_key, sequence);
                            }
                            return FactProjection::Applied;
                        }
                    }
                    let parent = value["proposal"]["parent_id"].as_str();
                    let follows = old_binding.as_ref().is_none_or(|binding| {
                        parent == Some(binding.proposal_id.as_str()) || value["current"] == true
                    });
                    if !same && !follows {
                        // Valid historical-family reads are not current state
                        // and do not imply transport or projection corruption.
                        return FactProjection::Superseded;
                    }
                    if same || follows {
                        if !same {
                            if self.snapshot.approval_card_restore.as_ref().is_some_and(
                                |restored| {
                                    restored.proposal_id == proposal.binding.proposal_id
                                        && sequence.is_some_and(|seq| seq > restored.fact_sequence)
                                },
                            ) {
                                self.snapshot.approval_card_restore = None;
                            }
                            self.capture_approval_card(self.highest_sequence);
                            if let Some(parent) = parent {
                                self.invalidate_approval_cards_for_proposal(parent);
                            }
                            if let Some(old) = old_binding.as_ref() {
                                if old.proposal_id == proposal.binding.proposal_id
                                    || parent == Some(old.proposal_id.as_str())
                                {
                                    self.invalidate_approval_card(old);
                                }
                            }
                            self.invalidate_other_approval_versions(&proposal.binding);
                            self.snapshot.application = None;
                            self.snapshot.operation = None;
                            self.snapshot.artifacts.clear();
                            self.snapshot.review_images.clear();
                            self.snapshot.approval_restore_blocked = false;
                            self.snapshot.approval_restore_build_id = None;
                            self.fact_sequences
                                .retain(|key, _| !key.starts_with("artifact:"));
                            for kind in ["application", "operation"] {
                                self.fact_sequences.remove(kind);
                            }
                        }
                        self.snapshot.preview_selection = None;
                        self.snapshot.first_use_review = Some(first_use);
                        self.replace_review_images(images, true);
                        self.snapshot.proposal_categories =
                            proposal_category_counts(&value["proposal"]);
                        self.auto_approval_binding = (value["auto_approval"]["contract"]
                            == "reviewed-template-approval/1"
                            && value["auto_approval"]["eligible"] == true
                            && value["current"] == true
                            && value["source_current"] == true
                            && value["proposal"]["component_first_use_requirements"]["required"]
                                .as_array()
                                .is_some_and(Vec::is_empty)
                            && value["proposal"]["component_first_use_requirements"]["reused"]
                                .as_array()
                                .is_some_and(|items| !items.is_empty())
                            && value["diagnostics"].as_array().is_some_and(Vec::is_empty))
                        .then(|| proposal.binding.clone());
                        self.snapshot.proposal = Some(proposal);
                        self.snapshot.source_current = value["source_current"].as_bool();
                        self.snapshot.proposal_not_approvable = value["approval_review"]["status"]
                            == "proposal_not_approvable"
                            || matches!(
                                value["proposal"]["state"].as_str(),
                                Some(
                                    "superseded"
                                        | "discarded"
                                        | "invalidated"
                                        | "cancelled"
                                        | "approved"
                                        | "authorized"
                                        | "applied"
                                        | "built"
                                )
                            );
                        accepted = true;
                    }
                }
            }
            Some("source_groups") => {
                if let Some(groups) = source_groups_fact(value) {
                    if let Err(projection) = self.accept_source_policy(value) {
                        if projection == FactProjection::Invalid {
                            return invalid!("source policy declaration");
                        }
                        return projection;
                    }
                    self.snapshot.groups = Some(groups);
                    if !same_source_view {
                        self.snapshot.ledger = None;
                    }
                    accepted = true;
                }
            }
            Some("source_ledger") => {
                if let Some(ledger) = source_ledger_fact(value) {
                    if let Err(projection) = self.accept_source_policy(value) {
                        if projection == FactProjection::Invalid {
                            return invalid!("source policy declaration");
                        }
                        return projection;
                    }
                    self.snapshot.ledger = Some(ledger);
                    if !same_source_view {
                        self.snapshot.groups = None;
                    }
                    accepted = true;
                }
            }
            Some("source_linework" | "source_linework_selection") => {
                if linework.is_some() && incoming_source_view.is_some() {
                    if let Err(projection) = self.accept_source_policy(value) {
                        if projection == FactProjection::Invalid {
                            return invalid!("source policy declaration");
                        }
                        return projection;
                    }
                    if !same_source_view {
                        self.snapshot.groups = None;
                        self.snapshot.ledger = None;
                    }
                    accepted = true;
                }
            }
            Some("operation") | Some("application") => {
                let owner = &value["receipt"];
                let binding = if kind == "application" {
                    self.snapshot.application_binding_for_owner(owner)
                } else {
                    self.snapshot.proposal_binding_for_owner(owner)
                };
                let Some(binding) = binding else {
                    return if self.snapshot.proposal.is_none() {
                        FactProjection::Deferred
                    } else {
                        FactProjection::Superseded
                    };
                };
                let operation = operation_fact(value, &binding);
                let Some(card) = self.snapshot.approval_card_mut(&binding) else {
                    return FactProjection::Superseded;
                };
                if kind == "operation" {
                    let Some(operation) = operation else {
                        return FactProjection::Unrelated;
                    };
                    card.application = Some(format!(
                        "{} · current={}",
                        operation.state,
                        value["receipt"]["candidate_sha256"] == operation.current_sha256
                    ));
                    card.operation = Some(operation);
                } else {
                    if let Some(operation) = operation {
                        card.operation = Some(operation);
                    }
                    card.application = value["receipt"]["state"]
                        .as_str()
                        .map(|state| format!("{state} · current={}", value["current"]));
                }
                if let Some(proposal) = &mut card.proposal {
                    proposal.ready = false;
                }
                card.invalidate_artifact_currency();
                accepted = true;
            }
            Some("component_first_use_review") => {
                let owner = &value["component_first_use_review"];
                let Some(binding) = self.snapshot.proposal_binding_for_owner(owner) else {
                    return if self.snapshot.proposal.is_none() {
                        FactProjection::Deferred
                    } else {
                        FactProjection::Superseded
                    };
                };
                let Some(first_use) = self
                    .snapshot
                    .approval_card(&binding)
                    .and_then(|card| card.first_use_review.as_ref())
                    .cloned()
                else {
                    return FactProjection::Deferred;
                };
                let Some((first_use, images)) = review::project(value, &binding, &first_use) else {
                    self.remember_projection_failure(value, review::project_failure_reason(value));
                    return invalid!();
                };
                let Some(card) = self.snapshot.approval_card_mut(&binding) else {
                    return FactProjection::Superseded;
                };
                card.first_use_review = Some(first_use);
                card.replace_review_images(images, true);
                accepted = true;
            }
            Some("artifact" | "artifact_validation") => {
                let owner = &value["artifact"];
                if owner["proposal_id"].as_str().is_none()
                    || owner["revision"].as_u64().is_none()
                    || owner["prepare_token"].as_str().is_none()
                {
                    return invalid!("artifact proposal identity");
                }
                let Some(binding) = self.snapshot.proposal_binding_for_owner(owner) else {
                    return if self.snapshot.proposal.is_none() {
                        FactProjection::Deferred
                    } else {
                        FactProjection::Superseded
                    };
                };
                if let Some(mut artifact) = artifact_fact(value, &binding) {
                    let current_sha256 = self
                        .snapshot
                        .approval_card(&binding)
                        .and_then(|card| card.operation.as_ref())
                        .map(|operation| operation.current_sha256.clone());
                    if sequence.is_none_or(|seq| {
                        self.fact_sequences
                            .get(&proposal_operation_sequence_key(&binding))
                            .is_some_and(|last| seq < *last)
                    }) {
                        artifact.current = current_sha256.as_ref().and_then(|current| {
                            if *current != artifact.binding.authority_sha256 {
                                Some(false)
                            } else if !artifact.binding.candidate {
                                Some(true)
                            } else {
                                None
                            }
                        });
                    }
                    let Some(card) = self.snapshot.approval_card_mut(&binding) else {
                        return FactProjection::Superseded;
                    };
                    let existing = card
                        .artifacts
                        .iter_mut()
                        .find(|a| a.binding.build_id == artifact.binding.build_id);
                    if let Some(old) = existing {
                        if old.binding == artifact.binding {
                            if kind == "artifact" && old.state == artifact.state {
                                artifact.valid = old.valid;
                            }
                            // A later receipt without an image never erases a delivered review.
                            artifact.reviewed |= old.reviewed && old.state == artifact.state;
                            *old = artifact;
                            accepted = true;
                        }
                    } else {
                        // Receipts remain durable in Python; bound the in-memory display history.
                        if card.artifacts.len() >= 32 {
                            let evicted = card.artifacts.remove(0);
                            self.fact_sequences
                                .remove(&format!("artifact:{}", evicted.binding.build_id));
                        }
                        card.artifacts.push(artifact);
                        accepted = true;
                    }
                    if accepted {
                        if let Some(image) = review::artifact_image(value, &binding) {
                            card.replace_review_images(vec![image], false);
                        } else if value["artifact"]["review_image"]
                            .get("review_web")
                            .is_some()
                        {
                            self.remember_projection_failure(
                                value,
                                "artifact.review_image.review_web binding".into(),
                            );
                            return invalid!("artifact.review_image.review_web binding");
                        }
                    }
                }
            }
            Some("capabilities") => {
                let declaration = value["capabilities"].get("source_coincidence_review");
                let policy = if let Some(declaration) = declaration {
                    let Some(policy) = declaration["contract_version"]
                        .as_str()
                        .filter(|v| !v.is_empty() && v.len() <= 80)
                    else {
                        return invalid!();
                    };
                    Some(policy)
                } else {
                    None
                };
                let view_declaration = value["capabilities"].get("source_view");
                if view_declaration.is_some_and(|d| {
                    !matches!(
                        d["contract_version"].as_str(),
                        Some("source-view/1" | "source-view/2")
                    )
                }) {
                    return invalid!();
                }
                if (policy.is_some() || view_declaration.is_some()) && sequence.is_none() {
                    return invalid!();
                }
                if let Some(policy) = policy {
                    if self.source_policy.as_deref() != Some(policy) {
                        self.source_policy = Some(policy.to_owned());
                        if self.source_view_policy.as_deref() != Some(policy) {
                            self.clear_source_view();
                        }
                    }
                    accepted = true;
                }
                if let Some(declaration) = view_declaration {
                    let required = declaration["contract_version"].as_str().unwrap();
                    self.source_view_required = Some(required.to_owned());
                    if self
                        .source_view
                        .as_ref()
                        .is_none_or(|view| view.contract != required)
                    {
                        self.clear_source_view();
                    }
                    accepted = true;
                }
                if openings.is_some() {
                    accepted = true;
                }
            }
            Some("diagnostic") => {
                self.snapshot.notice = value["diagnostics"][0]["message"]
                    .as_str()
                    .map(str::to_owned);
                accepted = true;
            }
            Some("approval_card_restore") => {
                let restored = if value["agent_view"]["value"].is_object() {
                    &value["agent_view"]["value"]
                } else {
                    value
                };
                let (Ok(proposal_id), Some(missing), Some(status)) = (
                    text(restored, "proposal_id"),
                    restored["missing"].as_array(),
                    restored["status"].as_str(),
                ) else {
                    return invalid!();
                };
                if value["ok"] != true || !matches!(status, "ready" | "blocked") {
                    return invalid!();
                }
                if self
                    .snapshot
                    .approval_card_request
                    .as_ref()
                    .is_some_and(|request| {
                        request.binding.proposal_id == proposal_id
                            && request.fact_sequence >= sequence.unwrap_or(0)
                    })
                {
                    return FactProjection::Superseded;
                }
                if self
                    .snapshot
                    .approval_card_request
                    .as_ref()
                    .is_some_and(|request| request.binding.proposal_id == proposal_id)
                {
                    self.snapshot.approval_card_request = None;
                }
                self.snapshot.approval_card_restore = Some(ApprovalCardRestore {
                    proposal_id,
                    build_id: restored["build_id"].as_str().map(str::to_owned),
                    fact_sequence: sequence.unwrap_or(0),
                    ready: status == "ready" && missing.is_empty(),
                    awaiting_user: restored["next_call"]["action"] == "end_turn_for_core_approval",
                });
                accepted = true;
            }
            Some(
                "floor_advance"
                | "circulation_revision"
                | "drawing_batch"
                | "drawing_sheet"
                | "drawing_comparison",
            ) => {
                if kind == "floor_advance" {
                    let needs = &value["needs"];
                    let needs_type = needs["type"].as_str().unwrap_or("unknown").to_owned();
                    if needs_type == "approve_card"
                        && self
                            .snapshot
                            .approval_card_restore
                            .as_ref()
                            .is_some_and(|restored| {
                                needs["proposal_id"] == restored.proposal_id
                                    && sequence
                                        .is_some_and(|incoming| incoming <= restored.fact_sequence)
                            })
                    {
                        return FactProjection::Superseded;
                    }
                    if needs_type == "approve_card"
                        && self
                            .fact_sequences
                            .get("floor_advance")
                            .is_none_or(|last| sequence.is_some_and(|incoming| incoming > *last))
                    {
                        // The sealed advance result already owns this proposal. Project
                        // it before its image receipts; a later historical read must
                        // not replace the newer proposal/evidence from this result.
                        let mut proposal = value.clone();
                        proposal["kind"] = json!("proposal");
                        match self.project_fact(&proposal) {
                            FactProjection::Applied | FactProjection::Superseded => {}
                            result => return result,
                        }
                    }
                    if needs_type == "approve_card" {
                        let request = (|| {
                            Some(ApprovalCardRequest {
                                fact_sequence: sequence.unwrap_or(0),
                                binding: ProposalBinding {
                                    run_id: text(value, "run_id").ok()?,
                                    context_sha256: text(value, "context_sha256").ok()?,
                                    proposal_id: text(needs, "proposal_id").ok()?,
                                    revision: needs["revision"].as_u64()?,
                                    prepare_token: text(needs, "prepare_token").ok()?,
                                    base_sha256: text(needs, "base_sha256").ok()?,
                                    candidate_sha256: text(needs, "candidate_sha256").ok()?,
                                },
                                build_id: text(needs, "build_id").ok()?,
                                card_ref: text(needs, "card_ref").ok()?,
                            })
                        })();
                        let Some(request) = request else {
                            return invalid!();
                        };
                        if self
                            .snapshot
                            .proposal
                            .as_ref()
                            .is_none_or(|proposal| proposal.binding != request.binding)
                        {
                            return invalid!("approval request and proposal identity mismatch");
                        }
                        if self
                            .snapshot
                            .approval_card_restore
                            .as_ref()
                            .is_some_and(|restored| {
                                sequence.is_some_and(|incoming| incoming > restored.fact_sequence)
                            })
                        {
                            self.snapshot.approval_card_restore = None;
                            self.snapshot.approval_restore_build_id = None;
                            self.snapshot.approval_restore_blocked = false;
                        }
                        self.snapshot.approval_card_request = Some(request);
                    }
                    let parsed = FloorAdvanceNeeds {
                        needs_type,
                        diagnostic_code: needs["diagnostic"]["code"].as_str().map(str::to_owned),
                        rejection_category: needs["diagnostic"]["rejection"]["category"]
                            .as_str()
                            .map(str::to_owned),
                        split_batch_allowed: needs["split_batch_allowed"].as_bool(),
                        next_action: needs["next_action"].as_str().map(str::to_owned),
                    };
                    self.snapshot.floor_advance_auto_continue_paused =
                        parsed.blocks_auto_continue();
                    self.snapshot.floor_advance_needs = Some(parsed);
                }
                accepted = true;
            }
            _ => {}
        }
        if accepted {
            if matches!(kind, "proposal" | "project_scope") {
                self.snapshot.structure_revision =
                    (!value["proposal"]["structure_revision_source"].is_null()
                        || !value["proposal"]["slab_revision_source"].is_null())
                    .then(|| {
                        let audit = &value["structure_revision"];
                        StructureRevisionFact {
                            proposal: self.snapshot.proposal.as_ref().unwrap().binding.clone(),
                            host_mapping_count: audit["host_mapping"]
                                .as_object()
                                .map_or(0, |m| m.len()),
                            changes: value["proposal"]["draft"]["operations"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|op| {
                                    let action = match op["op"].as_str() {
                                        Some("remove") => "移除",
                                        Some("set") => "修改",
                                        _ => "新增",
                                    };
                                    let collection = match op["collection"].as_str() {
                                        Some("walls") => "墙",
                                        Some("slabs") => "楼板",
                                        Some("openings") => "洞口",
                                        Some("constraints") => "约束",
                                        _ => "构件",
                                    };
                                    format!(
                                        "{action}{collection} {} · {}",
                                        op["target_id"].as_str().unwrap(),
                                        op["reason"].as_str().unwrap()
                                    )
                                })
                                .collect(),
                            void_decisions: audit["slab"]["void_decisions"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|d| {
                                    format!(
                                        "{}：{} · {} · 用户原话 {}",
                                        d["candidate_id"].as_str().unwrap(),
                                        if d["choice"] == "cut_void" {
                                            "确认挖洞"
                                        } else {
                                            "保留楼板"
                                        },
                                        d["reason"].as_str().unwrap(),
                                        d["user_statement"].as_str().unwrap()
                                    )
                                })
                                .collect(),
                            verification_questions: audit["slab"]["verification_questions"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|q| {
                                    format!(
                                        "{} · {}",
                                        q["question"].as_str().unwrap(),
                                        if q["status"] == "unanswered" {
                                            "待用户决定"
                                        } else {
                                            "已记录决定"
                                        }
                                    )
                                })
                                .collect(),
                        }
                    });
            }
            if inventory_kind
                && sequence.is_some_and(|seq| {
                    self.fact_sequences
                        .get("opening_inventory")
                        .is_none_or(|last| seq >= *last)
                })
            {
                self.snapshot.openings = openings;
                self.fact_sequences
                    .insert("opening_inventory".into(), sequence.unwrap());
            }
            if source_kind {
                self.snapshot.floor_progress_v2 =
                    floor_progress.as_ref().and_then(|(_, v2)| v2.clone());
                self.snapshot.floor_progress = floor_progress.map(|(progress, _)| progress);
                self.snapshot.linework = linework;
                self.snapshot.existing_structure = existing_structure;
                self.snapshot.source_regions = source_regions;
                self.source_view = incoming_source_view;
            }
            if let Some(sequence) = sequence {
                self.highest_sequence = self.highest_sequence.max(sequence);
                if source_kind {
                    self.source_view_sequence = Some(sequence);
                }
                self.fact_sequences
                    .entry(fact_key)
                    .and_modify(|last| *last = (*last).max(sequence))
                    .or_insert(sequence);
            }
            FactProjection::Applied
        } else if matches!(kind, "proposal" | "artifact" | "artifact_validation") || source_kind {
            invalid!()
        } else {
            FactProjection::Unrelated
        }
    }

    fn clear_project_scope(&mut self) {
        self.snapshot.approval_card_request = None;
        self.snapshot.approval_card_restore = None;
        self.snapshot.approval_restore_blocked = false;
        self.snapshot.approval_restore_build_id = None;
        let approved = self.snapshot.approval_card_tombstones.clone();
        self.snapshot.approval_cards.retain(|card| {
            card.binding()
                .is_some_and(|binding| approved.contains(binding))
        });
        self.source_view = None;
        self.snapshot.project_scope = None;
        self.snapshot.structure_review = None;
        self.snapshot.structure_revision = None;
        self.snapshot.drawing_audit = None;
        self.snapshot.proposal = None;
        self.snapshot.source_current = None;
        self.snapshot.proposal_not_approvable = false;
        self.snapshot.proposal_categories.clear();
        self.snapshot.ledger = None;
        self.snapshot.floor_progress = None;
        self.snapshot.floor_progress_v2 = None;
        self.snapshot.groups = None;
        self.snapshot.linework = None;
        self.snapshot.existing_structure = None;
        self.snapshot.source_regions = None;
        self.snapshot.openings = None;
        self.snapshot.operation = None;
        self.snapshot.application = None;
        self.snapshot.artifacts.clear();
        self.snapshot.review_images.clear();
        self.snapshot.first_use_review = None;
        self.snapshot.floor_id = "项目目标".into();
    }

    fn clear_source_view(&mut self) {
        self.snapshot.floor_progress = None;
        self.snapshot.floor_progress_v2 = None;
        self.snapshot.groups = None;
        self.snapshot.ledger = None;
        self.snapshot.linework = None;
        self.snapshot.existing_structure = None;
        self.snapshot.source_regions = None;
        self.snapshot.openings = None;
        if self.snapshot.verified_fact.as_ref().is_some_and(|f| {
            matches!(
                f.kind.as_str(),
                "source_groups" | "source_ledger" | "source_linework" | "source_linework_selection"
            )
        }) {
            self.snapshot.verified_fact = None;
        }
        self.source_view_policy = None;
        self.source_view = None;
        self.source_view_sequence = None;
        self.fact_sequences.remove("sources");
        self.synchronization_errors.remove("source_groups");
        self.synchronization_errors.remove("source_ledger");
        self.synchronization_errors.remove("source_linework");
        self.synchronization_errors
            .remove("source_linework_selection");
    }

    fn accept_source_policy(&mut self, value: &Value) -> Result<(), FactProjection> {
        let raw = value.get("source_coincidence_contract_version");
        if raw.is_some_and(|v| v.as_str().is_none_or(|s| s.is_empty() || s.len() > 80)) {
            return Err(FactProjection::Invalid);
        }
        let policy = raw.and_then(Value::as_str);
        if self
            .source_policy
            .as_deref()
            .is_some_and(|expected| Some(expected) != policy)
        {
            return Err(FactProjection::Superseded);
        }
        self.source_view_policy = policy.map(str::to_owned);
        Ok(())
    }

    pub(crate) fn synchronization_failed(&mut self, kind: &str, sequence: u64, reason: String) {
        if self
            .snapshot
            .rejected_facts
            .iter()
            .any(|fact| fact.kind == kind && fact.sequence == sequence)
        {
            self.refresh_rejections();
            return;
        }
        let reason = self
            .projection_failure
            .take()
            .filter(|(failed_kind, failed_sequence, _)| {
                failed_kind == kind && *failed_sequence == sequence
            })
            .map(|(_, _, reason)| reason)
            .unwrap_or(reason);
        self.synchronization_errors
            .entry(kind.into())
            .and_modify(|last| *last = (*last).max(sequence))
            .or_insert(sequence);
        self.snapshot.fact_sync_notice = Some(format!(
            "领域事实同步失败：{kind} (seq {sequence})，{reason}；请重新检查当前方案"
        ));
    }

    fn remember_projection_failure(&mut self, value: &Value, reason: String) {
        let kind = value["kind"].as_str().unwrap_or("unknown").to_owned();
        let sequence = value["fact_sequence"].as_u64().unwrap_or(0);
        self.projection_failure = Some((kind, sequence, reason));
    }

    /// Only a completed poll advances this cursor; tool receipts cannot hide gaps.
    /// Permanent rejections are consumed, while unresolved dependencies are retried.
    pub(crate) fn poll_floor(&self) -> u64 {
        let floor = if self.deferred_sequences.is_empty() {
            self.polled_sequence
        } else {
            self.polled_sequence.min(
                self.fact_sequences
                    .get("project_scope")
                    .copied()
                    .unwrap_or(0),
            )
        };
        self.deferred_sequences
            .keys()
            .chain(self.synchronization_errors.values())
            .filter(|sequence| **sequence > 0)
            .fold(floor, |floor, sequence| {
                floor.min(sequence.saturating_sub(1))
            })
    }

    /// Retry dependency failures even when a later independent fact was applied.
    pub(crate) fn apply_polled_fact(&mut self, value: &Value) -> Option<FactProjection> {
        let sequence = value["fact_sequence"].as_u64()?;
        let previous = self.polled_sequence;
        self.polled_sequence = previous.max(sequence);
        let late_scope = value["kind"] == "project_scope"
            && self
                .fact_sequences
                .get("project_scope")
                .is_none_or(|last| sequence > *last);
        if sequence >= previous
            || late_scope
            || self.deferred_sequences.contains_key(&sequence)
            || self
                .synchronization_errors
                .values()
                .any(|failed| *failed == sequence)
        {
            Some(self.apply_verified_fact(value))
        } else {
            None
        }
    }

    pub(crate) fn synchronization_finished(&mut self, receipt: &receipt::FactReceipt) {
        for key in [receipt.kind.as_str(), "transport"] {
            if self
                .synchronization_errors
                .get(key)
                .is_some_and(|last| *last <= receipt.fact_sequence)
            {
                self.synchronization_errors.remove(key);
            }
        }
        let source_receipt = matches!(
            receipt.kind.as_str(),
            "source_groups" | "source_ledger" | "source_linework" | "source_linework_selection"
        );
        let current_source_receipt = !source_receipt
            || (self.source_view_sequence == Some(receipt.fact_sequence)
                && ((receipt.kind == "source_groups" && self.snapshot.groups.is_some())
                    || (receipt.kind == "source_ledger" && self.snapshot.ledger.is_some())
                    || (matches!(
                        receipt.kind.as_str(),
                        "source_linework" | "source_linework_selection"
                    ) && self.snapshot.linework.is_some())));
        let stamped_current_source =
            source_receipt && current_source_receipt && self.source_view.is_some();
        if current_source_receipt
            && (stamped_current_source
                || self
                    .snapshot
                    .verified_fact
                    .as_ref()
                    .is_none_or(|fact| fact.sequence <= receipt.fact_sequence))
        {
            self.snapshot.verified_fact = Some(VerifiedFact {
                kind: receipt.kind.clone(),
                sequence: receipt.fact_sequence,
                sha256: receipt.fact_sha256.clone(),
                bytes: receipt.fact_bytes,
            });
        }
        if self.synchronization_errors.is_empty() {
            self.snapshot.notice = None;
            self.refresh_rejections();
        } else {
            self.show_remaining_synchronization_errors();
        }
    }

    fn show_remaining_synchronization_errors(&mut self) {
        let pending = self
            .synchronization_errors
            .iter()
            .take(8)
            .map(|(kind, sequence)| format!("{kind} (seq {sequence})"))
            .collect::<Vec<_>>()
            .join("、");
        self.snapshot.fact_sync_notice = Some(format!(
            "领域事实同步失败：尚未核验 {pending}；请重新查询对应事实"
        ));
    }

    pub(crate) fn synchronization_queue_drained(&mut self) {
        self.synchronizing = false;
        if self.synchronization_errors.is_empty() {
            self.refresh_rejections();
        } else {
            self.show_remaining_synchronization_errors();
        }
    }

    pub(crate) fn synchronization_has_errors(&self) -> bool {
        !self.synchronization_errors.is_empty()
    }

    fn refresh_rejections(&mut self) {
        let scope = self.snapshot.project_scope.as_ref();
        let proposal = self.snapshot.proposal.as_ref().map(|p| &p.binding);
        if let Some(activation) = self.fact_sequences.get("project_scope") {
            self.deferred_sequences
                .retain(|sequence, _| sequence > activation);
        }
        for fact in &mut self.snapshot.rejected_facts {
            let old_activation = fact.sequence < self.activation_sequence;
            let other_scope = scope.is_some_and(|scope| {
                fact.scope_id
                    .as_ref()
                    .is_some_and(|id| id != &scope.scope_id)
                    || scope.activation_revision.is_some_and(|current| {
                        fact.activation_revision.is_some_and(|old| old < current)
                    })
            });
            let other_proposal = matches!(
                fact.kind.as_str(),
                "proposal"
                    | "artifact"
                    | "artifact_validation"
                    | "component_first_use_review"
                    | "application"
                    | "operation"
            ) && proposal.is_some_and(|binding| {
                let same_proposal = fact.proposal_id.as_ref() == Some(&binding.proposal_id);
                fact.proposal_id
                    .as_ref()
                    .is_some_and(|id| id != &binding.proposal_id)
                    || (same_proposal
                        && (fact
                            .revision
                            .is_some_and(|revision| revision < binding.revision)
                            || (fact.revision == Some(binding.revision)
                                && fact
                                    .prepare_token
                                    .as_ref()
                                    .is_some_and(|token| token != &binding.prepare_token))))
            });
            let key = fact
                .build_id
                .as_ref()
                .map(|id| format!("{}:{id}", fact.kind));
            let replaced = self
                .projected_sequences
                .get(key.as_deref().unwrap_or(&fact.kind))
                .is_some_and(|seq| *seq > fact.sequence);
            fact.blocking = !old_activation
                && !other_scope
                && !other_proposal
                && !replaced
                && matches!(
                    fact.kind.as_str(),
                    "project_scope"
                        | "project_selection"
                        | "project_catalog"
                        | "proposal"
                        | "artifact"
                        | "artifact_validation"
                        | "component_first_use_review"
                        | "application"
                        | "operation"
                );
        }
        if self.synchronization_errors.is_empty() {
            self.snapshot.fact_sync_notice = self
                .snapshot
                .rejected_facts
                .iter()
                .find(|fact| fact.blocking)
                .map(|fact| {
                    format!(
                        "领域事实{}：{} (seq {})，{}",
                        if fact.dependency_rejected { "依赖被拒绝" } else { "已拒绝" },
                        fact.kind, fact.sequence, fact.reason
                    )
                })
                .or_else(|| self.deferred_sequences.iter().next().map(|(sequence, kind)| {
                    format!("领域事实依赖尚未到达：{kind} (seq {sequence})；等待对应作用域或前置事实")
                }))
                .or_else(|| self.synchronizing.then(|| "正在同步完整领域事实".into()));
        }
    }

    pub fn remember_control(&mut self, text: &str) {
        while self.control_messages.len() >= 8 && !self.control_messages.contains_key(text) {
            let key = self.control_messages.keys().next().cloned().unwrap();
            self.control_messages.remove(&key);
        }
        let opinion = text
            .rsplit_once('\n')
            .filter(|(_, payload)| {
                serde_json::from_str::<Value>(payload)
                    .ok()
                    .is_some_and(|v| {
                        matches!(
                            v["user_action"].as_str(),
                            Some("revise_preview_selection" | "selected_preview_context")
                        )
                    })
            })
            .map(|(visible, _)| visible);
        self.control_messages.insert(
            text.into(),
            opinion
                .unwrap_or_else(|| text.lines().next().unwrap_or("具体版本已批准"))
                .into(),
        );
    }

    pub fn is_control_message(&self, text: &str) -> bool {
        self.control_messages.contains_key(text)
    }

    pub fn project_control_messages(&self, messages: &mut [crate::state::ConversationItem]) {
        for message in messages {
            if let Some(display) = self.control_messages.get(&message.text) {
                message.text.clone_from(display);
            }
        }
    }

    fn capture_approval_card(&mut self, fact_sequence: u64) {
        let Some(binding) = self
            .snapshot
            .proposal
            .as_ref()
            .map(|proposal| proposal.binding.clone())
        else {
            return;
        };
        let card = ApprovalCard {
            snapshot: self.snapshot.approval_card_snapshot(),
            fact_sequence: self
                .snapshot
                .approval_cards
                .iter()
                .find(|existing| existing.binding() == Some(&binding))
                .map_or(fact_sequence, |existing| existing.fact_sequence),
        };
        self.snapshot
            .approval_cards
            .retain(|existing| existing.binding() != Some(&binding));
        self.snapshot.approval_cards.push(card);
    }

    fn invalidate_approval_card(&mut self, binding: &ProposalBinding) {
        if self.snapshot.approval_card_tombstones.contains(binding) {
            return;
        }
        self.snapshot
            .approval_cards
            .retain(|card| card.binding() != Some(binding));
    }

    fn invalidate_approval_cards_for_proposal(&mut self, proposal_id: &str) {
        if self
            .snapshot
            .approval_card_restore
            .as_ref()
            .is_some_and(|restored| restored.proposal_id == proposal_id)
        {
            self.snapshot.approval_card_restore = None;
        }
        let approved = self.snapshot.approval_card_tombstones.clone();
        self.snapshot.approval_cards.retain(|card| {
            card.binding().is_none_or(|binding| {
                binding.proposal_id != proposal_id || approved.contains(binding)
            })
        });
    }

    fn invalidate_other_approval_versions(&mut self, binding: &ProposalBinding) {
        let approved = self.snapshot.approval_card_tombstones.clone();
        self.snapshot.approval_cards.retain(|card| {
            card.binding().is_none_or(|existing| {
                existing.proposal_id != binding.proposal_id
                    || existing == binding
                    || approved.contains(existing)
            })
        });
    }

    /// Retain the exact card for a grant that may need redelivery, while removing it
    /// from the set of cards awaiting approval.
    pub fn remove_approval_card(&mut self, binding: &ProposalBinding) {
        if self
            .snapshot
            .proposal
            .as_ref()
            .is_some_and(|proposal| proposal.binding == *binding)
        {
            self.capture_approval_card(self.highest_sequence);
        }
        if !self.snapshot.approval_card_tombstones.contains(binding) {
            self.snapshot.approval_card_tombstones.push(binding.clone());
        }
    }

    pub(crate) fn can_auto_authorize(&self, binding: &ProposalBinding) -> bool {
        self.auto_approval_binding.as_ref() == Some(binding)
            && self
                .snapshot
                .approval_card(binding)
                .and_then(|card| card.first_use_review.as_ref())
                .is_some_and(|review| review.required_count == 0)
            && self.can_authorize(binding)
    }

    /// The displayed proposal is still exactly this version and nothing applied it yet.
    pub fn shows_binding(&self, binding: &ProposalBinding) -> bool {
        self.snapshot.approval_card(binding).is_some_and(|card| {
            card.source_current != Some(false)
                && !card.proposal_not_approvable
                && card
                    .proposal
                    .as_ref()
                    .is_some_and(|proposal| &proposal.binding == binding)
                && card.application.is_none()
                && card.operation.is_none()
        }) && !self
            .snapshot
            .rejected_facts
            .iter()
            .any(|fact| fact.blocking)
    }

    pub fn can_authorize(&self, binding: &ProposalBinding) -> bool {
        let Some(card) = self.snapshot.approval_card(binding) else {
            return false;
        };
        self.setup.as_ref().is_some_and(|setup| {
            setup.run_id == binding.run_id
                && setup.context_sha256 == binding.context_sha256
                && (setup.mode == DomainMode::Floor
                    || card.project_scope.as_ref().is_some_and(|scope| {
                        card.selection.as_ref().is_some_and(|selection| {
                            selection.selection_id == scope.selection_id
                                && selection.state == SelectionState::Selected
                        }) && scope.assembly_binding.as_ref().is_none_or(|binding| {
                            card.buildings.iter().any(|building| {
                                building.build_id == binding.build_id
                                    && building.input_binding == binding.input_binding
                                    && building.current
                                    && building.valid == Some(true)
                                    && building.state == "built"
                            })
                        })
                    }))
        }) && !self.synchronizing
            && self.synchronization_errors.is_empty()
            && !self
                .snapshot
                .rejected_facts
                .iter()
                .any(|fact| fact.blocking)
            && self.deferred_sequences.is_empty()
            && !self.snapshot.authorizing
            && self.snapshot.approval_card_is_pending(binding)
            && card.source_current != Some(false)
            && !card.proposal_not_approvable
            && !card.approval_restore_blocked
            && card.application.is_none()
            && card.operation.is_none()
            && card.candidate_review_delivered(binding)
            && card.first_use_satisfied()
            && card
                .proposal
                .as_ref()
                .is_some_and(|proposal| proposal.ready && proposal.binding == *binding)
    }

    fn replace_review_images(&mut self, images: Vec<ReviewImageFact>, required: bool) {
        self.snapshot.replace_review_images(images, required);
    }
}

fn artifact_fact(value: &Value, proposal: &ProposalBinding) -> Option<ArtifactFact> {
    let a = &value["artifact"];
    let candidate = a["candidate"].as_bool()?;
    let binding = ArtifactBinding {
        build_id: text(a, "build_id").ok()?,
        proposal_id: text(a, "proposal_id").ok()?,
        revision: a["revision"].as_u64()?,
        prepare_token: text(a, "prepare_token").ok()?,
        candidate,
        authority_sha256: text(a, "authority_sha256").ok()?,
        input_sha256: text(a, "input_sha256").ok()?,
        output: text(a, "output").ok()?,
    };
    if binding.proposal_id != proposal.proposal_id
        || binding.revision != proposal.revision
        || binding.prepare_token != proposal.prepare_token
        || binding.input_sha256 != proposal.candidate_sha256
        || binding.authority_sha256.as_str()
            != if candidate {
                proposal.base_sha256.as_str()
            } else {
                proposal.candidate_sha256.as_str()
            }
    {
        return None;
    }
    let state = match a["state"].as_str()? {
        "queued" => ArtifactState::Queued,
        "building" => ArtifactState::Building,
        "unknown" => ArtifactState::Unknown,
        "cancelled" => ArtifactState::Cancelled,
        "failed" => ArtifactState::Failed,
        "built" => ArtifactState::Built,
        _ => return None,
    };
    let valid = if value["kind"] == "artifact_validation" {
        Some(a["valid"].as_bool()?)
    } else {
        None
    };
    if valid == Some(true) && state != ArtifactState::Built {
        return None;
    }
    let image = &a["review_image"];
    let reviewed = value["kind"] == "artifact_validation"
        && image["delivery"] == "attached_png"
        && image["image_sha256"]
            .as_str()
            .is_some_and(|sha| sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()));
    Some(ArtifactFact {
        binding,
        state,
        current: a["current"].as_bool(),
        valid,
        reviewed,
    })
}

fn operation_fact(value: &Value, binding: &ProposalBinding) -> Option<OperationFact> {
    let receipt = &value["receipt"];
    if receipt["revision"] != binding.revision
        || receipt["base_sha256"] != binding.base_sha256
        || receipt["candidate_sha256"] != binding.candidate_sha256
    {
        return None;
    }
    let operation_id = text(receipt, "operation_id").ok()?;
    if operation_id.len() > 80
        || !operation_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return None;
    }
    let state = text(receipt, "state").ok()?;
    if !matches!(
        state.as_str(),
        "applying" | "applied" | "built" | "rejected" | "failed"
    ) {
        return None;
    }
    let recovery = receipt["recovery"].as_str().unwrap_or("not_required");
    if !matches!(
        recovery,
        "candidate_present"
            | "base_present"
            | "conflict"
            | "not_required"
            | "reconciled_without_replay"
    ) || (state == "applying" && recovery == "not_required")
    {
        return None;
    }
    let current_sha256 = receipt["current_sha256"]
        .as_str()
        .or_else(|| value["current_authority_sha256"].as_str())?;
    if current_sha256.len() != 64 || !current_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(OperationFact {
        operation_id,
        state,
        recovery: recovery.into(),
        current_sha256: current_sha256.into(),
    })
}

fn linework_coverage_fact(value: &Value) -> Result<Option<LineworkCoverageFact>, ()> {
    let Some(summary) = value.get("linework_summary") else {
        return if value["source_view_contract_version"] == "source-view/2" {
            Err(())
        } else {
            Ok(None)
        };
    };
    if summary["contract_version"] != "source-linework/1" {
        return Err(());
    }
    let count = |key: &str| usize::try_from(summary[key].as_u64().ok_or(())?).map_err(|_| ());
    let fact = LineworkCoverageFact {
        scan_id: text(value, "scan_id").map_err(|_| ())?,
        scene_sha256: text(value, "scene_sha256").map_err(|_| ())?,
        member_count: count("member_count")?,
        selected_member_count: count("selected_member_count")?,
        unselected_member_count: count("unselected_member_count")?,
        candidate_count: count("candidate_count")?,
        unclassified_candidate_count: count("unclassified_candidate_count")?,
        region_count: count("region_count")?,
        annotation_count: count("annotation_count")?,
        uncovered_count: count("uncovered_count")?,
        diagnostic_member_count: count("diagnostic_member_count")?,
        stale_selection_count: count("stale_selection_count")?,
    };
    if fact
        .selected_member_count
        .checked_add(fact.unselected_member_count)
        != Some(fact.member_count)
        || fact.candidate_count > fact.selected_member_count
        || fact.unclassified_candidate_count > fact.candidate_count
        || fact.diagnostic_member_count > fact.member_count
    {
        return Err(());
    }
    Ok(Some(fact))
}

fn source_region_review_fact(
    value: &Value,
    linework: Option<&LineworkCoverageFact>,
    structure: Option<&ExistingStructureCoverageFact>,
) -> Result<Option<SourceRegionReviewFact>, ()> {
    let Some(summary) = value.get("source_region_summary") else {
        return Ok(None);
    };
    // /2 adds the skipped_unknown decision; such regions count as reviewed.
    if !matches!(
        summary["contract_version"].as_str(),
        Some("source-region-review/1" | "source-region-review/2")
    ) {
        return Err(());
    }
    let linework = linework.ok_or(())?;
    let count = |key: &str| usize::try_from(summary[key].as_u64().ok_or(())?).map_err(|_| ());
    let fact = SourceRegionReviewFact {
        scan_id: linework.scan_id.clone(),
        scene_sha256: linework.scene_sha256.clone(),
        region_count: count("region_count")?,
        reviewed_region_count: count("reviewed_region_count")?,
        unreviewed_region_count: count("unreviewed_region_count")?,
        structure_represented_region_count: match summary.get("structure_represented_region_count")
        {
            None => 0,
            Some(_) => count("structure_represented_region_count")?,
        },
        uncertain_region_count: count("uncertain_region_count")?,
        stale_review_count: count("stale_review_count")?,
        orphaned_stale_review_count: count("orphaned_stale_review_count")?,
    };
    if fact
        .reviewed_region_count
        .checked_add(fact.unreviewed_region_count)
        .and_then(|total| total.checked_add(fact.structure_represented_region_count))
        != Some(fact.region_count)
        || fact.region_count != linework.region_count
        || fact.uncertain_region_count > fact.reviewed_region_count
        || summary
            .get("skipped_unknown_region_count")
            .is_some_and(|skipped| {
                skipped.as_u64().is_none_or(|skipped| {
                    fact.uncertain_region_count as u64 + skipped > fact.reviewed_region_count as u64
                })
            })
        || fact.stale_review_count > fact.unreviewed_region_count
        || (fact.structure_represented_region_count > 0
            && structure.is_none_or(|coverage| {
                coverage.complete_member_count < fact.structure_represented_region_count
            }))
    {
        return Err(());
    }
    Ok(Some(fact))
}

fn existing_structure_coverage_fact(
    value: &Value,
    linework: Option<&LineworkCoverageFact>,
) -> Result<Option<ExistingStructureCoverageFact>, ()> {
    let Some(summary) = value.get("existing_structure_summary") else {
        return Ok(None);
    };
    if summary["contract_version"] != "source-structure-coverage/1" {
        return Err(());
    }
    let linework = linework.ok_or(())?;
    let count = |key: &str| usize::try_from(summary[key].as_u64().ok_or(())?).map_err(|_| ());
    let fact = ExistingStructureCoverageFact {
        scan_id: linework.scan_id.clone(),
        scene_sha256: linework.scene_sha256.clone(),
        member_count: count("member_count")?,
        complete_member_count: count("complete_member_count")?,
        partial_member_count: count("partial_member_count")?,
        reference_only_member_count: count("reference_only_member_count")?,
        unproven_member_count: count("unproven_member_count")?,
        unselected_complete_member_count: count("unselected_complete_member_count")?,
    };
    if [
        fact.complete_member_count,
        fact.partial_member_count,
        fact.reference_only_member_count,
        fact.unproven_member_count,
    ]
    .into_iter()
    .try_fold(0usize, usize::checked_add)
        != Some(fact.member_count)
        || fact.member_count != linework.member_count
        || fact.unselected_complete_member_count > fact.complete_member_count
        || fact.unselected_complete_member_count > linework.unselected_member_count
    {
        return Err(());
    }
    Ok(Some(fact))
}

fn opening_inventory_fact(value: &Value) -> Result<Option<OpeningInventoryFact>, ()> {
    let Some(summary) = value.get("opening_summary") else {
        return Ok(None);
    };
    if summary["contract"] != "approved-opening-inventory/1" {
        return Err(());
    }
    let count = |key: &str| usize::try_from(summary[key].as_u64().ok_or(())?).map_err(|_| ());
    let fact = OpeningInventoryFact {
        scan_id: if value["kind"] == "capabilities" && value.get("scan_id").is_none() {
            None
        } else {
            Some(text(value, "scan_id").map_err(|_| ())?)
        },
        scene_sha256: text(value, "scene_sha256").map_err(|_| ())?,
        inventory_verified: match summary["inventory_status"].as_str() {
            Some("verified") => true,
            Some("unverified") => false,
            _ => return Err(()),
        },
        total_count: count("total_count")?,
        verified_aperture_count: count("verified_aperture_count")?,
        missing_entity_count: count("missing_entity_count")?,
        complete_entity_count: count("complete_entity_count")?,
        unverified_count: count("unverified_count")?,
    };
    if fact.scene_sha256.len() != 64
        || !fact
            .scene_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || fact.verified_aperture_count > fact.total_count
        || fact
            .missing_entity_count
            .checked_add(fact.complete_entity_count)
            .is_none_or(|count| count > fact.total_count)
        || fact.unverified_count > fact.total_count
    {
        return Err(());
    }
    Ok(Some(fact))
}

fn source_groups_fact(value: &Value) -> Option<SourceGroupsFact> {
    let groups = value["groups"].as_array()?;
    let total_groups = usize::try_from(value["total_groups"].as_u64()?).ok()?;
    let total_objects = usize::try_from(value["total_objects"].as_u64()?).ok()?;
    let assessed_groups = value["assessed_groups"].as_u64().unwrap_or(0) as usize;
    if assessed_groups > total_groups || groups.len() != total_groups || groups.len() > 1000 {
        return None;
    }
    let mut sum = 0usize;
    let mut assessed = 0usize;
    let mut category_counts = BTreeMap::<String, usize>::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    for group in groups {
        let id = text(group, "group_id").ok()?;
        let count = usize::try_from(group["instance_count"].as_u64()?).ok()?;
        if !ids.insert(id.clone()) || count == 0 || group["instances"].as_array()?.len() != count {
            return None;
        }
        sum = sum.checked_add(count)?;
        let assessment = if group["assessment"].is_null() {
            None
        } else {
            assessed += 1;
            Some(bounded_text(&group["assessment"], "reason", 2000)?)
        };
        let group_counts: BTreeMap<String, usize> =
            serde_json::from_value(group["state_counts"].clone()).ok()?;
        let categories: BTreeMap<String, usize> =
            serde_json::from_value(group["category_counts"].clone()).ok()?;
        if categories
            .values()
            .try_fold(0usize, |n, count| n.checked_add(*count))?
            != count
            || categories
                .keys()
                .any(|category| category.is_empty() || category.chars().count() > 80)
        {
            return None;
        }
        for (category, count) in categories {
            let total = category_counts.entry(category).or_default();
            *total = total.checked_add(count)?;
        }
        if group_counts
            .values()
            .try_fold(0usize, |n, c| n.checked_add(*c))?
            != count
        {
            return None;
        }
        if ids.len() <= 100 {
            let block = match group["block"].as_str() {
                Some("") if group["evidence_kind"] == "linework" => "AI 选择的线稿对象".to_owned(),
                _ => text(group, "block").ok()?,
            };
            rows.push(format!(
                "{} · {} 个 · 类别 {} · 状态 {} · {}",
                block, count, group["category_counts"], group["state_counts"], id
            ));
            if let Some(reason) = assessment {
                rows.push(format!(
                    "{}：{reason}",
                    if group["assessment"]["non_component"] == true
                        && group["instances"]
                            .as_array()?
                            .iter()
                            .all(|instance| instance["non_component"] == true)
                    {
                        "AI 判为非构件 · assumed · 依据"
                    } else {
                        "评估依据／问题"
                    }
                ));
            }
        }
    }
    if assessed != assessed_groups {
        return None;
    }
    let counts: BTreeMap<String, usize> =
        serde_json::from_value(value["state_counts"].clone()).ok()?;
    if sum != total_objects || counts.values().try_fold(0usize, |n, c| n.checked_add(*c))? != sum {
        return None;
    }
    if total_groups > 100 {
        rows.push(format!(
            "其余 {} 组已封存，请查询完整分组事实",
            total_groups - 100
        ));
    }
    Some(SourceGroupsFact {
        scan_id: text(value, "scan_id").ok()?,
        scene_sha256: text(value, "scene_sha256").ok()?,
        total_objects,
        total_groups,
        assessed_groups,
        category_counts,
        rows,
        counts,
    })
}

fn floor_progress_fact(
    value: &Value,
) -> Result<Option<(FloorProgress, Option<FloorProgressV2>)>, ()> {
    let Some(progress) = value.get("floor_progress") else {
        return Ok(None);
    };
    let parse = || {
        let count = |row: &Value, key: &str| usize::try_from(row[key].as_u64()?).ok();
        let entries = progress["categories"].as_array()?;
        let version_two = progress["contract"] == "floor-progress/2";
        if progress["contract"] != "floor-progress/1" && !version_two
            || progress["scan_id"] != value["scan_id"]
            || progress["scene_sha256"] != value["scene_sha256"]
            || entries.len() > 256
        {
            return None;
        }
        let mut names = std::collections::BTreeSet::new();
        let categories = entries
            .iter()
            .map(|row| {
                let category = bounded_text(row, "category", 80)?;
                if !names.insert(category.clone()) {
                    return None;
                }
                Some(CategoryProgress {
                    category,
                    built: count(row, "built")?,
                    pending: count(row, "pending")?,
                    blocked: count(row, "blocked")?,
                    unidentified: count(row, "unidentified")?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let coverage = &progress["drawing_coverage"];
        let covered = count(coverage, "covered")?;
        let total = count(coverage, "total")?;
        let unreviewed_regions = count(progress, "unreviewed_regions")?;
        if covered > total
            || Some(unreviewed_regions as u64)
                != value["source_region_summary"]["unreviewed_region_count"].as_u64()
        {
            return None;
        }
        let v2 = if version_two {
            let blockers = progress["completion_blockers"].as_array()?;
            let counts = progress["disposition_counts"].as_object()?;
            let deferrals = progress["deferrals"].as_array()?;
            let skips = progress["skips"].as_array()?;
            let legacy_items = progress["legacy_items"].as_array()?;
            if blockers.len() > 256
                || counts.len() > 256
                || [deferrals, skips, legacy_items]
                    .iter()
                    .any(|items| items.len() > 10_000 || items.iter().any(|item| !item.is_object()))
            {
                return None;
            }
            let blockers = blockers
                .iter()
                .map(|item| {
                    let blocker = item.as_str()?;
                    (!blocker.is_empty() && blocker.chars().count() <= 80)
                        .then(|| crate::state::display_text(blocker))
                })
                .collect::<Option<Vec<_>>>()?;
            let counts = counts
                .iter()
                .map(|(key, value)| {
                    (!key.is_empty() && key.chars().count() <= 80).then_some((
                        crate::state::display_text(key),
                        usize::try_from(value.as_u64()?).ok()?,
                    ))
                })
                .collect::<Option<BTreeMap<_, _>>>()?;
            Some(FloorProgressV2 {
                completion: progress["completion"].as_bool()?,
                completion_blockers: blockers,
                disposition_counts: counts,
                cross_floor_unverified: count(progress, "cross_floor_unverified")?,
                deferrals: deferrals.len(),
                skips: skips.len(),
                legacy_items: legacy_items.len(),
            })
        } else {
            None
        };
        Some((
            FloorProgress {
                scan_id: text(value, "scan_id").ok()?,
                scene_sha256: text(value, "scene_sha256").ok()?,
                categories,
                covered,
                total,
                coverage_unit: bounded_text(coverage, "unit", 240)?,
                unreviewed_regions,
            },
            v2,
        ))
    };
    parse().map(Some).ok_or(())
}

fn source_ledger_fact(value: &Value) -> Option<SourceLedgerFact> {
    let mut counts = BTreeMap::new();
    let mut rows = Vec::new();
    for row in value["objects"].as_array()?.iter().take(100) {
        let state = text(row, "state").ok()?;
        if !matches!(
            state.as_str(),
            "built"
                | "pending_addition"
                | "pending_confirmation"
                | "suspected_duplicate"
                | "skipped"
                | "rejected"
                | "capability_missing"
        ) {
            return None;
        }
        *counts.entry(state.clone()).or_insert(0) += 1;
        rows.push(format!(
            "{} · {} · {} · 位置 {} · 已有实例 {}",
            text(row, "source_id").ok()?,
            row["category"].as_str().unwrap_or("待确认类别"),
            source_state_label(&state),
            row["origin_m"],
            row["matched_component_ids"]
        ));
        if row["non_component"] == true {
            let reason = bounded_text(row, "classification_reason", 2000)?;
            let basis = text(row, "classification_basis").ok()?;
            if !matches!(basis.as_str(), "assumed" | "user_confirmed") {
                return None;
            }
            rows.push(format!("判为非构件 · {basis} · 依据：{reason}"));
        }
        if let Some(aperture) = row["aperture_status"].as_str() {
            let entity = row["entity_status"].as_str()?;
            if !matches!(aperture, "verified" | "unverified" | "geometry_bound")
                || !matches!(entity, "complete" | "missing" | "unverified")
            {
                return None;
            }
            rows.push(format!(
                "洞口 {} · 门窗实体 {}",
                match aperture {
                    "verified" => "已核验",
                    "geometry_bound" => "几何已绑定",
                    _ => "待核验",
                },
                match entity {
                    "complete" => "已建",
                    "missing" => "缺失",
                    _ => "待核验",
                }
            ));
        }
    }
    if let Some(total_counts) = value["state_counts"].as_object() {
        counts = total_counts
            .iter()
            .map(|(state, count)| Some((state.clone(), usize::try_from(count.as_u64()?).ok()?)))
            .collect::<Option<_>>()?;
    }
    Some(SourceLedgerFact {
        scan_id: text(value, "scan_id").ok()?,
        scene_sha256: text(value, "scene_sha256").ok()?,
        rows,
        counts,
    })
}

fn bounded_text(value: &Value, key: &str, max_characters: usize) -> Option<String> {
    let text = value[key].as_str()?;
    (!text.is_empty() && text.chars().count() <= max_characters)
        .then(|| crate::state::display_text(text))
}

fn proposal_category_counts(proposal: &Value) -> BTreeMap<String, usize> {
    let mut components = BTreeMap::new();
    for key in ["required", "reused"] {
        for item in proposal["component_first_use_requirements"][key]
            .as_array()
            .into_iter()
            .flatten()
        {
            for id in item["component_ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                components.insert(
                    id.to_owned(),
                    item["category"].as_str().unwrap_or("未分类").to_owned(),
                );
            }
        }
    }
    if let Some(selected) = proposal["component_edit"]["selected_parts"].as_object() {
        for (id, parts) in selected {
            components.entry(id.clone()).or_insert_with(|| {
                parts[0]["metadata"]["category"]
                    .as_str()
                    .unwrap_or("未分类")
                    .to_owned()
            });
        }
    }
    for op in proposal["draft"]["operations"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if !matches!(
            op["collection"].as_str(),
            Some("solids" | "elements" | "walls" | "slabs" | "openings")
        ) || op["op"] == "remove"
        {
            continue;
        }
        let Some(entity) = op["value_json"]
            .as_str()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        else {
            continue;
        };
        let Some(id) = entity["metadata"]["component_id"]
            .as_str()
            .or_else(|| entity["id"].as_str())
        else {
            continue;
        };
        let category = entity["metadata"]["category"]
            .as_str()
            .or_else(|| entity["role"].as_str())
            .unwrap_or("未分类");
        components
            .entry(id.to_owned())
            .or_insert_with(|| category.to_owned());
    }
    let mut counts = BTreeMap::new();
    for category in components.values() {
        *counts.entry(category.clone()).or_default() += 1;
    }
    counts
}

fn proposal_fact(value: &Value) -> Option<ProposalFact> {
    let p = &value["proposal"];
    let binding = ProposalBinding {
        run_id: text(value, "run_id").ok()?,
        context_sha256: text(value, "context_sha256").ok()?,
        proposal_id: text(p, "proposal_id").ok()?,
        revision: p["revision"].as_u64()?,
        prepare_token: text(p, "prepare_token").ok()?,
        base_sha256: text(p, "base_sha256").ok()?,
        candidate_sha256: p["candidate_sha256"].as_str().unwrap_or("").to_owned(),
    };
    let mut objects = Vec::new();
    let mut parameter_only = false;
    let mut seen_connections = std::collections::BTreeSet::new();
    let mut remaining_reference_parts = std::collections::BTreeSet::new();
    // 已验证事实受单文件及分页聚合预算约束，批准前必须逐条投影全部操作。
    let correction = p.get("drawing_review").filter(|v| !v.is_null());
    let edit = p.get("component_edit").filter(|v| !v.is_null());
    // 交通修订的方案和来源封存在编辑证书中，不另造普通 component_plan。
    let component_plan = edit
        .map(|proof| &proof["plan"]["replacements"])
        .unwrap_or(&p["component_plan"]);
    let vertical_sources = edit
        .map(|proof| &proof["sources"])
        .unwrap_or(&p["vertical_sources"]);
    let structure_revision =
        !p["structure_revision_source"].is_null() || !p["slab_revision_source"].is_null();
    if structure_revision {
        objects.extend(project::structure_revision_rows(value)?);
    }
    if edit.is_some() {
        objects.extend(component_edit::projection(p)?);
    }
    if let Some(review) = correction {
        if review["contract"] != "drawing-correction/1"
            || review["base_sha256"] != p["base_sha256"]
            || p["decision_drafts"].as_array()?.len() != 1
            || p["draft"]["operations"].as_array()?.len() > 100
        {
            return None;
        }
        let decision = &p["decision_drafts"][0];
        objects.push(format!(
            "图纸修正决策 {} · {}",
            text(decision, "id").ok()?,
            text(decision, "title").ok()?
        ));
        objects.push(
            decision["body"]
                .as_str()
                .filter(|body| !body.is_empty() && body.len() <= 65536)?
                .to_owned(),
        );
    }
    for op in p["draft"]["operations"].as_array()? {
        if op["op"] == "set_floor_story_height" {
            let decision: Value = serde_json::from_str(op["value_json"].as_str()?).ok()?;
            let record = &p["decision_drafts"][0];
            let body: Value = serde_json::from_str(record["body"].as_str()?).ok()?;
            let request = text(&decision, "request_id").ok()?;
            let floor = text(&decision, "floor_id").ok()?;
            let reason = bounded_text(&decision, "reason", 2000)?;
            if p["draft"]["operations"].as_array()?.len() != 1
                || p["decision_drafts"].as_array()?.len() != 1
                || structure_revision
                || edit.is_some()
                || correction.is_some()
                || op["target_id"] != floor
                || op["floor_id"] != floor
                || p["scope"]["floor_id"] != floor
                || (value["project_scope"].is_object()
                    && value["project_scope"]["floor_id"] != floor)
                || !op["collection"].is_null()
                || !op["field"].is_null()
                || op["basis"] != "decision"
                || !op["source_ids"].as_array()?.is_empty()
                || op["decision_id"] != format!("floor-height-{request}")
                || record["id"] != op["decision_id"]
                || body != decision
                || op["reason"] != decision["reason"]
                || decision["height_m"].as_f64().is_none_or(|v| v <= 0.0)
                || decision["no_split_levels"] != true
                || decision["allow_stair_above_floor"] != true
                || (!decision["reference_wall_height_m"].is_null()
                    && decision["reference_wall_height_m"]
                        .as_f64()
                        .is_none_or(|v| v <= 0.0))
                || text(&decision, "decided_at").is_err()
                || ["base_sha256", "candidate_sha256"].iter().any(|key| {
                    p[key].as_str().is_none_or(|sha| {
                        sha.len() != 64
                            || !sha
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    })
                })
                || !p["component_first_use_requirements"]["required"]
                    .as_array()?
                    .is_empty()
                || !p["component_first_use_requirements"]["reused"]
                    .as_array()?
                    .is_empty()
            {
                return None;
            }
            objects.push(format!(
                "楼层层高参数决定 · {floor} · {} m · 几何不变",
                decision["height_m"]
            ));
            parameter_only = true;
            objects.push(format!(
                "决定 {} · {} · {reason}",
                record["id"], decision["decided_at"]
            ));
            objects.push(format!(
                "无错层 {} · 允许楼梯超层 {} · 参考墙高 {} m",
                decision["no_split_levels"],
                decision["allow_stair_above_floor"],
                decision["reference_wall_height_m"]
            ));
            continue;
        }
        if structure_revision {
            continue; // Exact revision operations were projected with their certificate.
        }
        if edit.is_some() && op["op"] == "remove" {
            continue; // Exact removals and old geometry were projected from the verified certificate.
        }
        if correction.is_some() {
            if op["basis"] != "decision"
                || op["decision_id"] != p["decision_drafts"][0]["id"]
                || op["floor_id"] != p["scope"]["floor_id"]
                || op["source_ids"].as_array()?.is_empty()
            {
                return None;
            }
            let target = text(op, "target_id").ok()?;
            let reason = text(op, "reason").ok()?;
            match op["op"].as_str()? {
                "set" => {
                    let field = text(op, "field").ok()?;
                    if !matches!(
                        field.as_str(),
                        "start" | "end" | "thickness" | "offset" | "width" | "boundary" | "holes"
                    ) {
                        return None;
                    }
                    let new_value: Value = serde_json::from_str(op["value_json"].as_str()?).ok()?;
                    objects.push(format!(
                        "{target} · 修改 {field} = {new_value} · decision · {reason}"
                    ));
                }
                "remove" => objects.push(format!("{target} · 移除 · decision · {reason}")),
                "add" => {
                    if !matches!(
                        op["collection"].as_str()?,
                        "walls" | "openings" | "elements"
                    ) {
                        return None;
                    }
                    let entity: Value = serde_json::from_str(op["value_json"].as_str()?).ok()?;
                    if entity["id"] != target {
                        return None;
                    }
                    objects.push(format!(
                        "{target} · 新增 {} · {entity} · decision · {reason}",
                        op["collection"]
                    ));
                }
                _ => return None,
            }
            objects.push(format!("修正来源 {}", op["source_ids"]));
            continue;
        }
        // Dedicated projections above validate operations that may omit a collection.
        // Other operations must not fall through as an empty parameter-decision card.
        op["collection"].as_str()?;
        if value["project_structure"] == true
            && matches!(
                op["collection"].as_str(),
                Some("walls" | "slabs" | "elements" | "constraints")
            )
        {
            let entity: Value = serde_json::from_str(op["value_json"].as_str()?).ok()?;
            let id = entity["id"].as_str()?;
            match op["collection"].as_str()? {
                "walls" => {
                    for point in [&entity["start"], &entity["end"]] {
                        point["x"].as_f64()?;
                        point["y"].as_f64()?;
                    }
                    if entity["height"].as_f64()? <= 0.0 || entity["thickness"].as_f64()? <= 0.0 {
                        return None;
                    }
                    objects.push(format!(
                        "{id} · 墙 · {} → {} · 厚 {} · 高 {} · {}",
                        entity["start"],
                        entity["end"],
                        entity["thickness"],
                        entity["height"],
                        op["basis"]
                    ));
                }
                "slabs" | "elements" => {
                    let boundary = entity["boundary"].as_array()?;
                    if boundary.len() < 3
                        || boundary.iter().any(|point| {
                            point["x"].as_f64().is_none() || point["y"].as_f64().is_none()
                        })
                    {
                        return None;
                    }
                    let height = if op["collection"] == "slabs" {
                        entity["elevation"].as_f64()?;
                        entity["thickness"].as_f64()?
                    } else {
                        entity["base_z"].as_f64()?;
                        entity["height"].as_f64()?
                    };
                    if height <= 0.0 {
                        return None;
                    }
                    objects.push(format!(
                        "{id} · {} · 轮廓 {} · 高/厚 {height} · {}",
                        op["collection"], entity["boundary"], op["basis"]
                    ));
                }
                "constraints" => {
                    objects.push(format!(
                        "{id} · 约束 {} · 目标 {}",
                        entity["kind"].as_str()?,
                        entity["target_ids"]
                    ));
                }
                _ => return None,
            }
            continue;
        }
        if op["collection"] == "openings" {
            if op["op"] == "set" {
                let target = text(op, "target_id").ok()?;
                let specs = p["component_plan"]["openings"].as_array()?;
                let spec = specs.iter().find(|spec| {
                    spec["operation"] == "complete-entity"
                        && spec["target_opening_id"].as_str() == Some(target.as_str())
                })?;
                let assets: Value = serde_json::from_str(op["value_json"].as_str()?).ok()?;
                let kind = spec["kind"].as_str()?;
                if op["field"] != "assets"
                    || op["basis"] != "assumed"
                    || assets != spec["assets"]
                    || !assets.is_object()
                    || !matches!(
                        (kind, assets["style"].as_str()),
                        ("door", Some("door")) | ("window", Some("window" | "curtain_wall"))
                    )
                    || spec["height"].as_f64().is_none_or(|v| v <= 0.0)
                    || spec["sill_height"].as_f64().is_none_or(|v| v < 0.0)
                    || op["source_ids"].as_array()?.is_empty()
                {
                    return None;
                }
                let source = text(spec, "source_instance_id").ok()?;
                // Completion reuses an approved opening; it has no newly discovered
                // hosted source position. Keep that identity visible without inventing one.
                let location = if let Some(evidence) = p["hosted_sources"].get(&source) {
                    format!("来源位置 {}", evidence["origin_m"])
                } else if project::scope_fact(value)
                    .and_then(|scope| scope.opening_completion_binding)
                    .is_some_and(|binding| {
                        source == target && binding.opening_ids.contains(&target)
                    })
                {
                    format!("既有洞口 {source}")
                } else {
                    return None;
                };
                objects.push(format!(
                    "{target} · {kind} 实体补全 · 宿主 {} · {location}",
                    text(spec, "host_wall_id").ok()?,
                ));
                objects.push(format!(
                    "保留既有洞口尺寸与位置 · 高 {} · 窗台 {} · 实体参数 assumed：{assets}",
                    spec["height"], spec["sill_height"]
                ));
                continue;
            }
            let opening: Value = serde_json::from_str(op["value_json"].as_str()?).ok()?;
            let kind = opening["kind"].as_str()?;
            if !matches!(kind, "door" | "window")
                || ["offset", "width", "height", "sill_height"]
                    .iter()
                    .any(|key| opening[key].as_f64().is_none())
            {
                return None;
            }
            let source = opening["metadata"]["source_instance_id"]
                .as_str()
                .unwrap_or("");
            objects.push(format!(
                "{} · {} · 宿主 {} · 来源位置 {}",
                opening["id"].as_str()?,
                kind,
                opening["wall_id"].as_str()?,
                p["hosted_sources"][source]["origin_m"]
            ));
            objects.push(format!(
                "孔洞尺寸(m)：偏距 {} · 宽 {} · 高 {} · 窗台 {}",
                opening["offset"], opening["width"], opening["height"], opening["sill_height"]
            ));
            objects.push(format!(
                "参数依据：宽 {} · 高 {} · 窗台 {}",
                opening["metadata"]["width_basis"]
                    .as_str()
                    .unwrap_or("unknown"),
                opening["metadata"]["height_basis"]
                    .as_str()
                    .unwrap_or("unknown"),
                opening["metadata"]["sill_basis"]
                    .as_str()
                    .unwrap_or("unknown")
            ));
            continue;
        }
        if op["collection"] == "elements" {
            let element: Value = serde_json::from_str(op["value_json"].as_str()?).ok()?;
            let metadata = &element["metadata"];
            let schematic = metadata["relation_validator"] == "vertical-schematic/1";
            let reference = metadata["geometry_status"] == "reference_informed_schematic";
            let role = element["role"].as_str()?;
            let boundary = element["boundary"].as_array()?;
            let valid_role = if schematic {
                matches!(role, "stair_step" | "landing" | "partition" | "shaft_wall")
            } else {
                matches!(role, "stair_step" | "shaft_wall")
            };
            if !valid_role
                || reference && !schematic
                || !schematic
                    && !matches!(
                        metadata["relation_validator"].as_str(),
                        Some("vertical-connection/1" | "vertical-connection/2")
                    )
                || element["height"].as_f64()? <= 0.0
                || element["base_z"].as_f64().is_none()
                || boundary.len() < 3
                || boundary.len() > 512
                || boundary
                    .iter()
                    .any(|p| p["x"].as_f64().is_none() || p["y"].as_f64().is_none())
            {
                return None;
            }
            objects.push(format!(
                "{} · {} · 轮廓(m) {} · 底标高 {} · 高 {} · {}",
                element["id"].as_str()?,
                role,
                element["boundary"],
                element["base_z"],
                element["height"],
                op["basis"].as_str().unwrap_or("unknown")
            ));
            let component = metadata["component_id"].as_str()?;
            let first_connection = seen_connections.insert(component.to_owned());
            if !schematic && !first_connection {
                continue;
            }
            let connection = component_plan["connections"]
                .as_array()?
                .iter()
                .find(|c| c["component_id"].as_str() == Some(component))?;
            let source = metadata["source_instance_id"].as_str()?;
            let shaft = schematic && connection["kind"] == "shaft";
            let escalator =
                edit.is_some() && schematic && connection["circulation_type"] == "escalator";
            let matching_schematic_role = match connection["kind"].as_str() {
                Some("shaft") => role == "shaft_wall",
                Some("stair") => role != "shaft_wall",
                _ => false,
            };
            if schematic
                && (!matching_schematic_role
                    || connection["mode"] != "single_floor_schematic"
                    || connection["dimension_basis"] != "assumed"
                    || connection["rise_m"].as_f64().is_none_or(|v| v <= 0.0)
                    || connection["served_floors"].as_array()?.len() != 1
                    || connection["served_floors"][0] != p["scope"]["floor_id"]
                    || p["scope"]["floor_id"].as_str().is_none()
                    || !connection["openings"].as_array()?.is_empty()
                    || connection["source_instance_id"].as_str() != Some(source)
                    || !vertical_sources[source].is_object()
                    || metadata["modeling_mode"] != "single_floor_schematic"
                    || metadata["connection_status"] != "unverified"
                    || metadata["step_dimension_basis"] != "assumed"
                    || !matches!(
                        metadata["geometry_status"].as_str(),
                        Some("schematic" | "partial_schematic" | "reference_informed_schematic")
                    )
                    || bounded_text(metadata, "solid_representation", 1000).is_none()
                    || op["basis"] != "assumed")
            {
                return None;
            }
            if shaft || schematic && role == "partition" && !reference {
                let source_fact = &vertical_sources[source];
                if op["op"] != "add"
                    || op["target_id"] != element["id"]
                    || op["floor_id"] != p["scope"]["floor_id"]
                    || !op["source_ids"].as_array()?.contains(&json!(source))
                    || !element["source_ids"].as_array()?.contains(&json!(source))
                    || source_fact["source_id"] != source
                    || source_fact["component_id"] != component
                    || source_fact["floor_id"] != p["scope"]["floor_id"]
                {
                    return None;
                }
                for (source_key, metadata_key) in [
                    ("source_sha256", "source_sha256"),
                    ("geometry_sha256", "source_geometry_sha256"),
                ] {
                    let sha = source_fact[source_key].as_str()?;
                    if sha.len() != 64
                        || !sha
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        || metadata[metadata_key] != sha
                    {
                        return None;
                    }
                }
            }
            if shaft {
                let id = element["id"].as_str()?;
                if reference
                    || connection["shaft_representation"] != "exterior_massing"
                    || id != format!("{component}.enclosure")
                    || metadata["vertical_spec_owner"] != id
                    || metadata["circulation_type"] != "elevator"
                    || metadata["solid_representation"]
                        != "exterior massing only; cavity and entrance not inferred"
                    || !metadata["cavity_world_m"].is_null()
                    || !element["stair_soffit"].is_null()
                    || !element["stair_top"].is_null()
                    || !element["holes"].as_array()?.is_empty()
                    || ["flights", "landings", "reference_surfaces"]
                        .iter()
                        .any(|key| {
                            !connection[key].is_null()
                                && !connection[key].as_array().is_some_and(Vec::is_empty)
                        })
                    || !connection["steps"].is_null()
                    || !connection["riser_m"].is_null()
                    || (element["height"].as_f64()? - connection["rise_m"].as_f64()?).abs() > 0.001
                {
                    return None;
                }
            }
            if schematic && role == "partition" && !reference {
                // 扶梯的三段及精确方案已由编辑证书核验；它没有楼梯侧板线索引。
                if !escalator {
                    let indices: Vec<usize> =
                        serde_json::from_str(metadata["side_panel_line_indices"].as_str()?).ok()?;
                    let lines = vertical_sources[source]["linework_m"].as_array()?;
                    if indices.len() != 2 || indices.iter().any(|index| *index >= lines.len()) {
                        return None;
                    }
                    objects.push(format!(
                        "CAD 侧板来源折线(m) {} · {}",
                        lines[indices[0]], lines[indices[1]]
                    ));
                }
                let mut elevations = Vec::new();
                for plane in [&element["stair_soffit"], &element["stair_top"]] {
                    let sx = plane["start"]["x"].as_f64()?;
                    let sy = plane["start"]["y"].as_f64()?;
                    let dx = plane["end"]["x"].as_f64()? - sx;
                    let dy = plane["end"]["y"].as_f64()? - sy;
                    let length_squared = dx * dx + dy * dy;
                    if length_squared <= 1e-18 || !length_squared.is_finite() {
                        return None;
                    }
                    let start = plane["start_z"].as_f64()?;
                    let end = plane["end_z"].as_f64()?;
                    if boundary.iter().any(|point| {
                        let t = ((point["x"].as_f64().unwrap() - sx) * dx
                            + (point["y"].as_f64().unwrap() - sy) * dy)
                            / length_squared;
                        !(-1e-7..=1.0 + 1e-7).contains(&t)
                    }) {
                        return None;
                    }
                    elevations.push(
                        boundary
                            .iter()
                            .map(|point| {
                                start
                                    + (end - start)
                                        * ((point["x"].as_f64().unwrap() - sx) * dx
                                            + (point["y"].as_f64().unwrap() - sy) * dy)
                                        / length_squared
                            })
                            .collect::<Vec<_>>(),
                    );
                }
                let base = element["base_z"].as_f64()?;
                let height = element["height"].as_f64()?;
                if elevations[0]
                    .iter()
                    .zip(&elevations[1])
                    .any(|(bottom, top)| {
                        !bottom.is_finite()
                            || !top.is_finite()
                            || *bottom < base - 0.001
                            || *bottom >= *top - 1e-7
                            || *top > base + height + 0.001
                    })
                {
                    return None;
                }
                objects.push(format!(
                    "CAD 侧板线索引 {} · 斜底 {} · 顶面 {} · 高度 assumed",
                    metadata["side_panel_line_indices"],
                    element["stair_soffit"],
                    element["stair_top"]
                ));
            }
            let has_reference = !connection["reference_sha256"].is_null()
                || !connection["reference_surfaces"].is_null()
                    && !connection["reference_surfaces"]
                        .as_array()
                        .is_some_and(Vec::is_empty);
            if schematic
                && (has_reference != reference
                    || !reference && !metadata["reference_sha256"].is_null())
            {
                return None;
            }
            if reference {
                let reference_sha256 = connection["reference_sha256"].as_str()?;
                let surfaces = connection["reference_surfaces"].as_array()?;
                let source_fact = &vertical_sources[source];
                let source_path = source_fact["instance_path"]
                    .as_array()?
                    .iter()
                    .map(Value::as_str)
                    .collect::<Option<Vec<_>>>()?
                    .join("/");
                let owner = format!("{component}.surface.001");
                let id = element["id"].as_str()?;
                let number: usize = id
                    .strip_prefix(&format!("{component}.surface."))?
                    .parse()
                    .ok()?;
                let surface = surfaces.get(number.checked_sub(1)?)?;
                let vertices = surface["vertices_m"].as_array()?;
                let limitations = bounded_text(connection, "limitations", 2000)?;
                if reference_sha256.len() != 64
                    || !reference_sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    || metadata["reference_sha256"] != reference_sha256
                    || metadata["reference_status"]
                        != "caller-supplied trial geometry; reference hash is an evidence citation"
                    || connection["geometry_basis"] != "region_bounds"
                    || metadata["geometry_basis"] != "assumed_region_bounds"
                    || connection["limitations"] != metadata["limitations"]
                    || ["flights", "landings"].iter().any(|key| {
                        !connection[key].is_null()
                            && !connection[key].as_array().is_some_and(Vec::is_empty)
                    })
                    || !connection["steps"].is_null()
                    || !connection["riser_m"].is_null()
                    || surfaces.is_empty()
                    || surfaces.len() > 2000
                    || source_fact["source_id"] != source
                    || source_fact["component_id"] != component
                    || source_fact["floor_id"] != p["scope"]["floor_id"]
                    || metadata["source_instance_path"] != source_path
                    || ["source_sha256", "source_geometry_sha256"]
                        .iter()
                        .any(|key| {
                            let source_key = if *key == "source_sha256" {
                                "source_sha256"
                            } else {
                                "geometry_sha256"
                            };
                            source_fact[source_key].as_str().is_none_or(|sha| {
                                sha.len() != 64
                                    || !sha
                                        .bytes()
                                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                            }) || metadata[key] != source_fact[source_key]
                        })
                    || metadata["vertical_spec_owner"] != owner
                    || op["op"] != "add"
                    || op["target_id"] != id
                    || op["floor_id"] != p["scope"]["floor_id"]
                    || id != format!("{component}.surface.{number:03}")
                    || surface["role"] != role
                    || !matches!(role, "stair_step" | "landing" | "partition")
                    || !(3..=100).contains(&vertices.len())
                    || vertices.iter().any(|point| {
                        point.as_array().is_none_or(|point| {
                            point.len() != 3
                                || point.iter().any(|v| v.as_f64().is_none())
                                || point[2].as_f64().is_none_or(|z| z <= 0.0)
                        })
                    })
                    || surface["thickness_m"].as_f64().is_none_or(|v| v <= 0.0)
                {
                    return None;
                }
                if first_connection {
                    for index in 1..=surfaces.len() {
                        remaining_reference_parts.insert(format!("{component}.surface.{index:03}"));
                    }
                    let highest = surfaces.iter().try_fold(0.0_f64, |highest, surface| {
                        surface["vertices_m"]
                            .as_array()?
                            .iter()
                            .try_fold(highest, |highest, point| {
                                Some(highest.max(point[2].as_f64()?))
                            })
                    })?;
                    if (highest - connection["rise_m"].as_f64()?).abs() > 0.001 {
                        return None;
                    }
                    // 引用哈希只标识证据；参考构型和斜底仍是调用方假设。
                    objects.push(format!(
                        "参考 SHA256 {reference_sha256} · 仅证据引用，非认证 · 构型 assumed"
                    ));
                    objects.push(format!("参考限制：{limitations}"));
                }
                if id == owner {
                    if serde_json::from_str::<Value>(metadata["vertical_spec"].as_str()?)
                        .ok()?
                        .ne(connection)
                    {
                        return None;
                    }
                } else if metadata["reason_ref"] != owner {
                    return None;
                }
                if !remaining_reference_parts.remove(id) {
                    return None;
                }
                for (holes, local) in [(&surface["holes_m"], true), (&element["holes"], false)] {
                    if holes.is_null() {
                        continue;
                    }
                    for ring in holes.as_array()? {
                        let ring = ring.as_array()?;
                        if !(3..=512).contains(&ring.len())
                            || ring.iter().any(|point| {
                                if local {
                                    point.as_array().is_none_or(|point| {
                                        point.len() != 2
                                            || point.iter().any(|v| v.as_f64().is_none())
                                    })
                                } else {
                                    point["x"].as_f64().is_none() || point["y"].as_f64().is_none()
                                }
                            })
                        {
                            return None;
                        }
                    }
                }
                let surface_sources = surface["source_ids"].as_array()?;
                let element_sources = element["source_ids"].as_array()?;
                let operation_sources = op["source_ids"].as_array()?;
                if surface_sources.is_empty()
                    || surface_sources.len() > 100
                    || surface_sources.iter().any(|source| {
                        source.as_str().is_none_or(str::is_empty)
                            || !element_sources.contains(source)
                            || !operation_sources.contains(source)
                    })
                    || !element_sources.contains(&Value::String(source.to_owned()))
                    || !operation_sources.contains(&Value::String(source.to_owned()))
                {
                    return None;
                }
                let plane_at = |plane: &Value, x: f64, y: f64| -> Option<f64> {
                    let sx = plane["start"]["x"].as_f64()?;
                    let sy = plane["start"]["y"].as_f64()?;
                    let dx = plane["end"]["x"].as_f64()? - sx;
                    let dy = plane["end"]["y"].as_f64()? - sy;
                    let length_squared = dx * dx + dy * dy;
                    if length_squared <= 1e-18 || !length_squared.is_finite() {
                        return None;
                    }
                    let start = plane["start_z"].as_f64()?;
                    let end = plane["end_z"].as_f64()?;
                    let z =
                        start + (end - start) * ((x - sx) * dx + (y - sy) * dy) / length_squared;
                    z.is_finite().then_some(z)
                };
                if !surface["soffit_m"].is_null() {
                    for point in vertices {
                        let z =
                            plane_at(&surface["soffit_m"], point[0].as_f64()?, point[1].as_f64()?)?;
                        if z < 0.0 || z >= point[2].as_f64()? - 1e-7 {
                            return None;
                        }
                    }
                }
                let base = element["base_z"].as_f64()?;
                let height = element["height"].as_f64()?;
                let highest = vertices.iter().try_fold(0.0_f64, |highest, point| {
                    Some(highest.max(point[2].as_f64()?))
                })?;
                if (height - highest).abs() > 0.001 {
                    return None;
                }
                for point in boundary {
                    let x = point["x"].as_f64()?;
                    let y = point["y"].as_f64()?;
                    let underside = plane_at(&element["stair_soffit"], x, y)?;
                    let top = if element["stair_top"].is_null() {
                        base + height
                    } else {
                        plane_at(&element["stair_top"], x, y)?
                    };
                    if underside < base - 0.001
                        || underside >= top - 1e-7
                        || top <= base
                        || top > base + height + 0.001
                    {
                        return None;
                    }
                }
                objects.push(format!(
                    "参考表面局部坐标(m) {} · Z 相对楼面 · 厚 {} m · assumed",
                    surface["vertices_m"], surface["thickness_m"]
                ));
                if element["holes"]
                    .as_array()
                    .is_some_and(|holes| !holes.is_empty())
                {
                    objects.push(format!("参考表面孔洞(m) {} · assumed", element["holes"]));
                }
                objects.push(format!(
                    "斜底(m)：{} → {} · 标高 {} → {} · assumed",
                    element["stair_soffit"]["start"],
                    element["stair_soffit"]["end"],
                    element["stair_soffit"]["start_z"],
                    element["stair_soffit"]["end_z"]
                ));
                if !element["stair_top"].is_null() {
                    objects.push(format!(
                        "顶面(m)：{} → {} · 标高 {} → {} · assumed",
                        element["stair_top"]["start"],
                        element["stair_top"]["end"],
                        element["stair_top"]["start_z"],
                        element["stair_top"]["end_z"]
                    ));
                }
            }
            if first_connection {
                if schematic {
                    let rise_label = if reference {
                        "最高表面相对楼面"
                    } else {
                        "层高"
                    };
                    objects.push(format!(
                        "单层示意{} · {rise_label} {} m · assumed · 连接待核验 · 楼层 {} · 来源位置 {}",
                        if shaft { "电梯外部体量" } else if escalator { "扶梯" } else { "楼梯" },
                        connection["rise_m"],
                        connection["served_floors"],
                        vertical_sources[source]["origin_m"]
                    ));
                    objects.push(format!(
                        "体量表示：{}",
                        bounded_text(metadata, "solid_representation", 1000)?
                    ));
                } else {
                    objects.push(format!(
                        "服务楼层 {} · 既有洞口 {} · 来源位置 {} · 台阶参数依据 {}",
                        connection["served_floors"],
                        connection["openings"],
                        vertical_sources[source]["origin_m"],
                        metadata["step_dimension_basis"]
                    ));
                }
                objects.push(format!(
                    "未检查：{}",
                    element["metadata"]["not_checked"].as_str()?
                ));
            }
            continue;
        }
        if op["collection"] != "solids" {
            continue;
        }
        let solid: Value = serde_json::from_str(op["value_json"].as_str()?).ok()?;
        objects.push(format!(
            "{} · 类别 {} · 位置 {} · 来源 {} · {}",
            solid["id"].as_str()?,
            solid["metadata"]["category"].as_str().unwrap_or("未分类"),
            solid["origin"],
            solid["source_ids"],
            op["basis"].as_str().unwrap_or("unknown")
        ));
    }
    if !remaining_reference_parts.is_empty() {
        return None;
    }
    let assumptions = p["assumed"]
        .as_array()?
        .iter()
        .map(|i| {
            if i.as_object().is_some_and(|object| object.len() == 1) {
                i["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| i.to_string())
            } else {
                i.to_string()
            }
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let diagnostics: Vec<String> = value["diagnostics"]
        .as_array()?
        .iter()
        .take(100)
        .map(|i| {
            format!(
                "{} · {} · {}",
                i["code"].as_str().unwrap_or("unknown"),
                i["entity_id"],
                i["message"].as_str().unwrap_or("")
            )
        })
        .collect();
    let state = text(p, "state").ok()?;
    let ready = state == "prepared"
        && value["current"] == true
        && diagnostics.is_empty()
        && p["issues"].as_array()?.is_empty()
        && p["pending"].as_array()?.is_empty()
        && !binding.candidate_sha256.is_empty();
    Some(ProposalFact {
        binding,
        state,
        ready,
        parameter_only,
        summary: if correction.is_some() {
            bounded_text(p, "summary", 4000)?
        } else {
            text(p, "summary").ok()?
        },
        objects,
        assumptions,
        diagnostics,
    })
}

fn decode_envelope(raw: &str) -> Option<Value> {
    if raw.len() > LIMIT {
        return None;
    }
    unwrap_envelope(serde_json::from_str(raw).ok()?, 0)
}

fn unwrap_envelope(value: Value, depth: usize) -> Option<Value> {
    if depth > 4 {
        return None;
    }
    if value["contract"] == CONTRACT {
        return Some(value);
    }
    for key in ["structuredContent", "result"] {
        if let Some(inner) = value.get(key) {
            if let Some(result) = unwrap_envelope(inner.clone(), depth + 1) {
                return Some(result);
            }
        }
    }
    if let Some(content) = value["content"].as_array() {
        for item in content.iter().take(8) {
            if let Some(text) = item["text"].as_str().filter(|s| s.len() <= LIMIT) {
                if let Ok(inner) = serde_json::from_str(text) {
                    if let Some(result) = unwrap_envelope(inner, depth + 1) {
                        return Some(result);
                    }
                }
            }
        }
    }
    None
}

pub async fn authorize(
    setup: DomainSetup,
    binding: ProposalBinding,
    review_set_sha256: Option<String>,
    images: Vec<ReviewImageBinding>,
) -> Result<String, String> {
    authorize_with_policy(
        setup,
        binding,
        review_set_sha256,
        images,
        crate::config::DomainApprovalPolicy::Manual,
    )
    .await
}

pub async fn authorize_with_policy(
    setup: DomainSetup,
    binding: ProposalBinding,
    review_set_sha256: Option<String>,
    images: Vec<ReviewImageBinding>,
    policy: crate::config::DomainApprovalPolicy,
) -> Result<String, String> {
    let automatic = policy == crate::config::DomainApprovalPolicy::ReviewedTemplate;
    for image in &images {
        if image.proposal != binding
            || (image.review_set_sha256.is_some() && image.review_set_sha256 != review_set_sha256)
        {
            return Err("首用图片与当前批准版本不一致".into());
        }
        review::verify_image(image).await?;
        if let Some(package) = &image.web_review {
            crate::review_web::verify(&setup, package.clone()).await?;
        }
    }
    let raw = tokio::fs::read(&setup.context_path)
        .await
        .map_err(|e| e.to_string())?;
    if format!("{:x}", Sha256::digest(&raw)) != setup.context_sha256 {
        return Err("建模上下文已变化".into());
    }
    let mut command = tokio::process::Command::new(&setup.python);
    command
        .current_dir(&setup.repository)
        .env("PYTHONUTF8", "1")
        .kill_on_drop(true)
        .args(match setup.mode {
            DomainMode::Floor => vec![
                "-X",
                "utf8",
                "-m",
                "blender_agent.cli.main",
                "domain-authorize",
                "--context",
            ],
            DomainMode::Project => vec![
                "-X",
                "utf8",
                "-m",
                "blender_agent.integrations.project_mcp",
                "--authorize",
                "--context",
            ],
        })
        .arg(&setup.context_path)
        .args([
            "--proposal",
            &binding.proposal_id,
            "--revision",
            &binding.revision.to_string(),
            "--prepare-token",
            &binding.prepare_token,
            "--actor",
            if automatic {
                "native-agent-tui:policy:reviewed-template"
            } else {
                "native-agent-tui:user"
            },
        ]);
    if let Some(review_set) = &review_set_sha256 {
        command.args(["--expected-review-set-sha256", review_set]);
    }
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let output = tokio::time::timeout(std::time::Duration::from_secs(600), command.output())
        .await
        .map_err(|_| "批准进程中断或超时；需检查记录，不能自动重试".to_owned())?
        .map_err(|e| e.to_string())?;
    if output.stdout.len() > LIMIT {
        return Err("批准结果超过大小上限".into());
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    if !output.status.success()
        || value["ok"] != true
        || value["kind"] != "authorization"
        || value["run_id"] != binding.run_id
        || value["context_sha256"] != binding.context_sha256
        || value["grant"]["proposal_id"] != binding.proposal_id
        || value["grant"]["revision"] != binding.revision
        || value["grant"]["prepare_token"] != binding.prepare_token
        || value["grant"]["review_set_sha256"].as_str() != review_set_sha256.as_deref()
        || (automatic && value["grant"]["approval_policy"] != "reviewed-template")
    {
        return Err("批准未通过版本核对；请检查当前方案".into());
    }
    let authorization_id = text(&value["grant"], "authorization_id")?;
    let approval = if automatic {
        "已按用户配置的已审模板策略自动批准"
    } else {
        "用户已批准"
    };
    let display = format!(
        "{approval}当前方案的第 {} 版；请应用并完成构建与验证。",
        binding.revision
    );
    let mut payload = json!({"user_action":if automatic { "policy_approve_concrete_change" } else { "approve_concrete_change" }, "proposal":binding,
              "approval_policy": if automatic { "reviewed-template" } else { "manual" },
              "authorization_id":authorization_id,
              "review_set_sha256":review_set_sha256,
              "apply_change_arguments": {
                  "proposal_id": binding.proposal_id, "revision": binding.revision,
                  "prepare_token": binding.prepare_token,
                  "expected_sha256": binding.base_sha256,
                  "authorization_id": authorization_id
              },
              "instruction":"用户已批准此具体版本。使用 apply_change_arguments 并补充稳定 operation_id 调用领域 apply_change；expected_sha256 必须是 base_sha256。再独立正式构建、验证和复核产物，保留假设标签。有来源输入时，先调用 scan_source_instances 取得当前完整汇总，再读取本批相关分组及完整必要分页，逐项核对本批来源、稳定身份与实际已建结果；展示全层已建、跳过、未决数量及仍未读取的范围。汇总不代表逐项全读，本批对账不代表整层对账完成；最终宣称整层完成前必须覆盖全部绑定来源并完成逐项对账。完成本版后，沿对话中原始用户目标和后续范围约束继续推进。原目标为完整楼层时，主动识别剩余来源、准备下一批可复核方案并集中提出必要业务问题；个别对象缺能力或待澄清时保留记录，继续其他可推进对象，不要求用户反复回复继续。原目标仅局部修改或预览时保持该边界；目标范围不明先核对对话。后续每个具体版本仍须独立批准，本次授权仅适用于以上绑定版本。"});
    if automatic {
        payload["instruction"] = json!(payload["instruction"]
            .as_str()
            .unwrap_or_default()
            .replace(
                "用户已批准此具体版本。",
                "此具体版本已由用户配置的已审模板策略自动批准；不代表新的人工确认。"
            )
            .replace(
                "后续每个具体版本仍须独立批准",
                "后续每个具体版本仍须由平台按当前策略独立批准"
            ));
    }
    Ok(format!("{display}\n{payload}"))
}

#[cfg(test)]
pub(crate) mod tests;
