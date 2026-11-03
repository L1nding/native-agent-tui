//! Proposal-bound PNG evidence and Core-owned local file presentation.
use super::*;
use std::collections::BTreeSet;
use tokio::io::AsyncReadExt;

// Whole-floor batches can contain hundreds of review groups. Only metadata is
// retained here; PNG bytes are streamed under MAX_PNG_BYTES when checked.
pub const MAX_IMAGES: usize = 512;
const MAX_PNG_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageFileState {
    Pending,
    Checking,
    Available,
    Unavailable,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ReviewImageBinding {
    pub proposal: ProposalBinding,
    pub review_set_sha256: Option<String>,
    pub review_key: String,
    pub image_sha256: String,
    pub web_review: Option<crate::review_web::WebReviewPackage>,
    path: PathBuf,
    dimensions: Option<[u32; 2]>,
}

impl std::fmt::Debug for ReviewImageBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReviewImageBinding")
            .field("proposal_id", &self.proposal.proposal_id)
            .field("revision", &self.proposal.revision)
            .field("image_sha256", &self.image_sha256)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewImageView {
    Comparison,
    Overview,
    Detail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewImageFact {
    pub binding: ReviewImageBinding,
    pub label: String,
    pub view: ReviewImageView,
    pub required: bool,
    pub file_state: ImageFileState,
    pub externally_opened: bool,
    pub notice: Option<String>,
    pub detail: Option<String>,
}

impl ReviewImageFact {
    pub fn verified_path(&self) -> Option<&Path> {
        (self.file_state == ImageFileState::Available).then_some(self.binding.path.as_path())
    }

    pub(crate) fn finish_open(&mut self, outcome: ImageOpenOutcome) {
        self.externally_opened = outcome.file_valid && outcome.result.is_ok();
        if !outcome.file_valid {
            self.file_state = ImageFileState::Unavailable;
        }
        self.notice = Some(
            outcome
                .result
                .err()
                .unwrap_or_else(|| "已成功外部打开；不代表已经看见或完成AI观察".into()),
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstUseReviewFact {
    pub status: String,
    pub review_set_sha256: Option<String>,
    pub required_count: usize,
    pub blocked_count: usize,
    pub completed: Option<usize>,
    pub total: Option<usize>,
    pub job_id: Option<String>,
    pub dimensions: Vec<String>,
    requirements: Value,
    edit_parts: Option<Vec<Value>>,
    structure_parts: Option<Vec<(String, Value)>>,
}

impl FirstUseReviewFact {
    pub fn satisfied(&self, images: &[ReviewImageFact]) -> bool {
        if self.status == "not_required" {
            return self.required_count == 0;
        }
        let required: Vec<_> = images.iter().filter(|image| image.required).collect();
        self.status == "ready"
            && self.blocked_count == 0
            && self.required_count > 0
            && self.review_set_sha256.is_some()
            && required.len() >= self.required_count
            // A delivered, hash-verified review card is sufficient evidence for
            // the current version. Opening one image remains available for users
            // who want the external viewer, but approval must not require opening
            // every comparison/detail image one by one.
            && required.iter().any(|image| {
                image.binding.review_set_sha256 == self.review_set_sha256
                    && image.file_state == ImageFileState::Available
            })
    }
}

fn hash(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

// Python's geometry digest uses repr-style floats, including a signed,
// two-digit exponent. serde_json's default exponent spelling is different.
struct PythonGeometryFormatter;

impl serde_json::ser::Formatter for PythonGeometryFormatter {
    fn write_f64<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        value: f64,
    ) -> std::io::Result<()> {
        let text = if value == 0.0 || (1e-4..1e16).contains(&value.abs()) {
            let text = value.to_string();
            if text.contains('.') {
                text
            } else {
                format!("{text}.0")
            }
        } else {
            let text = format!("{value:e}");
            let (mantissa, exponent) = text.split_once('e').unwrap();
            format!("{mantissa}e{:+03}", exponent.parse::<i32>().unwrap())
        };
        writer.write_all(text.as_bytes())
    }
}

fn strings(value: &Value, limit: usize) -> Option<BTreeSet<String>> {
    let values = value.as_array()?;
    if values.is_empty() || values.len() > limit {
        return None;
    }
    let result: BTreeSet<_> = values
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty() && s.len() <= 4096)
                .map(str::to_owned)
        })
        .collect::<Option<_>>()?;
    (result.len() == values.len()).then_some(result)
}

pub(crate) fn category_label(category: &str) -> &str {
    match category {
        "armchair" => "扶手椅",
        "work_chair" => "工作椅",
        "chair" => "椅子",
        "bed" => "床",
        "examination_bed" | "examination_table" | "examination_couch" => "检查床",
        "hospital_bed" => "移动病床",
        "gynecological_exam_table" => "妇科检查台",
        "rack" | "storage_rack" => "置物架",
        "cabinet" => "柜子",
        "storage_cabinet" => "储物柜",
        "table" => "桌子",
        "sofa" => "沙发",
        "squat_toilet" => "蹲便器",
        "toilet" => "坐便器",
        "sanitary" => "卫生器具",
        "grab_bar" => "扶手",
        "stair" | "stairs" => "示意楼梯",
        "elevator" => "电梯",
        "wall" => "墙体",
        "slab" => "楼板",
        "column" => "柱",
        "door" => "门",
        "window" => "窗",
        "opening" => "洞口",
        "cad_reference" => "图纸参考线",
        "wall_detail" => "墙面细部",
        "structure_revision" => "结构修订",
        "unclassified" | "unidentified" | "未分类" => "其他结构",
        _ => category,
    }
}

pub(super) fn requirements(proposal: &Value, repository: &Path) -> Option<FirstUseReviewFact> {
    requirements_with_openings(proposal, None, repository)
}

pub(super) fn requirements_for_scope(
    proposal: &Value,
    scope: &ProjectScopeFact,
    repository: &Path,
) -> Option<FirstUseReviewFact> {
    let Some(completion) = &scope.opening_completion_binding else {
        return requirements(proposal, repository);
    };
    let needs_completion_sources = proposal["component_first_use_requirements"]["required"]
        .as_array()
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("source_sha256").is_some_and(Value::is_null)
                    || item
                        .get("source_geometry_sha256")
                        .is_some_and(Value::is_null)
            })
        });
    if !needs_completion_sources {
        return requirements(proposal, repository);
    }
    let evidence = &proposal["opening_completion_source"];
    let path = text(evidence, "path").ok()?;
    let certificate_sha = hash(&evidence["sha256"])?;
    let operations = proposal["draft"]["operations"].as_array()?;
    if evidence["contract"] != "project-opening-completion/1"
        || proposal["source_dependencies"][&path] != certificate_sha
        || operations.is_empty()
        || operations.iter().any(|operation| {
            operation["op"] != "set"
                || operation["collection"] != "openings"
                || operation["field"] != "assets"
                || operation["target_id"]
                    .as_str()
                    .is_none_or(|id| !completion.opening_ids.iter().any(|allowed| allowed == id))
        })
    {
        return None;
    }
    let operation_ids: BTreeSet<String> = operations
        .iter()
        .map(|operation| operation["target_id"].as_str().map(str::to_owned))
        .collect::<Option<_>>()?;
    if operation_ids.len() != operations.len() {
        return None;
    }
    requirements_with_openings(proposal, Some(&operation_ids), repository)
}

