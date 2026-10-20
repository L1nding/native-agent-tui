//! Bound project discovery facts; target selection is never scene authorization.
use super::*;
use std::io::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCatalogFact {
    pub catalog_id: String,
    pub project_path: String,
    pub source_sha256: String,
    pub total_floors: usize,
    pub floor_keys: Vec<String>,
    /// Drawing title of each floor key, e.g. `f02` → `二层平面`.
    pub floor_titles: std::collections::BTreeMap<String, String>,
    pub component_source_layers: Option<Vec<String>>,
    pub rows: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionState {
    NeedsSelection,
    Selected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSelectionFact {
    pub selection_id: String,
    pub catalog_id: String,
    pub target: String,
    pub state: SelectionState,
    pub selected_keys: Vec<String>,
    pub candidate_keys: Vec<String>,
    pub source_sha256: String,
    pub rows: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectLocationFact {
    pub location_id: String,
    pub catalog_id: String,
    pub selection_id: String,
    pub source_sha256: String,
    pub floor_key: String,
    pub status: String,
    pub layers: Vec<String>,
    pub rows: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectWallLayerChoice {
    pub expected_location_id: String,
    pub wall_layers: Vec<String>,
    pub reason: String,
}

pub(super) fn location_fact(value: &Value) -> Option<ProjectLocationFact> {
    let location = &value["project_location"];
    if value["ok"] != true
        || location["contract"] != "project-floor-location/v1"
        || location["authorization"] != "not_granted"
        || location["scene_application"] != "not_run"
    {
        return None;
    }
    let status = label(location, "status")?;
    if !matches!(
        status.as_str(),
        "configured" | "derived" | "needs_wall_layers"
    ) {
        return None;
    }
    let statistics = location["layer_statistics"].as_array()?;
    if statistics.len() > 4096 {
        return None;
    }
    let mut layers = Vec::new();
    let mut rows = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for row in statistics {
        let layer = row["layer"].as_str()?;
        let count = row["line_entities"].as_u64()?;
        if layer.is_empty() || layer.chars().count() > 255 || count == 0 || !seen.insert(layer) {
            return None;
        }
        layers.push(layer.to_owned());
        if rows.len() < 40 {
            rows.push(format!(
                "{} · {count} 线实体",
                crate::state::display_text(layer)
            ));
        }
    }
    if statistics.len() > rows.len() {
        rows.push(format!(
            "另有 {} 层；read_fact_view 分页读取完整定位证据",
            statistics.len() - rows.len()
        ));
    }
    Some(ProjectLocationFact {
        location_id: sha(location, "location_id")?,
        catalog_id: sha(location, "catalog_id")?,
        selection_id: sha(location, "selection_id")?,
        source_sha256: sha(location, "source_sha256")?,
        floor_key: label(location, "floor_key")?,
        status,
        layers,
        rows,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAssemblyBinding {
    pub build_id: String,
    pub input_binding: String,
    pub upper_floor_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectOpeningCompletionBinding {
    pub parent_scope_id: String,
    pub bootstrap_proposal_id: String,
    pub base_sha256: String,
    pub source_sha256: String,
    pub opening_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentDiscovery {
    AllLayers,
    ConfiguredLayers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectScopeFact {
    pub activation_revision: Option<u64>,
    pub assembly_binding: Option<ProjectAssemblyBinding>,
    pub opening_completion_binding: Option<ProjectOpeningCompletionBinding>,
    pub wall_layer_choice: Option<ProjectWallLayerChoice>,
    pub component_discovery: Option<ComponentDiscovery>,
    pub scope_id: String,
    pub floor_key: String,
    pub floor_id: String,
    pub selection_id: String,
    pub source_sha256: String,
}

pub(super) fn scope_fact(value: &Value) -> Option<ProjectScopeFact> {
    let scope = &value["project_scope"];
    let activation_revision = match scope.get("activation_revision") {
        None => None,
        Some(value) => Some(value.as_u64().filter(|revision| *revision > 0)?),
    };
    let assembly_binding = match scope.get("assembly_binding") {
        None => None,
        Some(binding) => {
            if binding["contract"] != "project-connection-scope/v1" || activation_revision.is_none()
            {
                return None;
            }
            let upper_floor_key = label(binding, "upper_floor_key")?;
            if scope["floor_key"] == upper_floor_key {
                return None;
            }
            Some(ProjectAssemblyBinding {
                build_id: label(binding, "build_id")?,
                input_binding: sha(binding, "input_binding")?,
                upper_floor_key,
            })
        }
    };
    let wall_layer_choice = match scope.get("wall_layer_choice") {
        None => None,
        Some(choice) => {
            let layers = choice["wall_layers"].as_array()?;
            if layers.is_empty() || layers.len() > 32 || assembly_binding.is_some() {
                return None;
            }
            let mut seen = std::collections::BTreeSet::new();
            let wall_layers = layers
                .iter()
                .map(|value| {
                    let layer = value.as_str()?;
                    if layer.is_empty() || layer.chars().count() > 255 || !seen.insert(layer) {
                        return None;
                    }
                    Some(layer.to_owned())
                })
                .collect::<Option<Vec<_>>>()?;
            Some(ProjectWallLayerChoice {
                expected_location_id: sha(choice, "expected_location_id")?,
                wall_layers,
                reason: bounded_text(choice, "reason", 500)?,
            })
        }
    };
    let opening_completion_binding = match scope.get("opening_completion_binding") {
        None => None,
        Some(binding) => {
            if binding["contract"] != "project-opening-completion/1"
                || activation_revision.is_none()
                || assembly_binding.is_some()
                || wall_layer_choice.is_some()
                || binding["source_sha256"] != scope["source_sha256"]
                || binding["parent_scope_id"] == scope["scope_id"]
            {
                return None;
            }
            let bootstrap_proposal_id = label(binding, "bootstrap_proposal_id")?;
            if bootstrap_proposal_id.len() != 32
                || !bootstrap_proposal_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return None;
            }
            let ids = binding["opening_ids"].as_array()?;
            // Sealed facts are byte-bounded; validate every bound opening rather
            // than imposing a display-sized limit on a complete floor batch.
            if ids.is_empty() {
                return None;
            }
            let mut seen = std::collections::BTreeSet::new();
            let opening_ids = ids
                .iter()
                .map(|value| {
                    let id = value.as_str()?;
                    if id.is_empty() || id.len() > 500 || !seen.insert(id) {
                        return None;
                    }
                    Some(id.to_owned())
                })
                .collect::<Option<Vec<_>>>()?;
            Some(ProjectOpeningCompletionBinding {
                parent_scope_id: sha(binding, "parent_scope_id")?,
                bootstrap_proposal_id,
                base_sha256: sha(binding, "base_sha256")?,
                source_sha256: sha(binding, "source_sha256")?,
                opening_ids,
            })
        }
    };
    Some(ProjectScopeFact {
        component_discovery: match scope.get("component_discovery") {
            None => None,
            Some(mode) => Some(match mode.as_str()? {
                "all_layers" => ComponentDiscovery::AllLayers,
                "configured_layers" => ComponentDiscovery::ConfiguredLayers,
                _ => return None,
            }),
        },
        activation_revision,
        wall_layer_choice,
        assembly_binding,
        opening_completion_binding,
        scope_id: sha(scope, "scope_id")?,
        floor_key: label(scope, "floor_key")?,
        floor_id: label(scope, "floor_id")?,
        selection_id: sha(scope, "selection_id")?,
        source_sha256: sha(scope, "source_sha256")?,
    })
}

pub(super) fn create_context(root: &Path, project: &Path) -> Result<PathBuf, String> {
    if project.is_absolute() {
        return Err("--domain-project 必须是当前工作目录内的相对项目路径".into());
    }
    let path = normalized_path(&root.join(project))?;
    if !path.starts_with(root) || !path.join("project.yaml").is_file() {
        let candidates = std::fs::read_dir(root.join("projects"))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().join("project.yaml").is_file())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .take(8)
            .map(|name| crate::state::display_text(&format!("projects/{name}")))
            .collect::<Vec<_>>();
        let hint = if candidates.is_empty() {
            String::new()
        } else {
            format!(
                "；此处是模型器仓库，可用项目：{}。请将 --domain-project . 改为实际项目相对路径",
                candidates.join("、")
            )
        };
        return Err(format!(
            "建模项目必须在 --cwd 工作目录内并包含 project.yaml{hint}"
        ));
    }
    let run_id = format!(
        "project-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    );
    let mut directory = root.to_path_buf();
    for segment in ["outputs", "domain", run_id.as_str()] {
        directory.push(segment);
        if !directory.exists() {
            std::fs::create_dir(&directory).map_err(|e| e.to_string())?;
        }
        directory = normalized_path(&directory)?;
        if !directory.starts_with(root) {
            return Err("领域运行目录超出项目范围".into());
        }
    }
    let context = directory.join("context.json");
    let raw = serde_json::to_vec(&json!({"contract":"modeling-project/v1", "run_id":run_id,
        "project_root":root, "project_path":path.strip_prefix(root).map_err(|e| e.to_string())?
    }))
    .map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&context)
        .map_err(|e| e.to_string())?;
    file.write_all(&raw).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(context)
}

fn sha(value: &Value, key: &str) -> Option<String> {
    let value = value[key].as_str()?;
    (value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then(|| value.into())
}

fn label(value: &Value, key: &str) -> Option<String> {
    let value = value[key].as_str()?;
    (!value.is_empty() && value.len() <= 512).then(|| crate::state::display_text(value))
}

fn floor_rows(floors: &[Value]) -> Option<Vec<String>> {
    if floors.len() > 1000 {
        return None;
    }
    let mut keys = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    for floor in floors {
        let key = label(floor, "key")?;
        let title = label(floor, "title")?;
        if !keys.insert(key.clone()) {
            return None;
        }
        if rows.len() < 100 {
            let evidence = floor["label_handle"]
                .as_str()
                .filter(|handle| handle.len() <= 80)
                .zip(
                    floor["label_world"]
                        .as_array()
                        .filter(|point| point.len() == 2),
                )
                .and_then(|(handle, point)| Some((handle, point[0].as_f64()?, point[1].as_f64()?)))
                .map_or_else(String::new, |(handle, x, y)| {
                    format!(
                        " · 标注 {} · CAD ({x},{y})",
                        crate::state::display_text(handle)
                    )
                });
            rows.push(format!("{key} · {title}{evidence}"));
        }
    }
    if floors.len() > 100 {
        rows.push(format!(
            "另有 {} 项；通过 read_fact_view 分页读取完整目录",
            floors.len() - 100
        ));
    }
    Some(rows)
}

pub(super) fn catalog_fact(value: &Value) -> Option<ProjectCatalogFact> {
    if value["ok"] != true {
        return None;
    }
    let project = &value["project"];
    let floors = project["floors"].as_array()?;
    let component_source_layers = if let Some(policy) = project.get("component_source_policy") {
        let layers = policy["layers"].as_array()?;
        if layers.is_empty() || layers.len() > 128 {
            return None;
        }
        let mut seen = std::collections::BTreeSet::new();
        Some(
            layers
                .iter()
                .map(|value| {
                    let layer = value.as_str()?;
                    if layer.is_empty() || layer.chars().count() > 255 || !seen.insert(layer) {
                        return None;
                    }
                    Some(crate::state::display_text(layer))
                })
                .collect::<Option<Vec<_>>>()?,
        )
    } else {
        None
    };
    Some(ProjectCatalogFact {
        catalog_id: sha(project, "catalog_id")?,
        project_path: label(project, "project_path")?,
        source_sha256: sha(project, "source_sha256")?,
        total_floors: floors.len(),
        component_source_layers,
        rows: floor_rows(floors)?,
        floor_keys: floors
            .iter()
            .map(|floor| label(floor, "key"))
            .collect::<Option<Vec<_>>>()?,
        floor_titles: floors
            .iter()
            .filter_map(|floor| Some((label(floor, "key")?, label(floor, "title")?)))
            .collect(),
    })
}

pub(super) fn selection_fact(value: &Value) -> Option<ProjectSelectionFact> {
    if value["ok"] != true {
        return None;
    }
    let selection = &value["selection"];
    if selection["scene_application"] != "not_run" || selection["authorization"] != "not_granted" {
        return None;
    }
    let state = match selection["state"].as_str()? {
        "selected" => SelectionState::Selected,
        "needs_selection" => SelectionState::NeedsSelection,
        _ => return None,
    };
    let groups = selection["candidate_groups"].as_array()?;
    if groups.len() > 1000 {
        return None;
    }
    let mut rows = Vec::new();
    let mut all_keys = std::collections::BTreeSet::new();
    let mut has_duplicates = false;
    for group in groups {
        let group = group.as_array()?;
        if group.is_empty() {
            return None;
        }
        let lines = floor_rows(group)?;
        for floor in group {
            let key = label(floor, "key")?;
            if !all_keys.insert(key.clone()) || all_keys.len() > 1000 {
                return None;
            }
            if group.len() > 1 {
                has_duplicates = true;
            }
        }
        if rows.len() < 100 {
            rows.push(if group.len() > 1 {
                if state == SelectionState::NeedsSelection {
                    "重复图纸：需用户选择".into()
                } else {
                    "重复图纸候选（已选择）".into()
                }
            } else {
                "唯一候选".into()
            });
            rows.extend(lines.into_iter().take(100usize.saturating_sub(rows.len())));
        }
    }
    let keys = selection["selected_keys"].as_array()?;
    let selected_keys = keys
        .iter()
        .map(|value| {
            let value = value.as_str()?;
            (value.len() <= 512 && all_keys.contains(value)).then(|| value.to_owned())
        })
        .collect::<Option<Vec<_>>>()?;
    let chosen = selected_keys
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if chosen.len() != selected_keys.len()
        || (state == SelectionState::NeedsSelection && (!chosen.is_empty() || !has_duplicates))
    {
        return None;
    }
    if state == SelectionState::Selected
        && (selected_keys.is_empty()
            || groups.iter().any(|group| {
                group
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|floor| chosen.contains(&floor["key"].as_str().unwrap().to_owned()))
                    .count()
                    != 1
            }))
    {
        return None;
    }
    if all_keys.len() > 100 {
        rows.push("完整候选请通过 read_fact_view 分页读取".into());
    }
    Some(ProjectSelectionFact {
        selection_id: sha(selection, "selection_id")?,
        catalog_id: sha(selection, "catalog_id")?,
        target: label(selection, "target")?,
        state,
        selected_keys,
        candidate_keys: all_keys.into_iter().collect(),
        source_sha256: sha(selection, "source_sha256")?,
        rows,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildingReportFact {
    pub state: String,
    pub technical_status: String,
    pub current: bool,
    pub artifact_valid: bool,
    pub source_total: u64,
    pub unresolved_sources: u64,
    pub unresolved_drawing: u64,
    pub missing_floor_reviews: usize,
    pub connection_issues: usize,
    pub source_discovery: BuildingSourceDiscoveryFact,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BuildingSourceDiscoveryFact {
    pub state: String,
    pub missing_floors: Vec<String>,
    pub unresolved_member_count: u64,
    pub unclassified_candidate_count: u64,
    pub uncovered_count: u64,
    pub diagnostic_member_count: u64,
    pub stale_selection_count: u64,
    pub region_count: u64,
    pub unreviewed_region_count: u64,
    pub structure_represented_region_count: u64,
    pub uncertain_region_count: u64,
    pub stale_review_count: u64,
    pub orphaned_stale_review_count: u64,
}

fn building_source_discovery(floors: &[Value]) -> Option<BuildingSourceDiscoveryFact> {
    let mut fact = BuildingSourceDiscoveryFact {
        state: "complete".into(),
        missing_floors: Vec::new(),
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
    };
    for floor in floors {
        let linework = linework_coverage_fact(floor).ok()?;
        let structure = existing_structure_coverage_fact(floor, linework.as_ref()).ok()?;
        let regions =
            source_region_review_fact(floor, linework.as_ref(), structure.as_ref()).ok()?;
        if linework.is_some() {
            sha(floor, "scan_id")?;
            sha(floor, "source_view_sha256")?;
        }
        if linework.is_none()
            || regions.is_none()
            || linework
                .as_ref()
                .is_some_and(|l| l.unselected_member_count > 0 && structure.is_none())
        {
            fact.missing_floors.push(label(floor, "floor_key")?);
        }
        if let Some(linework) = linework {
            let covered = structure.map_or(0, |s| s.unselected_complete_member_count);
            for (total, count) in [
                (
                    &mut fact.unresolved_member_count,
                    linework.unselected_member_count.checked_sub(covered)?,
                ),
                (
                    &mut fact.unclassified_candidate_count,
                    linework.unclassified_candidate_count,
                ),
                (&mut fact.uncovered_count, linework.uncovered_count),
                (
                    &mut fact.diagnostic_member_count,
                    linework.diagnostic_member_count,
                ),
                (
                    &mut fact.stale_selection_count,
                    linework.stale_selection_count,
                ),
            ] {
                *total = total.checked_add(count as u64)?;
            }
        }
        if let Some(regions) = regions {
            for (total, count) in [
                (&mut fact.region_count, regions.region_count),
                (
                    &mut fact.unreviewed_region_count,
                    regions.unreviewed_region_count,
                ),
                (
                    &mut fact.structure_represented_region_count,
                    regions.structure_represented_region_count,
                ),
                (
                    &mut fact.uncertain_region_count,
                    regions.uncertain_region_count,
                ),
                (&mut fact.stale_review_count, regions.stale_review_count),
                (
                    &mut fact.orphaned_stale_review_count,
                    regions.orphaned_stale_review_count,
                ),
            ] {
                *total = total.checked_add(count as u64)?;
            }
        }
    }
    fact.missing_floors.sort();
    fact.state = if !fact.missing_floors.is_empty() {
        "unknown"
    } else if [
        fact.unresolved_member_count,
        fact.unclassified_candidate_count,
        fact.uncovered_count,
        fact.diagnostic_member_count,
        fact.stale_selection_count,
        fact.unreviewed_region_count,
        fact.uncertain_region_count,
        fact.stale_review_count,
        fact.orphaned_stale_review_count,
    ]
    .into_iter()
    .any(|count| count > 0)
    {
        "attention"
    } else {
        "complete"
    }
    .into();
    Some(fact)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConnectionAuthority {
    pub scope_id: String,
    pub scene_sha256: String,
    pub parent_build_id: String,
    pub parent_input_binding: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectBuildingFact {
    pub build_id: String,
    pub selection_id: String,
    pub input_binding: String,
    pub input_sha256: String,
    pub state: String,
    pub current: bool,
    pub valid: Option<bool>,
    pub floor_keys: Vec<String>,
    pub floor_authorities: BTreeMap<String, String>,
    pub source_reconciliation: String,
    pub source_discovery: BuildingSourceDiscoveryFact,
    pub connection_check: String,
    pub report: Option<BuildingReportFact>,
    pub connection_authority: Option<ProjectConnectionAuthority>,
    pub output: String,
}

pub(super) fn building_fact(
    value: &Value,
    selection: &ProjectSelectionFact,
) -> Option<ProjectBuildingFact> {
    if value["ok"] != true || selection.state != SelectionState::Selected {
        return None;
    }
    let record = &value["project_building"];
    let inputs = &record["inputs"];
    if record["contract"] != "project-building/v1"
        || inputs["selection_id"] != selection.selection_id
        || inputs["catalog_id"] != selection.catalog_id
        || inputs["source_sha256"] != selection.source_sha256
    {
        return None;
    }
    let build_id = label(record, "build_id")?;
    if build_id.len() > 80
        || !build_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return None;
    }
    let state = label(record, "state")?;
    if !matches!(
        state.as_str(),
        "queued" | "building" | "unknown" | "cancelled" | "failed" | "built"
    ) {
        return None;
    }
    let floors = inputs["floors"].as_array()?;
    if floors.len() < 2 || floors.len() != selection.selected_keys.len() {
        return None;
    }
    let mut floor_keys = Vec::new();
    let mut floor_authorities = BTreeMap::new();
    let mut has_unbuilt_sources = false;
    for floor in floors {
        let key = label(floor, "floor_key")?;
        if floor_keys.contains(&key) || !selection.selected_keys.contains(&key) {
            return None;
        }
        sha(floor, "scope_id")?;
        let floor_id = label(floor, "floor_id")?;
        if floor_authorities
            .insert(floor_id, sha(floor, "scene_sha256")?)
            .is_some()
        {
            return None;
        }
        let counts: BTreeMap<String, u64> =
            serde_json::from_value(floor["source_counts"].clone()).ok()?;
        let sum = counts
            .values()
            .try_fold(0u64, |sum, count| sum.checked_add(*count))?;
        if floor["source_total"].as_u64()? != sum {
            return None;
        }
        has_unbuilt_sources |= counts
            .iter()
            .any(|(state, count)| state != "built" && *count > 0);
        floor_keys.push(key);
    }
    let valid = match record.get("valid") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_bool()?),
    };
    if valid == Some(true) && state != "built" {
        return None;
    }
    if state == "built" {
        for suffix in [".glb", ".blend", ".validation.json"] {
            sha(&record["artifact_hashes"], suffix)?;
        }
        for phase in ["built", "rebuilt", "glb_roundtrip"] {
            if record["checks"][phase] != "passed" {
                return None;
            }
        }
    }
    let source_reconciliation = label(record, "source_reconciliation")?;
    let source_discovery = building_source_discovery(floors)?;
    if (source_reconciliation == "complete"
        && (has_unbuilt_sources || source_discovery.state != "complete"))
        || !matches!(source_reconciliation.as_str(), "complete" | "attention")
        || record["human_review"] != "not_run"
    {
        return None;
    }
    let connection_check = label(record, "connection_check")?;
    let connection_issues = connection_review(record, &floor_authorities, valid)?;
    let report = if record["report"].is_null() {
        None
    } else {
        Some(building_report_fact(
            record,
            floors,
            valid,
            connection_issues,
        )?)
    };
    let connection_authority = match inputs.get("connection_authority") {
        None => None,
        Some(authority) => {
            label(authority, "proposal_id")?;
            label(authority, "authorization_id")?;
            label(authority, "operation_id")?;
            sha(authority, "prepare_token")?;
            authority["revision"]
                .as_u64()
                .filter(|revision| *revision > 0)?;
            let scene_sha256 = sha(authority, "scene_sha256")?;
            if scene_sha256 != sha(record, "input_sha256")? {
                return None;
            }
            Some(ProjectConnectionAuthority {
                scope_id: sha(authority, "scope_id")?,
                scene_sha256,
                parent_build_id: label(authority, "parent_build_id")?,
                parent_input_binding: sha(authority, "parent_input_binding")?,
            })
        }
    };
    Some(ProjectBuildingFact {
        connection_authority,
        build_id,
        selection_id: selection.selection_id.clone(),
        input_binding: sha(record, "input_binding")?,
        input_sha256: sha(record, "input_sha256")?,
        state,
        current: record["current"].as_bool()?,
        valid,
        floor_keys,
        floor_authorities,
        source_reconciliation,
        source_discovery,
        connection_check,
        report,
        output: label(record, "output")?,
    })
}

fn connection_review(
    record: &Value,
    floors: &BTreeMap<String, String>,
    valid: Option<bool>,
) -> Option<usize> {
    let state = record["connection_check"].as_str()?;
    if record["connection_review"].is_null() {
        return (state == "not_run").then_some(0);
    }
    let review = &record["connection_review"];
    if review["contract"] != "project-building-connections/v1" || review["state"] != state {
        return None;
    }
    let pairs = review["adjacent_pair_count"].as_u64()?;
    let covered = review["covered_pair_count"].as_u64()?;
    let declared = review["declared_count"].as_u64()?;
    let verified = review["verified_count"].as_u64()?;
    let rows = review["verified"].as_array()?;
    let issues = review["issues"].as_array()?;
    if covered > pairs || verified > declared || rows.len() as u64 != verified {
        return None;
    }
    match state {
        "passed"
            if valid == Some(true)
                && pairs == floors.len().saturating_sub(1) as u64
                && pairs > 0
                && covered == pairs
                && verified > 0
                && issues.is_empty() => {}
        "attention"
            if valid == Some(true)
                && pairs == floors.len().saturating_sub(1) as u64
                && !issues.is_empty() => {}
        "not_run" if valid != Some(true) && verified == 0 && covered == 0 => {}
        _ => return None,
    }
    if state == "passed" {
        sha(&record["inputs"], "connection_sources_sha256")?;
    }
    let mut components = std::collections::BTreeSet::new();
    for item in rows {
        let component = label(item, "component_id")?;
        label(item, "source_id")?;
        if !components.insert(component) || !matches!(item["kind"].as_str()?, "stair" | "shaft") {
            return None;
        }
        let served = item["served_floors"].as_array()?;
        if served.len() != 2
            || served[0] == served[1]
            || served
                .iter()
                .any(|id| id.as_str().is_none_or(|id| !floors.contains_key(id)))
        {
            return None;
        }
        let parts = item["part_ids"].as_array()?;
        let ids = parts
            .iter()
            .map(|id| id.as_str().filter(|id| !id.is_empty() && id.len() <= 512))
            .collect::<Option<std::collections::BTreeSet<_>>>()?;
        if parts.is_empty() || ids.len() != parts.len() {
            return None;
        }
    }
    for issue in issues {
        label(issue, "code")?;
        label(issue, "message")?;
    }
    Some(issues.len())
}

fn building_report_fact(
    record: &Value,
    floors: &[Value],
    valid: Option<bool>,
    connection_issues: usize,
) -> Option<BuildingReportFact> {
    let report = &record["report"];
    if report["contract"] != "project-building-report/v1"
        || report["build_id"] != record["build_id"]
        || report["input_binding"] != record["input_binding"]
        || report["input_sha256"] != record["input_sha256"]
        || report["human_review"] != "not_run"
        || report["connections"] != record["connection_review"]
        || report["artifact_valid"].as_bool()? != valid.unwrap_or(false)
    {
        return None;
    }
    let mut expected_counts = BTreeMap::<String, u64>::new();
    let mut candidates = 0u64;
    let mut judged = 0u64;
    let mut unresolved_drawing = 0u64;
    let mut missing = std::collections::BTreeSet::new();
    for floor in floors {
        let counts: BTreeMap<String, u64> =
            serde_json::from_value(floor["source_counts"].clone()).ok()?;
        for (state, count) in counts {
            let entry = expected_counts.entry(state).or_default();
            *entry = entry.checked_add(count)?;
        }
        let audit = &floor["drawing_review"];
        if audit.is_null() {
            missing.insert(label(floor, "floor_key")?);
            continue;
        }
        let count = audit["candidate_count"].as_u64()?;
        let done = audit["judged_count"].as_u64()?;
        let verdicts: BTreeMap<String, u64> =
            serde_json::from_value(audit["verdict_counts"].clone()).ok()?;
        if done > count
            || verdicts.values().try_fold(0u64, |n, c| n.checked_add(*c))? != done
            || verdicts
                .keys()
                .any(|key| !matches!(key.as_str(), "error" | "doubt" | "not_error"))
        {
            return None;
        }
        candidates = candidates.checked_add(count)?;
        judged = judged.checked_add(done)?;
        unresolved_drawing = unresolved_drawing.checked_add(count - done)?;
        for (state, count) in verdicts {
            if state != "not_error" {
                unresolved_drawing = unresolved_drawing.checked_add(count)?;
            }
        }
    }
    let source_total = expected_counts
        .values()
        .try_fold(0u64, |n, c| n.checked_add(*c))?;
    let unresolved_sources = expected_counts
        .iter()
        .filter(|(s, _)| s.as_str() != "built")
        .try_fold(0u64, |n, (_, c)| n.checked_add(*c))?;
    let observed: BTreeMap<String, u64> =
        serde_json::from_value(report["source_counts"].clone()).ok()?;
    let drawing = &report["drawing_review"];
    let observed_missing = drawing["missing_floors"]
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect::<Option<std::collections::BTreeSet<_>>>()?;
    if drawing["missing_floors"].as_array()?.len() != observed_missing.len() {
        return None;
    }
    if observed != expected_counts
        || report["source_total"].as_u64()? != source_total
        || report["unresolved_source_count"].as_u64()? != unresolved_sources
        || drawing["candidate_count"].as_u64()? != candidates
        || drawing["judged_count"].as_u64()? != judged
        || drawing["unresolved_count"].as_u64()? != unresolved_drawing
        || observed_missing != missing
    {
        return None;
    }
    let current = report["current"].as_bool()?;
    let source_discovery = building_source_discovery(floors)?;
    if report.get("source_discovery").is_some_and(|observed| {
        let mut observed = observed.clone();
        if let Some(fields) = observed.as_object_mut() {
            fields
                .entry("structure_represented_region_count")
                .or_insert(Value::from(0));
        }
        serde_json::to_value(&source_discovery).ok().as_ref() != Some(&observed)
    }) || (report.get("source_discovery").is_none() && source_discovery.state == "complete")
    {
        return None;
    }
    let expected = if valid != Some(true) {
        "not_ready"
    } else if !current
        || unresolved_sources > 0
        || source_discovery.state != "complete"
        || unresolved_drawing > 0
        || !missing.is_empty()
        || record["connection_check"] != "passed"
    {
        "attention"
    } else {
        "complete"
    };
    if report["technical_status"] != expected
        || report["state"]
            != if expected == "not_ready" {
                "not_ready"
            } else {
                "attention"
            }
    {
        return None;
    }
    Some(BuildingReportFact {
        state: label(report, "state")?,
        technical_status: expected.into(),
        current,
        artifact_valid: valid.unwrap_or(false),
        source_total,
        unresolved_sources,
        unresolved_drawing,
        missing_floor_reviews: missing.len(),
        connection_issues,
        source_discovery,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StairLineworkFact {
    pub line_count: usize,
    pub region_count: usize,
    pub reviewed_region_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawingAuditFact {
    pub audit_id: String,
    pub scene_sha256: String,
    pub candidate_count: usize,
    pub judged_count: usize,
    pub ai_review: String,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub stair_linework: Option<StairLineworkFact>,
    pub rows: Vec<String>,
}

pub(super) fn drawing_audit_fact(value: &Value) -> Option<DrawingAuditFact> {
    let audit = &value["drawing_audit"];
    if value["ok"] != true || audit["scene_application"] != "not_run" {
        return None;
    }
    if audit
        .get("human_review")
        .is_some_and(|value| value != "not_run")
    {
        return None;
    }
    let candidate_count = usize::try_from(audit["candidate_count"].as_u64()?).ok()?;
    let judged_count = match audit.get("judged_count") {
        None => 0,
        Some(count) => usize::try_from(count.as_u64()?).ok()?,
    };
    let ai_review = label(audit, "ai_review")?;
    if judged_count > candidate_count
        || match ai_review.as_str() {
            "not_run" => judged_count != 0,
            "partial" => judged_count == 0 || judged_count == candidate_count,
            "recorded" => judged_count == 0 || judged_count != candidate_count,
            _ => true,
        }
    {
        return None;
    }
    let offset = usize::try_from(audit["offset"].as_u64()?).ok()?;
    let candidates = audit["candidates"].as_array()?;
    let end = offset.checked_add(candidates.len())?;
    if offset > candidate_count || end > candidate_count || candidates.len() > 200 {
        return None;
    }
    let next_offset = if audit["next_offset"].is_null() {
        None
    } else {
        Some(usize::try_from(audit["next_offset"].as_u64()?).ok()?)
    };
    if (end < candidate_count && (candidates.is_empty() || next_offset != Some(end)))
        || (end == candidate_count && next_offset.is_some())
    {
        return None;
    }
    let visible_judgements = candidates
        .iter()
        .filter(|c| !c["judgement"].is_null())
        .count();
    if visible_judgements > judged_count
        || (offset == 0 && end == candidate_count && visible_judgements != judged_count)
    {
        return None;
    }
    let stair_linework = match audit.get("stair_linework") {
        None => None,
        Some(stairs) => {
            let line_count = usize::try_from(stairs["line_count"].as_u64()?).ok()?;
            let region_count = usize::try_from(stairs["region_count"].as_u64()?).ok()?;
            let reviewed_region_count =
                usize::try_from(stairs["reviewed_region_count"].as_u64()?).ok()?;
            if region_count > candidate_count
                || region_count > line_count
                || (line_count == 0) != (region_count == 0)
                || reviewed_region_count > region_count
                || reviewed_region_count > judged_count
            {
                return None;
            }
            let visible = candidates
                .iter()
                .filter(|c| c["detector"] == "stair_linework_reference")
                .count();
            let reviewed = candidates
                .iter()
                .filter(|c| {
                    c["detector"] == "stair_linework_reference" && !c["judgement"].is_null()
                })
                .count();
            if visible > region_count
                || reviewed > reviewed_region_count
                || (offset == 0
                    && end == candidate_count
                    && (visible != region_count || reviewed != reviewed_region_count))
            {
                return None;
            }
            Some(StairLineworkFact {
                line_count,
                region_count,
                reviewed_region_count,
            })
        }
    };
    let rows = candidates
        .iter()
        .map(|candidate| {
            let observation = if candidate["judgement"].is_null() {
                "未判断".into()
            } else {
                let record = &candidate["judgement"];
                let verdict = label(&record["judgement"], "verdict")?;
                if !matches!(verdict.as_str(), "error" | "doubt" | "not_error")
                    || record["candidate_id"] != candidate["id"]
                    || record["audit_id"] != audit["audit_id"]
                    || record["scene_sha256"] != audit["scene_sha256"]
                    || record["actor"] != "platform_agent"
                    || record["human_review"] != "not_run"
                    || record["scene_application"] != "not_run"
                {
                    return None;
                }
                sha(record, "image_sha256")?;
                format!(
                    "Agent {verdict}: {}",
                    bounded_text(&record["judgement"], "explanation", 4000)?
                )
            };
            let location = if candidate["detector"] == "stair_linework_reference" {
                let bbox = candidate["bbox_m"].as_array()?;
                if bbox.len() != 4 {
                    return None;
                }
                let bounds = bbox.iter().map(Value::as_f64).collect::<Option<Vec<_>>>()?;
                if bounds.iter().any(|value| !value.is_finite())
                    || bounds[0] > bounds[2]
                    || bounds[1] > bounds[3]
                {
                    return None;
                }
                format!(
                    " · 位置 ({:.2}, {:.2})—({:.2}, {:.2}) 米",
                    bounds[0], bounds[1], bounds[2], bounds[3]
                )
            } else {
                String::new()
            };
            Some(format!(
                "{} · {}{location} · {} · {observation}",
                label(candidate, "id")?,
                label(candidate, "detector")?,
                label(candidate, "message")?
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(DrawingAuditFact {
        audit_id: sha(audit, "audit_id")?,
        scene_sha256: sha(audit, "scene_sha256")?,
        candidate_count,
        judged_count,
        ai_review,
        offset,
        next_offset,
        stair_linework,
        rows,
    })
}

/// A modeler project the user can open: `projects/<dir>/project.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectEntry {
    /// Relative to the modeler repository, e.g. `projects/hospital-site`.
    pub path: String,
    pub name: String,
    pub description: String,
}

/// Drawing units the modeler accepts for a new project's DXF source.
pub const PROJECT_UNITS: [&str; 3] = ["millimeters", "centimeters", "meters"];

/// A user's request to create a project from one DXF drawing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewProject {
    pub name: String,
    pub description: String,
    pub source: String,
    pub units: String,
}

/// Top-level scalar from project.yaml. Only display fields are read here;
/// the modeler owns and validates the full format.
fn yaml_field(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix(':')?.trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'');
        (!value.is_empty() && value != "null").then(|| value.to_owned())
    })
}

/// Projects under `<cwd>/projects`, sorted by directory name. Empty when the
/// working directory is not a modeler repository.
pub fn list_projects(cwd: &Path) -> Vec<ProjectEntry> {
    let mut entries: Vec<ProjectEntry> = std::fs::read_dir(cwd.join("projects"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let dir = entry.file_name().into_string().ok()?;
            let text = std::fs::read_to_string(entry.path().join("project.yaml")).ok()?;
            Some(ProjectEntry {
                path: format!("projects/{dir}"),
                name: crate::state::display_text(&yaml_field(&text, "name").unwrap_or(dir.clone())),
                description: crate::state::display_text(
                    &yaml_field(&text, "description").unwrap_or_default(),
                ),
            })
        })
        .take(200)
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

/// The modeler repository's own interpreter, as used for the domain tools.
pub(super) fn modeler_python(repository: &Path) -> PathBuf {
    repository.join(if cfg!(windows) {
        ".venv/Scripts/python.exe"
    } else {
        ".venv/bin/python"
    })
}

/// Create a project through the modeler's `init-project` command, which owns
/// the project format. Returns the new project's relative path.
pub fn create_project(cwd: &Path, request: &NewProject) -> Result<String, String> {
    let repository = normalized_path(cwd)?;
    let python = modeler_python(&repository);
    if !python.is_file() {
        return Err("当前工作目录不是带 .venv 的模型器仓库，无法新建项目".into());
    }
    let source = request.source.trim().trim_matches('"');
    if request.name.trim().is_empty() || source.is_empty() {
        return Err("请填写项目名称和 DXF 图纸路径".into());
    }
    let output = std::process::Command::new(&python)
        .current_dir(&repository)
        .env("PYTHONUTF8", "1")
        .args(["-m", "blender_agent.cli.main", "init-project"])
        .arg(request.name.trim())
        .arg("--source")
        .arg(source)
        .arg("--units")
        .arg(&request.units)
        .arg("--description")
        .arg(request.description.trim())
        .arg("--root")
        .arg(&repository)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("无法运行模型器新建项目命令：{error}"))?;
    let reply: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        let detail = String::from_utf8_lossy(&output.stderr);
        format!(
            "新建项目失败：{}",
            crate::state::display_text(detail.lines().last().unwrap_or("模型器没有返回结果"))
        )
    })?;
    match (reply["ok"].as_bool(), reply["project_path"].as_str()) {
        (Some(true), Some(path)) if output.status.success() => Ok(path.to_owned()),
        _ => Err(format!(
            "新建项目失败：{}",
            crate::state::display_text(reply["error"].as_str().unwrap_or("未知错误"))
        )),
    }
}
