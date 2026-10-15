use super::*;

fn envelope() -> Value {
    json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx", "kind":"proposal", "current":true,
        "diagnostics":[],"proposal":{"proposal_id":"proposal", "revision":2,"prepare_token":"token",
        "base_sha256":"base","candidate_sha256":"candidate", "state":"prepared", "summary":"two beds",
        "draft":{"operations":[{"collection":"solids","value_json":json!({"id":"bed.1.body", "origin":[6,2,0],"source_ids":[]}).to_string(),"basis":"assumed"}]},
        "issues":[],"pending":[],"assumed":[{"message":"assumed dimensions"}]}})
}

#[test]
fn bound_proposal_preserves_identity_positions_and_assumptions() {
    let fact = proposal_fact(&envelope()).unwrap();
    assert!(fact.ready);
    assert_eq!(fact.binding.revision, 2);
    assert!(fact.objects[0].contains("bed.1.body"));
    assert!(fact.objects[0].contains("[6,2,0]"));
    assert_eq!(fact.assumptions, vec!["assumed dimensions"]);
}

#[test]
fn stale_or_blocked_proposal_cannot_authorize() {
    let mut value = envelope();
    value["current"] = json!(false);
    let fact = proposal_fact(&value).unwrap();
    let mut facts = sequenced_facts();
    facts.snapshot.proposal = Some(fact.clone());
    assert!(!facts.can_authorize(&fact.binding));
    value["current"] = json!(true);
    value["proposal"]["pending"] = json!([{"message":"which side?"}]);
    assert!(!proposal_fact(&value).unwrap().ready);
}

#[test]
fn decoder_accepts_mcp_structured_content_and_rejects_log_text() {
    let value = envelope();
    assert_eq!(
        decode_envelope(&json!({"structuredContent":value}).to_string()),
        Some(value.clone())
    );
    assert_eq!(
        decode_envelope(&json!({"content":[{"type":"text","text":value.to_string()}]}).to_string()),
        Some(value.clone())
    );
    assert!(decode_envelope(&format!("log: {value}")).is_none());
}

#[test]
fn authorization_must_match_exact_binding_and_cannot_repeat_while_pending() {
    let fact = proposal_fact(&envelope()).unwrap();
    let mut facts = sequenced_facts();
    facts.snapshot.proposal = Some(fact.clone());
    assert!(facts.can_authorize(&fact.binding));
    let mut old = fact.binding.clone();
    old.revision -= 1;
    assert!(!facts.can_authorize(&old));
    facts.snapshot.authorizing = true;
    assert!(!facts.can_authorize(&fact.binding));
}

#[test]
fn a_current_new_proposal_family_can_restart_at_revision_one_without_reviving_stale_facts() {
    let mut facts = sequenced_facts();
    let deliver = deliver_fact;
    deliver(&mut facts, "first", envelope());
    let mut current = envelope();
    current["proposal"]["proposal_id"] = json!("new-family");
    current["proposal"]["revision"] = json!(1);
    current["proposal"]["prepare_token"] = json!("new-token");
    current["proposal"]["base_sha256"] = json!("candidate");
    deliver(&mut facts, "new", current);
    let active = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert_eq!(active.proposal_id, "new-family");
    assert_eq!(active.revision, 1);
    let mut stale = envelope();
    stale["current"] = json!(false);
    stale["proposal"]["state"] = json!("superseded");
    deliver(&mut facts, "old-inspect", stale);
    assert_eq!(facts.snapshot.proposal.as_ref().unwrap().binding, active);
    assert!(facts.can_authorize(&active));
}

fn sequenced_facts() -> DomainFacts {
    DomainFacts::new(Some(DomainSetup {
        mode: crate::domain::DomainMode::Floor,
        context_path: "context".into(),
        repository: ".".into(),
        python: "python".into(),
        run_id: "run".into(),
        context_sha256: "ctx".into(),
        floor_id: "floor".into(),
    }))
}

fn deliver_fact(facts: &mut DomainFacts, id: &str, value: Value) {
    let item = json!({"id":id,"type":"mcpToolCall","server":"modeling_domain",
        "tool":"inspect_change","status":"completed","result":{"structuredContent":value}});
    let notice = crate::protocol::decode_observed_tool(
        "item/completed",
        &json!({"threadId":"root","turnId":"turn","item":item}),
    )
    .unwrap()
    .unwrap();
    facts.observe(&notice, &item);
}

#[test]
fn delayed_v1_and_v2_with_old_current_flags_cannot_replace_v3_or_reopen_applied_approval() {
    let mut facts = sequenced_facts();
    let mut v1 = envelope();
    v1["fact_sequence"] = json!(1);
    v1["proposal"]["revision"] = json!(1);
    deliver_fact(&mut facts, "v1", v1.clone());
    let mut v3 = envelope();
    v3["fact_sequence"] = json!(3);
    v3["proposal"]["revision"] = json!(3);
    v3["proposal"]["proposal_id"] = json!("v3");
    v3["proposal"]["prepare_token"] = json!("token3");
    deliver_fact(&mut facts, "v3", v3.clone());
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    deliver_fact(&mut facts, "delayed-v1", v1);
    let mut v2 = envelope();
    v2["fact_sequence"] = json!(2);
    deliver_fact(&mut facts, "delayed-v2", v2);
    assert_eq!(facts.snapshot.proposal.as_ref().unwrap().binding, binding);
    let applied = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"application","fact_sequence":5,"current":true,
        "receipt":{"proposal_id":"v3","prepare_token":"token3","state":"applied"}});
    deliver_fact(&mut facts, "apply", applied);
    deliver_fact(&mut facts, "delayed-v3-prepare", v3);
    assert!(!facts.can_authorize(&binding));
}

#[test]
fn source_ledger_keeps_duplicate_and_capability_states_and_ignores_delayed_pages() {
    let mut facts = sequenced_facts();
    let current = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx","kind":"source_ledger",
        "scan_id":"scan","scene_sha256":"authority","fact_sequence":8,"objects":[
            {"source_id":"chair.one","category":"chair","state":"built","origin_m":[4,5],"matched_component_ids":["chair.one"]},
            {"source_id":"table.duplicate","category":"table","state":"suspected_duplicate","origin_m":[5,10],"matched_component_ids":["table.a","table.b"]},
            {"source_id":"stair","category":"stair","state":"capability_missing","origin_m":[12,10],"matched_component_ids":[]}]});
    deliver_fact(&mut facts, "scan-new", current.clone());
    let mut stale = current;
    stale["fact_sequence"] = json!(2);
    stale["objects"][0]["state"] = json!("pending_addition");
    deliver_fact(&mut facts, "scan-old", stale);
    let ledger = facts.snapshot.ledger.as_ref().unwrap();
    assert_eq!(ledger.counts.get("built"), Some(&1));
    assert_eq!(ledger.counts.get("suspected_duplicate"), Some(&1));
    assert_eq!(ledger.counts.get("capability_missing"), Some(&1));
    assert!(ledger.rows[1].contains("table.a"));
}

#[test]
fn unrelated_historical_artifact_does_not_poison_current_artifact_sequence() {
    let mut facts = sequenced_facts();
    let mut proposal = envelope();
    proposal["fact_sequence"] = json!(3);
    deliver_fact(&mut facts, "proposal", proposal);
    let mut artifact = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx","kind":"artifact",
        "fact_sequence":100,"artifact":{"proposal_id":"old","prepare_token":"old","build_id":"job","revision":2,"candidate":true,"authority_sha256":"base",
        "input_sha256":"candidate","state":"built","output":"old.glb"}});
    deliver_fact(&mut facts, "old-artifact", artifact.clone());
    artifact["fact_sequence"] = json!(4);
    artifact["artifact"]["proposal_id"] = json!("proposal");
    artifact["artifact"]["prepare_token"] = json!("token");
    artifact["artifact"]["output"] = json!("current.glb");
    deliver_fact(&mut facts, "current-artifact", artifact);
    assert_eq!(facts.snapshot.artifacts.len(), 1);
    assert_eq!(facts.snapshot.artifacts[0].binding.output, "current.glb");
}

fn hosted_proposal() -> Value {
    let mut value = envelope();
    value["proposal"]["draft"]["operations"] = json!([{
        "collection":"openings", "basis":"assumed",
        "value_json":json!({"id":"door.1","kind":"door","wall_id":"wall.host",
            "offset":1.5,"width":1.0,"height":2.1,"sill_height":0.0,
            "source_ids":["source.door"],
            "metadata":{"source_instance_id":"source.door","width_basis":"source",
                "height_basis":"assumed","sill_basis":"assumed"}}).to_string()}]);
    value["proposal"]["hosted_sources"] = json!({"source.door":{"origin_m":[2,2]}});
    value
}

#[test]
fn hosted_aperture_fact_shows_host_dimensions_pose_and_provenance_with_exact_binding() {
    let fact = proposal_fact(&hosted_proposal()).unwrap();
    assert!(fact.ready);
    assert_eq!(fact.binding.proposal_id, "proposal");
    assert_eq!(fact.binding.revision, 2);
    assert!(fact.objects[0].contains("door.1 · door · 宿主 wall.host"));
    assert!(fact.objects[0].contains("[2,2]"));
    assert!(fact.objects[1].contains("偏距 1.5 · 宽 1.0 · 高 2.1 · 窗台 0.0"));
    assert!(fact.objects[2].contains("宽 source · 高 assumed · 窗台 assumed"));
    let mut facts = sequenced_facts();
    facts.snapshot.proposal = Some(fact.clone());
    assert!(facts.can_authorize(&fact.binding));
    let mut stale = fact.binding;
    stale.revision = 1;
    assert!(!facts.can_authorize(&stale));
}

#[test]
fn malformed_hosted_dimensions_cannot_become_an_approval_fact() {
    let mut value = hosted_proposal();
    let mut opening: Value = serde_json::from_str(
        value["proposal"]["draft"]["operations"][0]["value_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    opening["width"] = json!("unknown");
    value["proposal"]["draft"]["operations"][0]["value_json"] = json!(opening.to_string());
    assert!(proposal_fact(&value).is_none());
}

fn recovery_proposal() -> Value {
    let mut value = envelope();
    value["fact_sequence"] = json!(1);
    value["proposal"]["base_sha256"] = json!("a".repeat(64));
    value["proposal"]["candidate_sha256"] = json!("b".repeat(64));
    value
}

fn operation_envelope(state: &str, recovery: &str, sequence: u64) -> Value {
    json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"operation","fact_sequence":sequence,"receipt":{
            "operation_id":"apply-one","proposal_id":"proposal","revision":2,
            "prepare_token":"token","base_sha256":"a".repeat(64),
            "candidate_sha256":"b".repeat(64),"state":state,"recovery":recovery,
            "current_sha256":"b".repeat(64)}})
}