fn requirements_with_openings(
    proposal: &Value,
    opening_ids: Option<&BTreeSet<String>>,
    repository: &Path,
) -> Option<FirstUseReviewFact> {
    let value = &proposal["component_first_use_requirements"];
    if !proposal["structure_revision_source"].is_null()
        && proposal["candidate_sha256"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        && value["contract"] != "component-first-use/structure-revision-1"
    {
        return None;
    }
    if value.is_null() {
        return Some(FirstUseReviewFact {
            status: "unknown".into(),
            review_set_sha256: None,
            required_count: 0,
            blocked_count: 0,
            completed: None,
            total: None,
            job_id: None,
            dimensions: Vec::new(),
            requirements: Value::Null,
            edit_parts: None,
            structure_parts: None,
        });
    }
    let required = value["required"].as_array()?;
    if !matches!(
        value["contract"].as_str(),
        Some(
            "component-first-use/1"
                | "component-first-use/2"
                | "component-first-use/3"
                | "component-first-use/4"
                | "component-first-use/5"
                | "component-first-use/6"
                | "component-first-use/structure-revision-1"
        )
    ) || value
        .get("comparison_contract")
        .is_some_and(|contract| contract != "component-first-use-comparison/1")
        || required.len() > MAX_IMAGES
        || value["reused"].as_array()?.len() > 500
    {
        return None;
    }
    let edit = proposal
        .get("component_edit")
        .filter(|edit| !edit.is_null());
    let edit_parts = if edit.is_some() {
        component_edit::projection(proposal)?;
        Some(
            proposal["draft"]["operations"]
                .as_array()?
                .iter()
                .filter(|op| {
                    op["op"] == "add"
                        && matches!(op["collection"].as_str(), Some("solids" | "elements"))
                })
                .map(|op| serde_json::from_str(op["value_json"].as_str()?).ok())
                .collect::<Option<Vec<Value>>>()?,
        )
    } else {
        None
    };
    let plan = edit.map_or(&proposal["component_plan"], |edit| {
        &edit["plan"]["replacements"]
    });
    let structure_parts = if value["contract"] == "component-first-use/structure-revision-1" {
        let operations = proposal["draft"]["operations"].as_array()?;
        if operations.iter().any(|op| op["op"] == "set") {
            Some(project::local_glazing_review_parts(proposal, repository)?)
        } else {
            let mut parts = Vec::new();
            // Match structure_specs' candidate collection order, retaining operation
            // order inside each collection as apply_patch appends the replacements.
            for collection in ["walls", "slabs", "solids", "elements", "openings"] {
                for op in operations
                    .iter()
                    .filter(|op| op["op"] == "add" && op["collection"] == collection)
                {
                    parts.push((
                        collection.to_owned(),
                        serde_json::from_str(op["value_json"].as_str()?).ok()?,
                    ));
                }
            }
            Some(parts)
        }
    } else {
        None
    };
    let mut seen = BTreeSet::new();
    let mut components = BTreeSet::new();
    let mut reviewed_openings = BTreeSet::new();
    if value["contract"] == "component-first-use/structure-revision-1" && required.len() != 1 {
        return None;
    }
    for item in required {
        if !seen.insert(hash(&item["review_key"])?) {
            return None;
        }
        let opening_completion_item = opening_ids
            .and_then(|allowed| {
                let ids = strings(&item["component_ids"], 500)?;
                if item["source_kind"] != "cad"
                    || !matches!(item["category"].as_str(), Some("door" | "window"))
                    || item["component_id"] != item["source_instance_id"]
                    || !ids.contains(item["component_id"].as_str()?)
                    || ids.iter().any(|id| !allowed.contains(id))
                    || !item["source_sha256"].is_null()
                    || !item["source_geometry_sha256"].is_null()
                {
                    return Some(false);
                }
                let bindings = item["source_bindings"].as_array()?;
                let binding_ids: BTreeSet<_> = bindings
                    .iter()
                    .map(|binding| binding["component_id"].as_str())
                    .collect();
                Some(
                    bindings.len() == ids.len()
                        && binding_ids.len() == bindings.len()
                        && binding_ids
                            .iter()
                            .all(|id| id.is_some_and(|id| ids.contains(id)))
                        && bindings.iter().all(|binding| {
                            binding["source_instance_id"] == binding["component_id"]
                                && binding["source_sha256"].is_null()
                                && binding["source_geometry_sha256"].is_null()
                        }),
                )
            })
            .unwrap_or(false);
        if opening_completion_item {
            reviewed_openings.extend(strings(&item["component_ids"], 500)?);
        }
        for key in ["definition_sha256", "arguments_sha256"] {
            hash(&item[key])?;
        }
        if opening_completion_item {
            if !matches!(item["category"].as_str()?, "door" | "window") {
                return None;
            }
        } else {
            hash(&item["source_sha256"])?;
            hash(&item["source_geometry_sha256"])?;
        }
        for key in ["component_id", "category", "template_id"] {
            text(item, key).ok()?;
        }
        if value["contract"] == "component-first-use/structure-revision-1" {
            // Structural review has one CAD binding for a set of typed entities,
            // rather than one binding per furniture component.
            let ids = strings(&item["entity_ids"], 50000)?;
            let affected: BTreeSet<_> = structure_parts
                .as_ref()?
                .iter()
                .map(|(_, part)| part["id"].as_str().map(str::to_owned))
                .collect::<Option<_>>()?;
            let certificate = &proposal["structure_revision_source"];
            let path = text(certificate, "path").ok()?;
            let sha = hash(&certificate["sha256"])?;
            let binding = json!({"component_id":"structure.revision",
                "source_instance_id":"structure.revision",
                "source_sha256":item["source_sha256"],
                "source_geometry_sha256":item["source_geometry_sha256"]});
            let arguments = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&json!({
                "certificate":certificate,"base":proposal["base_sha256"]}))
                    .ok()?
                )
            );
            let key = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&json!({
                "component_id":"structure.revision", "source_instance_id":"structure.revision",
                "source_sha256":item["source_sha256"],
                "source_geometry_sha256":item["source_geometry_sha256"],
                "definition":item["definition_sha256"],"arguments":arguments}))
                    .ok()?
                )
            );
            if required.len() != 1
                || !value["reused"].as_array()?.is_empty()
                || item["category"] != "structure_revision"
                || item["template_id"] != "typed.structure_revision"
                || item["component_id"] != "structure.revision"
                || item["source_instance_id"] != "structure.revision"
                || item["source_kind"] != "cad"
                || strings(&item["component_ids"], 50000)? != ids
                || ids != affected
                || item["source_bindings"] != json!([binding])
                || certificate["contract"] != "cad-structure-revision/1"
                || proposal["source_dependencies"][&path] != sha
                || item["arguments_sha256"] != arguments
                || item["review_key"] != key
            {
                return None;
            }
            continue;
        }
        let kind = item["source_kind"].as_str()?;
        if !matches!(kind, "cad" | "user_request")
            || (kind == "cad" && text(item, "source_instance_id").is_err())
            || (kind == "user_request" && !item["source_instance_id"].is_null())
        {
            return None;
        }
        let ids = strings(&item["component_ids"], 500)?;
        if !ids.contains(item["component_id"].as_str()?) || !components.is_disjoint(&ids) {
            return None;
        }
        components.extend(ids.clone());
        let bindings = item["source_bindings"].as_array()?;
        if bindings.len() != ids.len() {
            return None;
        }
        let directional = matches!(
            value["contract"].as_str(),
            Some("component-first-use/5" | "component-first-use/6")
        );
        let template = plan["templates"]
            .as_array()
            .and_then(|templates| templates.iter().find(|t| t["id"] == item["template_id"]));
        let direction = if directional
            && (item.get("source_direction").is_some()
                || (kind == "cad"
                    && template.is_some_and(|t| {
                        matches!(
                            t["metadata"]["template_kind"].as_str(),
                            Some("schematic_extrusion" | "cad_minimal_form")
                        )
                    }))) {
            let direction = item["source_direction"].as_array()?;
            if direction.len() != 3 {
                return None;
            }
            let x = direction[0].as_bool()?;
            let y = direction[1].as_bool()?;
            let rotation = direction[2].as_f64()?;
            if !rotation.is_finite() || !(0.0..360.0).contains(&rotation) || kind != "cad" {
                return None;
            }
            Some((x, y, rotation))
        } else {
            None
        };
        // V4+ opening style groups retain each aperture's geometry hash. The
        // representative must still match, and project() binds every member to
        // the frozen requirements and the exact rendered review manifest.
        let opening_style_group = matches!(
            value["contract"].as_str(),
            Some("component-first-use/4" | "component-first-use/5" | "component-first-use/6")
        ) && kind == "cad"
            && matches!(item["category"].as_str(), Some("door" | "window"))
            && hash(&item["style_signature_sha256"]).is_some();
        let mut bound = BTreeSet::new();
        for binding in bindings {
            let id = text(binding, "component_id").ok()?;
            if !ids.contains(&id) || !bound.insert(id) {
                return None;
            }
            if let Some((mirror_x, mirror_y, rotation)) = direction {
                // Bind every member to its CAD direction. An explicit edit may
                // change its pose, but cannot change the sealed source identity.
                let mut placements = plan["components"]
                    .as_array()?
                    .iter()
                    .filter(|c| c["component_id"] == binding["component_id"]);
                let placement = placements.next()?;
                if placements.next().is_some()
                    || placement["template_id"] != item["template_id"]
                    || placement["source_instance_id"] != binding["source_instance_id"]
                {
                    return None;
                }
                let basis = placement["placement_basis"].as_str()?;
                if basis != "source" && !(edit.is_some() && basis == "user_request") {
                    return None;
                }
                let direction_owner = if let Some(edit) = edit {
                    let source_id = binding["source_instance_id"].as_str()?;
                    let source = edit["sources"].get(source_id)?;
                    if source["source_id"] != source_id
                        || source["component_id"] != binding["component_id"]
                        || source["source_sha256"] != binding["source_sha256"]
                        || source["geometry_sha256"] != binding["source_geometry_sha256"]
                    {
                        return None;
                    }
                    let scale = placement["scale"].as_array()?;
                    let origin = placement["origin"].as_array()?;
                    if scale.len() != 3
                        || origin.len() != 3
                        || scale
                            .iter()
                            .chain(origin)
                            .any(|v| v.as_f64().is_none_or(|n| !n.is_finite()))
                        || placement["rotation_z_deg"]
                            .as_f64()
                            .is_none_or(|n| !n.is_finite())
                    {
                        return None;
                    }
                    let source_scale = source["scale"].as_array()?;
                    if source_scale.len() < 2
                        || (0..2).any(|i| {
                            source_scale[i].as_f64().is_none_or(|n| {
                                !n.is_finite()
                                    || (scale[i].as_f64().unwrap().abs() - n.abs()).abs() > 1e-8
                            })
                        })
                    {
                        return None;
                    }
                    if basis == "source" {
                        let angle = placement["rotation_z_deg"].as_f64()?;
                        let source_angle = source["rotation_z_deg"].as_f64()?;
                        if ((angle - source_angle + 180.0).rem_euclid(360.0) - 180.0).abs() > 1e-6
                            || (0..2).any(|i| {
                                (scale[i].as_f64().unwrap() - source_scale[i].as_f64().unwrap())
                                    .abs()
                                    > 1e-8
                            })
                        {
                            return None;
                        }
                        let source_origin = source["origin_m"].as_array()?;
                        if source_origin.len() != 2
                            || (0..2).any(|i| {
                                source_origin[i].as_f64().is_none_or(|n| {
                                    !n.is_finite()
                                        || (origin[i].as_f64().unwrap() - n).abs() > 0.001
                                })
                            })
                        {
                            return None;
                        }
                    }
                    source
                } else {
                    placement
                };
                let scale = direction_owner["scale"].as_array()?;
                if !(2..=3).contains(&scale.len()) || (edit.is_none() && scale.len() != 3) {
                    return None;
                }
                let x = scale[0].as_f64()?;
                let y = scale[1].as_f64()?;
                let angle = direction_owner["rotation_z_deg"].as_f64()?;
                let angle = (angle.rem_euclid(360.0) * 1_000_000.0).round_ties_even() / 1_000_000.0;
                if !x.is_finite()
                    || !y.is_finite()
                    || !angle.is_finite()
                    || (x < 0.0) != mirror_x
                    || (y < 0.0) != mirror_y
                    || angle % 360.0 != rotation
                {
                    return None;
                }
            }
            for key in ["source_sha256", "source_geometry_sha256"] {
                if opening_completion_item {
                    if !binding[key].is_null() {
                        return None;
                    }
                } else {
                    hash(&binding[key])?;
                }
                if binding[key] != item[key]
                    && !(opening_style_group
                        && key == "source_geometry_sha256"
                        && binding["component_id"] != item["component_id"])
                {
                    return None;
                }
            }
            if (kind == "cad" && text(binding, "source_instance_id").is_err())
                || (kind == "user_request" && !binding["source_instance_id"].is_null())
            {
                return None;
            }
            if binding["component_id"] == item["component_id"]
                && binding["source_instance_id"] != item["source_instance_id"]
            {
                return None;
            }
        }
    }
    if opening_ids
        .is_some_and(|allowed| !reviewed_openings.is_empty() && &reviewed_openings != allowed)
    {
        return None;
    }
    Some(FirstUseReviewFact {
        status: if required.is_empty() {
            "not_required"
        } else {
            "blocked"
        }
        .into(),
        review_set_sha256: None,
        required_count: required.len(),
        blocked_count: 0,
        completed: None,
        total: None,
        job_id: None,
        dimensions: Vec::new(),
        requirements: value.clone(),
        edit_parts,
        structure_parts,
    })
}

