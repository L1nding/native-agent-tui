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
    let mut facts = DomainFacts::new(None);
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
    let mut facts = DomainFacts::new(None);
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
    let mut facts = DomainFacts::new(None);
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
    let mut facts = DomainFacts::new(None);
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