#[test]
fn interrupted_operation_blocks_new_approval_and_late_facts_cannot_undo_recovery() {
    let mut facts = sequenced_facts();
    let prepared = recovery_proposal();
    deliver_fact(&mut facts, "prepare", prepared.clone());
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert!(facts.can_authorize(&binding));
    let applying = operation_envelope("applying", "candidate_present", 2);
    deliver_fact(&mut facts, "inspect-operation", applying.clone());
    assert_eq!(
        facts.snapshot.operation.as_ref().unwrap().recovery,
        "candidate_present"
    );
    assert_eq!(
        facts.snapshot.application.as_deref(),
        Some("applying · current=true")
    );
    assert!(!facts.can_authorize(&binding));
    // Even a freshly inspected prepared record must not make a second grant legal.
    let mut still_prepared = prepared.clone();
    still_prepared["fact_sequence"] = json!(3);
    deliver_fact(&mut facts, "still-prepared", still_prepared);
    assert!(!facts.can_authorize(&binding));
    let mut recovered = operation_envelope("applied", "reconciled_without_replay", 4);
    recovered["kind"] = json!("application");
    recovered["current"] = json!(true);
    recovered["current_authority_sha256"] = json!("b".repeat(64));
    deliver_fact(&mut facts, "recover", recovered);
    deliver_fact(&mut facts, "late-operation", applying);
    deliver_fact(&mut facts, "late-prepare", prepared.clone());
    assert_eq!(facts.snapshot.operation.as_ref().unwrap().state, "applied");
    assert_eq!(
        facts.snapshot.operation.as_ref().unwrap().recovery,
        "reconciled_without_replay"
    );
    assert!(!facts.can_authorize(&binding));
    let mut next = prepared;
    next["fact_sequence"] = json!(5);
    next["proposal"]["proposal_id"] = json!("next");
    next["proposal"]["prepare_token"] = json!("next-token");
    next["proposal"]["parent_id"] = json!("proposal");
    next["proposal"]["revision"] = json!(3);
    deliver_fact(&mut facts, "next", next);
    assert!(facts.snapshot.operation.is_none());
    assert!(facts.can_authorize(&facts.snapshot.proposal.as_ref().unwrap().binding));
}

#[test]
fn foreign_or_malformed_operation_cannot_change_approval_or_its_watermark() {
    for (field, invalid) in [
        ("operation_id", json!("../other")),
        ("proposal_id", json!("other")),
        ("prepare_token", json!("foreign")),
        ("revision", json!(3)),
        ("base_sha256", json!("c".repeat(64))),
        ("candidate_sha256", json!("c".repeat(64))),
        ("current_sha256", json!("invalid")),
        ("state", json!("success")),
        ("recovery", json!("not_required")),
    ] {
        let mut facts = sequenced_facts();
        deliver_fact(&mut facts, "prepared", recovery_proposal());
        let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
        let mut invalid_operation = operation_envelope("applying", "candidate_present", 999);
        invalid_operation["receipt"][field] = invalid;
        deliver_fact(&mut facts, "invalid", invalid_operation);
        assert!(facts.snapshot.operation.is_none(), "{field}");
        assert!(facts.can_authorize(&binding), "{field}");
        deliver_fact(
            &mut facts,
            "actual",
            operation_envelope("applying", "candidate_present", 2),
        );
        assert_eq!(
            facts.snapshot.operation.as_ref().unwrap().operation_id,
            "apply-one"
        );
        assert!(!facts.can_authorize(&binding));
    }
}

#[test]
fn a_new_build_cannot_display_the_previous_builds_validation() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "proposal", envelope());
    let validation = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"artifact_validation","fact_sequence":4,
        "artifact":{"build_id":"build-a","proposal_id":"proposal","revision":2,
        "prepare_token":"token","candidate":true,"authority_sha256":"base",
        "input_sha256":"candidate","state":"built","output":"a.glb","valid":true,"current":true}});
    deliver_fact(&mut facts, "a-valid", validation);
    assert_eq!(facts.snapshot.artifacts[0].valid, Some(true));
    let building = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"artifact","fact_sequence":5,
        "artifact":{"build_id":"build-b","proposal_id":"proposal","revision":2,
        "prepare_token":"token","candidate":true,"authority_sha256":"base",
        "input_sha256":"candidate","state":"building","output":"b.glb","current":true}});
    deliver_fact(&mut facts, "b-building", building);
    assert_eq!(facts.snapshot.artifacts.len(), 2);
    assert_eq!(facts.snapshot.artifacts[0].binding.build_id, "build-a");
    assert_eq!(facts.snapshot.artifacts[0].valid, Some(true));
    assert_eq!(facts.snapshot.artifacts[1].binding.build_id, "build-b");
    assert_eq!(
        facts.snapshot.artifacts[1].valid, None,
        "build B inherited build A validation"
    );
}

fn artifact_envelope(id: &str, sequence: u64, state: &str, valid: Option<bool>) -> Value {
    let mut value = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":if valid.is_some() { "artifact_validation" } else { "artifact" },
        "fact_sequence":sequence,
        "artifact":{"build_id":id,"proposal_id":"proposal","revision":2,
        "prepare_token":"token","candidate":true,"authority_sha256":"base",
        "input_sha256":"candidate","state":state,"output":format!("{id}.glb"),"current":true}});
    if let Some(valid) = valid {
        value["artifact"]["valid"] = json!(valid);
    }
    value
}

#[test]
fn late_success_and_failure_remain_bound_to_each_job_and_ignore_older_states() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "proposal", envelope());
    deliver_fact(
        &mut facts,
        "a-start",
        artifact_envelope("a", 4, "building", None),
    );
    deliver_fact(
        &mut facts,
        "b-start",
        artifact_envelope("b", 5, "building", None),
    );
    deliver_fact(
        &mut facts,
        "b-fail",
        artifact_envelope("b", 9, "failed", Some(false)),
    );
    deliver_fact(
        &mut facts,
        "a-valid-delayed",
        artifact_envelope("a", 8, "built", Some(true)),
    );
    deliver_fact(
        &mut facts,
        "b-old-state",
        artifact_envelope("b", 6, "building", None),
    );
    assert_eq!(facts.snapshot.artifacts[0].state, ArtifactState::Built);
    assert_eq!(facts.snapshot.artifacts[0].valid, Some(true));
    assert_eq!(facts.snapshot.artifacts[1].state, ArtifactState::Failed);
    assert_eq!(facts.snapshot.artifacts[1].valid, Some(false));
    // An inspect preserves the validation only for this same immutable job/state.
    deliver_fact(
        &mut facts,
        "a-inspect",
        artifact_envelope("a", 10, "built", None),
    );
    assert_eq!(facts.snapshot.artifacts[0].valid, Some(true));
}

#[test]
fn malformed_build_binding_cannot_create_a_job_or_poison_its_sequence() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "proposal", envelope());
    let original = artifact_envelope("a", 100, "built", Some(true));
    for (key, value) in [
        ("revision", json!(9)),
        ("prepare_token", json!("foreign")),
        ("input_sha256", json!("different")),
        ("authority_sha256", json!("different")),
        ("candidate", json!(false)),
        ("build_id", json!(null)),
        ("state", json!("failed")),
    ] {
        let mut malformed = original.clone();
        malformed["artifact"][key] = value;
        deliver_fact(&mut facts, "malformed", malformed);
        assert!(
            facts.snapshot.artifacts.is_empty(),
            "accepted invalid {key}"
        );
    }
    deliver_fact(
        &mut facts,
        "a-valid",
        artifact_envelope("a", 4, "built", Some(true)),
    );
    let mut reused = artifact_envelope("a", 101, "built", Some(true));
    reused["artifact"]["output"] = json!("another.glb");
    deliver_fact(&mut facts, "reused-id", reused);
    deliver_fact(
        &mut facts,
        "a-validation-fails",
        artifact_envelope("a", 5, "built", Some(false)),
    );
    assert_eq!(facts.snapshot.artifacts.len(), 1);
    assert_eq!(facts.snapshot.artifacts[0].binding.output, "a.glb");
    assert_eq!(facts.snapshot.artifacts[0].valid, Some(false));
}

#[test]
fn application_keeps_preview_validation_but_invalidates_its_cached_currency() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "proposal", envelope());
    deliver_fact(
        &mut facts,
        "preview",
        artifact_envelope("a", 4, "built", Some(true)),
    );
    deliver_fact(
        &mut facts,
        "apply",
        json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"application","fact_sequence":7,"current":true,
        "receipt":{"proposal_id":"proposal","prepare_token":"token","state":"applied"}}),
    );
    assert_eq!(facts.snapshot.artifacts[0].valid, Some(true));
    assert_eq!(facts.snapshot.artifacts[0].current, None);
    // A delayed pre-apply query remains historical, never restores current=true.
    deliver_fact(
        &mut facts,
        "delayed-validation",
        artifact_envelope("a", 6, "built", Some(true)),
    );
    assert_eq!(facts.snapshot.artifacts[0].current, None);
    let mut queried = artifact_envelope("a", 8, "built", Some(true));
    queried["artifact"]["current"] = json!(false);
    deliver_fact(&mut facts, "post-apply-validation", queried);
    assert_eq!(facts.snapshot.artifacts[0].current, Some(false));
}

#[test]
fn build_history_is_bounded_and_new_proposal_clears_its_job_bindings() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "proposal", envelope());
    for number in 0..40 {
        deliver_fact(
            &mut facts,
            "job",
            artifact_envelope(&format!("job-{number}"), number + 4, "built", Some(true)),
        );
    }
    assert_eq!(facts.snapshot.artifacts.len(), 32);
    assert_eq!(
        facts
            .fact_sequences
            .keys()
            .filter(|key| key.starts_with("artifact:"))
            .count(),
        32
    );
    let mut next = envelope();
    next["fact_sequence"] = json!(50);
    next["proposal"]["proposal_id"] = json!("next");
    next["proposal"]["prepare_token"] = json!("next-token");
    deliver_fact(&mut facts, "next", next);
    assert!(facts.snapshot.artifacts.is_empty());
    assert!(!facts
        .fact_sequences
        .keys()
        .any(|key| key.starts_with("artifact:")));
}