fn local_png(path: &Path) -> bool {
    let name = path.to_string_lossy();
    path.is_absolute()
        && !name.contains("://")
        && !name.contains('\0')
        && !name.starts_with(r"\\")
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
}

fn image_fact(
    value: &Value,
    binding: &ProposalBinding,
    set: Option<String>,
    key: String,
    label: String,
    view: ReviewImageView,
    required: bool,
) -> Option<ReviewImageFact> {
    let path = PathBuf::from(text(value, "image_path").ok()?);
    if !local_png(&path) {
        return None;
    }
    let dimensions = {
        let dims = value["dimensions_px"].as_array()?;
        if dims.len() != 2 {
            return None;
        }
        let w = u32::try_from(dims[0].as_u64()?).ok()?;
        let h = u32::try_from(dims[1].as_u64()?).ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        Some([w, h])
    };
    Some(ReviewImageFact {
        binding: ReviewImageBinding {
            proposal: binding.clone(),
            review_set_sha256: set,
            review_key: key,
            image_sha256: hash(&value["image_sha256"])?.into(),
            web_review: None,
            path,
            dimensions,
        },
        label,
        view,
        required,
        file_state: ImageFileState::Pending,
        externally_opened: false,
        notice: None,
        detail: None,
    })
}

fn web_package(
    value: &Value,
    image: &ReviewImageBinding,
    url_hash: String,
) -> Option<crate::review_web::WebReviewPackage> {
    let html_path = PathBuf::from(text(value, "html_path").ok()?);
    let name = html_path.to_string_lossy();
    if !html_path.is_absolute()
        || name.contains("://")
        || name.contains('\0')
        || name.starts_with(r"\\")
        || !html_path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("html"))
    {
        return None;
    }
    if value.get("png_path").is_some_and(|path| {
        path.as_str()
            .is_none_or(|path| Path::new(path) != image.path)
    }) || value
        .get("url_hash")
        .is_some_and(|value| value.as_str() != Some(url_hash.as_str()))
    {
        return None;
    }
    let data_sha256 = hash(&value["data_sha256"])?;
    let manifest_sha256 = hash(&value["manifest_sha256"])?;
    let artifact_sha256 = hash(&value["artifact_sha256"])?;
    if let Some(manifest) = value.get("manifest") {
        if manifest["data_sha256"] != data_sha256
            || manifest["artifact_sha256"] != artifact_sha256
            || manifest["image_sha256"] != image.image_sha256
        {
            return None;
        }
    }
    Some(crate::review_web::WebReviewPackage {
        html_path,
        png_path: image.path.clone(),
        data_sha256: data_sha256.into(),
        manifest_sha256: Some(manifest_sha256.into()),
        artifact_sha256: Some(artifact_sha256.into()),
        url_hash: Some(url_hash),
        first_use_comparison: None,
    })
}

