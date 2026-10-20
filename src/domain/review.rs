//! Proposal-bound PNG evidence and Core-owned local file presentation.
use super::*;
use std::collections::BTreeSet;
use tokio::io::AsyncReadExt;

pub const MAX_IMAGES: usize = 128;
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
        "unclassified" | "unidentified" | "未分类" => "其他结构",
        _ => category,
    }
}

pub(super) fn requirements(proposal: &Value) -> Option<FirstUseReviewFact> {
    let value = &proposal["component_first_use_requirements"];
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
        )
    ) || required.len() > MAX_IMAGES
        || value["reused"].as_array()?.len() > 500
    {
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut components = BTreeSet::new();
    for item in required {
        if !seen.insert(hash(&item["review_key"])?) {
            return None;
        }
        for key in [
            "definition_sha256",
            "arguments_sha256",
            "source_sha256",
            "source_geometry_sha256",
        ] {
            hash(&item[key])?;
        }
        for key in ["component_id", "category", "template_id"] {
            text(item, key).ok()?;
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
        let directional = value["contract"] == "component-first-use/5";
        let template = proposal["component_plan"]["templates"]
            .as_array()
            .and_then(|templates| templates.iter().find(|t| t["id"] == item["template_id"]));
        let direction = if directional
            && (item.get("source_direction").is_some()
                || (kind == "cad"
                    && template
                        .is_some_and(|t| t["metadata"]["template_kind"] == "schematic_extrusion")))
        {
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
        // V4/V5 opening style groups retain each aperture's geometry hash. The
        // representative must still match, and project() binds every member to
        // the frozen requirements and the exact rendered review manifest.
        let opening_style_group = matches!(
            value["contract"].as_str(),
            Some("component-first-use/4" | "component-first-use/5")
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
                // Source-bound schematic placements carry the CAD transform in
                // the sealed proposal. Check every member, not just the image's
                // representative, so distinct directions cannot share a group.
                let mut placements = proposal["component_plan"]["components"]
                    .as_array()?
                    .iter()
                    .filter(|c| c["component_id"] == binding["component_id"]);
                let placement = placements.next()?;
                if placements.next().is_some()
                    || placement["template_id"] != item["template_id"]
                    || placement["source_instance_id"] != binding["source_instance_id"]
                    || placement["placement_basis"] != "source"
                {
                    return None;
                }
                let scale = placement["scale"].as_array()?;
                if scale.len() != 3 {
                    return None;
                }
                let x = scale[0].as_f64()?;
                let y = scale[1].as_f64()?;
                let angle = placement["rotation_z_deg"].as_f64()?;
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
                hash(&binding[key])?;
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

pub(super) fn project(
    value: &Value,
    proposal: &ProposalBinding,
    previous: &FirstUseReviewFact,
) -> Option<(FirstUseReviewFact, Vec<ReviewImageFact>)> {
    let review = value.get("component_first_use_review")?;
    let mut result = previous.clone();
    if previous.requirements.is_null() {
        return Some((result, Vec::new()));
    }
    if previous.required_count == 0 {
        if review["status"] != "not_required" {
            return None;
        }
        return Some((result, Vec::new()));
    }
    result.completed = review["completed"].as_u64().map(|value| value as usize);
    result.total = review["total"].as_u64().map(|value| value as usize);
    result.job_id = review["job_id"].as_str().map(str::to_owned);
    if result
        .completed
        .zip(result.total)
        .is_some_and(|(completed, total)| total == 0 || completed > total)
    {
        return None;
    }
    if review["current"] == true && matches!(review["status"].as_str(), Some("running" | "stalled"))
    {
        result.status = review["status"].as_str()?.to_owned();
        result.review_set_sha256 = None;
        result.blocked_count = 0;
        result.dimensions.clear();
        return Some((result, Vec::new()));
    }
    result.review_set_sha256 = None;
    result.dimensions.clear();
    if review["status"] == "blocked" || review["current"] == false {
        result.status = "blocked".into();
        result.blocked_count = review["manifest"]["blocked"].as_array().map_or(1, Vec::len);
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
    hash(&manifest["blend_sha256"])?;
    let required = previous.requirements["required"].as_array()?;
    let items = manifest["items"].as_array()?;
    if items.len() != required.len() {
        return None;
    }
    if top_level_set.is_none() {
        let delivery = &review["delivery"];
        let delivery_key = hash(&delivery["review_key"])?;
        let delivery_image = hash(&delivery["image_sha256"])?;
        if delivery["delivery"] != "attached_png"
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
        match item["renderer"]["engine"].as_str() {
            Some("BLENDER_WORKBENCH") => {
                hash(&item["renderer"]["script_sha256"])?;
                if item["renderer"]["blender_version"].is_null() {
                    return None;
                }
            }
            // Openings, schematic fixtures and server schematic extrusions use a fast
            // sealed 2-D overlay; the modeler decides eligibility before sealing, so Core
            // does not keep its own (inevitably stale) category list.
            Some("PIL_2D_OVERLAY") => {}
            _ => return None,
        }
        hash(&item["cad_evidence_sha256"])?;
        strings(&item["part_ids"], 64)?;
        let dimensions = &item["dimensions"];
        if !dimensions.is_object()
            || dimensions.get("assumed").is_none()
            || dimensions.get("parameters").is_none()
            || dimensions["model_xyz_m"].as_array()?.len() != 3
            || (item["source_kind"] == "cad" && dimensions["cad_xy_m"].as_array()?.len() != 2)
        {
            return None;
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
    if !matches!(
        review["status"].as_str(),
        Some("ready" | "blocked" | "running" | "stalled" | "not_required")
    ) {
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
    if request["build_id"] != a["build_id"]
        || request["proposal_id"] != binding.proposal_id
        || request["revision"] != binding.revision
        || request["prepare_token"] != binding.prepare_token
        || request["input_sha256"] != binding.candidate_sha256
        || hash(&request["blend_sha256"]).is_none()
    {
        return None;
    }
    let label = if a["review_image"]["request"]["focus_m"].is_null() {
        "候选总览"
    } else {
        "候选局部"
    };
    image_fact(
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
    )
}

pub async fn verify_image(binding: &ReviewImageBinding) -> Result<(), String> {
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