#[test]
fn actual_operation_query_marks_old_applied_artifact_noncurrent_after_later_edit() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "proposal", recovery_proposal());
    let mut applied = artifact_envelope("applied-a", 4, "built", Some(true));
    applied["artifact"]["candidate"] = json!(false);
    applied["artifact"]["input_sha256"] = json!("b".repeat(64));
    applied["artifact"]["authority_sha256"] = json!("b".repeat(64));
    deliver_fact(&mut facts, "artifact", applied.clone());
    assert_eq!(facts.snapshot.artifacts[0].current, Some(true));
    let mut inspected = operation_envelope("applied", "not_required", 7);
    inspected["receipt"]["current_sha256"] = json!("c".repeat(64));
    deliver_fact(&mut facts, "later-edit", inspected);
    assert_eq!(facts.snapshot.artifacts[0].current, Some(false));
    assert_eq!(facts.snapshot.artifacts[0].valid, Some(true));
    applied["fact_sequence"] = json!(6);
    deliver_fact(&mut facts, "old-query", applied);
    assert_eq!(facts.snapshot.artifacts[0].current, Some(false));
}

fn vertical_proposal() -> Value {
    let mut value = envelope();
    let element = json!({"id":"stair.1.step.01","role":"stair_step",
        "boundary":[{"x":2,"y":2},{"x":2.3,"y":2},{"x":2.3,"y":3.2},{"x":2,"y":3.2}],
        "base_z":0,"height":0.15,"source_ids":["source.stair"],
        "metadata":{"relation_validator":"vertical-connection/1","component_id":"stair.1",
            "source_instance_id":"source.stair","step_dimension_basis":"assumed",
            "not_checked":"headroom; structural design; building-code compliance; elevator"}});
    value["proposal"]["draft"]["operations"] = json!([{
        "collection":"elements","basis":"assumed","value_json":element.to_string()}]);
    value["proposal"]["component_plan"] = json!({"connections":[{
        "component_id":"stair.1","served_floors":["floor.lower","floor.upper"],
        "openings":[{"floor_id":"floor.upper","slab_id":"slab.upper","hole_index":0}]}]});
    value["proposal"]["vertical_sources"] = json!({"source.stair":{"origin_m":[2,2]}});
    value
}

#[test]
fn vertical_elements_show_actual_geometry_read_only_dependencies_and_unchecked_rules() {
    let fact = proposal_fact(&vertical_proposal()).unwrap();
    assert!(fact.ready);
    assert!(fact.objects[0].contains("stair.1.step.01 · stair_step"));
    assert!(fact.objects[0].contains("底标高 0 · 高 0.15 · assumed"));
    assert!(fact.objects[1].contains("floor.lower"));
    assert!(fact.objects[1].contains("slab.upper"));
    assert!(fact.objects[2].contains("headroom; structural design"));
    let mut facts = sequenced_facts();
    facts.snapshot.proposal = Some(fact.clone());
    assert!(facts.can_authorize(&fact.binding));
    let mut stale = fact.binding;
    stale.revision = 1;
    assert!(!facts.can_authorize(&stale));
}

#[test]
fn malformed_vertical_geometry_cannot_become_an_approval_fact() {
    let mut value = vertical_proposal();
    let mut element: Value = serde_json::from_str(
        value["proposal"]["draft"]["operations"][0]["value_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    element["height"] = json!("unknown");
    value["proposal"]["draft"]["operations"][0]["value_json"] = json!(element.to_string());
    assert!(proposal_fact(&value).is_none());
    let mut missing = vertical_proposal();
    missing["proposal"]["component_plan"] = json!({});
    assert!(proposal_fact(&missing).is_none());
}

#[test]
fn vertical_anchor_proof_contract_keeps_concrete_approval_bound() {
    let mut value = vertical_proposal();
    let mut element: Value = serde_json::from_str(
        value["proposal"]["draft"]["operations"][0]["value_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    element["metadata"]["relation_validator"] = json!("vertical-connection/2");
    element["metadata"]["vertical_spec_owner"] = json!("stair.1.step.01");
    value["proposal"]["draft"]["operations"][0]["value_json"] = json!(element.to_string());
    let fact = proposal_fact(&value).unwrap();
    assert!(fact.ready);
    assert!(fact.objects[1].contains("slab.upper"));
    assert_eq!(
        fact.binding,
        proposal_fact(&vertical_proposal()).unwrap().binding
    );
}

fn private_receipt(value: &Value) -> receipt::FactReceipt {
    let raw = serde_json::to_vec(value).unwrap();
    receipt::FactReceipt {
        contract: "modeling-fact-receipt/v1".into(),
        run_id: "run".into(),
        context_sha256: "ctx".into(),
        kind: value["kind"].as_str().unwrap().into(),
        ok: true,
        fact_sha256: format!("{:x}", Sha256::digest(&raw)),
        fact_bytes: raw.len(),
        fact_sequence: value["fact_sequence"].as_u64().unwrap(),
        binding: json!({"proposal_id":"proposal","revision":2,"prepare_token":"token",
            "base_sha256":"base","candidate_sha256":"candidate"}),
    }
}

#[test]
fn private_fact_verification_checks_all_receipt_bindings_above_model_budget() {
    let mut value = envelope();
    value["fact_sequence"] = json!(7);
    value["ok"] = json!(true);
    value["proposal"]["large_evidence"] = json!("测量依据".repeat(10000));
    let raw = serde_json::to_vec(&value).unwrap();
    assert!(raw.len() > 64 * 1024);
    let receipt = private_receipt(&value);
    let setup = DomainSetup {
        mode: crate::domain::DomainMode::Floor,
        context_path: "context".into(),
        repository: ".".into(),
        python: "python".into(),
        run_id: "run".into(),
        context_sha256: "ctx".into(),
        floor_id: "floor".into(),
    };
    assert_eq!(receipt::verify(&setup, &receipt, &raw).unwrap(), value);
    for key in [
        "run_id",
        "context_sha256",
        "kind",
        "fact_sequence",
        "binding",
        "ok",
        "fact_bytes",
        "fact_sha256",
    ] {
        let mut changed = serde_json::to_value(&receipt).unwrap();
        changed[key] = match key {
            "fact_sequence" => json!(8),
            "fact_bytes" => json!(raw.len() + 1),
            "binding" => json!({"proposal_id":"other"}),
            "ok" => json!(false),
            _ => json!("foreign"),
        };
        let changed = serde_json::from_value(changed).unwrap();
        assert!(receipt::verify(&setup, &changed, &raw).is_err(), "{key}");
    }
    let mut corrupt = raw.clone();
    corrupt[1] = b'x';
    assert!(receipt::verify(&setup, &receipt, &corrupt).is_err());
    let mut facts = DomainFacts::new(Some(setup));
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert!(facts.can_authorize(&binding));
    facts.synchronizing = true;
    assert!(!facts.can_authorize(&binding));
    facts.synchronizing = false;
    facts.synchronization_failed("proposal", 8, "missing file".into());
    assert!(!facts.can_authorize(&binding));
    facts.synchronization_finished(&receipt);
    assert!(!facts.can_authorize(&binding)); // Old success cannot clear a newer failure.
    let mut newer = receipt;
    newer.fact_sequence = 8;
    facts.synchronization_finished(&newer);
    assert!(facts.can_authorize(&binding));
    // A sealed fact that passes hashing but cannot be projected is not a silent no-op.
    let mut unusable = value;
    unusable["fact_sequence"] = json!(9);
    unusable["proposal"]["summary"] = json!(null);
    assert_eq!(
        facts.apply_verified_fact(&unusable),
        FactProjection::Invalid
    );
    unusable["fact_sequence"] = json!(1);
    assert_eq!(
        facts.apply_verified_fact(&unusable),
        FactProjection::Superseded
    );
}

#[test]
fn verified_source_groups_project_complete_counts_and_reject_malformed_groups() {
    let mut facts = sequenced_facts();
    let setup = facts.setup.as_ref().unwrap().clone();
    let mut value = json!({"contract":"modeling-domain/v2","kind":"source_groups","ok":true,
    "run_id":setup.run_id,"context_sha256":setup.context_sha256,"fact_sequence":4,
    "scan_id":"scan-1","scene_sha256":"scene-1","total_groups":1,"total_objects":2,
    "state_counts":{"pending_confirmation":2},"groups":[{
        "group_id":"group.same","block":"凳-医生","instance_count":2,
        "instances":[{"source_id":"a"},{"source_id":"b"}],
        "category_counts":{"unresolved":2},"state_counts":{"pending_confirmation":2}
    }]});
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    let projection = facts.snapshot.groups.as_ref().unwrap();
    assert_eq!(projection.total_objects, 2);
    assert!(projection.rows[0].contains("凳-医生"));
    value["fact_sequence"] = json!(5);
    value["groups"][0]["instance_count"] = json!(3);
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Invalid);
    assert_eq!(facts.snapshot.groups.as_ref().unwrap().total_objects, 2);
    value["groups"][0]["instance_count"] = json!(2);
    value["total_objects"] = json!(3);
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Invalid);
}

#[test]
fn source_group_assessment_count_requires_actual_bound_reasons() {
    let mut facts = sequenced_facts();
    let mut value = json!({"contract":CONTRACT,"kind":"source_groups","run_id":"run", "context_sha256":"ctx",
    "fact_sequence":1,"ok":true,"scan_id":"scan","scene_sha256":"scene",
    "total_objects":1,"total_groups":1,"assessed_groups":1,
    "state_counts":{"pending_confirmation":1},"groups":[{
        "group_id":"group.x","block":"ambiguous","instance_count":1,
        "instances":[{"source_id":"source"}],"category_counts":{"unresolved":1},
        "state_counts":{"pending_confirmation":1},"assessment":null
    }]});
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Invalid);
    value["groups"][0]["assessment"] =
        json!({"category":null,"reason":"unclear host; needs user review"});
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    assert_eq!(facts.snapshot.groups.as_ref().unwrap().assessed_groups, 1);
    assert!(facts.snapshot.groups.as_ref().unwrap().rows[1].contains("unclear host"));
}

#[test]
fn large_multipart_batch_projects_every_approved_part_after_512_operations() {
    let mut value = envelope();
    value["fact_sequence"] = json!(10);
    value["proposal"]["draft"]["operations"] = Value::Array(
        (0..600)
            .map(|index| {
                json!({"collection":"solids","value_json":json!({
            "id":format!("bed.{}.part.{}",index / 6,index % 6),
            "origin":[index,2,0],"source_ids":[]
        }).to_string(),"basis":"assumed"})
            })
            .collect(),
    );
    let mut facts = sequenced_facts();
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    let proposal = facts.snapshot.proposal.as_ref().unwrap();
    assert_eq!(proposal.objects.len(), 600);
    assert!(proposal.objects.last().unwrap().contains("bed.99.part.5"));
    assert!(facts.can_authorize(&proposal.binding));
    let original = proposal.clone();
    value["fact_sequence"] = json!(11);
    value["proposal"]["draft"]["operations"][599]["value_json"] = json!("{broken}");
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Invalid);
    assert_eq!(facts.snapshot.proposal.as_ref(), Some(&original));
}