fn web_detail(review_type: &str, engine: &str, image: &ReviewImageBinding) -> String {
    let package = image.web_review.as_ref().expect("projected web package");
    let mut detail = format!(
        "复核类型：{review_type}\n渲染引擎：{}\nPNG SHA256：{}\n数据 SHA256：{}\nmanifest SHA256：{}\nartifact SHA256：{}",
        crate::state::display_text(engine),
        image.image_sha256,
        package.data_sha256,
        package.manifest_sha256.as_deref().unwrap_or("未提供"),
        package.artifact_sha256.as_deref().unwrap_or("未提供"),
    );
    if let Some(set) = &image.review_set_sha256 {
        detail.push_str(&format!("\nreview_set SHA256：{set}"));
    }
    detail
}

// These are observations of unfinished review/recovery work, never approval proof.
// Job states come from artifact_review_images; recovery summaries come from
// domain_tools._approval_review and can also arrive in a first-use review slot.
const PENDING_REVIEW_STATUSES: &[&str] = &[
    "queued",
    "running",
    "building",
    "preparing",
    "unknown",
    "not_started",
    "failed",
    "cancelled",
    "stalled",
    "restore_applied_result",
    "proposal_not_approvable",
    "preview_required",
    "candidate_failed",
    "candidate_outcome_unknown",
    "restore_candidate_review",
];

