//! The model carries only references. Core verifies sealed facts on a private channel.

use super::*;
use tokio::io::AsyncReadExt;

pub const FACT_LIMIT: usize = 8 * 1024 * 1024;
const FACT_GRAPH_BUDGET: usize = 64 * 1024 * 1024;
const RECEIPT_LIMIT: usize = 64 * 1024;
const RECEIPT_CONTRACT: &str = "modeling-fact-receipt/v1";
const BINDING_KEYS: &[&str] = &[
    "proposal_id",
    "revision",
    "prepare_token",
    "base_sha256",
    "candidate_sha256",
    "operation_id",
    "build_id",
    "authority_sha256",
    "authorization_id",
];

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactReceipt {
    pub contract: String,
    pub run_id: String,
    pub context_sha256: String,
    pub kind: String,
    pub ok: bool,
    pub fact_sha256: String,
    pub fact_bytes: usize,
    pub fact_sequence: u64,
    pub binding: Value,
}

impl std::fmt::Debug for FactReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactReceipt")
            .field("kind", &self.kind)
            .field("fact_sequence", &self.fact_sequence)
            .finish_non_exhaustive()
    }
}

fn unwrap_receipt(value: &Value, depth: usize) -> Option<Value> {
    if depth > 4 {
        return None;
    }
    if value
        .get("isError")
        .is_some_and(|error| !error.is_null() && error.as_bool() != Some(false))
    {
        return None;
    }
    if value["contract"] == RECEIPT_CONTRACT {
        return Some(value.clone());
    }
    if let Some(raw) = value.as_str().filter(|raw| raw.len() <= RECEIPT_LIMIT) {
        return unwrap_receipt(&serde_json::from_str::<Value>(raw).ok()?, depth + 1);
    }
    for key in ["structuredContent", "result"] {
        if let Some(result) = value.get(key).and_then(|v| unwrap_receipt(v, depth + 1)) {
            return Some(result);
        }
    }
    for block in value["content"].as_array().into_iter().flatten().take(8) {
        if let Some(result) = unwrap_receipt(&block["text"], depth + 1) {
            return Some(result);
        }
    }
    None
}

pub fn from_tool(
    setup: &DomainSetup,
    notice: &ObservedTool,
    item: &Value,
) -> Result<Option<FactReceipt>, String> {
    if notice.category != ToolCategory::Mcp
        || notice.outcome.is_none()
        || item["type"] != "mcpToolCall"
        || item["server"] != SERVER
    {
        return Ok(None);
    }
    // A failed or aborted call produces no fact; the tool row already shows the
    // failure, and approval stays bound to the server-checked displayed version.
    if notice.outcome != Some(ObservedToolOutcome::Completed) || item["result"]["isError"] == true {
        return Ok(None);
    }
    if super::unwrap_envelope(item["result"].clone(), 0).is_some() {
        return Ok(None);
    }
    if serde_json::to_vec(&item["result"]).map_or(true, |v| v.len() > RECEIPT_LIMIT) {
        return Err("领域事实回执超过大小上限；完整事实尚未同步".into());
    }
    let value =
        unwrap_receipt(&item["result"], 0).ok_or("领域事实回执缺失或损坏；完整事实尚未同步")?;
    let receipt: FactReceipt =
        serde_json::from_value(value).map_err(|_| "领域事实回执字段无效；完整事实尚未同步")?;
    validate(setup, &receipt)?;
    Ok(Some(receipt))
}

pub fn validate(setup: &DomainSetup, receipt: &FactReceipt) -> Result<(), String> {
    if receipt.contract != RECEIPT_CONTRACT
        || receipt.run_id != setup.run_id
        || receipt.context_sha256 != setup.context_sha256
        || receipt.kind.is_empty()
        || receipt.kind.len() > 64
        || !matches!(
            receipt.kind.as_str(),
            "capabilities"
                | "project_catalog"
                | "project_selection"
                | "project_location"
                | "project_scope"
                | "project_building"
                | "project_floor_discarded"
                | "drawing_audit"
                | "source_ledger"
                | "source_groups"
                | "source_linework"
                | "source_linework_selection"
                | "source_interpretation"
                | "source_disposition"
                | "proposal"
                | "operation"
                | "application"
                | "artifact"
                | "artifact_validation"
                | "component_first_use_review"
                | "authorization"
                | "diagnostic"
                | "diagnostic_log"
                | "project_floor_preparing"
                | "tool_operation"
                | "wall_mount_inspection"
                | "source_region_inventory"
                | "job_index"
                | "delivery_status"
                | "reference_evaluation"
                // Server-driven batches; projected as unrelated facts.
                | "auto_batch"
                | "precedent_auto_batch"
                | "precedent_job"
                | "linework_presort"
                | "group_contact_sheet"
                | "group_contact_sheet_image"
                | "group_interpretation_batch"
                | "schematic_templates"
                | "schematic_circulation"
                | "opening_type_representatives"
                | "unknown_box_presort"
        )
        || receipt.fact_sequence == 0
        || receipt.fact_bytes == 0
        || receipt.fact_bytes > FACT_LIMIT
        || receipt.fact_sha256.len() != 64
        || !receipt
            .fact_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !receipt.binding.is_object()
    {
        return Err("领域事实回执身份或大小无效；完整事实尚未同步".into());
    }
    Ok(())
}

pub fn verify(setup: &DomainSetup, receipt: &FactReceipt, raw: &[u8]) -> Result<Value, String> {
    validate(setup, receipt)?;
    if raw.len() > FACT_LIMIT
        || raw.len() != receipt.fact_bytes
        || format!("{:x}", Sha256::digest(raw)) != receipt.fact_sha256
    {
        return Err("完整领域事实的大小或 SHA256 与回执不一致".into());
    }
    let mut value: Value = serde_json::from_slice(raw).map_err(|_| "完整领域事实不是有效 JSON")?;
    let owner = [
        "proposal",
        "receipt",
        "artifact",
        "grant",
        "component_first_use_review",
    ]
    .into_iter()
    .find_map(|key| value[key].as_object());
    let binding: serde_json::Map<String, Value> = BINDING_KEYS
        .iter()
        .filter_map(|key| {
            owner
                .and_then(|map| map.get(*key))
                .map(|v| ((*key).into(), v.clone()))
        })
        .collect();
    if value["contract"] != CONTRACT
        || receipt.contract != RECEIPT_CONTRACT
        || receipt.run_id != setup.run_id
        || receipt.context_sha256 != setup.context_sha256
        || value["run_id"] != receipt.run_id
        || value["context_sha256"] != receipt.context_sha256
        || value["kind"] != receipt.kind
        || value["ok"] != receipt.ok
        || value["fact_sequence"] != receipt.fact_sequence
        || Value::Object(binding) != receipt.binding
    {
        return Err("完整领域事实的上下文、序号或提案绑定与回执不一致".into());
    }
    // 回执仍绑定根 manifest 原字节；展开仅生成 Core 内部的已验证事实图。
    expand_fact_pages(setup, &mut value, raw.len())?;
    verify_revision_certificate(setup, &value)?;
    Ok(value)
}