#[test]
fn valid_historical_proposal_inspection_is_superseded_not_a_sync_failure() {
    let mut facts = sequenced_facts();
    let mut current = envelope();
    current["fact_sequence"] = json!(20);
    current["proposal"]["proposal_id"] = json!("current-family");
    assert_eq!(facts.apply_verified_fact(&current), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    let mut old = envelope();
    old["current"] = json!(false);
    old["fact_sequence"] = json!(21);
    old["proposal"]["state"] = json!("blocked");
    assert_eq!(facts.apply_verified_fact(&old), FactProjection::Superseded);
    assert_eq!(facts.snapshot.proposal.as_ref().unwrap().binding, binding);
    assert!(facts.can_authorize(&binding));
    old["proposal"]["summary"] = Value::Null;
    assert_eq!(facts.apply_verified_fact(&old), FactProjection::Invalid);
}

#[test]
fn drained_fact_queue_preserves_specific_unresolved_failures_and_blocks_approval() {
    let mut facts = sequenced_facts();
    let mut proposal = envelope();
    proposal["fact_sequence"] = json!(20);
    assert_eq!(
        facts.apply_verified_fact(&proposal),
        FactProjection::Applied
    );
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    facts.synchronization_failed("proposal", 21, "actual malformed proposal".into());
    facts.synchronizing = true;
    facts.snapshot.fact_sync_notice = Some("正在同步完整领域事实".into());
    let mut unrelated = private_receipt(&proposal);
    unrelated.kind = "source_groups".into();
    unrelated.fact_sequence = 22;
    facts.synchronization_finished(&unrelated);
    facts.synchronization_queue_drained();
    assert!(!facts.synchronizing);
    let notice = facts.snapshot.fact_sync_notice.as_deref().unwrap();
    assert!(notice.contains("领域事实同步失败"));
    assert!(notice.contains("proposal (seq 21)"));
    assert!(!notice.contains("正在同步"));
    assert!(!facts.can_authorize(&binding));
    let mut repaired = private_receipt(&proposal);
    repaired.fact_sequence = 23;
    facts.synchronization_finished(&repaired);
    facts.synchronization_queue_drained();
    assert!(facts.snapshot.fact_sync_notice.is_none());
    assert!(facts.can_authorize(&binding));
}

#[test]
fn readonly_recovery_of_older_applied_artifact_uses_verified_operation_authority() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "proposal", recovery_proposal());
    deliver_fact(
        &mut facts,
        "readonly-operation",
        operation_envelope("applied", "not_required", 7),
    );
    let mut applied = artifact_envelope("applied-a", 4, "built", Some(true));
    applied["artifact"]["candidate"] = json!(false);
    applied["artifact"]["input_sha256"] = json!("b".repeat(64));
    applied["artifact"]["authority_sha256"] = json!("b".repeat(64));
    deliver_fact(&mut facts, "existing-validation", applied);
    assert_eq!(facts.snapshot.artifacts[0].valid, Some(true));
    assert_eq!(
        facts.snapshot.artifacts[0].current,
        Some(true),
        "later read of the same applied authority must not erase current build status"
    );
}

fn grouped_source_view(sequence: u64) -> Value {
    json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
    "kind":"source_groups","fact_sequence":sequence,
    "scan_id":"scan","scene_sha256":"scene","total_groups":1,"total_objects":2,
    "state_counts":{"pending_addition":1,"skipped":1},"groups":[{
        "group_id":"group.chairs","block":"chairs","instance_count":2,
        "instances":[{"source_id":"a"},{"source_id":"b"}],
        "category_counts":{"chair":2},"state_counts":{"pending_addition":1,"skipped":1}
    }]})
}

fn instance_source_view(sequence: u64) -> Value {
    json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
    "kind":"source_ledger","fact_sequence":sequence,"scan_id":"scan","scene_sha256":"scene",
    "objects":[
        {"source_id":"a","category":"chair","state":"skipped","origin_m":[4,5],"matched_component_ids":[]},
        {"source_id":"b","category":"chair","state":"pending_addition","origin_m":[6,5],"matched_component_ids":[]}
    ]})
}

#[test]
fn source_views_new_ledger_replaces_group_snapshot_and_rejects_late_groups() {
    let mut facts = sequenced_facts();
    let proposal = envelope();
    deliver_fact(&mut facts, "proposal", proposal);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    deliver_fact(&mut facts, "groups", grouped_source_view(4));
    assert!(facts.snapshot.groups.is_some());
    deliver_fact(&mut facts, "ledger", instance_source_view(5));
    assert!(
        facts.snapshot.groups.is_none(),
        "older group rows are not current ledger facts"
    );
    assert_eq!(
        facts
            .snapshot
            .ledger
            .as_ref()
            .unwrap()
            .counts
            .get("skipped"),
        Some(&1)
    );
    // Equal totals and unchanged scan IDs do not prove that every member state is unchanged.
    assert_eq!(
        facts.apply_verified_fact(&grouped_source_view(4)),
        FactProjection::Superseded
    );
    assert!(facts.snapshot.groups.is_none());
    assert_eq!(facts.snapshot.proposal.as_ref().unwrap().binding, binding);
    assert!(facts.can_authorize(&binding));
}

#[test]
fn source_views_new_groups_replace_ledger_and_malformed_or_late_ledger_cannot_displace_them() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "ledger", instance_source_view(4));
    deliver_fact(&mut facts, "groups", grouped_source_view(5));
    assert!(
        facts.snapshot.ledger.is_none(),
        "older instance rows must not appear as current"
    );
    assert!(facts.snapshot.groups.is_some());
    assert_eq!(
        facts.apply_verified_fact(&instance_source_view(4)),
        FactProjection::Superseded
    );
    let mut malformed = instance_source_view(100);
    malformed["objects"][0]["state"] = json!("invented-state");
    assert_eq!(
        facts.apply_verified_fact(&malformed),
        FactProjection::Invalid
    );
    assert!(facts.snapshot.groups.is_some());
    assert!(facts.snapshot.ledger.is_none());
    assert_eq!(
        facts.apply_verified_fact(&instance_source_view(6)),
        FactProjection::Applied
    );
    assert!(facts.snapshot.groups.is_none());
    assert!(facts.snapshot.ledger.is_some());
}

fn project_catalog(sequence: u64) -> Value {
    json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx","ok":true,
    "kind":"project_catalog","fact_sequence":sequence,
    "project":{"catalog_id":"a".repeat(64),"source_sha256":"b".repeat(64),
    "project_path":"projects/hospital","floors":[
        {"key":"f08","title":"八层"}, {"key":"f09.a","title":"九层图纸A"},
        {"key":"f09.b","title":"九层图纸B"}
    ]}})
}

fn project_selection(sequence: u64, selected: bool) -> Value {
    json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx","ok":true,
        "kind":"project_selection","fact_sequence":sequence,"selection":{
        "selection_id":"c".repeat(64),"catalog_id":"a".repeat(64),"source_sha256":"b".repeat(64),
        "target":"整栋","state":if selected {"selected"} else {"needs_selection"},
        "scene_application":"not_run","authorization":"not_granted",
        "selected_keys":if selected {json!(["f08","f09.b"])} else {json!([])},
        "candidate_groups":[[{"key":"f08","title":"八层"}],
            [{"key":"f09.a","title":"九层图纸A"},{"key":"f09.b","title":"九层图纸B"}]]}})
}

#[test]
fn project_catalog_and_selection_are_bound_sequenced_and_never_authorize_scene_writes() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    assert_eq!(
        facts.apply_verified_fact(&project_selection(2, true)),
        FactProjection::Invalid
    );
    assert_eq!(
        facts.apply_verified_fact(&project_catalog(1)),
        FactProjection::Applied
    );
    assert_eq!(facts.snapshot.project.as_ref().unwrap().total_floors, 3);
    assert_eq!(
        facts.apply_verified_fact(&project_selection(2, false)),
        FactProjection::Applied
    );
    assert_eq!(
        facts.snapshot.selection.as_ref().unwrap().state,
        SelectionState::NeedsSelection
    );
    assert_eq!(
        facts.apply_verified_fact(&project_selection(3, true)),
        FactProjection::Applied
    );
    assert_eq!(
        facts.snapshot.selection.as_ref().unwrap().selected_keys,
        ["f08", "f09.b"]
    );
    assert_eq!(
        facts.apply_verified_fact(&project_selection(2, false)),
        FactProjection::Superseded
    );
    assert_eq!(
        facts.apply_verified_fact(&envelope()),
        FactProjection::Invalid
    );
    assert!(!facts.can_authorize(&proposal_fact(&envelope()).unwrap().binding));
    for (key, value) in [
        ("catalog_id", json!("d".repeat(64))),
        ("source_sha256", json!("d".repeat(64))),
        ("authorization", json!("granted")),
        ("selected_keys", json!(["f08", "f09.a", "f09.b"])),
    ] {
        let mut invalid = project_selection(999, true);
        invalid["selection"][key] = value;
        assert_eq!(
            facts.apply_verified_fact(&invalid),
            FactProjection::Invalid,
            "{key}"
        );
    }
    let mut next = project_catalog(4);
    next["project"]["catalog_id"] = json!("d".repeat(64));
    assert_eq!(facts.apply_verified_fact(&next), FactProjection::Applied);
    assert!(facts.snapshot.selection.is_none());
    assert_eq!(
        facts.apply_verified_fact(&project_selection(5, true)),
        FactProjection::Invalid
    );
}