pub(super) fn project(
    value: &Value,
    proposal: &ProposalBinding,
    previous: &FirstUseReviewFact,
) -> Option<(FirstUseReviewFact, Vec<ReviewImageFact>)> {
    let review = value.get("component_first_use_review")?;
    let mut result = previous.clone();
    let status = review["status"].as_str()?;
    let pending = PENDING_REVIEW_STATUSES.contains(&status);
    if !pending && !matches!(status, "ready" | "blocked" | "not_required") {
        return None;
    }
    result.completed = review["completed"].as_u64().map(|value| value as usize);
    result.total = review["total"].as_u64().map(|value| value as usize);
    result.job_id = review["job_id"].as_str().map(str::to_owned);
    if result
        .completed
        .zip(result.total)
        .is_some_and(|(completed, total)| completed > total || (total == 0 && status == "ready"))
    {
        return None;
    }
    if pending {
        result.status = status.to_owned();
        result.review_set_sha256 = None;
        result.blocked_count = 0;
        result.dimensions.clear();
        return Some((result, Vec::new()));
    }
    if previous.requirements.is_null() {
        return Some((result, Vec::new()));
    }
    result.review_set_sha256 = None;
    result.dimensions.clear();
    if review["status"] == "blocked" || review["current"] == false {
        result.status = "blocked".into();
        result.blocked_count = review["manifest"]["blocked"].as_array().map_or(1, Vec::len);
        return Some((result, Vec::new()));
    }
    if previous.required_count == 0 {
        if status != "not_required" {
            return None;
        }
        result.status = "not_required".into();
        result.blocked_count = 0;
        return Some((result, Vec::new()));
    }
    if review["status"] != "ready" || review["current"] != true {
        return None;
    }
    let top_level_set = hash(&review["review_set_sha256"]);
    let set = top_level_set
        .or_else(|| hash(&review["delivery"]["review_set_sha256"]))?
        .to_owned();
    let manifest = &review["manifest"];
    if manifest["contract"] != "component-first-use-review/1"
        || manifest["proposal_id"] != proposal.proposal_id
        || manifest["revision"] != proposal.revision
        || manifest["prepare_token"] != proposal.prepare_token
        || manifest["base_sha256"] != proposal.base_sha256
        || manifest["candidate_sha256"] != proposal.candidate_sha256
        || manifest["input_sha256"] != proposal.candidate_sha256
        || manifest["requirements_sha256"]
            != format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&previous.requirements).ok()?)
            )
    {
        return None;
    }
    result.blocked_count = manifest["blocked"].as_array().map_or(0, Vec::len);
    text(manifest, "build_id").ok()?;
    let blend_sha256 = match manifest.get("blend_sha256") {
        Some(value) => Some(hash(value)?),
        None => None,
    };
    let glb_sha256 = match manifest.get("glb_sha256") {
        Some(value) => Some(hash(value)?),
        None => None,
    };
    if blend_sha256.is_none() && glb_sha256.is_none() {
        return None;
    }
    let required = previous.requirements["required"].as_array()?;
    let items = manifest["items"].as_array()?;
    if items.len() != required.len() {
        return None;
    }
    if top_level_set.is_none() || !review["delivery"].is_null() {
        let delivery = &review["delivery"];
        let delivery_key = hash(&delivery["review_key"])?;
        let delivery_image = hash(&delivery["image_sha256"])?;
        if delivery["delivery"] != "attached_png"
            || delivery["review_set_sha256"] != set
            || !items.iter().any(|item| {
                item["review_key"] == delivery_key
                    && item["images"].as_array().is_some_and(|images| {
                        images
                            .iter()
                            .any(|image| image["image_sha256"] == delivery_image)
                    })
            })
        {
            return None;
        }
    }
    let mut seen = BTreeSet::new();
    let mut images = Vec::new();
    for item in items {
        let key = hash(&item["review_key"])?;
        if !seen.insert(key) {
            return None;
        }
        let requirement = required.iter().find(|r| r["review_key"] == key)?;
        if requirement
            .as_object()?
            .iter()
            .any(|(k, v)| item.get(k) != Some(v))
        {
            return None;
        }
        if previous.requirements.get("comparison_contract").is_some()
            && item["renderer"]["engine"] != "REVIEW_WEB_3D"
        {
            return None;
        }
        match item["renderer"]["engine"].as_str() {
            Some("BLENDER_WORKBENCH") => {
                blend_sha256?;
                hash(&item["renderer"]["script_sha256"])?;
                if item["renderer"]["blender_version"].is_null() {
                    return None;
                }
            }
            // Openings, schematic fixtures and server schematic extrusions use a fast
            // sealed 2-D overlay; the modeler decides eligibility before sealing, so Core
            // does not keep its own (inevitably stale) category list.
            Some("PIL_2D_OVERLAY") if blend_sha256.is_some() => {}
            Some("REVIEW_WEB_3D") if item["review_web"].is_object() => {
                if glb_sha256
                    .is_some_and(|sha| hash(&item["review_web"]["artifact_sha256"]) != Some(sha))
                {
                    return None;
                }
            }
            _ => return None,
        }
        hash(&item["cad_evidence_sha256"])?;
        let edit_part_limit = if previous
            .edit_parts
            .as_ref()
            .is_some_and(|parts| parts.iter().any(|part| part.get("boundary").is_some()))
        {
            300
        } else {
            64
        };
        strings(
            &item["part_ids"],
            if item["category"] == "structure_revision" {
                50000
            } else {
                edit_part_limit
            },
        )?;
        let dimensions = &item["dimensions"];
        let assumptions_valid = if let Some(reference) = dimensions.get("assumed_ref") {
            reference
                .as_object()
                .is_some_and(|fields| fields.len() == 2)
                && hash(&reference["sha256"]).is_some()
                && reference["count"].as_u64().is_some()
                && dimensions.get("assumed").is_none()
                && dimensions["assumed_rows"].as_array().is_some_and(|rows| {
                    rows.iter()
                        .all(|row| row.as_str().is_some_and(|row| !row.is_empty()))
                })
        } else {
            dimensions["assumed"].is_array()
        };
        if !dimensions.is_object()
            || !assumptions_valid
            || dimensions.get("parameters").is_none()
            || dimensions["model_xyz_m"].as_array()?.len() != 3
            || (item["source_kind"] == "cad" && dimensions["cad_xy_m"].as_array()?.len() != 2)
        {
            return None;
        }
        if let Some(parts) = &previous.structure_parts {
            let rendered = dimensions["parts"].as_array()?;
            if rendered.len() != parts.len() {
                return None;
            }
            let mut seen = BTreeSet::new();
            for part in rendered {
                if !seen.insert(part["id"].as_str()?) {
                    return None;
                }
            }
            let mut ordered = Vec::new();
            for (_, expected) in parts {
                let actual = rendered.iter().find(|part| part["id"] == expected["id"])?;
                for (key, value) in expected.as_object()? {
                    if key == "metadata" {
                        for (key, value) in value.as_object()? {
                            // apply_patch replaces only these provenance stamps.
                            if !matches!(
                                key.as_str(),
                                "patch_id" | "patch_reason" | "value_basis" | "decision_id"
                            ) && actual["metadata"].get(key) != Some(value)
                            {
                                return None;
                            }
                        }
                    } else if actual.get(key) != Some(value) {
                        return None;
                    }
                }
                ordered.push(actual);
            }
            let mut encoded = Vec::new();
            ordered
                .serialize(&mut serde_json::Serializer::with_formatter(
                    &mut encoded,
                    PythonGeometryFormatter,
                ))
                .ok()?;
            if item["definition_sha256"] != format!("{:x}", Sha256::digest(&encoded)) {
                return None;
            }
            let mesh_ids = strings(&item["part_ids"], 50000)?;
            if !seen.iter().all(|id| mesh_ids.contains(*id))
                || mesh_ids.iter().any(|id| {
                    !seen.contains(id.as_str())
                        && !parts.iter().any(|(collection, part)| {
                            collection == "openings"
                                && !part["assets"].is_null()
                                && part["id"]
                                    .as_str()
                                    .is_some_and(|owner| id.starts_with(&format!("{owner}.")))
                        })
                })
            {
                return None;
            }
        }
        if let Some(parts) = &previous.edit_parts {
            // The CAD direction describes the source. The rendered representative
            // must separately retain the actual edited shape and pose from the draft.
            let expected: Vec<_> = parts
                .iter()
                .filter(|part| part["metadata"]["component_id"] == item["component_id"])
                .collect();
            let rendered = dimensions["parts"].as_array()?;
            if expected.is_empty() || rendered.len() != expected.len() {
                return None;
            }
            let mut seen_parts = BTreeSet::new();
            for part in rendered {
                let id = part["id"].as_str()?;
                let original = expected.iter().find(|old| old["id"] == id)?;
                let geometry: &[&str] = if original.get("boundary").is_some() {
                    &[
                        "role",
                        "boundary",
                        "holes",
                        "base_z",
                        "height",
                        "stair_soffit",
                        "stair_top",
                        "source_ids",
                    ]
                } else {
                    &[
                        "primitive_id",
                        "arguments",
                        "origin",
                        "rotation_z_deg",
                        "scale",
                    ]
                };
                if !seen_parts.insert(id)
                    || geometry
                        .iter()
                        .any(|key| part.get(*key) != original.get(*key))
                    || original.get("boundary").is_some()
                        && original["metadata"]
                            .as_object()?
                            .iter()
                            .any(|(key, value)| {
                                !matches!(
                                    key.as_str(),
                                    "patch_id" | "patch_reason" | "value_basis" | "decision_id"
                                ) && part["metadata"].get(key) != Some(value)
                            })
                {
                    return None;
                }
            }
            if strings(&item["part_ids"], edit_part_limit)?
                != seen_parts.iter().map(|id| (*id).to_owned()).collect()
            {
                return None;
            }
        }
        let kind = if item["source_kind"] == "user_request" {
            "用户请求（无CAD）"
        } else {
            "图纸原线稿"
        };
        let cad_dimensions = if item["source_kind"] == "user_request" {
            "不适用（无CAD）".into()
        } else {
            dimensions["cad_xy_m"].to_string()
        };
        let mut display_lines = vec![format!(
            "{} · {} · 图纸尺寸（米）{} · 三维尺寸（米）{}",
            dimensions["category_label"]
                .as_str()
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| category_label(item["category"].as_str().unwrap_or_default())),
            kind,
            cad_dimensions,
            dimensions["model_xyz_m"]
        )];
        if let Some(direction) = item.get("source_direction") {
            display_lines.push(format!("CAD来源方向（镜像X、镜像Y、角度）：{direction}"));
        }
        if let Some(metadata) = dimensions["parameters"].get("metadata") {
            metadata.as_object()?;
            for (key, label, limit) in [
                ("template_kind", "构形类型", 128),
                ("form_version", "构形版本", 128),
                ("category_family", "类别族", 256),
                ("geometry_basis", "几何依据", 2000),
                ("diagnostics", "构形诊断", 4000),
                ("reason", "构形说明", 4000),
            ] {
                if let Some(value) = metadata.get(key) {
                    let value = value.as_str()?;
                    if value.chars().count() > limit || (value.is_empty() && key != "diagnostics") {
                        return None;
                    }
                    if !value.is_empty() {
                        display_lines
                            .push(format!("{label}：{}", crate::state::display_text(value)));
                    }
                }
            }
        }
        if let Some(parameters) = dimensions["display_parameters"].as_array() {
            let mut seen = BTreeSet::new();
            for parameter in parameters {
                if !seen.insert(parameter.to_string()) {
                    continue;
                }
                let label = parameter["label"].as_str().unwrap_or("其他参数");
                let basis = parameter["basis_label"].as_str().unwrap_or("见封存依据");
                let unit = parameter["unit_label"].as_str().unwrap_or("");
                let value = match &parameter["value"] {
                    Value::String(value) => value.clone(),
                    Value::Number(_) | Value::Bool(_) | Value::Array(_) => {
                        parameter["value"].to_string()
                    }
                    _ => "参数值详见封存清单".into(),
                };
                display_lines.push(format!("{label}：{value}{unit}（{basis}）"));
            }
        } else {
            display_lines.push("参数说明未提供，详见封存清单".into());
        }
        if let Some(interpretation) = dimensions["interpretation"].as_array() {
            let mut seen = BTreeSet::new();
            for description in interpretation.iter().filter_map(Value::as_str) {
                if seen.insert(description) {
                    display_lines.push(format!("判读说明：{description}"));
                }
            }
        }
        let detail = display_lines.join("\n");
        result.dimensions.push(detail.clone());
        let item_images = item["images"].as_array()?;
        if item_images.is_empty()
            || !item_images
                .iter()
                .any(|image| image["view"] == "comparison")
        {
            return None;
        }
        for image in item_images {
            let view = image["view"].as_str()?;
            if !matches!(view, "comparison" | "overview" | "detail") || images.len() >= MAX_IMAGES {
                return None;
            }
            let mut fact = image_fact(
                image,
                proposal,
                Some(set.clone()),
                key.into(),
                format!(
                    "{} · {} · {}",
                    category_label(text(item, "category").ok()?.as_str()),
                    kind,
                    match view {
                        "comparison" if item["source_kind"] == "user_request" => "三维与尺寸对照",
                        "comparison" => "三维与原图对照",
                        "overview" => "总览",
                        _ => "局部",
                    }
                ),
                match view {
                    "comparison" => ReviewImageView::Comparison,
                    "overview" => ReviewImageView::Overview,
                    _ => ReviewImageView::Detail,
                },
                true,
            )?;
            fact.detail = Some(detail.clone());
            if let Some(web) = item.get("review_web") {
                if web["review_type"] != "component_first_use_3d"
                    || web["version"].as_str() != Some(&proposal.revision.to_string())
                {
                    return None;
                }
                fact.binding.web_review =
                    Some(web_package(web, &fact.binding, "view=perspective".into())?);
                if previous.requirements.get("comparison_contract").is_some() {
                    fact.binding.web_review.as_mut()?.first_use_comparison =
                        Some(crate::review_web::FirstUseComparison {
                            proposal_sha256: proposal.candidate_sha256.clone(),
                            cad_required: item["source_kind"] == "cad",
                            part_ids: item["part_ids"]
                                .as_array()?
                                .iter()
                                .map(|id| id.as_str().map(str::to_owned))
                                .collect::<Option<_>>()?,
                            dimensions: dimensions.clone(),
                        });
                }
                let mut web_description = format!(
                    "{detail}\n{}\nCAD 证据 SHA256：{}",
                    web_detail(
                        "component_first_use_3d",
                        item["renderer"]["engine"].as_str()?,
                        &fact.binding,
                    ),
                    item["cad_evidence_sha256"].as_str()?,
                );
                if let Some(sha) = blend_sha256 {
                    web_description.push_str(&format!("\nBLEND SHA256：{sha}"));
                }
                if let Some(sha) = glb_sha256 {
                    web_description.push_str(&format!("\nGLB SHA256：{sha}"));
                }
                fact.detail = Some(web_description);
            }
            images.push(fact);
        }
    }
    let identities: BTreeSet<_> = images
        .iter()
        .map(|image| (&image.binding.review_key, &image.binding.image_sha256))
        .collect();
    if identities.len() != images.len() {
        return None;
    }
    // The private receipt authenticates the Python-canonical manifest and its SHA.
    // Do not re-encode floating point dimensions with Rust's different JSON formatting.
    result.status = "ready".into();
    result.review_set_sha256 = Some(set);
    Some((result, images))
}

