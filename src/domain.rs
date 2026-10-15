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
mod project;
pub(crate) mod receipt;
pub use project::{
    BuildingReportFact, ComponentDiscovery, DrawingAuditFact, ProjectAssemblyBinding,
    ProjectBuildingFact, ProjectCatalogFact, ProjectConnectionAuthority, ProjectLocationFact,
    ProjectScopeFact, ProjectSelectionFact, ProjectWallLayerChoice, SelectionState,
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
    let python = repository.join(if cfg!(windows) {
        ".venv/Scripts/python.exe"
    } else {
        ".venv/bin/python"
    });
    if !python.is_file() {
        return Err("模型器 .venv Python 不存在".into());
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalFact {
    pub binding: ProposalBinding,
    pub state: String,
    pub summary: String,
    pub objects: Vec<String>,
    pub assumptions: Vec<String>,
    pub diagnostics: Vec<String>,
    pub ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLedgerFact {
    pub scan_id: String,
    pub scene_sha256: String,
    pub rows: Vec<String>,
    pub counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceGroupsFact {
    pub scan_id: String,
    pub scene_sha256: String,
    pub total_objects: usize,
    pub total_groups: usize,
    pub assessed_groups: usize,
    pub rows: Vec<String>,
    pub counts: BTreeMap<String, usize>,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFact {
    pub kind: String,
    pub sequence: u64,
    pub sha256: String,
    pub bytes: usize,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct DomainSnapshot {
    pub enabled: bool,
    pub floor_id: String,
    pub proposal: Option<ProposalFact>,
    pub project: Option<ProjectCatalogFact>,
    pub selection: Option<ProjectSelectionFact>,
    pub project_location: Option<ProjectLocationFact>,
    pub project_scope: Option<ProjectScopeFact>,
    pub buildings: Vec<ProjectBuildingFact>,
    pub drawing_audit: Option<DrawingAuditFact>,
    pub ledger: Option<SourceLedgerFact>,
    pub groups: Option<SourceGroupsFact>,
    pub application: Option<String>,
    pub operation: Option<OperationFact>,
    pub artifacts: Vec<ArtifactFact>,
    pub authorizing: bool,
    pub notice: Option<String>,
    pub fact_sync_notice: Option<String>,
    pub verified_fact: Option<VerifiedFact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentProgress {
    pub total: usize,
    pub built: usize,
    pub pending: usize,
    pub skipped: usize,
    pub rejected: usize,
}

impl DomainSnapshot {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FactProjection {
    Applied,
    Superseded,
    Unrelated,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceViewIdentity {
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
        if value["source_view_contract_version"] != "source-view/1" {
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
    source_view: Option<SourceViewIdentity>,
    pub(crate) synchronizing: bool,
    synchronization_errors: BTreeMap<String, u64>,
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
        Self {
            setup,
            snapshot,
            control_messages: BTreeMap::new(),
            fact_sequences: BTreeMap::new(),
            source_view: None,
            synchronizing: false,
            synchronization_errors: BTreeMap::new(),
        }
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
        let Some(setup) = self.setup.as_ref() else {
            return FactProjection::Invalid;
        };
        if value["contract"] != CONTRACT {
            return FactProjection::Invalid;
        }
        if value["run_id"] != setup.run_id || value["context_sha256"] != setup.context_sha256 {
            return FactProjection::Invalid;
        }
        let Some(kind) = value["kind"].as_str() else {
            return FactProjection::Invalid;
        };
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
                return FactProjection::Invalid;
            };
            if self.snapshot.selection.as_ref().is_none_or(|selection| {
                selection.state != SelectionState::Selected
                    || selection.selection_id != scope.selection_id
                    || selection.source_sha256 != scope.source_sha256
                    || !selection.selected_keys.contains(&scope.floor_key)
            }) || (kind != "project_scope"
                && self.snapshot.project_scope.as_ref() != Some(&scope))
            {
                return FactProjection::Invalid;
            }
            if kind == "project_scope" {
                if let Some(choice) = &scope.wall_layer_choice {
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
                        return FactProjection::Invalid;
                    }
                }
                if let Some(binding) = &scope.assembly_binding {
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
                        return FactProjection::Invalid;
                    }
                }
                if let Some(current) = self.snapshot.project_scope.as_ref() {
                    if let Some(revision) = current.activation_revision {
                        if scope.activation_revision.is_none_or(|incoming| {
                            incoming < revision || (incoming == revision && scope != *current)
                        }) {
                            return FactProjection::Invalid;
                        }
                    }
                }
            }
            Some(scope)
        } else {
            if setup.mode == DomainMode::Floor
                && matches!(
                    kind,
                    "project_catalog"
                        | "project_selection"
                        | "project_location"
                        | "project_scope"
                        | "project_building"
                )
            {
                return FactProjection::Invalid;
            }
            None
        };
        let source_kind = matches!(kind, "source_ledger" | "source_groups");
        let incoming_source_view = if source_kind {
            match SourceViewIdentity::read(value) {
                Ok(view) => view,
                Err(()) => return FactProjection::Invalid,
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
                _ => false,
            };
        let artifact_kind = matches!(kind, "artifact" | "artifact_validation");
        let fact_key = if kind == "project_building" {
            let Some(id) = value["project_building"]["build_id"].as_str() else {
                return FactProjection::Invalid;
            };
            format!("building:{id}")
        } else if artifact_kind {
            let Some(id) = value["artifact"]["build_id"].as_str() else {
                return FactProjection::Invalid;
            };
            format!("artifact:{id}")
        } else if matches!(kind, "source_ledger" | "source_groups") {
            // Both shapes describe the same mutable source state. Delayed pages
            // from either shape must not replace a newer verified source view.
            "sources".to_owned()
        } else {
            kind.to_owned()
        };
        let sequence = value["fact_sequence"].as_u64();
        if self
            .fact_sequences
            .get(&fact_key)
            .is_some_and(|last| sequence.is_none_or(|seq| seq < *last))
            && !recover_source_shape
        {
            return FactProjection::Superseded;
        }
        let mut accepted = false;
        match Some(kind) {
            Some("project_building") => {
                let Some(mut fact) = self
                    .snapshot
                    .selection
                    .as_ref()
                    .and_then(|selection| project::building_fact(value, selection))
                else {
                    return FactProjection::Invalid;
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
                        return FactProjection::Invalid;
                    }
                    if fact.valid.is_none() && old.state == fact.state {
                        fact.valid = old.valid;
                    }
                    if fact.report.is_none()
                        && fact.connection_check == "not_run"
                        && old.state == fact.state
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
                    return FactProjection::Invalid;
                };
                self.snapshot.drawing_audit = Some(fact);
                accepted = true;
            }
            Some("project_scope") => {
                let Some(scope) = incoming_scope else {
                    return FactProjection::Invalid;
                };
                let Some(proposal) = proposal_fact(value) else {
                    return FactProjection::Invalid;
                };
                if self.snapshot.project_scope.as_ref() != Some(&scope) {
                    self.clear_project_scope();
                }
                self.snapshot.floor_id = scope.floor_id.clone();
                self.snapshot.project_scope = Some(scope);
                self.snapshot.proposal = Some(proposal);
                accepted = true;
            }
            Some("project_location") => {
                let Some(location) = project::location_fact(value) else {
                    return FactProjection::Invalid;
                };
                if self.snapshot.selection.as_ref().is_none_or(|selection| {
                    selection.state != SelectionState::Selected
                        || selection.catalog_id != location.catalog_id
                        || selection.selection_id != location.selection_id
                        || selection.source_sha256 != location.source_sha256
                        || !selection.selected_keys.contains(&location.floor_key)
                }) {
                    return FactProjection::Invalid;
                }
                self.snapshot.project_location = Some(location);
                accepted = true;
            }
            Some("project_catalog") => {
                let Some(project) = project::catalog_fact(value) else {
                    return FactProjection::Invalid;
                };
                // A read-only catalog includes the original persisted target. Restore
                // this selection before accepting floor facts; it grants no write access.
                let recovered_selection = if value["recovery_index"]["catalog_current"] == true {
                    let Some(selection) = project::selection_fact(&json!({
                        "ok":true,"selection":value["recovery_index"]["selection"]
                    })) else {
                        return FactProjection::Invalid;
                    };
                    if selection.catalog_id != project.catalog_id
                        || selection.source_sha256 != project.source_sha256
                        || selection
                            .candidate_keys
                            .iter()
                            .any(|key| !project.floor_keys.contains(key))
                    {
                        return FactProjection::Invalid;
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
                let Some(selection) = project::selection_fact(value) else {
                    return FactProjection::Invalid;
                };
                if self.snapshot.project.as_ref().is_none_or(|project| {
                    project.catalog_id != selection.catalog_id
                        || project.source_sha256 != selection.source_sha256
                        || selection
                            .candidate_keys
                            .iter()
                            .any(|key| !project.floor_keys.contains(key))
                }) {
                    return FactProjection::Invalid;
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
                    let old = self.snapshot.proposal.as_ref();
                    let same = old.is_some_and(|p| p.binding == proposal.binding);
                    let parent = value["proposal"]["parent_id"].as_str();
                    let follows = old.is_none_or(|p| {
                        parent == Some(p.binding.proposal_id.as_str()) || value["current"] == true
                    });
                    if !same && !follows {
                        // Valid historical-family reads are not current state
                        // and do not imply transport or projection corruption.
                        return FactProjection::Superseded;
                    }
                    if same || follows {
                        if !same {
                            self.snapshot.application = None;
                            self.snapshot.operation = None;
                            self.snapshot.artifacts.clear();
                            self.fact_sequences
                                .retain(|key, _| !key.starts_with("artifact:"));
                            for kind in ["application", "operation"] {
                                self.fact_sequences.remove(kind);
                            }
                        }
                        self.snapshot.proposal = Some(proposal);
                        accepted = true;
                    }
                }
            }
            Some("source_groups") => {
                if let Some(groups) = source_groups_fact(value) {
                    self.snapshot.groups = Some(groups);
                    if !same_source_view {
                        self.snapshot.ledger = None;
                    }
                    accepted = true;
                }
            }
            Some("source_ledger") => {
                if let Some(ledger) = source_ledger_fact(value) {
                    self.snapshot.ledger = Some(ledger);
                    if !same_source_view {
                        self.snapshot.groups = None;
                    }
                    accepted = true;
                }
            }
            Some("operation") if matches_proposal(&self.snapshot, &value["receipt"]) => {
                if let Some(operation) = self
                    .snapshot
                    .proposal
                    .as_ref()
                    .and_then(|p| operation_fact(value, &p.binding))
                {
                    self.snapshot.application = Some(format!(
                        "{} · current={}",
                        operation.state,
                        value["receipt"]["candidate_sha256"] == operation.current_sha256
                    ));
                    self.snapshot.operation = Some(operation);
                    self.invalidate_artifact_currency();
                    if let Some(proposal) = &mut self.snapshot.proposal {
                        proposal.ready = false;
                    }
                    if let Some(sequence) = sequence {
                        for kind in ["proposal", "application"] {
                            let watermark = self.fact_sequences.entry(kind.into()).or_default();
                            *watermark = (*watermark).max(sequence);
                        }
                    }
                    accepted = true;
                }
            }
            Some("application") if matches_proposal(&self.snapshot, &value["receipt"]) => {
                if let Some(operation) = self
                    .snapshot
                    .proposal
                    .as_ref()
                    .and_then(|p| operation_fact(value, &p.binding))
                {
                    self.snapshot.operation = Some(operation);
                }
                self.snapshot.application = value["receipt"]["state"]
                    .as_str()
                    .map(|state| format!("{state} · current={}", value["current"]));
                self.invalidate_artifact_currency();
                if let Some(proposal) = &mut self.snapshot.proposal {
                    proposal.ready = false;
                }
                if let Some(sequence) = sequence {
                    for kind in ["proposal", "operation"] {
                        let watermark = self.fact_sequences.entry(kind.into()).or_default();
                        *watermark = (*watermark).max(sequence);
                    }
                }
                accepted = true;
            }
            Some("artifact" | "artifact_validation") => {
                if let Some(mut artifact) = self
                    .snapshot
                    .proposal
                    .as_ref()
                    .and_then(|p| artifact_fact(value, &p.binding))
                {
                    if sequence.is_none_or(|seq| {
                        self.fact_sequences
                            .get("application")
                            .is_some_and(|last| seq < *last)
                    }) {
                        artifact.current = self.snapshot.operation.as_ref().and_then(|operation| {
                            if operation.current_sha256 != artifact.binding.authority_sha256 {
                                Some(false)
                            } else if !artifact.binding.candidate {
                                // A later read of the same applied authority proves that
                                // the sealed older build still belongs to this scene.
                                Some(true)
                            } else {
                                None
                            }
                        });
                    }
                    let existing = self
                        .snapshot
                        .artifacts
                        .iter_mut()
                        .find(|a| a.binding.build_id == artifact.binding.build_id);
                    if let Some(old) = existing {
                        if old.binding == artifact.binding {
                            if kind == "artifact" && old.state == artifact.state {
                                artifact.valid = old.valid;
                            }
                            *old = artifact;
                            accepted = true;
                        }
                    } else {
                        // Receipts remain durable in Python; bound the in-memory display history.
                        if self.snapshot.artifacts.len() >= 32 {
                            let evicted = self.snapshot.artifacts.remove(0);
                            self.fact_sequences
                                .remove(&format!("artifact:{}", evicted.binding.build_id));
                        }
                        self.snapshot.artifacts.push(artifact);
                        accepted = true;
                    }
                }
            }
            Some("diagnostic") => {
                self.snapshot.notice = value["diagnostics"][0]["message"]
                    .as_str()
                    .map(str::to_owned);
                accepted = true;
            }
            _ => {}
        }
        if accepted {
            if source_kind {
                self.source_view = incoming_source_view;
            }
            if let Some(sequence) = sequence {
                self.fact_sequences
                    .entry(fact_key)
                    .and_modify(|last| *last = (*last).max(sequence))
                    .or_insert(sequence);
            }
            FactProjection::Applied
        } else if matches!(kind, "proposal" | "source_ledger" | "source_groups") {
            FactProjection::Invalid
        } else {
            FactProjection::Unrelated
        }
    }

    fn clear_project_scope(&mut self) {
        self.source_view = None;
        self.snapshot.project_scope = None;
        self.snapshot.drawing_audit = None;
        self.snapshot.proposal = None;
        self.snapshot.ledger = None;
        self.snapshot.groups = None;
        self.snapshot.operation = None;
        self.snapshot.application = None;
        self.snapshot.artifacts.clear();
        self.snapshot.floor_id = "项目目标".into();
    }

    pub(crate) fn synchronization_failed(&mut self, kind: &str, sequence: u64, reason: String) {
        self.synchronization_errors
            .entry(kind.into())
            .and_modify(|last| *last = (*last).max(sequence))
            .or_insert(sequence);
        self.snapshot.notice = Some(format!("领域事实同步失败：{reason}；请重新检查当前方案"));
        self.snapshot
            .fact_sync_notice
            .clone_from(&self.snapshot.notice);
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
        if self
            .snapshot
            .verified_fact
            .as_ref()
            .is_none_or(|fact| fact.sequence <= receipt.fact_sequence)
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
            self.snapshot.fact_sync_notice = None;
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
            self.snapshot.fact_sync_notice = None;
        } else {
            self.show_remaining_synchronization_errors();
        }
    }

    fn invalidate_artifact_currency(&mut self) {
        if let Some(operation) = &self.snapshot.operation {
            if self
                .snapshot
                .drawing_audit
                .as_ref()
                .is_some_and(|audit| audit.scene_sha256 != operation.current_sha256)
            {
                self.snapshot.drawing_audit = None;
            }
            for building in &mut self.snapshot.buildings {
                let connection_scope = self
                    .snapshot
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
                        .get(&self.snapshot.floor_id)
                        .is_some_and(|sha| sha != &operation.current_sha256)
                };
                if changed {
                    building.current = false;
                }
            }
        }
        for artifact in &mut self.snapshot.artifacts {
            if self.snapshot.operation.as_ref().is_some_and(|operation| {
                operation.current_sha256 != artifact.binding.authority_sha256
            }) {
                artifact.current = Some(false);
            } else if artifact.binding.candidate {
                // Applying without an inspected authority cannot prove preview currency.
                artifact.current = None;
            }
        }
    }

    pub fn remember_control(&mut self, text: &str) {
        while self.control_messages.len() >= 8 && !self.control_messages.contains_key(text) {
            let key = self.control_messages.keys().next().cloned().unwrap();
            self.control_messages.remove(&key);
        }
        self.control_messages.insert(
            text.into(),
            text.lines().next().unwrap_or("具体版本已批准").into(),
        );
    }

    pub fn project_control_messages(&self, messages: &mut [crate::state::ConversationItem]) {
        for message in messages {
            if let Some(display) = self.control_messages.get(&message.text) {
                message.text.clone_from(display);
            }
        }
    }

    pub fn can_authorize(&self, binding: &ProposalBinding) -> bool {
        self.setup.as_ref().is_some_and(|setup| {
            setup.run_id == binding.run_id
                && setup.context_sha256 == binding.context_sha256
                && (setup.mode == DomainMode::Floor
                    || self.snapshot.project_scope.as_ref().is_some_and(|scope| {
                        self.snapshot.selection.as_ref().is_some_and(|selection| {
                            selection.selection_id == scope.selection_id
                                && selection.state == SelectionState::Selected
                        }) && scope.assembly_binding.as_ref().is_none_or(|binding| {
                            self.snapshot.buildings.iter().any(|building| {
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
            && !self.snapshot.authorizing
            && self.snapshot.operation.is_none()
            && self
                .snapshot
                .proposal
                .as_ref()
                .is_some_and(|p| p.ready && p.binding == *binding)
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
    Some(ArtifactFact {
        binding,
        state,
        current: a["current"].as_bool(),
        valid,
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

fn matches_proposal(snapshot: &DomainSnapshot, value: &Value) -> bool {
    snapshot.proposal.as_ref().is_some_and(|p| {
        value["proposal_id"] == p.binding.proposal_id
            && value["prepare_token"] == p.binding.prepare_token
    })
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
        if group_counts
            .values()
            .try_fold(0usize, |n, c| n.checked_add(*c))?
            != count
        {
            return None;
        }
        if ids.len() <= 100 {
            rows.push(format!(
                "{} · {} 个 · 类别 {} · 状态 {} · {}",
                text(group, "block").ok()?,
                count,
                group["category_counts"],
                group["state_counts"],
                id
            ));
            if let Some(reason) = assessment {
                rows.push(format!("评估依据／问题：{reason}"));
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
        rows,
        counts,
    })
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
    let mut seen_connections = std::collections::BTreeSet::new();
    // Verified facts already have an 8 MiB transport bound. Every approved operation
    // must be projected and validated, including parts after a large batch boundary.
    let correction = p.get("drawing_review").filter(|v| !v.is_null());
    let edit = p.get("component_edit").filter(|v| !v.is_null());
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
            let role = element["role"].as_str()?;
            let boundary = element["boundary"].as_array()?;
            if !matches!(role, "stair_step" | "shaft_wall")
                || !matches!(
                    element["metadata"]["relation_validator"].as_str(),
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
            let component = element["metadata"]["component_id"].as_str()?;
            if seen_connections.insert(component.to_owned()) {
                let connection = p["component_plan"]["connections"]
                    .as_array()?
                    .iter()
                    .find(|c| c["component_id"].as_str() == Some(component))?;
                let source = element["metadata"]["source_instance_id"].as_str()?;
                objects.push(format!(
                    "服务楼层 {} · 既有洞口 {} · 来源位置 {} · 台阶参数依据 {}",
                    connection["served_floors"],
                    connection["openings"],
                    p["vertical_sources"][source]["origin_m"],
                    element["metadata"]["step_dimension_basis"]
                ));
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
    let assumptions = p["assumed"]
        .as_array()?
        .iter()
        .take(100)
        .filter_map(|i| i["message"].as_str().map(str::to_owned))
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

pub async fn authorize(setup: DomainSetup, binding: ProposalBinding) -> Result<String, String> {
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
            "native-agent-tui:user",
        ]);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), command.output())
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
    {
        return Err("批准未通过版本核对；请检查当前方案".into());
    }
    let authorization_id = text(&value["grant"], "authorization_id")?;
    let display = format!(
        "已批准当前方案的第 {} 版；请应用并完成构建与验证。",
        binding.revision
    );
    let payload = json!({"user_action":"approve_concrete_change", "proposal":binding,
              "authorization_id":authorization_id,
              "apply_change_arguments": {
                  "proposal_id": binding.proposal_id, "revision": binding.revision,
                  "prepare_token": binding.prepare_token,
                  "expected_sha256": binding.base_sha256,
                  "authorization_id": authorization_id
              },
              "instruction":"用户已批准此具体版本。使用 apply_change_arguments 并补充稳定 operation_id 调用领域 apply_change；expected_sha256 必须是 base_sha256。再独立构建和验证产物，保留假设标签。有来源输入时，最后调用 scan_source_instances 并按 next_offset 读完全部页，展示已建、跳过和未决来源的完整对账；仅完成构建不能报告来源对账已完成。"});
    Ok(format!("{display}\n{payload}"))
}

#[cfg(test)]
mod tests;