#[test]
fn ordinary_project_launch_creates_a_bound_context_and_registers_pure_tools() {
    let root = std::env::temp_dir().join(format!(
        "native-project-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("projects/test")).unwrap();
    std::fs::create_dir_all(
        root.join(".venv")
            .join(if cfg!(windows) { "Scripts" } else { "bin" }),
    )
    .unwrap();
    std::fs::write(
        root.join(if cfg!(windows) {
            ".venv/Scripts/python.exe"
        } else {
            ".venv/bin/python"
        }),
        "",
    )
    .unwrap();
    std::fs::write(root.join("projects/test/project.yaml"), "sources: []").unwrap();
    let bad_root = configure(Config {
        cwd: root.clone(),
        domain_project: Some(".".into()),
        ..Default::default()
    })
    .unwrap_err();
    assert!(bad_root.contains("projects/test"), "{bad_root}");
    assert!(bad_root.contains("--domain-project ."), "{bad_root}");
    assert!(!root.join("outputs").exists());
    let config = configure(Config {
        cwd: root.clone(),
        domain_project: Some("projects/test".into()),
        ..Default::default()
    })
    .unwrap();
    let setup = config.domain_setup.as_ref().unwrap();
    assert_eq!(setup.mode, DomainMode::Project);
    assert_eq!(config.permission.as_deref(), Some("read-only"));
    assert!(setup
        .context_path
        .starts_with(normalized_path(&root).unwrap()));
    let raw = std::fs::read(&setup.context_path).unwrap();
    let value: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(value["contract"], "modeling-project/v1");
    assert_eq!(setup.context_sha256, format!("{:x}", Sha256::digest(raw)));
    assert!(
        matches!(&config.mcp_servers[0], McpServerConfig::Stdio {args,..} if args.iter().any(|arg| arg == "blender_agent.integrations.project_mcp"))
    );
    for path in [root.clone(), root.join("..")] {
        assert!(configure(Config {
            cwd: root.clone(),
            domain_project: Some(path),
            ..Default::default()
        })
        .is_err());
    }
    assert!(configure(Config {
        cwd: root.clone(),
        domain_project: Some("projects/test".into()),
        permission: Some("workspace-write".into()),
        ..Default::default()
    })
    .is_err());
    std::fs::remove_dir_all(root).unwrap();
}

fn project_scope(sequence: u64, floor_key: &str, scope_id: &str) -> Value {
    let mut value = envelope();
    value["ok"] = json!(true);
    value["kind"] = json!("project_scope");
    value["fact_sequence"] = json!(sequence);
    value["project_structure"] = json!(true);
    value["project_scope"] = json!({"scope_id":scope_id,"floor_key":floor_key,"floor_id":format!("floor.{floor_key}"),
        "selection_id":"c".repeat(64),"source_sha256":"b".repeat(64)});
    value["proposal"]["proposal_id"] = json!(format!("structure.{floor_key}"));
    value["proposal"]["draft"]["operations"] = json!([{"collection":"walls","basis":"assumed","value_json":
        json!({"id":format!("wall.{floor_key}"),"start":{"x":0,"y":0},"end":{"x":8,"y":0},"height":3.0,"thickness":0.2}).to_string()}]);
    value
}

#[test]
fn component_discovery_mode_is_bound_to_the_registered_floor_scope() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let mut scope = project_scope(3, "f08", &"d".repeat(64));
    scope["project_scope"]["component_discovery"] = json!("all_layers");
    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Applied);
    assert_eq!(
        facts
            .snapshot
            .project_scope
            .as_ref()
            .unwrap()
            .component_discovery,
        Some(ComponentDiscovery::AllLayers)
    );
    let mut changed = scope.clone();
    changed["kind"] = json!("proposal");
    changed["fact_sequence"] = json!(4);
    changed["project_scope"]["component_discovery"] = json!("configured_layers");
    assert_eq!(facts.apply_verified_fact(&changed), FactProjection::Invalid);
    changed["project_scope"]["component_discovery"] = json!("unverified_mode");
    assert!(project::scope_fact(&changed).is_none());
    scope["project_scope"]
        .as_object_mut()
        .unwrap()
        .remove("component_discovery");
    assert_eq!(
        project::scope_fact(&scope).unwrap().component_discovery,
        None
    );
}

#[test]
fn lost_project_scope_rejects_proposal_until_read_only_scope_and_proposal_recovery() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let scope = project_scope(3, "f08", &"d".repeat(64));
    let binding = proposal_fact(&scope).unwrap().binding;
    let mut inspected = scope.clone();
    inspected["kind"] = json!("proposal");
    inspected["fact_sequence"] = json!(5);
    assert_eq!(
        facts.apply_verified_fact(&inspected),
        FactProjection::Invalid
    );
    facts.synchronization_failed("proposal", 5, "floor scope response was lost".into());
    assert!(facts.snapshot.proposal.is_none());
    assert!(!facts.can_authorize(&binding));

    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Applied);
    facts.synchronization_finished(&private_receipt(&scope));
    assert!(!facts.can_authorize(&binding));
    assert!(facts.snapshot.fact_sync_notice.is_some());
    assert_eq!(
        facts.apply_verified_fact(&inspected),
        FactProjection::Applied
    );
    facts.synchronization_finished(&private_receipt(&inspected));
    assert!(facts.snapshot.fact_sync_notice.is_none());
    assert!(facts.can_authorize(&binding));
}

#[test]
fn selected_project_scopes_bind_structure_approval_and_reject_other_floor_or_stale_facts() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let first = project_scope(3, "f08", &"d".repeat(64));
    assert_eq!(facts.apply_verified_fact(&first), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert!(facts.can_authorize(&binding));
    assert!(facts.snapshot.proposal.as_ref().unwrap().objects[0].contains("墙"));
    let second = project_scope(6, "f09.b", &"e".repeat(64));
    assert_eq!(facts.apply_verified_fact(&second), FactProjection::Applied);
    assert!(!facts.can_authorize(&binding));
    let current = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    let mut late = first.clone();
    late["kind"] = json!("proposal");
    late["fact_sequence"] = json!(100);
    assert_eq!(facts.apply_verified_fact(&late), FactProjection::Invalid);
    assert_eq!(
        facts.apply_verified_fact(&first),
        FactProjection::Superseded
    );
    assert!(facts.can_authorize(&current));
    let mut malformed = second.clone();
    malformed["fact_sequence"] = json!(999);
    malformed["project_scope"]["floor_key"] = json!("unknown");
    assert_eq!(
        facts.apply_verified_fact(&malformed),
        FactProjection::Invalid
    );
    malformed = second.clone();
    malformed["fact_sequence"] = json!(999);
    let mut wall: Value = serde_json::from_str(
        malformed["proposal"]["draft"]["operations"][0]["value_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    wall["height"] = json!(-3);
    malformed["proposal"]["draft"]["operations"][0]["value_json"] = json!(wall.to_string());
    assert_eq!(
        facts.apply_verified_fact(&malformed),
        FactProjection::Invalid
    );
    assert!(facts.can_authorize(&current));
    let mut changed = project_selection(7, true);
    changed["selection"]["selection_id"] = json!("f".repeat(64));
    assert_eq!(facts.apply_verified_fact(&changed), FactProjection::Applied);
    assert!(facts.snapshot.project_scope.is_none());
    assert!(!facts.can_authorize(&current));
}

fn project_building(sequence: u64, build_id: &str, state: &str, valid: Option<bool>) -> Value {
    json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx","ok":true,
        "kind":"project_building","fact_sequence":sequence,"project_building":{
        "contract":"project-building/v1","build_id":build_id,
        "input_binding":"d".repeat(64),"input_sha256":"e".repeat(64),
        "state":state,"current":true,"valid":valid,"output":"building/model.glb",
        "source_reconciliation":"attention","connection_check":"not_run","human_review":"not_run",
        "artifact_hashes": {".glb":"f".repeat(64),".blend":"f".repeat(64),".validation.json":"f".repeat(64)},
        "checks":{"built":"passed","rebuilt":"passed","glb_roundtrip":"passed"},
        "inputs":{"selection_id":"c".repeat(64),"catalog_id":"a".repeat(64),"source_sha256":"b".repeat(64),
        "floors":[{"floor_key":"f08","floor_id":"floor.f08","scope_id":"d".repeat(64),"scene_sha256":"e".repeat(64),
                   "source_counts":{"unresolved":1},"source_total":1},
                  {"floor_key":"f09.b","floor_id":"floor.f09.b","scope_id":"f".repeat(64),"scene_sha256":"e".repeat(64),
                   "source_counts":{"built":1},"source_total":1}]}}})
}

#[test]
fn building_jobs_bind_full_selection_and_never_reuse_another_jobs_success_or_grant_approval() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    assert_eq!(
        facts.apply_verified_fact(&project_building(3, "first", "built", Some(true))),
        FactProjection::Applied
    );
    assert_eq!(
        facts.apply_verified_fact(&project_building(4, "second", "unknown", None)),
        FactProjection::Applied
    );
    assert_eq!(facts.snapshot.buildings.len(), 2);
    assert_eq!(facts.snapshot.buildings[0].valid, Some(true));
    assert_eq!(facts.snapshot.buildings[1].valid, None);
    assert!(facts.snapshot.proposal.is_none());
    let valid = project_building(50, "first", "built", Some(true));
    for (pointer, value) in [
        (
            "/project_building/inputs/selection_id",
            json!("9".repeat(64)),
        ),
        ("/project_building/inputs/floors/1/floor_key", json!("f08")),
        (
            "/project_building/inputs/floors/1/floor_id",
            json!("floor.f08"),
        ),
        ("/project_building/source_reconciliation", json!("complete")),
        ("/project_building/checks/glb_roundtrip", json!("not_run")),
        ("/project_building/state", json!("building")),
        ("/project_building/input_binding", json!("8".repeat(64))),
    ] {
        let mut malformed = valid.clone();
        *malformed.pointer_mut(pointer).unwrap() = value;
        assert_eq!(
            facts.apply_verified_fact(&malformed),
            FactProjection::Invalid,
            "{pointer}"
        );
    }
    assert_eq!(
        facts.apply_verified_fact(&project_building(5, "first", "built", Some(false))),
        FactProjection::Applied
    );
    assert_eq!(facts.snapshot.buildings[0].valid, Some(false));
    assert_eq!(
        facts.apply_verified_fact(&project_building(3, "first", "built", Some(true))),
        FactProjection::Superseded
    );
    let mut selected = project_selection(6, true);
    selected["selection"]["selection_id"] = json!("7".repeat(64));
    facts.apply_verified_fact(&selected);
    assert!(facts.snapshot.buildings.iter().all(|b| !b.current));
}

#[test]
fn drawing_audit_pages_are_scope_bound_and_never_mean_ai_review_or_scene_approval() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let scope = project_scope(3, "f08", &"d".repeat(64));
    facts.apply_verified_fact(&scope);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    let mut audit = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx","ok":true,
        "kind":"drawing_audit","fact_sequence":4,"project_scope":scope["project_scope"],
        "drawing_audit":{"audit_id":"a".repeat(64),"scene_sha256":"b".repeat(64),"candidate_count":2,
        "offset":0,"next_offset":1,"candidates":[{"id":"gap.1","detector":"wall_gap","message":"gap requires review"}],
        "ai_review":"not_run","scene_application":"not_run"}});
    assert_eq!(facts.apply_verified_fact(&audit), FactProjection::Applied);
    assert_eq!(
        facts.snapshot.drawing_audit.as_ref().unwrap().next_offset,
        Some(1)
    );
    assert!(facts.can_authorize(&binding));
    audit["drawing_audit"]["next_offset"] = json!(null);
    audit["fact_sequence"] = json!(99);
    assert_eq!(facts.apply_verified_fact(&audit), FactProjection::Invalid);
    facts.apply_verified_fact(&project_scope(5, "f09.b", &"f".repeat(64)));
    assert!(facts.snapshot.drawing_audit.is_none());
    assert_eq!(facts.apply_verified_fact(&audit), FactProjection::Invalid);
    assert!(!facts.can_authorize(&binding));
}