pub(super) fn project_failure_reason(value: &Value) -> String {
    let review = &value["component_first_use_review"];
    if review["status"].as_str().is_none_or(|status| {
        !PENDING_REVIEW_STATUSES.contains(&status)
            && !matches!(status, "ready" | "blocked" | "not_required")
    }) {
        return "字段 component_first_use_review.status 不是可投影的复核状态".into();
    }
    if hash(&review["review_set_sha256"]).is_none()
        && hash(&review["delivery"]["review_set_sha256"]).is_none()
    {
        return "字段 component_first_use_review.review_set_sha256（或 delivery.review_set_sha256）缺失或无效".into();
    }
    if review["manifest"].is_null() {
        return "字段 component_first_use_review.manifest 缺失".into();
    }
    if let (Some(items), Some(required)) = (
        review["manifest"]["items"].as_array(),
        value["proposal"]["component_first_use_requirements"]["required"].as_array(),
    ) {
        if items.len() != required.len() {
            return format!(
                "字段 manifest.items 数量为 {}，required 数量为 {}",
                items.len(),
                required.len()
            );
        }
    }
    "component_first_use_review manifest 字段校验失败（详见事实结构）".into()
}

pub(super) fn artifact_image(value: &Value, binding: &ProposalBinding) -> Option<ReviewImageFact> {
    let a = &value["artifact"];
    if a["candidate"] != true || a["state"] != "built" || a["current"] != true {
        return None;
    }
    let request = &a["review_image"]["request"];
    let blend_sha256 = match request.get("blend_sha256") {
        Some(value) => Some(hash(value)?),
        None => None,
    };
    let glb_sha256 = match request.get("glb_sha256") {
        Some(value) => Some(hash(value)?),
        None => None,
    };
    if request["build_id"] != a["build_id"]
        || request["proposal_id"] != binding.proposal_id
        || request["revision"] != binding.revision
        || request["prepare_token"] != binding.prepare_token
        || request["input_sha256"] != binding.candidate_sha256
        || (blend_sha256.is_none() && glb_sha256.is_none())
        || (blend_sha256.is_none() && !a["review_image"]["review_web"].is_object())
    {
        return None;
    }
    let label = if a["review_image"]["request"]["focus_m"].is_null() {
        "候选总览"
    } else {
        "候选局部"
    };
    let mut image = image_fact(
        &a["review_image"],
        binding,
        None,
        text(a, "build_id").ok()?,
        label.into(),
        if request["focus_m"].is_null() {
            ReviewImageView::Overview
        } else {
            ReviewImageView::Detail
        },
        false,
    )?;
    if let Some(web) = a["review_image"].get("review_web") {
        if web["review_type"] != "component_form_3d"
            || web["version"].as_str() != Some(&binding.revision.to_string())
            || hash(&request["glb_sha256"])? != hash(&web["artifact_sha256"])?
        {
            return None;
        }
        let view = request["view"].as_str()?;
        if !matches!(view, "perspective" | "top" | "front") {
            return None;
        }
        let mut fragment = format!("view={view}");
        if !request["focus_m"].is_null() {
            let focus = request["focus_m"].as_array()?;
            if focus.len() != 4 || view == "perspective" {
                return None;
            }
            let mut values = Vec::new();
            for value in focus {
                let number = value.as_f64()?;
                if !number.is_finite() {
                    return None;
                }
                let mut encoded = Vec::new();
                number
                    .serialize(&mut serde_json::Serializer::with_formatter(
                        &mut encoded,
                        PythonGeometryFormatter,
                    ))
                    .ok()?;
                values.push(String::from_utf8(encoded).ok()?);
            }
            if focus[0].as_f64()? >= focus[2].as_f64()?
                || focus[1].as_f64()? >= focus[3].as_f64()?
            {
                return None;
            }
            fragment.push_str("&focus=");
            fragment.push_str(&values.join(","));
        }
        image.binding.web_review = Some(web_package(web, &image.binding, fragment)?);
        let renderer = &a["review_image"]["renderer"];
        let engine = renderer
            .as_str()
            .or_else(|| renderer["engine"].as_str())
            .or_else(|| renderer["backend"].as_str())
            .unwrap_or("REVIEW_WEB_3D");
        let mut detail = web_detail("component_form_3d", engine, &image.binding);
        if let Some(sha) = blend_sha256 {
            detail.push_str(&format!("\nBLEND SHA256：{sha}"));
        }
        if let Some(sha) = glb_sha256 {
            detail.push_str(&format!("\nGLB SHA256：{sha}"));
        }
        image.detail = Some(detail);
    }
    Some(image)
}

