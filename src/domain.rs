//! Modeling business facts and human control, owned by platform Core.
//! No model client or workflow loop lives here. Only bound MCP facts are projected.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::config::{Config, McpServerConfig};
use crate::protocol::{ObservedTool, ObservedToolOutcome, ToolCategory};

pub(crate) mod receipt;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainSetup {
    pub context_path: PathBuf,
    pub repository: PathBuf,
    pub python: PathBuf,
    pub run_id: String,
    pub context_sha256: String,
    pub floor_id: String,
}

pub fn configure(mut config: Config) -> Result<Config, String> {
    let Some(path) = config.domain_context.as_ref() else {
        return Ok(config);
    };
    if config
        .permission
        .as_deref()
        .is_some_and(|p| p != "read-only")
    {
        return Err("领域建模会话的通用工具必须使用 read-only；受控写入由领域工具执行".into());
    }
    config.permission = Some("read-only".into());
    let repository = normalized_path(&config.cwd)?;
    let context_path = normalized_path(path)?;
    let raw = std::fs::read(&context_path).map_err(|e| e.to_string())?;
    if raw.len() > 16 * 1024 {
        return Err("建模上下文超过 16 KiB".into());
    }
    let value: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if value["contract"] != "modeling-context/v2" {
        return Err("不支持的建模上下文版本".into());
    }
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
    let scene = normalized_path(&repository.join(text(&value, "scene_path")?))?;
    if !scene.starts_with(&repository) {
        return Err("场景超出项目范围".into());
    }
    let floor_id = text(&value["scope"], "floor_id")?;
    let python = repository.join(if cfg!(windows) {
        ".venv/Scripts/python.exe"
    } else {
        ".venv/bin/python"
    });
    if !python.is_file() {
        return Err("模型器 .venv Python 不存在".into());
    }
    if config.mcp_servers.iter().any(|s| matches!(s, McpServerConfig::Stdio { name, .. } | McpServerConfig::Http { name, .. } if name == SERVER)) {
        return Err("建模领域 MCP 已被配置".into());
    }
    config.mcp_servers.push(McpServerConfig::Stdio {
        name: SERVER.into(),
        command: python.to_string_lossy().into_owned(),
        args: vec![
            "-m".into(),
            "blender_agent.integrations.domain_mcp".into(),
            "--context".into(),
            context_path.to_string_lossy().into_owned(),
        ],
        env: Vec::new(),
    });
    config.domain_setup = Some(DomainSetup {
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
    pub ledger: Option<SourceLedgerFact>,
    pub application: Option<String>,
    pub operation: Option<OperationFact>,
    pub artifacts: Vec<ArtifactFact>,
    pub authorizing: bool,
    pub notice: Option<String>,
    pub fact_sync_notice: Option<String>,
    pub verified_fact: Option<VerifiedFact>,
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

pub struct DomainFacts {
    setup: Option<DomainSetup>,
    pub snapshot: DomainSnapshot,
    control_messages: BTreeMap<String, String>,
    fact_sequences: BTreeMap<String, u64>,
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
        let artifact_kind = matches!(kind, "artifact" | "artifact_validation");
        let fact_key = if artifact_kind {
            let Some(id) = value["artifact"]["build_id"].as_str() else {
                return FactProjection::Invalid;
            };
            format!("artifact:{id}")
        } else {
            kind.to_owned()
        };
        let sequence = value["fact_sequence"].as_u64();
        if self
            .fact_sequences
            .get(&fact_key)
            .is_some_and(|last| sequence.is_none_or(|seq| seq < *last))
        {
            return FactProjection::Superseded;
        }
        let mut accepted = false;
        match Some(kind) {
            Some("proposal") => {
                if let Some(proposal) = proposal_fact(value) {
                    let old = self.snapshot.proposal.as_ref();
                    let same = old.is_some_and(|p| p.binding == proposal.binding);
                    let parent = value["proposal"]["parent_id"].as_str();
                    let follows = old.is_none_or(|p| {
                        parent == Some(p.binding.proposal_id.as_str()) || value["current"] == true
                    });
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
            Some("source_ledger") => {
                if let Some(ledger) = source_ledger_fact(value) {
                    self.snapshot.ledger = Some(ledger);
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
                        artifact.current = self
                            .snapshot
                            .operation
                            .as_ref()
                            .filter(|operation| {
                                operation.current_sha256 != artifact.binding.authority_sha256
                            })
                            .map(|_| false);
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
            if let Some(sequence) = sequence {
                self.fact_sequences.insert(fact_key, sequence);
            }
            FactProjection::Applied
        } else if matches!(kind, "proposal" | "source_ledger") {
            FactProjection::Invalid
        } else {
            FactProjection::Unrelated
        }
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
        }
    }

    fn invalidate_artifact_currency(&mut self) {
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
        !self.synchronizing
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
            state,
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
    for op in p["draft"]["operations"].as_array()?.iter().take(512) {
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
        summary: text(p, "summary").ok()?,
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
        .args([
            "-m",
            "blender_agent.cli.main",
            "domain-authorize",
            "--context",
        ])
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
              "instruction":"用户已批准此具体版本。调用领域 apply_change，再独立构建和验证产物。保留假设标签。"});
    Ok(format!("{display}\n{payload}"))
}

#[cfg(test)]
mod tests;