#[test]
fn drawing_correction_projects_every_decision_bound_edit_without_becoming_cad_initialization() {
    let mut value = envelope();
    value["proposal"]["drawing_review"] =
        json!({"contract":"drawing-correction/1","base_sha256":value["proposal"]["base_sha256"]});
    value["proposal"]["scope"] = json!({"floor_id":"floor"});
    value["proposal"]["decision_drafts"] = json!([{"id":"drawing-123", "title":"reviewed candidate", "body":"Agent observation; DXF unchanged; exact approval required"}]);
    value["proposal"]["draft"]["operations"] = json!([{"op":"set","target_id":"wall.1","floor_id":"floor","field":"thickness","value_json":"0.2","basis":"decision","decision_id":"drawing-123","source_ids":["dxf.wall"],"reason":"explicit reviewed deviation"}]);
    value["proposal"]["summary"] = json!("中文判断解释".repeat(500));
    let fact = proposal_fact(&value).unwrap();
    assert!(fact.ready);
    let display = fact.objects.join("\n");
    assert!(display.contains("DXF unchanged"));
    assert!(display.contains("wall.1 · 修改 thickness = 0.2"));
    assert!(display.contains("dxf.wall"));
    for key in ["basis", "decision_id", "floor_id"] {
        let mut wrong = value.clone();
        wrong["proposal"]["draft"]["operations"][0][key] = json!("other");
        assert!(proposal_fact(&wrong).is_none(), "{key}");
    }
    value["proposal"]["decision_drafts"] = json!([]);
    assert!(proposal_fact(&value).is_none());
}

#[test]
fn drawing_judgements_retain_actor_image_and_unapproved_scene_status() {
    let mut value = json!({"ok":true,"drawing_audit":{
        "audit_id":"a".repeat(64),"scene_sha256":"b".repeat(64),
        "candidate_count":1,"judged_count":1,"ai_review":"recorded","human_review":"not_run",
        "scene_application":"not_run","offset":0,"next_offset":null,
        "candidates":[{"id":"drw.gap.1","detector":"gap","message":"gap candidate",
        "judgement":{"audit_id":"a".repeat(64),"candidate_id":"drw.gap.1",
        "scene_sha256":"b".repeat(64),"image_sha256":"c".repeat(64),"actor":"platform_agent",
        "human_review":"not_run","scene_application":"not_run",
        "judgement":{"verdict":"doubt","explanation":"ambiguous line junction"}}}]}});
    let fact = project::drawing_audit_fact(&value).unwrap();
    assert_eq!(fact.judged_count, 1);
    assert!(fact.rows[0].contains("Agent doubt: ambiguous line junction"));
    let long_explanation = "中文判断解释".repeat(500);
    value["drawing_audit"]["candidates"][0]["judgement"]["judgement"]["explanation"] =
        json!(long_explanation);
    assert!(project::drawing_audit_fact(&value).is_some());
    let mut oversized = value.clone();
    oversized["drawing_audit"]["candidates"][0]["judgement"]["judgement"]["explanation"] =
        json!("字".repeat(4001));
    assert!(project::drawing_audit_fact(&oversized).is_none());
    for (pointer, changed) in [
        ("/drawing_audit/judged_count", json!(0)),
        ("/drawing_audit/ai_review", json!("passed")),
        ("/drawing_audit/human_review", json!("passed")),
        (
            "/drawing_audit/candidates/0/judgement/image_sha256",
            json!("bad"),
        ),
        (
            "/drawing_audit/candidates/0/judgement/candidate_id",
            json!("other"),
        ),
        (
            "/drawing_audit/candidates/0/judgement/judgement/verdict",
            json!("passed"),
        ),
    ] {
        let mut wrong = value.clone();
        *wrong.pointer_mut(pointer).unwrap() = changed;
        assert!(project::drawing_audit_fact(&wrong).is_none(), "{pointer}");
    }
    value["drawing_audit"]["candidates"][0]["judgement"] = Value::Null;
    assert!(project::drawing_audit_fact(&value).is_none());
}

#[test]
fn drawing_audit_validates_and_projects_every_candidate_in_a_two_hundred_item_page() {
    let candidates: Vec<_> = (0..200)
        .map(|i| json!({"id":format!("drw.gap.{i}"),"detector":"gap","message":"review required"}))
        .collect();
    let mut value = json!({"ok":true,"drawing_audit":{"audit_id":"a".repeat(64),"scene_sha256":"b".repeat(64),"candidate_count":200,"judged_count":0,"ai_review":"not_run","scene_application":"not_run","offset":0,"next_offset":null,"candidates":candidates}});
    let fact = project::drawing_audit_fact(&value).unwrap();
    assert_eq!(fact.rows.len(), 200);
    assert!(fact.rows.last().unwrap().contains("drw.gap.199"));
    value["drawing_audit"]["candidates"][199]["message"] = Value::Null;
    assert!(project::drawing_audit_fact(&value).is_none());
}

#[test]
fn project_component_source_layers_are_explicit_bounded_and_not_inferred_from_catalog_count() {
    let mut value = project_catalog(1);
    assert!(project::catalog_fact(&value)
        .unwrap()
        .component_source_layers
        .is_none());
    value["project"]["component_source_policy"] =
        json!({"layers":["USER-FIXTURES","PL-家具"],"rules":[]});
    assert_eq!(
        project::catalog_fact(&value)
            .unwrap()
            .component_source_layers
            .unwrap(),
        vec!["USER-FIXTURES", "PL-家具"]
    );
    for layers in [
        json!([]),
        json!(["DUPLICATE", "DUPLICATE"]),
        json!([""]),
        json!([12]),
    ] {
        value["project"]["component_source_policy"]["layers"] = layers;
        assert!(project::catalog_fact(&value).is_none());
    }
}

#[test]
fn source_group_assessment_retains_the_backend_unicode_character_limit() {
    let mut value = json!({"total_groups":1,"total_objects":2,"assessed_groups":1,"scan_id":"scan","scene_sha256":"scene",
        "state_counts":{"pending_confirmation":2},"groups":[{"group_id":"group.same","block":"示意图块","instance_count":2,
        "instances":[{"source_id":"a"},{"source_id":"b"}],"category_counts":{"unresolved":2},"state_counts":{"pending_confirmation":2},
        "assessment":{"reason":"中文判断".repeat(500)}}]});
    let fact = source_groups_fact(&value).unwrap();
    assert_eq!(fact.assessed_groups, 1);
    assert!(fact
        .rows
        .iter()
        .any(|row| row.contains(&"中文判断".repeat(500))));
    value["groups"][0]["assessment"]["reason"] = json!("字".repeat(2001));
    assert!(source_groups_fact(&value).is_none());
}

fn project_building_report(sequence: u64) -> Value {
    let mut value = project_building(sequence, "reported", "built", Some(true));
    let connection = json!({"contract":"project-building-connections/v1","state":"attention",
        "adjacent_pair_count":1,"covered_pair_count":0,"declared_count":0,"verified_count":0,"verified":[],
        "issues":[{"code":"adjacent_connection_missing","message":"no source-bound connection"}]});
    value["project_building"]["connection_check"] = json!("attention");
    value["project_building"]["connection_review"] = connection.clone();
    value["project_building"]["report"] = json!({"contract":"project-building-report/v1","build_id":"reported",
        "input_binding":"d".repeat(64),"input_sha256":"e".repeat(64),"current":true,
        "state":"attention","technical_status":"attention","artifact_valid":true,"human_review":"not_run",
        "source_counts":{"built":1,"unresolved":1},"source_total":2,"unresolved_source_count":1,
        "drawing_review":{"candidate_count":0,"judged_count":0,"unresolved_count":0,"missing_floors":["f08","f09.b"]},
        "connections":connection});
    value
}

#[test]
fn building_reports_retain_explicit_gaps_across_inspection_without_approving_a_scene() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let value = project_building_report(3);
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    let report = facts.snapshot.buildings[0].report.clone().unwrap();
    assert_eq!(report.unresolved_sources, 1);
    assert_eq!(report.missing_floor_reviews, 2);
    assert_eq!(report.connection_issues, 1);
    assert!(facts.snapshot.proposal.is_none());
    assert_eq!(
        facts.apply_verified_fact(&project_building(4, "reported", "built", None)),
        FactProjection::Applied
    );
    assert_eq!(facts.snapshot.buildings[0].report, Some(report));
    assert_eq!(facts.snapshot.buildings[0].connection_check, "attention");
    for (pointer, changed) in [
        ("/project_building/report/source_total", json!(0)),
        ("/project_building/report/unresolved_source_count", json!(0)),
        ("/project_building/report/human_review", json!("passed")),
        ("/project_building/report/state", json!("complete")),
        (
            "/project_building/report/technical_status",
            json!("complete"),
        ),
        (
            "/project_building/connection_review/verified_count",
            json!(1),
        ),
        ("/project_building/connection_check", json!("passed")),
    ] {
        let mut wrong = value.clone();
        wrong["fact_sequence"] = json!(99);
        *wrong.pointer_mut(pointer).unwrap() = changed;
        assert_eq!(
            facts.apply_verified_fact(&wrong),
            FactProjection::Invalid,
            "{pointer}"
        );
    }
}

#[test]
fn technical_building_checks_require_bound_connection_proof_and_keep_human_review_unrun() {
    let mut value = project_building_report(3);
    let record = &mut value["project_building"];
    for floor in record["inputs"]["floors"].as_array_mut().unwrap() {
        floor["source_counts"] = json!({"built":1});
        floor["drawing_review"] = json!({"candidate_count":0,"judged_count":0,"verdict_counts":{}});
    }
    record["source_reconciliation"] = json!("complete");
    record["inputs"]["connection_sources_sha256"] = json!("a".repeat(64));
    let connection = json!({"contract":"project-building-connections/v1","state":"passed","adjacent_pair_count":1,
        "covered_pair_count":1,"declared_count":1,"verified_count":1,"issues":[],
        "verified":[{"component_id":"stairs","source_id":"source.stairs","kind":"stair","served_floors":["floor.f08","floor.f09.b"],"part_ids":["stairs.step.01"]}]});
    record["connection_check"] = json!("passed");
    record["connection_review"] = connection.clone();
    record["report"]["connections"] = connection;
    record["report"]["technical_status"] = json!("complete");
    record["report"]["source_counts"] = json!({"built":2});
    record["report"]["unresolved_source_count"] = json!(0);
    record["report"]["drawing_review"]["missing_floors"] = json!([]);
    let selection = project::selection_fact(&project_selection(2, true)).unwrap();
    let fact = project::building_fact(&value, &selection).unwrap();
    assert_eq!(fact.report.as_ref().unwrap().technical_status, "complete");
    assert_eq!(fact.report.as_ref().unwrap().state, "attention");
    value["project_building"]["inputs"]["connection_sources_sha256"] = Value::Null;
    assert!(project::building_fact(&value, &selection).is_none());
}