pub async fn verify_image(binding: &ReviewImageBinding) -> Result<(), String> {
    if binding
        .web_review
        .as_ref()
        .is_some_and(|package| package.png_path != binding.path)
    {
        return Err("网页复核截图与当前图片绑定不一致".into());
    }
    if !local_png(&binding.path) {
        return Err("图片不可用：仅支持绝对本地PNG文件".into());
    }
    let path = tokio::fs::canonicalize(&binding.path)
        .await
        .map_err(|_| "图片不可用：文件不存在或不可读取")?;
    // Windows canonical paths use an extended drive prefix; UNC/device targets stay forbidden.
    let name = path.to_string_lossy();
    let path = PathBuf::from(name.strip_prefix(r"\\?\").unwrap_or(&name));
    if !local_png(&path) {
        return Err("图片不可用：目标不是本地PNG".into());
    }
    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| "图片不可用：无法打开文件")?;
    let metadata = file
        .metadata()
        .await
        .map_err(|_| "图片不可用：无法读取文件信息")?;
    if !metadata.is_file() || metadata.len() < 24 || metadata.len() > MAX_PNG_BYTES {
        return Err("图片不可用：PNG大小必须为24字节至16MiB".into());
    }
    let mut header = [0u8; 24];
    file.read_exact(&mut header)
        .await
        .map_err(|_| "图片不可用：PNG头不完整")?;
    if &header[..8] != b"\x89PNG\r\n\x1a\n" || &header[12..16] != b"IHDR" {
        return Err("图片不可用：文件不是PNG".into());
    }
    let dimensions = [
        u32::from_be_bytes(header[16..20].try_into().unwrap()),
        u32::from_be_bytes(header[20..24].try_into().unwrap()),
    ];
    if dimensions.contains(&0)
        || binding
            .dimensions
            .is_some_and(|expected| expected != dimensions)
    {
        return Err("图片不可用：PNG尺寸与回执不符".into());
    }
    let mut sha = Sha256::new();
    sha.update(header);
    let mut count = 24u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|_| "图片不可用：读取失败")?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > MAX_PNG_BYTES {
            return Err("图片不可用：PNG超过16MiB".into());
        }
        sha.update(&buffer[..n]);
    }
    if format!("{:x}", sha.finalize()) != binding.image_sha256 {
        return Err("图片不可用：SHA256不匹配".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageOpenOutcome {
    pub file_valid: bool,
    pub result: Result<(), String>,
}

#[cfg(test)]
pub(crate) fn test_image(proposal: &ProposalBinding, build_id: &str) -> ReviewImageFact {
    ReviewImageFact {
        binding: ReviewImageBinding {
            proposal: proposal.clone(),
            review_set_sha256: None,
            review_key: build_id.into(),
            image_sha256: "a".repeat(64),
            web_review: None,
            path: std::env::temp_dir().join("verified-candidate.png"),
            dimensions: Some([1, 1]),
        },
        label: "候选总览".into(),
        view: ReviewImageView::Overview,
        required: false,
        file_state: ImageFileState::Available,
        externally_opened: false,
        notice: None,
        detail: None,
    }
}

pub async fn open_image(binding: ReviewImageBinding) -> ImageOpenOutcome {
    if let Err(reason) = verify_image(&binding).await {
        return ImageOpenOutcome {
            file_valid: false,
            result: Err(reason),
        };
    }
    let result = tokio::task::spawn_blocking(move || launch_image(&binding.path))
        .await
        .map_err(|_| "外部打开失败：看图任务中断".to_owned())
        .and_then(|result| result);
    ImageOpenOutcome {
        file_valid: true,
        result,
    }
}

#[cfg(windows)]
fn launch_image(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    let operation: Vec<u16> = "open\0".encode_utf16().collect();
    let file: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // ShellExecute receives a verified PNG as a separate file argument, never shell code.
    let result = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    } as isize;
    if result <= 32 {
        Err(format!("外部打开失败：系统看图程序返回 {result}"))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn launch_image(_path: &Path) -> Result<(), String> {
    Err("外部打开尚未支持此平台".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_geometry_digest_preserves_python_float_spelling() {
        let text = "[1e-06,1e-05,0.0001,1.0,1000000000000000.0,1e+16,1e+20,-0.0,0.19999999999999574,2.099999999999998,1.2345678901234567e-06]";
        let value: Value = serde_json::from_str(text).unwrap();
        let mut encoded = Vec::new();
        value
            .serialize(&mut serde_json::Serializer::with_formatter(
                &mut encoded,
                PythonGeometryFormatter,
            ))
            .unwrap();
        assert_eq!(String::from_utf8(encoded).unwrap(), text);
    }
}