fn expand_fact_pages(setup: &DomainSetup, value: &mut Value, bytes: usize) -> Result<(), String> {
    struct Pages<'a> {
        setup: &'a DomainSetup,
        sequence: Value,
        ok: Value,
        allowed: bool,
        directory: Option<PathBuf>,
        remaining: usize,
        references: usize,
    }
    impl Pages<'_> {
        fn charge(&mut self, bytes: usize) -> Result<(), String> {
            self.remaining = self
                .remaining
                .checked_sub(bytes)
                .ok_or("领域事实分页超过资源上限")?;
            Ok(())
        }

        fn walk(
            &mut self,
            value: &mut Value,
            depth: usize,
            allow_pages: bool,
        ) -> Result<(), String> {
            use std::io::Read;

            if depth > 64 {
                return Err("领域事实分页嵌套超过资源上限".into());
            }
            // 同时计入 JSON 字节与节点开销，不能靠大量短值绕过聚合预算。
            self.charge(64)?;
            if value
                .as_object()
                .is_some_and(|map| map.contains_key("$fact_pages"))
            {
                // Python 只分割原数组，不递归分割页；拒绝页内引用也阻断循环图。
                if !self.allowed || !allow_pages || self.ok.as_bool().is_none() {
                    return Err("领域事实分页包含不支持的递归引用".into());
                }
                let fields = value.as_object_mut().unwrap();
                if fields.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "$fact_pages" | "page_counts" | "count" | "instruction"
                    )
                }) || fields
                    .get("instruction")
                    .is_some_and(|value| !value.is_string())
                {
                    return Err("领域事实分页占位字段无效".into());
                }
                // 占位元数据也属于原始图，类型验证不能代替节点与深度计费。
                for child in fields.values_mut() {
                    self.walk(child, depth + 1, false)?;
                }
                let hashes = value["$fact_pages"]
                    .as_array()
                    .ok_or("领域事实分页索引无效")?;
                let counts = value["page_counts"]
                    .as_array()
                    .ok_or("领域事实分页计数缺失")?;
                let total = value["count"].as_u64().ok_or("领域事实分页总数无效")?;
                if hashes.is_empty()
                    || hashes.len() != counts.len()
                    || hashes.len() > 1024 - self.references
                {
                    return Err("领域事实分页索引或资源上限无效".into());
                }
                self.references += hashes.len();
                let sum = counts.iter().try_fold(0u64, |sum, count| {
                    sum.checked_add(count.as_u64().filter(|count| *count > 0)?)
                });
                if sum != Some(total) {
                    return Err("领域事实分页总数与分片覆盖不一致".into());
                }
                if self.directory.is_none() {
                    let context = std::fs::File::open(&self.setup.context_path)
                        .map_err(|_| "无法读取建模上下文")?;
                    let mut raw = Vec::new();
                    context
                        .take(16 * 1024 + 1)
                        .read_to_end(&mut raw)
                        .map_err(|_| "无法读取建模上下文")?;
                    if raw.len() > 16 * 1024
                        || format!("{:x}", Sha256::digest(raw)) != self.setup.context_sha256
                    {
                        return Err("建模上下文已变化；分页事实尚未同步".into());
                    }
                    let root = self
                        .setup
                        .repository
                        .canonicalize()
                        .map_err(|_| "领域事实仓库不可用")?;
                    let directory = root
                        .join("outputs/domain")
                        .join(&self.setup.run_id)
                        .join(match self.setup.mode {
                            DomainMode::Floor => "changes",
                            DomainMode::Project => "project",
                        })
                        .join("facts")
                        .canonicalize()
                        .map_err(|_| "领域事实分页目录不存在")?;
                    if !directory.starts_with(&root) {
                        return Err("领域事实分页目录越界".into());
                    }
                    self.directory = Some(directory);
                }
                let mut items = Vec::new();
                for (hash, count) in hashes.iter().zip(counts) {
                    let hash = hash
                        .as_str()
                        .filter(|sha| {
                            sha.len() == 64
                                && sha
                                    .bytes()
                                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        })
                        .ok_or("领域事实分页 SHA256 无效")?;
                    let directory = self.directory.as_ref().unwrap();
                    let path = directory
                        .join(format!("{hash}.json"))
                        .canonicalize()
                        .map_err(|_| "领域事实分页缺失")?;
                    if !path.starts_with(directory) {
                        return Err("领域事实分页路径越界".into());
                    }
                    let file = std::fs::File::open(path).map_err(|_| "无法读取领域事实分页")?;
                    let metadata = file.metadata().map_err(|_| "无法读取领域事实分页")?;
                    if !metadata.is_file() || metadata.len() > FACT_LIMIT as u64 {
                        return Err("领域事实分页超过单文件资源上限".into());
                    }
                    self.charge(
                        usize::try_from(metadata.len()).map_err(|_| "领域事实分页大小无效")?,
                    )?;
                    let mut raw = Vec::new();
                    file.take(FACT_LIMIT as u64 + 1)
                        .read_to_end(&mut raw)
                        .map_err(|_| "无法读取领域事实分页")?;
                    if raw.len() > FACT_LIMIT
                        || raw.len() as u64 != metadata.len()
                        || format!("{:x}", Sha256::digest(&raw)) != hash
                    {
                        return Err("领域事实分页大小或 SHA256 无效".into());
                    }
                    let mut page: Value =
                        serde_json::from_slice(&raw).map_err(|_| "领域事实分页不是有效 JSON")?;
                    if page.as_object().is_none_or(|fields| fields.len() != 7)
                        || page["contract"] != CONTRACT
                        || page["kind"] != "fact_page"
                        || page["run_id"] != self.setup.run_id
                        || page["context_sha256"] != self.setup.context_sha256
                        || page["fact_sequence"] != self.sequence
                        || page["ok"] != self.ok
                        || page["items"]
                            .as_array()
                            .is_none_or(|items| Some(items.len() as u64) != count.as_u64())
                    {
                        return Err("领域事实分页上下文、序号或计数不一致".into());
                    }
                    // 页封装也计费；items 的深度保持为展开后数组所在位置。
                    self.walk(&mut page, depth.saturating_sub(1), false)?;
                    items.append(page["items"].as_array_mut().unwrap());
                }
                *value = Value::Array(items);
            } else {
                match value {
                    Value::Array(items) => {
                        for child in items {
                            self.walk(child, depth + 1, allow_pages)?;
                        }
                    }
                    Value::Object(fields) => {
                        for child in fields.values_mut() {
                            self.walk(child, depth + 1, allow_pages)?;
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        }
    }
    let mut pages = Pages {
        setup,
        sequence: value["fact_sequence"].clone(),
        ok: value["ok"].clone(),
        allowed: matches!(
            value["kind"].as_str(),
            Some("proposal" | "capabilities" | "project_scope")
        ),
        directory: None,
        remaining: FACT_GRAPH_BUDGET,
        references: 0,
    };
    pages.charge(bytes)?;
    pages.walk(value, 0, true)
}

fn revision_proof_matches(value: &Value, proof: &Value, slab_only: bool) -> bool {
    let proposal = &value["proposal"];
    let certificate = &proposal[if slab_only {
        "slab_revision_source"
    } else {
        "structure_revision_source"
    }];
    let contract = if slab_only {
        "cad-slab-revision/1"
    } else {
        "cad-structure-revision/1"
    };
    let is_sha = |value: &Value| {
        value.as_str().is_some_and(|sha| {
            sha.len() == 64
                && sha
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    };
    let Some(path) = certificate["path"].as_str().filter(|path| !path.is_empty()) else {
        return false;
    };
    let Some(dependencies) = proposal["source_dependencies"].as_object() else {
        return false;
    };
    let Some(proof_dependencies) = proof["dependencies"].as_object() else {
        return false;
    };
    if certificate
        .as_object()
        .is_none_or(|fields| fields.len() != 3)
        || certificate["contract"] != contract
        || proof["contract"] != contract
        || !is_sha(&certificate["sha256"])
        || dependencies.get(path) != Some(&certificate["sha256"])
        || !is_sha(&proof["base_sha256"])
        || proof["base_sha256"] != proposal["base_sha256"]
        || proof_dependencies.iter().any(|(path, sha)| {
            path.is_empty() || !is_sha(sha) || dependencies.get(path) != Some(sha)
        })
    {
        return false;
    }
    if slab_only {
        let Some(operations) = proposal["draft"]["operations"].as_array() else {
            return false;
        };
        operations.len() == 1
            && proof["slab_id"].as_str().is_some_and(|id| !id.is_empty())
            && proof["slab_id"] == operations[0]["target_id"]
            && proof["reason"]
                .as_str()
                .is_some_and(|reason| !reason.trim().is_empty())
            && proof["reason"] == operations[0]["reason"]
    } else {
        proof["audit"].is_object() && proof["audit"] == value["structure_revision"]
    }
}

fn verify_revision_certificate(setup: &DomainSetup, value: &Value) -> Result<(), String> {
    use std::io::Read;

    let proposal = &value["proposal"];
    let slab_only = !proposal["slab_revision_source"].is_null();
    let structure = !proposal["structure_revision_source"].is_null();
    if !slab_only && !structure {
        return Ok(());
    }
    let checked = || -> Option<()> {
        if slab_only && structure {
            return None;
        }
        let certificate = &proposal[if slab_only {
            "slab_revision_source"
        } else {
            "structure_revision_source"
        }];
        let root = setup.repository.canonicalize().ok()?;
        let name = text(certificate, "path").ok()?;
        let path = root.join(name).canonicalize().ok()?;
        if !path.starts_with(&root) {
            return None;
        }
        let file = std::fs::File::open(path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > FACT_LIMIT as u64 {
            return None;
        }
        let mut raw = Vec::new();
        file.take(FACT_LIMIT as u64 + 1)
            .read_to_end(&mut raw)
            .ok()?;
        if raw.len() > FACT_LIMIT || certificate["sha256"] != format!("{:x}", Sha256::digest(&raw))
        {
            return None;
        }
        let proof: Value = serde_json::from_slice(&raw).ok()?;
        // CAD and historical dependencies remain sealed hash bindings here; only
        // the bounded certificate is read on Core's private verification path.
        revision_proof_matches(value, &proof, slab_only).then_some(())
    };
    checked().ok_or_else(|| "结构或楼板修订证书的路径、SHA256 或封存绑定无效".into())
}

/// Replay sealed facts belonging to this context, in semantic sequence order.
/// The sequence index names each fact file and bounds membership; it is never proof
/// that a fact is valid, so every file is re-hashed and checked before `apply` sees it.
/// Facts are read one at a time, so memory and time do not depend on keeping the
/// whole history resident.
pub(super) fn baseline(setup: &DomainSetup, mut apply: impl FnMut(Value)) {
    scan(setup, 0, &mut std::collections::HashMap::new(), |value| {
        apply(value);
        true
    });
}

// A fact may be published before the sequence index. Retry only that file when
// the index changes; unchanged malformed files need no repeated large reads.
pub(crate) type FactFiles =
    std::collections::HashMap<PathBuf, (std::time::SystemTime, Option<String>, Option<u64>)>;

pub(crate) fn scan(
    setup: &DomainSetup,
    after: u64,
    checked: &mut FactFiles,
    mut apply: impl FnMut(Value) -> bool,
) {
    use std::io::Read;
    let mut load = || -> Option<()> {
        let context = std::fs::read(&setup.context_path).ok()?;
        if context.len() > 16 * 1024
            || format!("{:x}", Sha256::digest(&context)) != setup.context_sha256
        {
            return None;
        }
        let root = setup.repository.canonicalize().ok()?;
        let directory = root
            .join("outputs/domain")
            .join(&setup.run_id)
            .join(match setup.mode {
                DomainMode::Floor => "changes",
                DomainMode::Project => "project",
            })
            .canonicalize()
            .ok()?;
        if !directory.starts_with(&root) {
            return None;
        }
        let read = |path: &Path, limit: usize| -> Option<Vec<u8>> {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .ok()?
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() <= limit).then_some(bytes)
        };
        let index_raw = read(&directory.join("fact-sequences.json"), 256 * 1024)?;
        let index_sha = format!("{:x}", Sha256::digest(&index_raw));
        let index: Value = serde_json::from_slice(&index_raw).ok()?;
        // The index is keyed by the semantic result digest, while fact files are named
        // by the digest of their sealed bytes, so the two never share a key. Use the
        // index only as a membership bound on sequence numbers.
        let next = index["next"].as_u64()?;
        let known: std::collections::BTreeSet<u64> = index["facts"]
            .as_object()?
            .values()
            .filter_map(Value::as_u64)
            .filter(|sequence| *sequence > 0 && *sequence < next)
            .collect();
        let facts = directory.join("facts").canonicalize().ok()?;
        if !facts.starts_with(&directory) {
            return None;
        }
        // Load, verify and check one sealed fact; nothing is retained by the caller
        // beyond what it chooses to keep.
        let open = |path: &Path| -> Option<(u64, Value, usize)> {
            let sha = path.file_stem()?.to_str()?;
            if path.extension().and_then(|s| s.to_str()) != Some("json")
                || sha.len() != 64
                || !sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !path.canonicalize().ok()?.starts_with(&facts)
            {
                return None;
            }
            let raw = read(path, FACT_LIMIT)?;
            if format!("{:x}", Sha256::digest(&raw)) != sha {
                return None;
            }
            let value = serde_json::from_slice::<Value>(&raw).ok()?;
            let sequence = value["fact_sequence"].as_u64()?;
            (value["contract"] == CONTRACT
                && value["kind"] != "fact_page"
                && value["run_id"] == setup.run_id
                && value["context_sha256"] == setup.context_sha256)
                .then_some((sequence, value, raw.len()))
        };
        // First pass records only (sequence, path); the second replays in order.
        let mut ordered: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(&facts)
            .ok()?
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                let modified = path.metadata().ok()?.modified().ok()?;
                if let Some((previous, pending, sequence)) = checked.get(&path) {
                    if *previous == modified {
                        if let Some(sequence) = sequence {
                            return (*sequence > after && known.contains(sequence))
                                .then_some((*sequence, path));
                        }
                        if pending.as_ref().is_none_or(|hash| *hash == index_sha) {
                            return None;
                        }
                    }
                }
                if checked.len() >= 65536 {
                    checked.clear();
                }
                let Some((sequence, _, _)) = open(&path) else {
                    checked.insert(path, (modified, None, None));
                    return None;
                };
                if !known.contains(&sequence) {
                    checked.insert(path, (modified, Some(index_sha.clone()), None));
                    return None;
                }
                checked.insert(path.clone(), (modified, None, Some(sequence)));
                (sequence > after).then_some((sequence, path))
            })
            .collect();
        ordered.sort_unstable();
        for (sequence, path) in ordered {
            if let Some((reread, mut value, bytes)) = open(&path) {
                if reread == sequence
                    && known.contains(&sequence)
                    && expand_fact_pages(setup, &mut value, bytes).is_ok()
                    && verify_revision_certificate(setup, &value).is_ok()
                    && !apply(value)
                {
                    break;
                }
            }
        }
        Some(())
    };
    let _ = load();
}

pub async fn read(setup: DomainSetup, receipt: FactReceipt) -> Result<Vec<u8>, String> {
    read_with_deadline(setup, receipt, std::time::Duration::from_secs(10)).await
}

async fn read_with_deadline(
    setup: DomainSetup,
    receipt: FactReceipt,
    deadline: std::time::Duration,
) -> Result<Vec<u8>, String> {
    let raw = tokio::fs::read(&setup.context_path)
        .await
        .map_err(|_| "无法读取建模上下文")?;
    if raw.len() > 16 * 1024 || format!("{:x}", Sha256::digest(raw)) != setup.context_sha256 {
        return Err("建模上下文已变化；完整事实尚未同步".into());
    }
    let mut command = crate::owned_process::Command::new(&setup.python);
    command
        .current_dir(&setup.repository)
        .args(match setup.mode {
            DomainMode::Floor => vec![
                "-X",
                "utf8",
                "-m",
                "blender_agent.cli.main",
                "domain-read-fact",
                "--context",
            ],
            DomainMode::Project => vec![
                "-X",
                "utf8",
                "-m",
                "blender_agent.integrations.project_mcp",
                "--context",
            ],
        })
        .args([setup.context_path.as_os_str()])
        .args([
            if setup.mode == DomainMode::Floor {
                "--fact-sha256"
            } else {
                "--read-fact"
            },
            &receipt.fact_sha256,
        ]);
    let mut child = crate::owned_process::spawn(command, crate::owned_process::Input::Null)
        .map_err(|_| "无法启动领域事实读取进程")?;
    let mut stdout = child.stdout.take().ok_or("缺少领域事实读取通道")?;
    let mut stderr = child.stderr.take().ok_or("缺少领域事实错误通道")?;
    async fn bounded<R: tokio::io::AsyncRead + Unpin>(
        stream: &mut R,
        limit: usize,
    ) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        stream
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "领域事实读取通道失败")?;
        if bytes.len() > limit {
            return Err("领域事实读取超过资源上限".into());
        }
        Ok(bytes)
    }
    let work = async {
        let (bytes, _) =
            tokio::try_join!(bounded(&mut stdout, FACT_LIMIT), bounded(&mut stderr, 4096))?;
        let status = child.wait().await.map_err(|_| "领域事实读取进程异常退出")?;
        if !status.success() {
            return Err("封存领域事实不存在或校验失败".to_owned());
        }
        Ok(bytes)
    };
    let result = tokio::time::timeout(deadline, work)
        .await
        .map_err(|_| "领域事实读取超时；请重新检查提案以同步事实".to_owned())
        .and_then(|result| result);
    if result.is_err() {
        child
            .kill()
            .await
            .map_err(|_| "领域事实读取进程退出未确认".to_owned())?;
    }
    result
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn python() -> PathBuf {
        if let Ok(path) = std::env::var("MODELING_TEST_PYTHON") {
            return path.into();
        }
        if cfg!(windows) {
            for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
                for name in ["python.exe", "python.cmd", "python.bat"] {
                    let path = directory.join(name);
                    if path.is_file() {
                        return path;
                    }
                }
            }
        }
        if cfg!(windows) { "python" } else { "python3" }.into()
    }

    fn fixture(script: &str) -> (DomainSetup, FactReceipt, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "domain-reader-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("blender_agent/cli")).unwrap();
        for file in ["blender_agent/__init__.py", "blender_agent/cli/__init__.py"] {
            std::fs::write(root.join(file), "").unwrap();
        }
        std::fs::write(root.join("blender_agent/cli/main.py"), script).unwrap();
        std::fs::write(root.join("context.json"), b"{}").unwrap();
        let setup = DomainSetup {
            mode: crate::domain::DomainMode::Floor,
            context_path: root.join("context.json"),
            repository: root.clone(),
            python: python(),
            run_id: "run".into(),
            context_sha256: format!("{:x}", Sha256::digest(b"{}")),
            floor_id: "floor".into(),
        };
        let receipt = FactReceipt {
            contract: RECEIPT_CONTRACT.into(),
            run_id: "run".into(),
            context_sha256: setup.context_sha256.clone(),
            kind: "proposal".into(),
            ok: true,
            fact_sequence: 1,
            fact_sha256: "0".repeat(64),
            fact_bytes: 2,
            binding: json!({}),
        };
        (setup, receipt, root)
    }

    #[test]
    fn paged_proposal_projects_every_operation_and_replays_the_same_binding() {
        let (setup, mut receipt, root) = fixture("");
        let directory = root.join("outputs/domain/run/changes");
        std::fs::create_dir_all(directory.join("facts")).unwrap();
        let operations: Vec<Value> = (0..4500).map(|index| json!({
            "collection":"solids","basis":"assumed", "padding":"x".repeat(2000),
            "value_json":json!({"id":format!("solid.{index}"),"origin":[index,0,0],"source_ids":[]}).to_string()
        })).collect();
        assert!(serde_json::to_vec(&operations).unwrap().len() > FACT_LIMIT);
        let mut hashes = Vec::new();
        let mut counts = Vec::new();
        for items in operations.chunks(200) {
            let page = json!({"contract":CONTRACT,"run_id":setup.run_id,
                "context_sha256":setup.context_sha256,"fact_sequence":1,"ok":true,
                "kind":"fact_page","items":items});
            let bytes = serde_json::to_vec(&page).unwrap();
            assert!(bytes.len() <= FACT_LIMIT);
            let hash = format!("{:x}", Sha256::digest(&bytes));
            std::fs::write(directory.join("facts").join(format!("{hash}.json")), bytes).unwrap();
            hashes.push(hash);
            counts.push(items.len());
        }
        let value = json!({"contract":CONTRACT,"run_id":setup.run_id,
            "context_sha256":setup.context_sha256,"kind":"proposal","fact_sequence":1,"ok":true,
            "current":true,"diagnostics":[],"proposal":{"proposal_id":"proposal","revision":2,
            "prepare_token":"token","base_sha256":"base","candidate_sha256":"candidate",
            "state":"prepared","summary":"all operations","issues":[],"pending":[],"assumed":[],
            "draft":{"operations":{"$fact_pages":hashes,"page_counts":counts,"count":operations.len(),
            "instruction":"Read read_fact_view"}}}});
        let bytes = serde_json::to_vec(&value).unwrap();
        receipt.fact_bytes = bytes.len();
        receipt.fact_sha256 = format!("{:x}", Sha256::digest(&bytes));
        receipt.binding = json!({"proposal_id":"proposal","revision":2,"prepare_token":"token",
            "base_sha256":"base","candidate_sha256":"candidate"});
        let verified = verify(&setup, &receipt, &bytes).unwrap();
        let proposal =
            proposal_fact(&verified).expect("all sealed operation pages must be projected");
        assert!(proposal.ready);
        assert_eq!(proposal.objects.len(), operations.len());
        assert!(proposal.objects.last().unwrap().starts_with("solid.4499 ·"));
        assert_eq!(
            proposal.binding.prepare_token,
            receipt.binding["prepare_token"]
        );
        assert_eq!(receipt.fact_bytes, bytes.len());
        assert_eq!(receipt.fact_sha256, format!("{:x}", Sha256::digest(&bytes)));
        let mut scope = value.clone();
        scope["kind"] = json!("project_scope");
        let scope_bytes = serde_json::to_vec(&scope).unwrap();
        let mut scope_receipt = receipt.clone();
        scope_receipt.kind = "project_scope".into();
        scope_receipt.fact_bytes = scope_bytes.len();
        scope_receipt.fact_sha256 = format!("{:x}", Sha256::digest(&scope_bytes));
        let scope_verified = verify(&setup, &scope_receipt, &scope_bytes).unwrap();
        assert_eq!(proposal_fact(&scope_verified).unwrap(), proposal);
        std::fs::write(
            directory
                .join("facts")
                .join(format!("{}.json", scope_receipt.fact_sha256)),
            &scope_bytes,
        )
        .unwrap();
        std::fs::write(
            directory
                .join("facts")
                .join(format!("{}.json", receipt.fact_sha256)),
            &bytes,
        )
        .unwrap();
        std::fs::write(
            directory.join("fact-sequences.json"),
            br#"{"next":2,"facts":{"semantic":1}}"#,
        )
        .unwrap();
        let mut replayed = Vec::new();
        baseline(&setup, |fact| {
            if matches!(fact["kind"].as_str(), Some("proposal" | "project_scope")) {
                replayed.push(proposal_fact(&fact).unwrap());
            }
        });
        assert_eq!(replayed, vec![proposal.clone(), proposal]);
        let last_path = directory
            .join("facts")
            .join(format!("{}.json", hashes.last().unwrap()));
        let mut last: Value = serde_json::from_slice(&std::fs::read(&last_path).unwrap()).unwrap();
        last["items"].as_array_mut().unwrap().last_mut().unwrap()["value_json"] = json!("invalid");
        let invalid_raw = serde_json::to_vec(&last).unwrap();
        let invalid_hash = format!("{:x}", Sha256::digest(&invalid_raw));
        std::fs::write(
            directory.join("facts").join(format!("{invalid_hash}.json")),
            invalid_raw,
        )
        .unwrap();
        let mut invalid_manifest = value.clone();
        *invalid_manifest["proposal"]["draft"]["operations"]["$fact_pages"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap() = json!(invalid_hash);
        let invalid_raw = serde_json::to_vec(&invalid_manifest).unwrap();
        let mut invalid_receipt = receipt.clone();
        invalid_receipt.fact_bytes = invalid_raw.len();
        invalid_receipt.fact_sha256 = format!("{:x}", Sha256::digest(&invalid_raw));
        let invalid = verify(&setup, &invalid_receipt, &invalid_raw).unwrap();
        assert!(
            proposal_fact(&invalid).is_none(),
            "the final operation must still be validated"
        );
        let mut checked = FactFiles::new();
        scan(&setup, 0, &mut checked, |_| true);
        std::fs::write(
            directory
                .join("facts")
                .join(format!("{}.json", hashes.last().unwrap())),
            b"{}",
        )
        .unwrap();
        assert!(verify(&setup, &receipt, &bytes).is_err());
        assert!(verify(&setup, &scope_receipt, &scope_bytes).is_err());
        let mut replayed = 0;
        scan(&setup, 0, &mut checked, |fact| {
            if matches!(fact["kind"].as_str(), Some("proposal" | "project_scope")) {
                replayed += 1;
            }
            true
        });
        assert_eq!(replayed, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fact_pages_reject_broken_identity_coverage_and_recursive_references() {
        let (setup, mut receipt, root) = fixture("");
        let directory = root.join("outputs/domain/run/changes/facts");
        std::fs::create_dir_all(&directory).unwrap();
        let page = json!({"contract":CONTRACT,"run_id":setup.run_id,
            "context_sha256":setup.context_sha256,"fact_sequence":1,"ok":true,
            "kind":"fact_page","items":["first","last"]});
        let seal = |page: &Value| {
            let raw = serde_json::to_vec(page).unwrap();
            let sha = format!("{:x}", Sha256::digest(&raw));
            std::fs::write(directory.join(format!("{sha}.json")), &raw).unwrap();
            sha
        };
        let hash = seal(&page);
        let mut value = json!({"contract":CONTRACT,"run_id":setup.run_id,
            "context_sha256":setup.context_sha256,"fact_sequence":1,"ok":true,
            "kind":"capabilities","entries":{"$fact_pages":[hash],"page_counts":[2],"count":2}});
        receipt.kind = "capabilities".into();
        let mut check = |value: &Value| {
            let raw = serde_json::to_vec(value).unwrap();
            receipt.fact_sha256 = format!("{:x}", Sha256::digest(&raw));
            receipt.fact_bytes = raw.len();
            verify(&setup, &receipt, &raw)
        };
        assert_eq!(check(&value).unwrap()["entries"], json!(["first", "last"]));
        for (field, invalid) in [
            ("contract", json!("other/v2")),
            ("kind", json!("proposal")),
            ("run_id", json!("other")),
            ("context_sha256", json!("0".repeat(64))),
            ("fact_sequence", json!(2)),
            ("ok", json!(false)),
            ("items", json!(["first"])),
        ] {
            let mut changed = page.clone();
            changed[field] = invalid;
            let mut manifest = value.clone();
            manifest["entries"]["$fact_pages"] = json!([seal(&changed)]);
            assert!(check(&manifest).is_err(), "page {field}");
        }
        for entries in [
            json!({"$fact_pages":[hash],"count":2}),
            json!({"$fact_pages":[hash],"page_counts":[],"count":2}),
            json!({"$fact_pages":[hash],"page_counts":[1],"count":2}),
            json!({"$fact_pages":[hash],"page_counts":[1],"count":1}),
            json!({"$fact_pages":[hash],"page_counts":[0],"count":0}),
            json!({"$fact_pages":[hash,hash],"page_counts":[u64::MAX,1],"count":0}),
            json!({"$fact_pages":["../escape"],"page_counts":[2],"count":2}),
            json!({"$fact_pages":["0".repeat(64)],"page_counts":[2],"count":2}),
            json!({"$fact_pages":[],"page_counts":[],"count":0}),
        ] {
            let mut manifest = value.clone();
            manifest["entries"] = entries;
            assert!(check(&manifest).is_err(), "coverage {manifest}");
        }
        let mut recursive = page.clone();
        recursive["items"] = json!([value["entries"], "last"]);
        value["entries"]["$fact_pages"] = json!([seal(&recursive)]);
        assert!(check(&value).unwrap_err().contains("递归"));
        value["entries"]["$fact_pages"] = json!([hash]);
        std::fs::write(directory.join(format!("{hash}.json")), b"{}").unwrap();
        assert!(check(&value).unwrap_err().contains("SHA256"));
        std::fs::remove_file(directory.join(format!("{hash}.json"))).unwrap();
        assert!(check(&value).unwrap_err().contains("缺失"));
        seal(&page);
        std::fs::write(&setup.context_path, b"{\"changed\":true}").unwrap();
        assert!(check(&value).unwrap_err().contains("上下文"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fact_pages_reject_unbudgeted_metadata() {
        let (setup, mut receipt, root) = fixture("");
        let directory = root.join("outputs/domain/run/changes/facts");
        std::fs::create_dir_all(&directory).unwrap();
        let page = json!({"contract":CONTRACT,"run_id":setup.run_id,
            "context_sha256":setup.context_sha256,"fact_sequence":1,"ok":true,
            "kind":"fact_page","items":[1]});
        receipt.kind = "capabilities".into();
        let mut check = |page: &Value, padding: Option<&Value>, instruction: Value| {
            let raw = serde_json::to_vec(page).unwrap();
            assert!(raw.len() <= FACT_LIMIT);
            let hash = format!("{:x}", Sha256::digest(&raw));
            std::fs::write(directory.join(format!("{hash}.json")), raw).unwrap();
            let mut value = json!({"contract":CONTRACT,"run_id":setup.run_id,
                "context_sha256":setup.context_sha256,"fact_sequence":1,"ok":true,
                "kind":"capabilities","entries":{"$fact_pages":[hash],"page_counts":[1],
                "count":1,"instruction":instruction}});
            if let Some(padding) = padding {
                value["entries"]["padding"] = padding.clone();
            }
            let raw = serde_json::to_vec(&value).unwrap();
            assert!(raw.len() <= FACT_LIMIT);
            receipt.fact_bytes = raw.len();
            receipt.fact_sha256 = format!("{:x}", Sha256::digest(&raw));
            let result = verify(&setup, &receipt, &raw);
            if result.is_err() {
                let path = directory.join(format!("{}.json", receipt.fact_sha256));
                std::fs::write(&path, &raw).unwrap();
                std::fs::write(
                    directory.parent().unwrap().join("fact-sequences.json"),
                    br#"{"next":2,"facts":{"semantic":1}}"#,
                )
                .unwrap();
                let mut replayed = false;
                baseline(&setup, |_| replayed = true);
                assert!(!replayed, "invalid metadata must not enter baseline replay");
                std::fs::remove_file(path).unwrap();
            }
            result
        };
        assert!(check(&page, None, json!("Read read_fact_view")).is_ok());
        let mut nested = Value::Null;
        for _ in 0..65 {
            nested = json!([nested]);
        }
        for padding in [json!(vec![0; 2_000_000]), nested] {
            let mut padded_page = page.clone();
            padded_page["padding"] = padding.clone();
            assert!(
                check(&padded_page, None, json!("Read read_fact_view")).is_err(),
                "page envelope must reject unbudgeted extra fields"
            );
            assert!(
                check(&page, Some(&padding), json!("Read read_fact_view")).is_err(),
                "array placeholder must reject unbudgeted extra fields"
            );
            assert!(
                check(&page, None, padding).is_err(),
                "instruction must be a string rather than an unbudgeted subtree"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fact_pages_bound_files_aggregate_bytes_nodes_depth_and_references() {
        let (setup, mut receipt, root) = fixture("");
        let directory = root.join("outputs/domain/run/changes/facts");
        std::fs::create_dir_all(&directory).unwrap();
        receipt.kind = "capabilities".into();
        let mut value = json!({"contract":CONTRACT,"run_id":setup.run_id,
            "context_sha256":setup.context_sha256,"fact_sequence":1,"ok":true,"kind":"capabilities"});
        let page = json!({"contract":CONTRACT,"run_id":setup.run_id,
            "context_sha256":setup.context_sha256,"fact_sequence":1,"ok":true,
            "kind":"fact_page","items":["x".repeat(1024*1024)]});
        let raw = serde_json::to_vec(&page).unwrap();
        let hash = format!("{:x}", Sha256::digest(&raw));
        std::fs::write(directory.join(format!("{hash}.json")), raw).unwrap();
        let mut check = |value: &Value| {
            let raw = serde_json::to_vec(value).unwrap();
            receipt.fact_sha256 = format!("{:x}", Sha256::digest(&raw));
            receipt.fact_bytes = raw.len();
            verify(&setup, &receipt, &raw)
        };
        for references in [65, 1025] {
            value["entries"] = json!({"$fact_pages":vec![hash.clone(); references],
                "page_counts":vec![1; references],"count":references});
            assert!(check(&value).unwrap_err().contains("资源上限"));
        }
        value["entries"] = json!({"$fact_pages":[hash],"page_counts":[1],"count":1});
        std::fs::File::create(directory.join(format!("{hash}.json")))
            .unwrap()
            .set_len(FACT_LIMIT as u64 + 1)
            .unwrap();
        assert!(check(&value).unwrap_err().contains("单文件"));
        value["entries"] = json!(vec![Value::Null; FACT_GRAPH_BUDGET / 64]);
        assert!(check(&value).unwrap_err().contains("资源上限"));
        let mut nested = Value::Null;
        for _ in 0..66 {
            nested = json!([nested]);
        }
        value["entries"] = nested;
        assert!(check(&value).unwrap_err().contains("嵌套"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn server_batch_receipts_are_valid_kinds() {
        let (setup, mut receipt, root) = fixture("");
        for kind in [
            "auto_batch",
            "precedent_auto_batch",
            "precedent_job",
            "group_contact_sheet",
            "group_interpretation_batch",
            "schematic_templates",
            "schematic_circulation",
            "opening_type_representatives",
            "linework_presort",
            "wall_mount_inspection",
            "source_region_inventory",
            "job_index",
            "delivery_status",
            "diagnostic_log",
            "project_floor_preparing",
        ] {
            receipt.kind = kind.into();
            assert!(validate(&setup, &receipt).is_ok(), "{kind}");
        }
        receipt.kind = "invented".into();
        assert!(validate(&setup, &receipt).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    fn revision_fixture(slab_only: bool) -> (Value, Value) {
        let contract = if slab_only {
            "cad-slab-revision/1"
        } else {
            "cad-structure-revision/1"
        };
        let key = if slab_only {
            "slab_revision_source"
        } else {
            "structure_revision_source"
        };
        let audit = json!({"host_mapping":{},"slab":{"verification_questions":[]}});
        let mut value = json!({
            "contract":CONTRACT,"run_id":"run","context_sha256":format!("{:x}", Sha256::digest(b"{}")),
            "kind":"proposal","ok":true,"fact_sequence":1,"structure_revision":audit,
            "proposal":{"base_sha256":"b".repeat(64),
                "source_dependencies":{"certificate.json":"c".repeat(64),"unread-cad.dxf":"d".repeat(64)},
                "draft":{"operations":[{"target_id":"slab.1","reason":"restore CAD boundary"}]}}
        });
        value["proposal"][key] =
            json!({"contract":contract,"path":"certificate.json","sha256":"c".repeat(64)});
        let mut proof = json!({"contract":contract,"base_sha256":"b".repeat(64),
            "dependencies":{"unread-cad.dxf":"d".repeat(64)}});
        if slab_only {
            proof["slab_id"] = json!("slab.1");
            proof["reason"] = json!("restore CAD boundary");
        } else {
            proof["audit"] = audit;
        }
        (value, proof)
    }

    #[test]
    fn revision_certificates_bind_proof_audit_base_and_dependencies() {
        let (value, proof) = revision_fixture(false);
        assert!(revision_proof_matches(&value, &proof, false));
        for (pointer, changed) in [
            ("/contract", json!("cad-slab-revision/1")),
            ("/base_sha256", json!("a".repeat(64))),
            ("/dependencies", Value::Null),
            ("/dependencies/unread-cad.dxf", json!("e".repeat(64))),
            ("/dependencies/unlisted.dxf", json!("a".repeat(64))),
            ("/audit", json!({"host_mapping":{"old":"unrelated"}})),
        ] {
            let mut changed_proof = proof.clone();
            if pointer == "/dependencies/unlisted.dxf" {
                changed_proof["dependencies"]["unlisted.dxf"] = changed;
            } else {
                *changed_proof.pointer_mut(pointer).unwrap() = changed;
            }
            assert!(
                !revision_proof_matches(&value, &changed_proof, false),
                "{pointer}"
            );
        }
        let mut changed = value.clone();
        changed["proposal"]["source_dependencies"]["certificate.json"] = json!("a".repeat(64));
        assert!(!revision_proof_matches(&changed, &proof, false));
        changed = value;
        changed["structure_revision"]["slab"]["verification_questions"] = json!(["changed"]);
        assert!(!revision_proof_matches(&changed, &proof, false));
    }

    #[test]
    fn revision_certificates_bind_slab_identity_and_reason() {
        let (value, proof) = revision_fixture(true);
        assert!(revision_proof_matches(&value, &proof, true));
        for key in ["slab_id", "reason", "base_sha256", "contract"] {
            let mut changed = proof.clone();
            changed[key] = json!("changed");
            assert!(!revision_proof_matches(&value, &changed, true), "{key}");
        }
        let mut changed = value;
        changed["proposal"]["draft"]["operations"] = json!([]);
        assert!(!revision_proof_matches(&changed, &proof, true));
    }

    #[test]
    fn revision_certificates_are_checked_by_receipt_and_cached_scan() {
        let (setup, mut receipt, root) = fixture("");
        let (mut value, mut proof) = revision_fixture(false);
        proof["audit"]["slab"]["verification_questions"] = json!(["check slab"]);
        let directory = root.join("outputs/domain/run/changes");
        std::fs::create_dir_all(directory.join("facts")).unwrap();
        let page = json!({"contract":CONTRACT,"run_id":setup.run_id,
            "context_sha256":setup.context_sha256,"fact_sequence":1,"ok":true,
            "kind":"fact_page","items":proof["audit"]["slab"]["verification_questions"]});
        let page_raw = serde_json::to_vec(&page).unwrap();
        let page_sha = format!("{:x}", Sha256::digest(&page_raw));
        std::fs::write(
            directory.join("facts").join(format!("{page_sha}.json")),
            page_raw,
        )
        .unwrap();
        value["structure_revision"]["slab"]["verification_questions"] = json!({
            "$fact_pages":[page_sha],"page_counts":[1],"count":1});
        let proof_raw = serde_json::to_vec(&proof).unwrap();
        let proof_sha = format!("{:x}", Sha256::digest(&proof_raw));
        std::fs::write(root.join("certificate.json"), &proof_raw).unwrap();
        value["proposal"]["structure_revision_source"]["sha256"] = json!(proof_sha);
        value["proposal"]["source_dependencies"]["certificate.json"] = json!(proof_sha);
        let raw = serde_json::to_vec(&value).unwrap();
        receipt.fact_bytes = raw.len();
        receipt.fact_sha256 = format!("{:x}", Sha256::digest(&raw));
        receipt.binding = json!({"base_sha256":"b".repeat(64)});
        assert!(verify(&setup, &receipt, &raw).is_ok());
        assert!(!root.join("unread-cad.dxf").exists());
        std::fs::write(
            directory.join("fact-sequences.json"),
            br#"{"next":2,"facts":{"semantic":1}}"#,
        )
        .unwrap();
        std::fs::write(
            directory
                .join("facts")
                .join(format!("{}.json", receipt.fact_sha256)),
            &raw,
        )
        .unwrap();
        let mut checked = FactFiles::new();
        let mut count = 0;
        scan(&setup, 0, &mut checked, |_| {
            count += 1;
            true
        });
        assert_eq!(count, 1);
        std::fs::write(root.join("certificate.json"), b"{}").unwrap();
        assert!(verify(&setup, &receipt, &raw).is_err());
        scan(&setup, 0, &mut checked, |_| {
            count += 1;
            true
        });
        assert_eq!(count, 1);
        std::fs::write(root.join("certificate.json"), proof_raw).unwrap();
        scan(&setup, 0, &mut checked, |_| {
            count += 1;
            true
        });
        assert_eq!(count, 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn revision_certificates_reject_outside_and_oversized_files() {
        let (mut setup, _, root) = fixture("");
        let (mut value, proof) = revision_fixture(false);
        let proof_raw = serde_json::to_vec(&proof).unwrap();
        let proof_sha = format!("{:x}", Sha256::digest(&proof_raw));
        std::fs::write(root.join("certificate.json"), &proof_raw).unwrap();
        value["proposal"]["structure_revision_source"] = json!({
            "contract":"cad-structure-revision/1","path":"../certificate.json","sha256":proof_sha});
        value["proposal"]["source_dependencies"]["../certificate.json"] = json!(proof_sha);
        setup.repository = root.join("bound");
        std::fs::create_dir(&setup.repository).unwrap();
        assert!(verify_revision_certificate(&setup, &value).is_err());
        setup.repository = root.clone();
        value["proposal"]["structure_revision_source"]["path"] = json!("certificate.json");
        value["proposal"]["source_dependencies"]["certificate.json"] = json!(proof_sha);
        assert!(verify_revision_certificate(&setup, &value).is_ok());
        std::fs::File::create(root.join("certificate.json"))
            .unwrap()
            .set_len(FACT_LIMIT as u64 + 1)
            .unwrap();
        assert!(verify_revision_certificate(&setup, &value).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn revision_rows_reject_mixed_authority_and_slab_parent_feedback() {
        let (mut value, _) = revision_fixture(true);
        value["project_structure"] = json!(true);
        value["proposal"]["scope"] = json!({"floor_id":"floor","structural_additions":true});
        value["proposal"]["draft"]["operations"][0] = json!({
            "op":"set","collection":"slabs","field":"boundary","floor_id":"floor",
            "target_id":"slab.1","reason":"restore CAD boundary","basis":"assumed",
            "source_ids":["cad.1"],"value_json":"[{\"x\":0,\"y\":0},{\"x\":1,\"y\":0},{\"x\":1,\"y\":1}]"});
        value["proposal"]["hosted_sources"] = json!({});
        value["proposal"]["vertical_sources"] = json!({});
        value["proposal"]["decision_drafts"] = json!([]);
        assert!(project::structure_revision_rows(&value).is_some());
        for (key, mixed) in [
            ("source_evidence", json!({})),
            ("hosted_sources", json!({"source":{}})),
            ("vertical_sources", json!({"source":{}})),
            ("decision_drafts", json!([{}])),
            ("parent_id", json!("parent")),
            ("feedback", json!({})),
        ] {
            let mut changed = value.clone();
            changed["proposal"][key] = mixed;
            assert!(
                project::structure_revision_rows(&changed).is_none(),
                "{key}"
            );
        }
    }

    #[test]
    fn revision_rows_accept_full_host_mapping_and_long_ids() {
        let (mut value, _) = revision_fixture(false);
        value["project_structure"] = json!(true);
        value["proposal"]["scope"] = json!({"floor_id":"floor","structural_additions":true});
        value["proposal"]["draft"]["operations"] = json!([{
            "op":"remove","collection":"walls","floor_id":"floor","target_id":"old",
            "reason":"merge walls","basis":"assumed","source_ids":[]}]);
        let mut hosts: serde_json::Map<String, Value> = (0..50001)
            .map(|index| (format!("wall.{index}"), json!(format!("wall.{index}"))))
            .collect();
        hosts.insert("a".repeat(257), json!("b".repeat(257)));
        value["structure_revision"] = json!({"host_mapping":hosts,"merged_runs":[],
            "closed_joints":[],"cad_bridges":[],"pending":[],
            "slab":{"verification_questions":[],"void_decisions":[]}});
        value["verification_questions"] = json!([]);
        assert!(serde_json::to_vec(&value).unwrap().len() < FACT_LIMIT);
        assert!(project::structure_revision_rows(&value).is_some());
    }

    /// A just-terminated process can hold its working directory for a moment on Windows.
    async fn remove(root: &Path) {
        for _ in 0..150 {
            if std::fs::remove_dir_all(root).is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn exited(root: &Path) {
        let pid: u32 = std::fs::read_to_string(root.join("reader.pid"))
            .unwrap()
            .parse()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while crate::owned_process::process_alive(pid) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        #[cfg(windows)]
        {
            let child: u32 = std::fs::read_to_string(root.join("descendant.pid"))
                .unwrap()
                .parse()
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while crate::owned_process::process_alive(child) {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
        }
    }

    // Pid files appear atomically, so the test never reads a half-written one.
    const SLEEP: &str = "import os,time,subprocess,sys\nfrom pathlib import Path\ndef record(name,pid):\n    Path(name+'.tmp').write_text(str(pid))\n    os.replace(name+'.tmp',name)\nchild=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'])\nrecord('descendant.pid',child.pid)\nrecord('reader.pid',os.getpid())\ntime.sleep(60)\n";

    #[tokio::test]
    async fn deadline_and_cancellation_terminate_private_readers() {
        let (setup, receipt, root) = fixture(SLEEP);
        let result = read_with_deadline(setup, receipt, std::time::Duration::from_secs(3)).await;
        assert!(
            result.as_ref().is_err_and(|error| error.contains("超时")),
            "{result:?}"
        );
        exited(&root).await;
        remove(&root).await;
        let (setup, receipt, root) = fixture(SLEEP);
        let task = tokio::spawn(read(setup, receipt));
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !root.join("reader.pid").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        let _ = task.await;
        exited(&root).await;
        remove(&root).await;
    }

    #[tokio::test]
    async fn independent_reader_rejects_oversized_stdout_and_stderr() {
        for script in [
            "import sys\nsys.stdout.buffer.write(b'x'*(8*1024*1024+1))\n",
            "import sys\nsys.stderr.buffer.write(b'x'*4097)\n",
        ] {
            let (setup, receipt, root) = fixture(script);
            let result = read(setup, receipt).await;
            assert!(
                result
                    .as_ref()
                    .is_err_and(|error| error.contains("资源上限")),
                "{}",
                result.as_ref().err().unwrap()
            );
            remove(&root).await;
        }
    }
}