fn sealed_source_view(mut value: Value, hash: char) -> Value {
    value["source_view_contract_version"] = json!("source-view/1");
    value["source_view_sha256"] = json!(hash.to_string().repeat(64));
    value["scan_id"] = json!("a".repeat(64));
    value["scene_sha256"] = json!("b".repeat(64));
    value
}

#[test]
fn identical_sealed_source_state_preserves_groups_during_instance_pagination() {
    let mut facts = sequenced_facts();
    let groups = sealed_source_view(grouped_source_view(43), 'c');
    let ledger = sealed_source_view(instance_source_view(59), 'c');
    assert_eq!(facts.apply_verified_fact(&groups), FactProjection::Applied);
    assert_eq!(facts.apply_verified_fact(&ledger), FactProjection::Applied);
    assert!(
        facts.snapshot.groups.is_some(),
        "equivalent instance pages erased group assessments"
    );
    assert!(facts.snapshot.ledger.is_some());
    let newer = sealed_source_view(instance_source_view(65), 'd');
    assert_eq!(facts.apply_verified_fact(&newer), FactProjection::Applied);
    assert!(facts.snapshot.groups.is_none());
    assert_eq!(
        facts.apply_verified_fact(&groups),
        FactProjection::Superseded
    );
}

#[test]
fn older_equivalent_source_shape_can_be_restored_without_lowering_state_watermark() {
    let mut facts = sequenced_facts();
    let groups = sealed_source_view(grouped_source_view(43), 'c');
    let ledger = sealed_source_view(instance_source_view(59), 'c');
    facts.apply_verified_fact(&ledger);
    assert_eq!(facts.apply_verified_fact(&groups), FactProjection::Applied);
    assert!(facts.snapshot.groups.is_some());
    assert!(facts.snapshot.ledger.is_some());
    assert_eq!(
        facts.apply_verified_fact(&sealed_source_view(instance_source_view(55), 'd')),
        FactProjection::Superseded
    );
    assert_eq!(
        facts.apply_verified_fact(&instance_source_view(58)),
        FactProjection::Superseded
    );
    let mut malformed = sealed_source_view(instance_source_view(80), 'c');
    malformed["objects"][0]["state"] = json!("invalid");
    assert_eq!(
        facts.apply_verified_fact(&malformed),
        FactProjection::Invalid
    );
    assert!(facts.snapshot.groups.is_some());
    malformed = sealed_source_view(instance_source_view(81), 'c');
    malformed["source_view_sha256"] = json!("broken");
    assert_eq!(
        facts.apply_verified_fact(&malformed),
        FactProjection::Invalid
    );
}

#[test]
fn project_activation_rejects_previous_floor_incarnation_even_with_newer_sequence() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let mut first = project_scope(3, "f08", &"d".repeat(64));
    first["project_scope"]["activation_revision"] = json!(1);
    assert_eq!(facts.apply_verified_fact(&first), FactProjection::Applied);
    let mut second = project_scope(6, "f09.b", &"e".repeat(64));
    second["project_scope"]["activation_revision"] = json!(2);
    assert_eq!(facts.apply_verified_fact(&second), FactProjection::Applied);
    let mut restored = first.clone();
    restored["fact_sequence"] = json!(7);
    restored["project_scope"]["activation_revision"] = json!(3);
    assert_eq!(
        facts.apply_verified_fact(&restored),
        FactProjection::Applied
    );
    let mut late = first.clone();
    late["kind"] = json!("proposal");
    late["fact_sequence"] = json!(100);
    assert_eq!(facts.apply_verified_fact(&late), FactProjection::Invalid);
    late["kind"] = json!("project_scope");
    assert_eq!(facts.apply_verified_fact(&late), FactProjection::Invalid);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert!(facts.can_authorize(&binding));
    for invalid in [json!(0), json!(-1), json!("3"), json!(null)] {
        let mut malformed = restored.clone();
        malformed["fact_sequence"] = json!(101);
        malformed["project_scope"]["activation_revision"] = invalid;
        assert_eq!(
            facts.apply_verified_fact(&malformed),
            FactProjection::Invalid
        );
    }
    let mut legacy = restored.clone();
    legacy["fact_sequence"] = json!(102);
    legacy["project_scope"]
        .as_object_mut()
        .unwrap()
        .remove("activation_revision");
    assert_eq!(facts.apply_verified_fact(&legacy), FactProjection::Invalid);
    assert!(facts.can_authorize(&binding));
}

#[test]
fn project_connection_scope_requires_verified_building_and_selected_adjacent_reference() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let mut scope = project_scope(4, "f08", &"9".repeat(64));
    scope["project_structure"] = json!(false);
    scope["project_scope"]["activation_revision"] = json!(1);
    scope["project_scope"]["assembly_binding"] = json!({
        "contract":"project-connection-scope/v1", "build_id":"base",
        "input_binding":"d".repeat(64), "upper_floor_key":"f09.b"
    });
    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Invalid);
    facts.apply_verified_fact(&project_building(3, "base", "built", Some(true)));
    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert!(facts.can_authorize(&binding));
    let mut late = scope.clone();
    late["kind"] = json!("proposal");
    late["fact_sequence"] = json!(99);
    late["project_scope"]["assembly_binding"]["input_binding"] = json!("8".repeat(64));
    assert_eq!(facts.apply_verified_fact(&late), FactProjection::Invalid);
    for (key, value) in [
        ("contract", json!("unsupported")),
        ("upper_floor_key", json!("f08")),
        ("upper_floor_key", json!("unselected")),
        ("input_binding", json!("bad")),
        ("build_id", json!("unknown")),
    ] {
        let mut invalid = scope.clone();
        invalid["fact_sequence"] = json!(100);
        invalid["project_scope"]["assembly_binding"][key] = value;
        assert_eq!(
            facts.apply_verified_fact(&invalid),
            FactProjection::Invalid,
            "{key}"
        );
    }
    let mut parent = project_building(5, "base", "built", Some(true));
    parent["project_building"]["current"] = json!(false);
    assert_eq!(facts.apply_verified_fact(&parent), FactProjection::Applied);
    assert!(!facts.can_authorize(&binding));
}

#[test]
fn connection_authority_does_not_invalidate_original_floor_building_but_tracks_its_own_branch() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    facts.apply_verified_fact(&project_building(3, "base", "built", Some(true)));
    let mut scope = project_scope(4, "f08", &"9".repeat(64));
    scope["project_structure"] = json!(false);
    scope["project_scope"]["activation_revision"] = json!(1);
    scope["project_scope"]["assembly_binding"] = json!({"contract":"project-connection-scope/v1",
        "build_id":"base","input_binding":"d".repeat(64),"upper_floor_key":"f09.b"});
    facts.apply_verified_fact(&scope);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    let mut operation = operation_envelope("applied", "not_required", 5);
    operation["project_scope"] = scope["project_scope"].clone();
    operation["receipt"]["proposal_id"] = json!(binding.proposal_id);
    operation["receipt"]["revision"] = json!(binding.revision);
    operation["receipt"]["prepare_token"] = json!(binding.prepare_token);
    operation["receipt"]["base_sha256"] = json!(binding.base_sha256);
    operation["receipt"]["candidate_sha256"] = json!(binding.candidate_sha256);
    assert_eq!(
        facts.apply_verified_fact(&operation),
        FactProjection::Applied
    );
    assert!(facts.snapshot.buildings[0].current);
    let mut connected = project_building(6, "connected", "built", Some(true));
    connected["project_building"]["inputs"]["connection_authority"] = json!({
        "scope_id":"9".repeat(64), "scene_sha256":"e".repeat(64),
        "parent_build_id":"base", "parent_input_binding":"d".repeat(64),
        "proposal_id":"connection", "revision":1, "prepare_token":"f".repeat(64),
        "authorization_id":"grant", "operation_id":"operation"
    });
    assert_eq!(
        facts.apply_verified_fact(&connected),
        FactProjection::Applied
    );
    operation["fact_sequence"] = json!(7);
    operation["receipt"]["current_sha256"] = json!("7".repeat(64));
    assert_eq!(
        facts.apply_verified_fact(&operation),
        FactProjection::Applied
    );
    assert!(facts.snapshot.buildings[0].current);
    assert!(!facts.snapshot.buildings[1].current);
    for (field, invalid) in [
        ("scope_id", json!("bad")),
        ("scene_sha256", json!("0".repeat(64))),
        ("revision", json!(0)),
        ("prepare_token", json!("bad")),
    ] {
        let mut malformed = connected.clone();
        malformed["fact_sequence"] = json!(100);
        malformed["project_building"]["inputs"]["connection_authority"][field] = invalid;
        assert_eq!(
            facts.apply_verified_fact(&malformed),
            FactProjection::Invalid
        );
    }
}

fn project_location(sequence: u64) -> Value {
    json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx","ok":true,
        "kind":"project_location","fact_sequence":sequence,"project_location":{
        "contract":"project-floor-location/v1","location_id":"e".repeat(64),
        "catalog_id":"a".repeat(64),"selection_id":"c".repeat(64),
        "floor_key":"f08","source_sha256":"b".repeat(64),"status":"needs_wall_layers",
        "authorization":"not_granted","scene_application":"not_run",
        "layer_statistics":[{"layer":"AUTHORED-ENCLOSURE","line_entities":24}]}})
}

fn project_layer_choice_scope(sequence: u64) -> Value {
    let mut scope = project_scope(sequence, "f08", &"d".repeat(64));
    scope["project_scope"]["wall_layer_choice"] = json!({
        "expected_location_id":"e".repeat(64),"wall_layers":["AUTHORED-ENCLOSURE"],
        "reason":"paired source faces; isolated trial"});
    scope
}

