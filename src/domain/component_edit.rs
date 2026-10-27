//! Project exact selected edit evidence; opaque model summaries cannot supply edit scope.
use super::*;
use std::collections::BTreeSet;

/// Matches the modeler's `selected_component_ids` cap (max_length=200).
const MAX_EDIT_COMPONENTS: usize = 200;

fn ids(value: &Value) -> Option<BTreeSet<String>> {
    let values = value.as_array()?;
    if values.len() > MAX_EDIT_COMPONENTS {
        return None;
    }
    let mut result = BTreeSet::new();
    for value in values {
        let id = value.as_str()?;
        if id.is_empty() || id.len() > 256 || !result.insert(id.to_owned()) {
            return None;
        }
    }
    Some(result)
}

pub(super) fn projection(proposal: &Value) -> Option<Vec<String>> {
    let proof = &proposal["component_edit"];
    if proof["contract"] != "selected-component-edit/v1"
        || !proposal["component_plan"].is_null()
        || !proposal["structural_source"].is_null()
        || !proposal["assembly_source"].is_null()
        || !proposal["drawing_review"].is_null()
        || !proof["source_state"].is_object()
        || !proof["removal_state"].is_object()
        || !proof["sources"].is_object()
        || proof["feedback"] != proposal["feedback"]
        || proof["feedback"]["action"] != "revise"
    {
        return None;
    }
    let selected = ids(&proof["plan"]["selected_component_ids"])?;
    if selected.is_empty() || ids(&proof["feedback"]["object_ids"])? != selected {
        return None;
    }
    let removed = ids(&proof["plan"]["remove_component_ids"])?;
    let mut outcomes = removed.clone();
    let mut direct = BTreeSet::new();
    let mut connections = BTreeMap::new();
    let mut escalator_parts = BTreeSet::new();
    if let Some(values) = proof["plan"]["replacements"]["connections"].as_array() {
        for connection in values {
            let id = text(connection, "component_id").ok()?;
            if !outcomes.insert(id.clone())
                || connection["mode"] != "single_floor_schematic"
                || !matches!(connection["kind"].as_str(), Some("stair" | "shaft"))
                || connection["kind"] == "stair" && connection["cad_completion"] != true
            {
                return None;
            }
            if connection["circulation_type"] == "escalator" {
                let flights = connection["flights"].as_array()?;
                if connection["kind"] != "stair" || flights.is_empty() || flights.len() > 100 {
                    return None;
                }
                for flight in 1..=flights.len() {
                    for segment in 1..=3 {
                        escalator_parts
                            .insert(format!("{id}.flight.{flight:02}.step.{segment:03}"));
                    }
                }
            }
            connections.insert(id, connection);
        }
    } else if !proof["plan"]["replacements"]["connections"].is_null() {
        return None;
    }
    if proof["plan"]["replacements"]["openings"]
        .as_array()
        .is_some_and(|values| !values.is_empty())
        || proposal["scope"]["protected_ids"]
            .as_array()?
            .contains(&proposal["scope"]["floor_id"])
    {
        return None;
    }
    for (key, list) in [
        ("components", &proof["plan"]["replacements"]),
        ("direct_components", &proof["plan"]),
    ] {
        for component in list[key].as_array()? {
            let id = text(component, "component_id").ok()?;
            if !outcomes.insert(id.clone()) {
                return None;
            }
            if key == "direct_components" {
                direct.insert(id);
            }
        }
    }
    if selected != outcomes {
        return None;
    }
    let baseline = proof["selected_parts"].as_object()?;
    if baseline.keys().cloned().collect::<BTreeSet<_>>() != selected {
        return None;
    }
    let mut old_parts = BTreeMap::new();
    let mut refs = BTreeMap::new();
    let mut rows = vec![
        format!(
            "已应用组件编辑 · 选中 {} · 整组件删除 {} · 具体版本绑定",
            selected.len(),
            removed.len()
        ),
        format!(
            "用户编辑意图 {}",
            bounded_text(&proof["feedback"], "text", 4000)?
        ),
    ];
    for (component, parts) in baseline {
        let parts = parts.as_array()?;
        let vertical = connections.contains_key(component);
        if parts.is_empty() || parts.len() > if vertical { 2000 } else { 64 } {
            return None;
        }
        let mut source_refs = BTreeSet::new();
        for part in parts {
            let id = text(part, "id").ok()?;
            if (if vertical {
                !matches!(
                    part["role"].as_str()?,
                    "stair_step" | "landing" | "partition" | "shaft_wall"
                ) || part["metadata"]["relation_validator"] != "vertical-schematic/1"
                    || part["metadata"]["component_id"] != *component
            } else {
                !matches!(part["role"].as_str()?, "furniture" | "fixture" | "sanitary")
            }) || old_parts
                .insert(id.clone(), (component.clone(), part.clone()))
                .is_some()
                || proposal["scope"]["protected_ids"]
                    .as_array()
                    .is_some_and(|protected| protected.iter().any(|p| p == &id))
            {
                return None;
            }
            for reference in part["source_ids"].as_array()? {
                source_refs.insert(reference.as_str()?.to_owned());
            }
            if vertical {
                let source = part["metadata"]["source_instance_id"].as_str()?;
                let evidence = &proof["sources"][source];
                if evidence["source_id"] != source
                    || evidence["component_id"] != *component
                    || evidence["floor_id"] != proposal["scope"]["floor_id"]
                    || !part["source_ids"].as_array()?.contains(&json!(source))
                    || connections.get(component)?["source_instance_id"] != source
                    || ["source_sha256", "geometry_sha256"].iter().any(|key| {
                        let metadata_key = if *key == "geometry_sha256" {
                            "source_geometry_sha256"
                        } else {
                            "source_sha256"
                        };
                        evidence[key].as_str().is_none_or(|sha| {
                            sha.len() != 64
                                || !sha
                                    .bytes()
                                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        }) || part["metadata"][metadata_key] != evidence[key]
                    })
                {
                    return None;
                }
                let boundary = part["boundary"].as_array()?;
                if !(3..=512).contains(&boundary.len())
                    || boundary.iter().any(|point| {
                        ["x", "y"]
                            .iter()
                            .any(|key| point[key].as_f64().is_none_or(|n| !n.is_finite()))
                    })
                    || part["base_z"].as_f64().is_none_or(|n| !n.is_finite())
                    || part["height"]
                        .as_f64()
                        .is_none_or(|n| !n.is_finite() || n <= 0.0)
                {
                    return None;
                }
                rows.push(format!("选中 {component} · 原部件 {id} · 原轮廓 {} · 原底标高 {} · 原高度 {} · 原斜底 {} · 原顶面 {}",
                    part["boundary"], part["base_z"], part["height"], part["stair_soffit"], part["stair_top"]));
                continue;
            }
            text(part, "primitive_id").ok()?;
            if part["arguments"]
                .as_object()?
                .values()
                .any(|v| v.as_f64().is_none_or(|n| !n.is_finite()))
            {
                return None;
            }
            for key in ["origin", "scale"] {
                let values = part[key].as_array()?;
                if values.len() != 3
                    || values
                        .iter()
                        .any(|v| v.as_f64().is_none_or(|n| !n.is_finite()))
                {
                    return None;
                }
            }
            part["rotation_z_deg"].as_f64().filter(|n| n.is_finite())?;
            rows.push(format!(
                "选中 {component} · 原部件 {id} · 原位置 {} · 原 primitive {} · 原参数 {} · 原旋转 {} · 原缩放 {}",
                part["origin"], part["primitive_id"].as_str()?, part["arguments"], part["rotation_z_deg"], part["scale"]
            ));
        }
        refs.insert(component.clone(), source_refs);
        if removed.contains(component) {
            rows.push(format!(
                "整组件删除 {component} · 来源仍待确认，重建须显式重新开启"
            ));
        }
    }
    let mut deleted_parts = BTreeSet::new();
    let mut added_parts = BTreeSet::new();
    for op in proposal["draft"]["operations"].as_array()? {
        let target = text(op, "target_id").ok()?;
        if op["basis"] != "assumed" {
            return None;
        }
        match op["op"].as_str()? {
            "remove" => {
                let (component, old) = old_parts.get(&target)?;
                if op["floor_id"] != proposal["scope"]["floor_id"]
                    || op["collection"]
                        != if connections.contains_key(component) {
                            "elements"
                        } else {
                            "solids"
                        }
                    || op["source_ids"] != old["source_ids"]
                    || !deleted_parts.insert(target.clone())
                {
                    return None;
                }
                rows.push(format!(
                    "{target} · 移除原部件 · {} {} · {}",
                    if connections.contains_key(component) {
                        "原轮廓"
                    } else {
                        "原位置"
                    },
                    if connections.contains_key(component) {
                        &old["boundary"]
                    } else {
                        &old["origin"]
                    },
                    text(op, "reason").ok()?
                ));
            }
            "add" => {
                let entity: Value = serde_json::from_str(op["value_json"].as_str()?).ok()?;
                if entity["id"] != target {
                    return None;
                }
                match op["collection"].as_str()? {
                    "solids" | "elements" => {
                        let component = entity["metadata"]["component_id"].as_str()?;
                        let vertical = connections.contains_key(component);
                        if !selected.contains(component)
                            || removed.contains(component)
                            || op["floor_id"] != proposal["scope"]["floor_id"]
                            || !added_parts.insert(target.clone())
                            || op["collection"] != if vertical { "elements" } else { "solids" }
                            || !vertical
                                && !matches!(
                                    entity["role"].as_str()?,
                                    "furniture" | "fixture" | "sanitary"
                                )
                        {
                            return None;
                        }
                        if let Some((owner, _)) = old_parts.get(&target) {
                            if owner != component {
                                return None;
                            }
                        } else if !target.starts_with(&format!("{component}.")) {
                            return None;
                        }
                        let required = if direct.contains(component) {
                            let (_, old) = old_parts.get(&target)?;
                            old["source_ids"]
                                .as_array()?
                                .iter()
                                .map(|v| v.as_str().map(str::to_owned))
                                .collect::<Option<BTreeSet<_>>>()?
                        } else {
                            refs.get(component)?.clone()
                        };
                        let actual = entity["source_ids"]
                            .as_array()?
                            .iter()
                            .map(|v| v.as_str().map(str::to_owned))
                            .collect::<Option<BTreeSet<_>>>()?;
                        if required != actual || op["source_ids"] != entity["source_ids"] {
                            return None;
                        }
                        if vertical {
                            let connection = connections.get(component)?;
                            let source = connection["source_instance_id"].as_str()?;
                            let expected = connection["source_ids"]
                                .as_array()?
                                .iter()
                                .map(|v| v.as_str().map(str::to_owned))
                                .collect::<Option<BTreeSet<_>>>()?;
                            let mut expected = expected;
                            expected.insert(source.to_owned());
                            if expected != actual
                                || entity["metadata"]["source_instance_id"] != source
                                || entity["metadata"]["relation_validator"]
                                    != "vertical-schematic/1"
                                || entity["metadata"]["source_sha256"]
                                    != proof["sources"][source]["source_sha256"]
                                || entity["metadata"]["source_geometry_sha256"]
                                    != proof["sources"][source]["geometry_sha256"]
                            {
                                return None;
                            }
                            if connection["circulation_type"] == "escalator" {
                                let metadata = &entity["metadata"];
                                let owner = format!("{component}.flight.01.step.001");
                                if !escalator_parts.remove(&target)
                                    || entity["role"] != "partition"
                                    || metadata["circulation_type"] != "escalator"
                                    || metadata["vertical_spec_owner"] != owner
                                    || metadata["escalator_part"] != if target.ends_with(".002") { "incline" } else { "level_end" }
                                    || metadata["solid_representation"] != "schematic escalator truss with level ends and continuous incline"
                                    || metadata["limitations"] != connection["limitations"]
                                    || metadata["level_end_depth_m"].as_str()?.parse::<f64>().ok().is_none_or(|n| !n.is_finite() || n <= 0.0)
                                {
                                    return None;
                                }
                                if target == owner {
                                    if serde_json::from_str::<Value>(
                                        metadata["vertical_spec"].as_str()?,
                                    )
                                    .ok()?
                                        != **connection
                                    {
                                        return None;
                                    }
                                } else if metadata["reason_ref"] != owner {
                                    return None;
                                }
                                let endpoints: Value =
                                    serde_json::from_str(metadata["endpoint_top_z_m"].as_str()?)
                                        .ok()?;
                                if endpoints
                                    != json!([
                                        entity["stair_top"]["start_z"],
                                        entity["stair_top"]["end_z"]
                                    ])
                                {
                                    return None;
                                }
                            }
                            continue;
                        }
                        for key in ["origin", "scale"] {
                            let values = entity[key].as_array()?;
                            if values.len() != 3 || values.iter().any(|v| v.as_f64().is_none()) {
                                return None;
                            }
                        }
                        entity["rotation_z_deg"].as_f64()?;
                        rows.push(format!("{target} · 编辑结果 · primitive {} · 参数 {} · 位置 {} · 旋转 {} · 缩放 {} · assumed",
                                          entity["primitive_id"].as_str()?, entity["arguments"], entity["origin"], entity["rotation_z_deg"], entity["scale"]));
                    }
                    "primitives" | "templates" => {
                        rows.push(format!("新增数据定义 {target} · assumed · {entity}"))
                    }
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
    if deleted_parts != old_parts.keys().cloned().collect() || !escalator_parts.is_empty() {
        return None;
    }
    for component in selected.difference(&removed) {
        if !added_parts.iter().any(|id| {
            old_parts
                .get(id)
                .is_some_and(|(owner, _)| owner == component)
                || id.starts_with(&format!("{component}."))
        }) {
            return None;
        }
    }
    Some(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escalator_edit() -> Value {
        let mut value = super::super::tests::floor_story_height_proposal();
        let connection = json!({"component_id":"escalator.1","source_instance_id":"source.1",
            "kind":"stair","mode":"single_floor_schematic","circulation_type":"escalator",
            "cad_completion":true,"dimension_basis":"assumed","rise_m":1,"served_floors":["floor.f02.trial"],
            "source_ids":["cad.1"],"flights":[{}],"openings":[],"limitations":"schematic only"});
        let source = json!({"source_id":"source.1","component_id":"escalator.1","floor_id":"floor.f02.trial",
            "source_sha256":"a".repeat(64),"geometry_sha256":"b".repeat(64),"origin_m":[0,0]});
        let owner = "escalator.1.flight.01.step.001";
        let old = json!({"id":owner,"role":"stair_step","source_ids":["source.1","cad.1"],
            "boundary":[{"x":0,"y":0},{"x":1,"y":0},{"x":1,"y":1},{"x":0,"y":1}],"base_z":0,"height":1,
            "metadata":{"component_id":"escalator.1","source_instance_id":"source.1",
                "relation_validator":"vertical-schematic/1","source_sha256":"a".repeat(64),"source_geometry_sha256":"b".repeat(64)}});
        let feedback = json!({"action":"revise","object_ids":["escalator.1"],"text":"revise selected escalator"});
        let p = &mut value["proposal"];
        p["scope"]["protected_ids"] = json!([]);
        p["decision_drafts"] = json!([]);
        p["feedback"] = feedback.clone();
        p["component_edit"] = json!({"contract":"selected-component-edit/v1","feedback":feedback,
            "source_state":{},"removal_state":{"source.1":null},"sources":{"source.1":source},
            "selected_parts":{"escalator.1":[old]},"plan":{"selected_component_ids":["escalator.1"],
                "remove_component_ids":[],"direct_components":[],"replacements":{"components":[],"connections":[connection]}}});
        let mut operations = vec![
            json!({"op":"remove","collection":"elements","target_id":owner,
            "floor_id":"floor.f02.trial","basis":"assumed","source_ids":["source.1","cad.1"],"reason":"selected revision"}),
        ];
        for segment in 1..=3 {
            let id = format!("escalator.1.flight.01.step.{segment:03}");
            let start_z = if segment == 3 { 0.0 } else { 1.0 };
            let end_z = if segment == 1 { 1.0 } else { 0.0 };
            let mut entity = json!({"id":id,"role":"partition","source_ids":["source.1","cad.1"],
                "boundary":[{"x":0,"y":0},{"x":1,"y":0},{"x":1,"y":1},{"x":0,"y":1}],"base_z":-0.1,"height":1.1,
                "stair_top":{"start":{"x":0,"y":0.5},"end":{"x":1,"y":0.5},"start_z":start_z,"end_z":end_z},
                "stair_soffit":{"start":{"x":0,"y":0.5},"end":{"x":1,"y":0.5},"start_z":start_z-0.1,"end_z":end_z-0.1},
                "metadata":{"component_id":"escalator.1","source_instance_id":"source.1","source_sha256":"a".repeat(64),
                    "source_geometry_sha256":"b".repeat(64),"relation_validator":"vertical-schematic/1",
                    "circulation_type":"escalator","modeling_mode":"single_floor_schematic","connection_status":"unverified",
                    "step_dimension_basis":"assumed","geometry_status":"schematic","vertical_spec_owner":owner,
                    "escalator_part":if segment == 2 {"incline"} else {"level_end"},"level_end_depth_m":"0.5",
                    "endpoint_top_z_m":json!([start_z,end_z]).to_string(),"limitations":"schematic only","not_checked":"cross-floor relation",
                    "solid_representation":"schematic escalator truss with level ends and continuous incline"}});
            if segment == 1 {
                entity["metadata"]["vertical_spec"] = json!(connection.to_string());
            } else {
                entity["metadata"]["reason_ref"] = json!(owner);
            }
            operations.push(json!({"op":"add","collection":"elements","target_id":id,"floor_id":"floor.f02.trial",
                "basis":"assumed","source_ids":["source.1","cad.1"],"value_json":entity.to_string()}));
        }
        p["draft"]["operations"] = json!(operations);
        value
    }

    #[test]
    fn selected_escalator_revision_projects_exact_original_parts_and_all_three_segments() {
        let value = escalator_edit();
        let fact = proposal_fact(&value).unwrap();
        assert!(fact.ready);
        assert!(fact.objects.iter().any(|row| row.contains("原轮廓")));
        assert!(fact.objects.iter().any(|row| row.contains("单层示意扶梯")));
    }

    #[test]
    fn selected_escalator_revision_rejects_incomplete_parts_sources_and_geometry() {
        for fault in [
            "selection",
            "old_owner",
            "old_source",
            "remove_collection",
            "missing_remove",
            "missing_add",
            "duplicate_add",
            "source",
            "new_source_version",
            "spec",
            "geometry",
        ] {
            let mut value = escalator_edit();
            let p = &mut value["proposal"];
            match fault {
                "selection" => {
                    p["component_edit"]["plan"]["selected_component_ids"] = json!(["other"])
                }
                "old_owner" => {
                    p["component_edit"]["selected_parts"]["escalator.1"][0]["metadata"]
                        ["component_id"] = json!("other")
                }
                "old_source" => {
                    p["component_edit"]["selected_parts"]["escalator.1"][0]["metadata"]
                        ["source_sha256"] = json!("c".repeat(64))
                }
                "remove_collection" => p["draft"]["operations"][0]["collection"] = json!("solids"),
                "missing_remove" => {
                    p["draft"]["operations"].as_array_mut().unwrap().remove(0);
                }
                "missing_add" => {
                    p["draft"]["operations"].as_array_mut().unwrap().pop();
                }
                "duplicate_add" => {
                    let op = p["draft"]["operations"][1].clone();
                    p["draft"]["operations"].as_array_mut().unwrap().push(op);
                }
                _ => {
                    let mut entity: Value = serde_json::from_str(
                        p["draft"]["operations"][1]["value_json"].as_str().unwrap(),
                    )
                    .unwrap();
                    match fault {
                        "source" => entity["source_ids"] = json!(["source.other"]),
                        "new_source_version" => {
                            entity["metadata"]["source_sha256"] = json!("c".repeat(64))
                        }
                        "spec" => entity["metadata"]["vertical_spec"] = json!("{}"),
                        "geometry" => entity["stair_soffit"]["start_z"] = json!(2),
                        _ => unreachable!(),
                    }
                    p["draft"]["operations"][1]["value_json"] = json!(entity.to_string());
                }
            }
            assert!(proposal_fact(&value).is_none(), "{fault}");
        }
    }
}
