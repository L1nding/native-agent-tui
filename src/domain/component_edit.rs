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
        if parts.is_empty() || parts.len() > 64 {
            return None;
        }
        let mut source_refs = BTreeSet::new();
        for part in parts {
            let id = text(part, "id").ok()?;
            if !matches!(part["role"].as_str()?, "furniture" | "fixture" | "sanitary")
                || old_parts
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
                let (_, old) = old_parts.get(&target)?;
                if op["floor_id"] != proposal["scope"]["floor_id"]
                    || op["collection"] != "solids"
                    || op["source_ids"] != old["source_ids"]
                    || !deleted_parts.insert(target.clone())
                {
                    return None;
                }
                rows.push(format!(
                    "{target} · 移除原部件 · 原位置 {} · {}",
                    old["origin"],
                    text(op, "reason").ok()?
                ));
            }
            "add" => {
                let entity: Value = serde_json::from_str(op["value_json"].as_str()?).ok()?;
                if entity["id"] != target {
                    return None;
                }
                match op["collection"].as_str()? {
                    "solids" => {
                        let component = entity["metadata"]["component_id"].as_str()?;
                        if !selected.contains(component)
                            || removed.contains(component)
                            || op["floor_id"] != proposal["scope"]["floor_id"]
                            || !added_parts.insert(target.clone())
                            || !matches!(
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
    if deleted_parts != old_parts.keys().cloned().collect() {
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