#[test]
fn floor_location_is_source_bound_evidence_not_scene_authorization() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    assert_eq!(
        facts.apply_verified_fact(&project_location(3)),
        FactProjection::Invalid
    );
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let scope = project_layer_choice_scope(4);
    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Invalid);
    assert_eq!(
        facts.apply_verified_fact(&project_location(3)),
        FactProjection::Applied
    );
    assert!(facts.snapshot.proposal.is_none());
    assert!(facts.snapshot.project_scope.is_none());
    for (key, value) in [
        ("catalog_id", json!("f".repeat(64))),
        ("selection_id", json!("f".repeat(64))),
        ("source_sha256", json!("f".repeat(64))),
        ("floor_key", json!("f09.a")),
        ("authorization", json!("granted")),
        ("scene_application", json!("applied")),
        ("status", json!("confirmed")),
        ("layer_statistics", json!([{"layer":"A","line_entities":0}])),
        (
            "layer_statistics",
            json!([{"layer":"A","line_entities":2},{"layer":"A","line_entities":3}]),
        ),
    ] {
        let mut invalid = project_location(99);
        invalid["project_location"][key] = value;
        assert_eq!(
            facts.apply_verified_fact(&invalid),
            FactProjection::Invalid,
            "{key}"
        );
    }
    for (key, value) in [
        ("expected_location_id", json!("f".repeat(64))),
        ("wall_layers", json!(["MADE-UP"])),
        (
            "wall_layers",
            json!(["AUTHORED-ENCLOSURE", "AUTHORED-ENCLOSURE"]),
        ),
        ("reason", json!("")),
    ] {
        let mut invalid = scope.clone();
        invalid["fact_sequence"] = json!(99);
        invalid["project_scope"]["wall_layer_choice"][key] = value;
        assert_eq!(
            facts.apply_verified_fact(&invalid),
            FactProjection::Invalid,
            "{key}"
        );
    }
    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert!(facts.can_authorize(&binding));
    let mut changed = project_selection(5, true);
    changed["selection"]["selection_id"] = json!("f".repeat(64));
    assert_eq!(facts.apply_verified_fact(&changed), FactProjection::Applied);
    assert!(facts.snapshot.project_location.is_none());
    assert!(!facts.can_authorize(&binding));
}

#[test]
fn configured_location_and_wrong_floor_mode_never_accept_layer_override() {
    let mut floor = sequenced_facts();
    assert_eq!(
        floor.apply_verified_fact(&project_location(3)),
        FactProjection::Invalid
    );
    let mut setup = floor.setup.take().unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let mut location = project_location(3);
    location["project_location"]["status"] = json!("configured");
    assert_eq!(
        facts.apply_verified_fact(&location),
        FactProjection::Applied
    );
    assert_eq!(
        facts.apply_verified_fact(&project_layer_choice_scope(4)),
        FactProjection::Invalid
    );
}

#[test]
fn location_full_layer_evidence_is_bounded_without_losing_choice_identity() {
    let mut location = project_location(3);
    let rows: Vec<_> = (0..45)
        .map(|n| json!({"layer":format!("层-{n}"),"line_entities":n+1}))
        .collect();
    location["project_location"]["layer_statistics"] = json!(rows);
    let projected = project::location_fact(&location).unwrap();
    assert_eq!(projected.layers.len(), 45);
    assert_eq!(projected.rows.len(), 41);
    assert!(projected.rows.last().unwrap().contains("5 层"));
    location["project_location"]["layer_statistics"] = json!((0..4097)
        .map(|n| json!({"layer":format!("L{n}"),"line_entities":1}))
        .collect::<Vec<_>>());
    assert!(project::location_fact(&location).is_none());
}

#[test]
fn layer_choice_preserves_unicode_reason_budget_and_rejects_oversized_reason() {
    let mut scope = project_layer_choice_scope(4);
    scope["project_scope"]["wall_layer_choice"]["reason"] = json!("墙".repeat(500));
    assert!(project::scope_fact(&scope).is_some());
    scope["project_scope"]["wall_layer_choice"]["reason"] = json!("墙".repeat(501));
    assert!(project::scope_fact(&scope).is_none());
}

fn edit_envelope() -> Value {
    let mut value = envelope();
    let old = json!({"id":"bed.1.body","role":"furniture","origin":[2,2,0],"source_ids":[]});
    let new = json!({"id":"bed.1.body","role":"furniture","origin":[4,2,0],"rotation_z_deg":0,
        "scale":[1,1,1],"primitive_id":"primitive.frame","arguments":{"height":0.9},
        "source_ids":[],"metadata":{"component_id":"bed.1"}});
    value["proposal"]["scope"] = json!({"floor_id":"floor.ground","protected_ids":[]});
    let feedback = json!({"feedback_id":"move","action":"revise","object_ids":["bed.1"],"text":"move and reshape applied bed"});
    value["proposal"]["feedback"] = feedback.clone();
    value["proposal"]["component_edit"] = json!({"contract":"selected-component-edit/v1",
        "feedback":feedback,"selected_parts":{"bed.1":[old]},"sources":{},"source_state":{},"removal_state":{},
        "plan":{"selected_component_ids":["bed.1"],"remove_component_ids":[],"direct_components":[],
        "replacements":{"components":[{"component_id":"bed.1"}]}}});
    value["proposal"]["draft"]["operations"] = json!([
        {"op":"remove","target_id":"bed.1.body","floor_id":"floor.ground","collection":"solids", "basis":"assumed","source_ids":[],"reason":"explicit selected movement"},
        {"op":"add","target_id":"bed.1.body","floor_id":"floor.ground","collection":"solids", "basis":"assumed","source_ids":[],"value_json":new.to_string(),"reason":"explicit selected replacement"}
    ]);
    value
}

#[test]
fn applied_component_edit_projects_selection_old_parts_shape_and_parameters_before_approval() {
    let value = edit_envelope();
    let fact = proposal_fact(&value).unwrap();
    assert!(fact.ready);
    for expected in [
        "已应用组件编辑",
        "原位置 [2,2,0]",
        "primitive.frame",
        "0.9",
        "位置 [4,2,0]",
        "移除原部件",
    ] {
        assert!(
            fact.objects.iter().any(|row| row.contains(expected)),
            "{expected}"
        );
    }
    let mut facts = sequenced_facts();
    facts.snapshot.proposal = Some(fact.clone());
    assert!(facts.can_authorize(&fact.binding));
}

#[test]
fn whole_component_removal_is_approvable_only_with_exact_original_parts_and_pending_source_disclosure(
) {
    let mut value = edit_envelope();
    value["proposal"]["component_edit"]["plan"]["remove_component_ids"] = json!(["bed.1"]);
    value["proposal"]["component_edit"]["plan"]["replacements"]["components"] = json!([]);
    value["proposal"]["draft"]["operations"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    let fact = proposal_fact(&value).unwrap();
    assert!(fact.ready);
    assert!(fact.objects.iter().any(|row| row.contains("来源仍待确认")));
    assert!(fact
        .objects
        .iter()
        .any(|row| row.contains("整组件删除 bed.1")));
    value["proposal"]["draft"]["operations"] = json!([]);
    assert!(proposal_fact(&value).is_none());
}

#[test]
fn edit_certificate_rejects_changed_selection_unknown_parts_protected_scope_and_mixed_routes() {
    for fault in [
        "feedback",
        "selection",
        "unknown_remove",
        "protected",
        "floor",
        "missing_remove",
        "duplicate_remove",
        "wrong_outcome",
        "raw_set",
        "structure",
        "source_refs",
        "role",
        "alias",
    ] {
        let mut value = edit_envelope();
        let p = &mut value["proposal"];
        match fault {
            "feedback" => p["feedback"]["text"] = json!("other intent"),
            "selection" => p["component_edit"]["plan"]["selected_component_ids"] = json!(["bed.2"]),
            "unknown_remove" => p["draft"]["operations"][0]["target_id"] = json!("bed.2.body"),
            "protected" => p["scope"]["protected_ids"] = json!(["bed.1.body"]),
            "floor" => p["draft"]["operations"][0]["floor_id"] = json!("floor.other"),
            "missing_remove" => {
                p["draft"]["operations"].as_array_mut().unwrap().remove(0);
            }
            "duplicate_remove" => {
                let op = p["draft"]["operations"][0].clone();
                p["draft"]["operations"].as_array_mut().unwrap().push(op);
            }
            "wrong_outcome" => {
                p["component_edit"]["plan"]["remove_component_ids"] = json!(["bed.2"])
            }
            "raw_set" => p["draft"]["operations"][0]["op"] = json!("set"),
            "structure" => p["structural_source"] = json!({"contract":"cad-structure/v1"}),
            "source_refs" => p["draft"]["operations"][0]["source_ids"] = json!(["invented"]),
            "role" | "alias" => {
                let mut part: Value = serde_json::from_str(
                    p["draft"]["operations"][1]["value_json"].as_str().unwrap(),
                )
                .unwrap();
                if fault == "role" {
                    part["role"] = json!("structural");
                } else {
                    part["metadata"]["component_id"] = json!("bed.2");
                }
                p["draft"]["operations"][1]["value_json"] = json!(part.to_string());
            }
            _ => unreachable!(),
        }
        assert!(proposal_fact(&value).is_none(), "{fault}");
    }
}

#[test]
fn edit_projection_validates_all_parts_after_the_hundredth_row() {
    let mut value = edit_envelope();
    let mut parts = Vec::new();
    let mut selected = Vec::new();
    let mut baseline = serde_json::Map::new();
    for i in 0..3 {
        let component = format!("bed.{i}");
        selected.push(component.clone());
        let mut originals = Vec::new();
        for j in 0..64 {
            let id = format!("{component}.p{j}");
            originals.push(json!({"id":id,"role":"furniture","origin":[2,2,0],"source_ids":[]}));
            parts.push(json!({"op":"remove","target_id":id,"floor_id":"floor.ground","collection":"solids", "basis":"assumed","source_ids":[],"reason":"whole selected removal"}));
        }
        baseline.insert(component, json!(originals));
    }
    value["proposal"]["component_edit"]["plan"]["selected_component_ids"] = json!(selected);
    value["proposal"]["component_edit"]["plan"]["remove_component_ids"] = json!(selected);
    value["proposal"]["component_edit"]["plan"]["replacements"]["components"] = json!([]);
    value["proposal"]["component_edit"]["selected_parts"] = json!(baseline);
    value["proposal"]["component_edit"]["feedback"]["object_ids"] = json!(selected);
    value["proposal"]["feedback"]["object_ids"] = json!(selected);
    value["proposal"]["draft"]["operations"] = json!(parts);
    assert!(proposal_fact(&value).unwrap().ready);
    value["proposal"]["draft"]["operations"][150]["target_id"] = json!("foreign");
    assert!(proposal_fact(&value).is_none());
}
