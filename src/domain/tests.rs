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
    use crate::observation::ActivityIdentity;
    use crate::protocol::{ObservedToolDetails, ToolCategory};
    use crate::tool_details::{ToolDetailLocator, ToolDetails};
    let setup = DomainSetup {
        context_path: "context".into(),
        repository: ".".into(),
        python: "python".into(),
        run_id: "run".into(),
        context_sha256: "ctx".into(),
        floor_id: "floor".into(),
    };
    let mut facts = DomainFacts::new(Some(setup));
    let mut cache = ToolDetails::default();
    let deliver = |facts: &mut DomainFacts, cache: &mut ToolDetails, id: &str, value: Value| {
        let locator = ToolDetailLocator {
            session_id: "session".into(),
            item_id: id.into(),
            identity: ActivityIdentity {
                agent_id: "root".into(),
                task_id: None,
                attempt_id: None,
                thread_id: Some("root".into()),
                turn_id: Some("turn".into()),
                generation: Some(1),
            },
        };
        cache.observe_completed(
            &locator,
            ToolCategory::Mcp,
            ToolLifecycle::Completed,
            ObservedToolDetails {
                name: Some("modeling_domain/inspect_change".into()),
                result: Some(value.to_string()),
                ..Default::default()
            },
            true,
        );
        facts.observe(&cache.snapshot());
    };
    deliver(&mut facts, &mut cache, "first", envelope());
    let mut current = envelope();
    current["proposal"]["proposal_id"] = json!("new-family");
    current["proposal"]["revision"] = json!(1);
    current["proposal"]["prepare_token"] = json!("new-token");
    current["proposal"]["base_sha256"] = json!("candidate");
    deliver(&mut facts, &mut cache, "new", current);
    let active = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert_eq!(active.proposal_id, "new-family");
    assert_eq!(active.revision, 1);
    let mut stale = envelope();
    stale["current"] = json!(false);
    stale["proposal"]["state"] = json!("superseded");
    deliver(&mut facts, &mut cache, "old-inspect", stale);
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

fn deliver_fact(
    facts: &mut DomainFacts,
    cache: &mut crate::tool_details::ToolDetails,
    id: &str,
    value: Value,
) {
    use crate::observation::ActivityIdentity;
    use crate::protocol::{ObservedToolDetails, ToolCategory};
    use crate::tool_details::ToolDetailLocator;
    let locator = ToolDetailLocator {
        session_id: "session".into(),
        item_id: id.into(),
        identity: ActivityIdentity {
            agent_id: "root".into(),
            task_id: None,
            attempt_id: None,
            thread_id: Some("root".into()),
            turn_id: Some("turn".into()),
            generation: Some(1),
        },
    };
    cache.observe_completed(
        &locator,
        ToolCategory::Mcp,
        ToolLifecycle::Completed,
        ObservedToolDetails {
            name: Some("modeling_domain/inspect_change".into()),
            result: Some(value.to_string()),
            ..Default::default()
        },
        true,
    );
    facts.observe(&cache.snapshot());
}

#[test]
fn delayed_v1_and_v2_with_old_current_flags_cannot_replace_v3_or_reopen_applied_approval() {
    let mut facts = sequenced_facts();
    let mut cache = crate::tool_details::ToolDetails::default();
    let mut v1 = envelope();
    v1["fact_sequence"] = json!(1);
    v1["proposal"]["revision"] = json!(1);
    deliver_fact(&mut facts, &mut cache, "v1", v1.clone());
    let mut v3 = envelope();
    v3["fact_sequence"] = json!(3);
    v3["proposal"]["revision"] = json!(3);
    v3["proposal"]["proposal_id"] = json!("v3");
    v3["proposal"]["prepare_token"] = json!("token3");
    deliver_fact(&mut facts, &mut cache, "v3", v3.clone());
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    deliver_fact(&mut facts, &mut cache, "delayed-v1", v1);
    let mut v2 = envelope();
    v2["fact_sequence"] = json!(2);
    deliver_fact(&mut facts, &mut cache, "delayed-v2", v2);
    assert_eq!(facts.snapshot.proposal.as_ref().unwrap().binding, binding);
    let applied = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"application","fact_sequence":5,"current":true,
        "receipt":{"proposal_id":"v3","prepare_token":"token3","state":"applied"}});
    deliver_fact(&mut facts, &mut cache, "apply", applied);
    deliver_fact(&mut facts, &mut cache, "delayed-v3-prepare", v3);
    assert!(!facts.can_authorize(&binding));
}

#[test]
fn source_ledger_keeps_duplicate_and_capability_states_and_ignores_delayed_pages() {
    let mut facts = sequenced_facts();
    let mut cache = crate::tool_details::ToolDetails::default();
    let current = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx","kind":"source_ledger",
        "scan_id":"scan","scene_sha256":"authority","fact_sequence":8,"objects":[
            {"source_id":"chair.one","category":"chair","state":"built","origin_m":[4,5],"matched_component_ids":["chair.one"]},
            {"source_id":"table.duplicate","category":"table","state":"suspected_duplicate","origin_m":[5,10],"matched_component_ids":["table.a","table.b"]},
            {"source_id":"stair","category":"stair","state":"capability_missing","origin_m":[12,10],"matched_component_ids":[]}]});
    deliver_fact(&mut facts, &mut cache, "scan-new", current.clone());
    let mut stale = current;
    stale["fact_sequence"] = json!(2);
    stale["objects"][0]["state"] = json!("pending_addition");
    deliver_fact(&mut facts, &mut cache, "scan-old", stale);
    let ledger = facts.snapshot.ledger.as_ref().unwrap();
    assert_eq!(ledger.counts.get("built"), Some(&1));
    assert_eq!(ledger.counts.get("suspected_duplicate"), Some(&1));
    assert_eq!(ledger.counts.get("capability_missing"), Some(&1));
    assert!(ledger.rows[1].contains("table.a"));
}

#[test]
fn unrelated_historical_artifact_does_not_poison_current_artifact_sequence() {
    let mut facts = sequenced_facts();
    let mut cache = crate::tool_details::ToolDetails::default();
    let mut proposal = envelope();
    proposal["fact_sequence"] = json!(3);
    deliver_fact(&mut facts, &mut cache, "proposal", proposal);
    let mut artifact = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx","kind":"artifact",
        "fact_sequence":100,"artifact":{"proposal_id":"old","prepare_token":"old","state":"built","output":"old.glb"}});
    deliver_fact(&mut facts, &mut cache, "old-artifact", artifact.clone());
    artifact["fact_sequence"] = json!(4);
    artifact["artifact"]["proposal_id"] = json!("proposal");
    artifact["artifact"]["prepare_token"] = json!("token");
    artifact["artifact"]["output"] = json!("current.glb");
    deliver_fact(&mut facts, &mut cache, "current-artifact", artifact);
    assert!(facts
        .snapshot
        .artifact
        .as_ref()
        .unwrap()
        .contains("current.glb"));
}
