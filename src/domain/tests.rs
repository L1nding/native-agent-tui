use super::*;

pub(crate) fn floor_story_height_proposal() -> Value {
    let mut value = envelope();
    let decision = json!({"request_id":"f02-height-4m","floor_id":"floor.f02.trial",
        "height_m":4.0,"no_split_levels":true,"allow_stair_above_floor":true,
        "decided_at":"2026-10-25","reason":"用户确认沿用 4 m","reference_wall_height_m":3.5});
    let p = &mut value["proposal"];
    p["base_sha256"] = json!("a".repeat(64));
    p["candidate_sha256"] = json!("b".repeat(64));
    p["scope"] = json!({"floor_id":"floor.f02.trial"});
    p["draft"]["operations"] = json!([{"op":"set_floor_story_height",
        "target_id":"floor.f02.trial","floor_id":"floor.f02.trial","collection":null,"field":null,
        "value_json":decision.to_string(),"basis":"decision","source_ids":[],
        "decision_id":"floor-height-f02-height-4m","reason":decision["reason"]}]);
    p["decision_drafts"] = json!([{"id":"floor-height-f02-height-4m",
        "title":"用户楼层层高决定","body":decision.to_string()}]);
    p["component_first_use_requirements"] = json!({"contract":"component-first-use/6",
        "comparison_contract":"component-first-use-comparison/1","required":[],"reused":[]});
    value
}

#[test]
fn parameter_decision_requires_bound_operations_validation_and_reviewed_candidate() {
    let value = floor_story_height_proposal();
    let mut facts = sequenced_facts();
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert!(facts
        .snapshot
        .proposal
        .as_ref()
        .unwrap()
        .objects
        .join("\n")
        .contains("几何不变"));
    assert!(facts.snapshot.first_use_satisfied());
    let mut multiline = value.clone();
    let mut decision: Value = serde_json::from_str(
        multiline["proposal"]["draft"]["operations"][0]["value_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    decision["reason"] = json!("用户确认 4 m\r\n沿用原方案");
    multiline["proposal"]["draft"]["operations"][0]["reason"] = decision["reason"].clone();
    multiline["proposal"]["draft"]["operations"][0]["value_json"] = json!(decision.to_string());
    multiline["proposal"]["decision_drafts"][0]["body"] = json!(decision.to_string());
    assert_eq!(
        sequenced_facts().apply_verified_fact(&multiline),
        FactProjection::Applied
    );
    assert!(!facts.can_authorize(&binding));
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(facts.can_authorize(&binding));
    for key in [
        "proposal_id",
        "revision",
        "prepare_token",
        "base_sha256",
        "candidate_sha256",
    ] {
        let mut stale = binding.clone();
        match key {
            "proposal_id" => stale.proposal_id.push('x'),
            "revision" => stale.revision += 1,
            "prepare_token" => stale.prepare_token.push('x'),
            "base_sha256" => stale.base_sha256 = "c".repeat(64),
            _ => stale.candidate_sha256 = "c".repeat(64),
        }
        assert!(!facts.can_authorize(&stale), "{key}");
    }
    facts.snapshot.artifacts[0].valid = Some(false);
    assert!(!facts.can_authorize(&binding));
    facts.snapshot.artifacts[0].valid = Some(true);
    facts.snapshot.review_images[0].file_state = ImageFileState::Unavailable;
    assert!(!facts.can_authorize(&binding));
    for (path, invalid) in [
        ("/proposal/draft/operations/0/op", json!("set")),
        ("/proposal/draft/operations/0/collection", json!("walls")),
        ("/proposal/draft/operations/0/floor_id", json!("foreign")),
        ("/proposal/draft/operations/0/basis", json!("assumed")),
        ("/proposal/decision_drafts/0/id", json!("foreign")),
        ("/proposal/decision_drafts/0/body", json!("{}")),
        ("/proposal/component_first_use_requirements", Value::Null),
        (
            "/proposal/component_first_use_requirements/required",
            json!([{}]),
        ),
        (
            "/proposal/component_first_use_requirements/reused",
            json!([{}]),
        ),
    ] {
        let mut bad = value.clone();
        *bad.pointer_mut(path).unwrap() = invalid;
        let mut facts = sequenced_facts();
        assert_eq!(
            facts.apply_verified_fact(&bad),
            FactProjection::Invalid,
            "{path}"
        );
        assert!(facts.snapshot.proposal.is_none(), "{path}");
    }
    let mut mixed = value.clone();
    mixed["proposal"]["draft"]["operations"]
        .as_array_mut()
        .unwrap()
        .push(envelope()["proposal"]["draft"]["operations"][0].clone());
    assert_eq!(
        sequenced_facts().apply_verified_fact(&mixed),
        FactProjection::Invalid
    );
    let mut invalid = value;
    invalid["diagnostics"] = json!([{"code":"invalid_candidate"}]);
    let mut facts = sequenced_facts();
    assert_eq!(facts.apply_verified_fact(&invalid), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(!facts.can_authorize(&binding));
}

#[tokio::test]
async fn domain_http_patch_file_has_global_server_and_long_timeout() {
    let path = write_http_mcp_patch(12345).await.unwrap();
    assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "- insert:\n    - id: mcp-modeling-domain\n      name: '@deepseek-ai/dsh-mcp-client'\n      config:\n        serverName: modeling_domain\n        transport: streamable-http\n        url: http://127.0.0.1:12345/mcp\n        toolCallTimeoutMs: 600000\n        failOnStartupError: true\n");
    tokio::fs::remove_file(path).await.unwrap();
}

#[tokio::test]
async fn domain_port_probe_reports_timeout_and_accepts_ready_listener() {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    wait_domain_port(port, std::time::Duration::from_secs(1))
        .await
        .unwrap();
    drop(listener);
    assert_eq!(
        wait_domain_port(port, std::time::Duration::from_millis(100))
            .await
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::TimedOut
    );
}

#[test]
fn domain_poll_projects_new_sealed_facts_and_ignores_tampering_foreign_and_old_sequences() {
    for mode in [DomainMode::Floor, DomainMode::Project] {
        let root = std::env::temp_dir().join(format!(
            "domain-poll-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let directory = root
            .join("outputs/domain/run")
            .join(if mode == DomainMode::Project {
                "project"
            } else {
                "changes"
            });
        std::fs::create_dir_all(directory.join("facts")).unwrap();
        std::fs::write(root.join("context.json"), b"{}").unwrap();
        let mut setup = sequenced_facts().setup.unwrap();
        setup.mode = mode;
        setup.repository = root.clone();
        setup.context_path = root.join("context.json");
        setup.context_sha256 = format!("{:x}", Sha256::digest(b"{}"));
        let mut facts = DomainFacts::new(Some(setup.clone()));
        let mut checked = receipt::FactFiles::new();
        let mut index = serde_json::Map::new();
        for sequence in [2, 4] {
            let mut value = if mode == DomainMode::Project {
                project_catalog(sequence)
            } else {
                envelope()
            };
            value["context_sha256"] = json!(setup.context_sha256);
            value["fact_sequence"] = json!(sequence);
            value["proposal"]["summary"] = json!(format!("sequence-{sequence}"));
            let raw = serde_json::to_vec(&value).unwrap();
            let sha = format!("{:x}", Sha256::digest(&raw));
            std::fs::write(directory.join("facts").join(format!("{sha}.json")), &raw).unwrap();
            index.insert(sha, json!(sequence));
            if sequence == 4 {
                for key in ["run_id", "context_sha256", "contract", "hash", "unindexed"] {
                    let mut invalid = value.clone();
                    invalid["fact_sequence"] = json!(if key == "unindexed" { 12 } else { 10 });
                    if !matches!(key, "hash" | "unindexed") {
                        invalid[key] = json!("foreign");
                    }
                    let raw = serde_json::to_vec(&invalid).unwrap();
                    let sha = if key == "hash" {
                        "0".repeat(64)
                    } else {
                        format!("{:x}", Sha256::digest(&raw))
                    };
                    std::fs::write(directory.join("facts").join(format!("{sha}.json")), raw)
                        .unwrap();
                }
                index.insert("invalid-sequence".into(), json!(10));
            }
            std::fs::write(
                directory.join("fact-sequences.json"),
                json!({"next":11,"facts":index}).to_string(),
            )
            .unwrap();
            let after = facts.highest_sequence;
            let mut applied = Vec::new();
            receipt::scan(&setup, after, &mut checked, |value| {
                applied.push(value["fact_sequence"].as_u64().unwrap());
                assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
                true
            });
            assert_eq!(applied, vec![sequence]);
            assert_eq!(facts.highest_sequence, sequence);
        }
        let snapshot = facts.snapshot.clone();
        receipt::scan(&setup, facts.highest_sequence, &mut checked, |_| {
            panic!("unchanged or old fact replayed")
        });
        assert!(facts.snapshot == snapshot);
        let mut old = if mode == DomainMode::Project {
            project_catalog(3)
        } else {
            envelope()
        };
        old["context_sha256"] = json!(setup.context_sha256);
        old["fact_sequence"] = json!(3);
        old["proposal"]["revision"] = json!(100);
        let raw = serde_json::to_vec(&old).unwrap();
        let sha = format!("{:x}", Sha256::digest(&raw));
        std::fs::write(directory.join("facts").join(format!("{sha}.json")), raw).unwrap();
        index.insert(sha, json!(3));
        index.insert("late-index".into(), json!(12));
        std::fs::write(
            directory.join("fact-sequences.json"),
            json!({"next":13,"facts":index}).to_string(),
        )
        .unwrap();
        let mut applied = Vec::new();
        receipt::scan(&setup, facts.highest_sequence, &mut checked, |value| {
            applied.push(value["fact_sequence"].as_u64().unwrap());
            assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
            true
        });
        assert_eq!(applied, [12]);
        if mode == DomainMode::Floor {
            assert_eq!(
                facts.snapshot.proposal.as_ref().unwrap().binding.revision,
                2
            );
        }
        let mut rescanned = Vec::new();
        receipt::scan(&setup, 0, &mut checked, |value| {
            rescanned.push(value["fact_sequence"].as_u64().unwrap());
            true
        });
        assert_eq!(rescanned, [2, 3, 4, 12]);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
#[ignore = "read-only replay; set DOMAIN_FACT_REPLAY to a sealed facts directory"]
fn replay_sealed_facts_projects_first_use_review() {
    let root = std::env::var_os("DOMAIN_FACT_REPLAY").expect("DOMAIN_FACT_REPLAY");
    let mut values: Vec<Value> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap()
        })
        .collect();
    values.sort_by_key(|value| value["fact_sequence"].as_u64().unwrap());
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    setup.run_id = values.last().unwrap()["run_id"].as_str().unwrap().into();
    setup.context_sha256 = values.last().unwrap()["context_sha256"]
        .as_str()
        .unwrap()
        .into();
    let mut facts = DomainFacts::new(Some(setup));
    for value in &values {
        let result = facts.apply_verified_fact(value);
        eprintln!(
            "{} {}: {result:?}, projection_failure={:?}",
            value["fact_sequence"], value["kind"], facts.projection_failure
        );
        if result == FactProjection::Invalid {
            facts.synchronization_failed(
                value["kind"].as_str().unwrap(),
                value["fact_sequence"].as_u64().unwrap(),
                "projection rejected".into(),
            );
        } else if result == FactProjection::Applied {
            facts.synchronization_finished(&private_receipt(value));
        }
    }
    eprintln!(
        "review={:?}, sync={:?}",
        facts
            .snapshot
            .first_use_review
            .as_ref()
            .map(|r| (&r.status, r.required_count)),
        facts.snapshot.fact_sync_notice
    );
    assert_eq!(
        facts.snapshot.first_use_review.as_ref().unwrap().status,
        "ready"
    );
    assert!(facts.snapshot.fact_sync_notice.is_none());
}

fn envelope() -> Value {
    json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx", "kind":"proposal", "current":true,
        "diagnostics":[],"proposal":{"proposal_id":"proposal", "revision":2,"prepare_token":"token",
        "base_sha256":"base","candidate_sha256":"candidate", "state":"prepared", "summary":"two beds",
        "draft":{"operations":[{"collection":"solids","value_json":json!({"id":"bed.1.body", "origin":[6,2,0],"source_ids":[]}).to_string(),"basis":"assumed"}]},
        "issues":[],"pending":[],"assumed":[{"message":"assumed dimensions"}],"component_first_use_requirements":{"contract":"component-first-use/1","required":[],"reused":[]}}})
}

fn reviewed_candidate(proposal: &ProposalBinding) -> ArtifactFact {
    ArtifactFact {
        binding: ArtifactBinding {
            build_id: format!("review-{}-{}", proposal.proposal_id, proposal.revision),
            proposal_id: proposal.proposal_id.clone(),
            revision: proposal.revision,
            prepare_token: proposal.prepare_token.clone(),
            candidate: true,
            authority_sha256: proposal.base_sha256.clone(),
            input_sha256: proposal.candidate_sha256.clone(),
            output: "candidate.glb".into(),
        },
        state: ArtifactState::Built,
        current: Some(true),
        valid: Some(true),
        reviewed: true,
    }
}

pub(crate) fn add_reviewed_candidate(snapshot: &mut DomainSnapshot, proposal: &ProposalBinding) {
    let artifact = reviewed_candidate(proposal);
    snapshot
        .review_images
        .push(review::test_image(proposal, &artifact.binding.build_id));
    snapshot.artifacts.push(artifact);
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
    add_reviewed_candidate(&mut facts.snapshot, &fact.binding);
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
    add_reviewed_candidate(&mut facts.snapshot, &fact.binding);
    assert!(facts.can_authorize(&fact.binding));
    let mut old = fact.binding.clone();
    old.revision -= 1;
    assert!(!facts.can_authorize(&old));
    facts.snapshot.authorizing = true;
    assert!(!facts.can_authorize(&fact.binding));
}

#[test]
fn pending_cards_keep_the_approvable_card_first_and_route_its_exact_grant() {
    let mut facts = sequenced_facts();
    let mut ready = recovery_proposal();
    ready["fact_sequence"] = json!(1);
    assert_eq!(facts.apply_verified_fact(&ready), FactProjection::Applied);
    let ready_binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &ready_binding);
    assert!(facts.can_authorize(&ready_binding));

    let mut unready = recovery_proposal();
    unready["fact_sequence"] = json!(2);
    unready["proposal"]["proposal_id"] = json!("other-proposal");
    unready["proposal"]["revision"] = json!(1);
    unready["proposal"]["prepare_token"] = json!("other-token");
    unready["proposal"]["base_sha256"] = json!("c".repeat(64));
    unready["proposal"]["candidate_sha256"] = json!("d".repeat(64));
    assert_eq!(facts.apply_verified_fact(&unready), FactProjection::Applied);

    let cards = facts.snapshot.approval_card_snapshots();
    assert_eq!(cards.len(), 2);
    assert_eq!(cards[0].proposal.as_ref().unwrap().binding, ready_binding);
    assert!(facts.snapshot.approval_card_waiting());
    assert!(facts.can_authorize(&ready_binding));
    assert!(!facts.can_authorize(&cards[1].proposal.as_ref().unwrap().binding));

    let mut grant_result = operation_envelope("applied", "not_required", 3);
    grant_result["receipt"]["operation_id"] = json!("ready-card-operation");
    assert_eq!(
        facts.apply_verified_fact(&grant_result),
        FactProjection::Applied
    );
    let ready_card = facts.snapshot.approval_card(&ready_binding).unwrap();
    assert_eq!(
        ready_card.operation.as_ref().unwrap().operation_id,
        "ready-card-operation"
    );
    assert_eq!(
        facts
            .snapshot
            .proposal
            .as_ref()
            .unwrap()
            .binding
            .proposal_id,
        "other-proposal"
    );
    assert_eq!(facts.snapshot.approval_card_snapshots().len(), 1);
}

#[test]
fn legacy_application_routes_to_its_unique_archived_card_and_rejects_ambiguous_tokens() {
    let mut facts = sequenced_facts();
    let mut old = recovery_proposal();
    old["fact_sequence"] = json!(1);
    old["proposal"]["proposal_id"] = json!("old-proposal");
    old["proposal"]["prepare_token"] = json!("old-token");
    assert_eq!(facts.apply_verified_fact(&old), FactProjection::Applied);
    let old_binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();

    let mut current = recovery_proposal();
    current["fact_sequence"] = json!(2);
    current["proposal"]["proposal_id"] = json!("current-proposal");
    current["proposal"]["revision"] = json!(1);
    current["proposal"]["prepare_token"] = json!("current-token");
    current["proposal"]["base_sha256"] = json!("c".repeat(64));
    current["proposal"]["candidate_sha256"] = json!("d".repeat(64));
    assert_eq!(facts.apply_verified_fact(&current), FactProjection::Applied);

    let legacy_application = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"application","fact_sequence":3,
        "receipt":{"proposal_id":"old-proposal","prepare_token":"old-token","state":"applied"}});
    assert_eq!(
        facts.apply_verified_fact(&legacy_application),
        FactProjection::Applied
    );
    let archived = facts.snapshot.approval_card(&old_binding).unwrap();
    assert!(archived
        .application
        .as_deref()
        .is_some_and(|application| application.starts_with("applied · current=")));
    assert!(!facts.shows_binding(&old_binding));
    assert_eq!(facts.snapshot.approval_card_snapshots().len(), 1);
    assert_eq!(
        facts.snapshot.approval_card_snapshots()[0]
            .proposal
            .as_ref()
            .unwrap()
            .binding
            .proposal_id,
        "current-proposal"
    );

    let mut ambiguous = sequenced_facts();
    let first = recovery_proposal();
    assert_eq!(
        ambiguous.apply_verified_fact(&first),
        FactProjection::Applied
    );
    let first_binding = ambiguous
        .snapshot
        .proposal
        .as_ref()
        .unwrap()
        .binding
        .clone();
    ambiguous.remove_approval_card(&first_binding);
    let mut second = recovery_proposal();
    second["fact_sequence"] = json!(2);
    second["proposal"]["revision"] = json!(3);
    second["proposal"]["base_sha256"] = json!("c".repeat(64));
    second["proposal"]["candidate_sha256"] = json!("d".repeat(64));
    assert_eq!(
        ambiguous.apply_verified_fact(&second),
        FactProjection::Applied
    );
    assert_eq!(
        ambiguous.apply_verified_fact(&legacy_application),
        FactProjection::Superseded
    );
}

#[test]
fn superseded_versions_and_explicit_parent_cards_leave_the_pending_set() {
    let mut facts = sequenced_facts();
    let mut first = recovery_proposal();
    first["fact_sequence"] = json!(1);
    assert_eq!(facts.apply_verified_fact(&first), FactProjection::Applied);
    let first_binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();

    let mut newer_version = recovery_proposal();
    newer_version["fact_sequence"] = json!(2);
    newer_version["proposal"]["revision"] = json!(3);
    newer_version["proposal"]["prepare_token"] = json!("new-token");
    newer_version["proposal"]["base_sha256"] = json!("c".repeat(64));
    newer_version["proposal"]["candidate_sha256"] = json!("d".repeat(64));
    assert_eq!(
        facts.apply_verified_fact(&newer_version),
        FactProjection::Applied
    );
    assert!(facts.snapshot.approval_card(&first_binding).is_none());
    let newer_binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert_eq!(facts.snapshot.approval_card_snapshots().len(), 1);
    assert_eq!(
        facts.snapshot.approval_card_snapshots()[0]
            .proposal
            .as_ref()
            .unwrap()
            .binding,
        newer_binding
    );

    let mut independent = recovery_proposal();
    independent["fact_sequence"] = json!(3);
    independent["proposal"]["proposal_id"] = json!("independent");
    independent["proposal"]["revision"] = json!(1);
    independent["proposal"]["prepare_token"] = json!("independent-token");
    independent["proposal"]["base_sha256"] = json!("e".repeat(64));
    independent["proposal"]["candidate_sha256"] = json!("f".repeat(64));
    assert_eq!(
        facts.apply_verified_fact(&independent),
        FactProjection::Applied
    );
    assert_eq!(facts.snapshot.approval_card_snapshots().len(), 2);

    let mut child = recovery_proposal();
    child["fact_sequence"] = json!(4);
    child["proposal"]["proposal_id"] = json!("child");
    child["proposal"]["revision"] = json!(1);
    child["proposal"]["prepare_token"] = json!("child-token");
    child["proposal"]["base_sha256"] = json!("1".repeat(64));
    child["proposal"]["candidate_sha256"] = json!("2".repeat(64));
    child["proposal"]["parent_id"] = json!(newer_binding.proposal_id);
    assert_eq!(facts.apply_verified_fact(&child), FactProjection::Applied);
    assert!(facts.snapshot.approval_card(&newer_binding).is_none());
    assert_eq!(facts.snapshot.approval_card_snapshots().len(), 2);
}

#[test]
fn approved_stale_and_non_approvable_cards_are_not_pending_but_approved_grants_remain_queryable() {
    for disposition in ["approved", "stale", "non_approvable"] {
        let mut facts = sequenced_facts();
        let mut proposal = recovery_proposal();
        proposal["source_current"] = json!(true);
        if disposition == "stale" {
            proposal["source_current"] = json!(false);
        } else if disposition == "non_approvable" {
            proposal["approval_review"] = json!({"status":"proposal_not_approvable"});
        }
        assert_eq!(
            facts.apply_verified_fact(&proposal),
            FactProjection::Applied
        );
        let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
        add_reviewed_candidate(&mut facts.snapshot, &binding);

        if disposition == "approved" {
            facts.remove_approval_card(&binding);
            assert!(facts.snapshot.approval_card(&binding).is_some());
        } else {
            assert!(facts.snapshot.approval_card(&binding).is_some());
        }
        assert!(facts.snapshot.approval_card_snapshots().is_empty());
        assert!(!facts.can_authorize(&binding));
        assert_eq!(facts.shows_binding(&binding), disposition == "approved");
    }
}

#[test]
fn historical_card_invalidation_removes_only_the_matching_pending_card() {
    for disposition in ["source_stale", "not_approvable", "superseded"] {
        let mut facts = sequenced_facts();
        let mut first = recovery_proposal();
        first["fact_sequence"] = json!(1);
        assert_eq!(facts.apply_verified_fact(&first), FactProjection::Applied);
        let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
        add_reviewed_candidate(&mut facts.snapshot, &binding);
        let mut other = recovery_proposal();
        other["fact_sequence"] = json!(2);
        other["proposal"]["proposal_id"] = json!("other");
        other["proposal"]["prepare_token"] = json!("other-token");
        assert_eq!(facts.apply_verified_fact(&other), FactProjection::Applied);
        assert_eq!(facts.snapshot.approval_card_snapshots().len(), 2);

        first["fact_sequence"] = json!(3);
        first["current"] = json!(false);
        match disposition {
            "source_stale" => first["source_current"] = json!(false),
            "not_approvable" => {
                first["approval_review"] = json!({"status":"proposal_not_approvable"})
            }
            _ => first["proposal"]["state"] = json!("superseded"),
        }
        assert_eq!(facts.apply_verified_fact(&first), FactProjection::Applied);
        let cards = facts.snapshot.approval_card_snapshots();
        assert_eq!(cards.len(), 1, "{disposition}");
        assert_eq!(
            cards[0].proposal.as_ref().unwrap().binding.proposal_id,
            "other"
        );
        assert_eq!(
            facts
                .snapshot
                .proposal
                .as_ref()
                .unwrap()
                .binding
                .proposal_id,
            "other"
        );
        assert!(!facts.can_authorize(&binding));
    }
}

#[test]
fn waiting_for_any_approvable_card_still_blocks_auto_continue_during_approval() {
    let mut facts = sequenced_facts();
    let mut ready = recovery_proposal();
    ready["fact_sequence"] = json!(1);
    assert_eq!(facts.apply_verified_fact(&ready), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);

    let mut unready = recovery_proposal();
    unready["fact_sequence"] = json!(2);
    unready["proposal"]["proposal_id"] = json!("later-unready");
    unready["proposal"]["revision"] = json!(1);
    unready["proposal"]["prepare_token"] = json!("later-token");
    unready["proposal"]["base_sha256"] = json!("c".repeat(64));
    unready["proposal"]["candidate_sha256"] = json!("d".repeat(64));
    assert_eq!(facts.apply_verified_fact(&unready), FactProjection::Applied);

    assert!(facts.snapshot.approval_card_waiting());
    facts.snapshot.authorizing = true;
    assert!(!facts.snapshot.approval_card_waiting());
    facts.snapshot.authorizing = false;
    facts.snapshot.fact_sync_notice = Some("同步中".into());
    assert!(!facts.snapshot.approval_card_waiting());
}

#[test]
fn baseline_replay_requires_fresh_reads_for_every_pending_card() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target/codex-tmp")
        .join(format!(
            "domain-baseline-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let directory = root.join("outputs/domain/run/changes");
    std::fs::create_dir_all(directory.join("facts")).unwrap();
    let context = b"{}";
    std::fs::write(root.join("context.json"), context).unwrap();
    let mut setup = sequenced_facts().setup.unwrap();
    setup.repository = root.clone();
    setup.context_path = root.join("context.json");
    setup.context_sha256 = format!("{:x}", Sha256::digest(context));

    let mut index = serde_json::Map::new();
    for (sequence, proposal_id) in [(1, "first"), (2, "second")] {
        let mut proposal = recovery_proposal();
        proposal["context_sha256"] = json!(setup.context_sha256);
        proposal["fact_sequence"] = json!(sequence);
        proposal["source_current"] = json!(true);
        proposal["proposal"]["proposal_id"] = json!(proposal_id);
        proposal["proposal"]["prepare_token"] = json!(format!("{proposal_id}-token"));
        let raw = serde_json::to_vec(&proposal).unwrap();
        let sha = format!("{:x}", Sha256::digest(&raw));
        std::fs::write(directory.join("facts").join(format!("{sha}.json")), raw).unwrap();
        index.insert(sha, json!(sequence));
    }
    std::fs::write(
        directory.join("fact-sequences.json"),
        json!({"next":3,"facts":index}).to_string(),
    )
    .unwrap();

    let facts = DomainFacts::new(Some(setup));
    assert_eq!(facts.snapshot.approval_card_snapshots().len(), 2);
    assert!(!facts.snapshot.proposal.as_ref().unwrap().ready);
    assert!(facts.snapshot.approval_cards.iter().all(|card| {
        card.snapshot
            .proposal
            .as_ref()
            .is_some_and(|proposal| !proposal.ready)
    }));
}

#[test]
fn automatic_approval_requires_current_eligibility_and_the_existing_candidate_review() {
    let mut facts = sequenced_facts();
    let mut value = envelope();
    value["source_current"] = json!(true);
    value["auto_approval"] = json!({"contract":"reviewed-template-approval/1","eligible":true});
    value["proposal"]["component_first_use_requirements"]["reused"] =
        json!([{"review_key":"reviewed"}]);
    deliver_fact(&mut facts, "eligible", value.clone());
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert!(!facts.can_auto_authorize(&binding));
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(facts.can_auto_authorize(&binding));
    let mut stale = binding.clone();
    stale.prepare_token = "different".into();
    assert!(!facts.can_auto_authorize(&stale));
    for (index, pointer, invalid) in [
        (0, "/auto_approval/eligible", json!(false)),
        (1, "/auto_approval/contract", json!("unknown")),
        (2, "/source_current", json!(false)),
        (3, "/diagnostics", json!([{"message":"collision"}])),
        (
            4,
            "/proposal/component_first_use_requirements/reused",
            json!([]),
        ),
    ] {
        let mut blocked = value.clone();
        *blocked.pointer_mut(pointer).unwrap() = invalid;
        deliver_fact(&mut facts, &format!("blocked-{index}"), blocked);
        assert!(!facts.can_auto_authorize(&binding), "{pointer}");
        deliver_fact(&mut facts, &format!("restored-{index}"), value.clone());
        assert!(facts.can_auto_authorize(&binding));
    }
    facts.snapshot.artifacts[0].valid = Some(false);
    assert!(!facts.can_auto_authorize(&binding));
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
    add_reviewed_candidate(&mut facts.snapshot, &active);
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

fn deliver_fact(facts: &mut DomainFacts, id: &str, mut value: Value) {
    // Existing transport tests model successful Core file verification separately.
    if value["artifact"]["review_image"].is_object() {
        let artifact = value["artifact"].clone();
        value["artifact"]["review_image"]["image_path"] =
            json!(std::env::temp_dir().join("candidate.png"));
        value["artifact"]["review_image"]["dimensions_px"] = json!([1, 1]);
        value["artifact"]["review_image"]["request"] = json!({
            "build_id":artifact["build_id"],"proposal_id":artifact["proposal_id"],
            "revision":artifact["revision"],"prepare_token":artifact["prepare_token"],
            "input_sha256":artifact["input_sha256"],"blend_sha256":"b".repeat(64),"focus_m":null});
    }
    let item = json!({"id":id,"type":"mcpToolCall","server":"modeling_domain",
        "tool":"inspect_change","status":"completed","result":{"structuredContent":value}});
    let notice = crate::protocol::decode_observed_tool(
        "item/completed",
        &json!({"threadId":"root","turnId":"turn","item":item}),
    )
    .unwrap()
    .unwrap();
    facts.observe(&notice, &item);
    for image in &mut facts.snapshot.review_images {
        if image.file_state == ImageFileState::Pending {
            image.file_state = ImageFileState::Available;
        }
    }
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
    add_reviewed_candidate(&mut facts.snapshot, &fact.binding);
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

#[test]
fn opening_entity_completion_projects_exact_assets_and_keeps_concrete_approval_binding() {
    let mut value = hosted_proposal();
    let assets = json!({"style":"door","leaf_count":1,"leaf_thickness":0.04});
    value["proposal"]["component_plan"] = json!({"openings":[{
        "operation":"complete-entity", "target_opening_id":"door.1", "component_id":"component.door",
        "source_instance_id":"source.door","kind":"door","host_wall_id":"wall.host",
        "height":2.1,"sill_height":0.0,"assets":assets
    }]});
    value["proposal"]["draft"]["operations"] = json!([{
        "op":"set","target_id":"door.1","collection":"openings","field":"assets",
        "basis":"assumed","source_ids":["source.door"],"value_json":assets.to_string()
    }]);
    let fact = proposal_fact(&value).unwrap();
    assert!(fact.objects[0].contains("door.1 · door 实体补全"));
    assert!(fact.objects[1].contains("保留既有洞口尺寸与位置"));
    assert!(fact.objects[1].contains("assumed"));
    let mut facts = sequenced_facts();
    facts.snapshot.proposal = Some(fact.clone());
    assert!(!facts.can_authorize(&fact.binding));
    add_reviewed_candidate(&mut facts.snapshot, &fact.binding);
    assert!(facts.can_authorize(&fact.binding));
    for (field, invalid) in [
        ("field", json!("width")),
        ("target_id", json!("other")),
        ("basis", json!("user_confirmed")),
    ] {
        let mut bad = value.clone();
        bad["proposal"]["draft"]["operations"][0][field] = invalid;
        assert!(proposal_fact(&bad).is_none(), "{field}");
    }
    value["proposal"]["component_plan"]["openings"][0]["assets"]["leaf_count"] = json!(2);
    assert!(proposal_fact(&value).is_none());
}

fn recovery_proposal() -> Value {
    let mut value = envelope();
    value["fact_sequence"] = json!(1);
    value["proposal"]["base_sha256"] = json!("a".repeat(64));
    value["proposal"]["candidate_sha256"] = json!("b".repeat(64));
    value
}

pub(crate) fn approval_restore_fact(proposal_id: &str, build_id: &str, sequence: u64) -> Value {
    json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"approval_card_restore","ok":true,"fact_sequence":sequence,
        "proposal_id":proposal_id,"build_id":build_id,"status":"ready","missing":[],
        "restore_id":"restore-one","card_ref":"opaque-card-reference",
        "first_use_readers":[],"overview_readers":[],
        "next_call":{"action":"end_turn_for_core_approval"},
        "replayed_facts":[{"kind":"proposal","fact_sequence":sequence-1,"fact_sha256":"a".repeat(64)}]})
}

#[test]
fn restored_card_becomes_current_and_waits_for_approval_through_nested_tool_text() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "prepared", recovery_proposal());
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    let mut other = recovery_proposal();
    other["fact_sequence"] = json!(2);
    other["proposal"]["proposal_id"] = json!("other");
    other["proposal"]["prepare_token"] = json!("other-token");
    deliver_fact(&mut facts, "other", other);
    let other_binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &other_binding);
    assert_eq!(
        facts
            .snapshot
            .selected_approval_card()
            .unwrap()
            .proposal
            .as_ref()
            .unwrap()
            .binding,
        other_binding
    );

    let restored = approval_restore_fact("proposal", "review-proposal-2", 3);
    let item = json!({"id":"restore","type":"mcpToolCall","server":"modeling_domain",
        "tool":"restore_approval_card","status":"completed",
        "result":{"content":[{"type":"text","text":restored.to_string()}]}});
    let notice = crate::protocol::decode_observed_tool(
        "item/completed",
        &json!({"threadId":"root","turnId":"turn","item":item}),
    )
    .unwrap()
    .unwrap();
    facts.observe(&notice, &item);
    assert_eq!(
        facts
            .snapshot
            .selected_approval_card()
            .unwrap()
            .proposal
            .as_ref()
            .unwrap()
            .binding,
        binding
    );
    assert_eq!(facts.snapshot.approval_card_snapshots().len(), 2);
    assert!(facts.snapshot.approval_card_waiting());
    assert!(facts.snapshot.approval_restore_waiting());
    assert!(facts.can_authorize(&binding));
    facts.remove_approval_card(&binding);
    assert!(!facts.snapshot.approval_restore_waiting());
    assert_ne!(
        facts
            .snapshot
            .selected_approval_card()
            .unwrap()
            .proposal
            .as_ref()
            .unwrap()
            .binding,
        binding
    );
}

#[test]
fn restore_missing_evidence_cannot_authorize_even_when_old_card_was_ready() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "prepared", recovery_proposal());
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    let mut restored = approval_restore_fact("proposal", "review-proposal-2", 3);
    restored["build_id"] = json!("not-yet-replayed-build");
    restored["missing"] = json!(["overview_delivery:top"]);
    // A contradictory ready status never overrides missing evidence.
    assert_eq!(
        facts.apply_verified_fact(&restored),
        FactProjection::Applied
    );
    assert!(!facts.can_authorize(&binding));
    assert!(!facts.snapshot.approval_card_waiting());
    assert!(facts.snapshot.approval_restore_waiting());
    assert!(facts.snapshot.restored_approval_selection().is_none());

    let mut replay = recovery_proposal();
    replay["fact_sequence"] = json!(2);
    facts.apply_verified_fact(&replay);
    assert!(!facts.can_authorize(&binding));
    restored["fact_sequence"] = json!(4);
    restored["build_id"] = json!("review-proposal-2");
    restored["missing"] = json!([]);
    assert_eq!(
        facts.apply_verified_fact(&restored),
        FactProjection::Applied
    );
    assert!(facts.can_authorize(&binding));
    let mut invalidated = recovery_proposal();
    invalidated["fact_sequence"] = json!(5);
    invalidated["proposal"]["state"] = json!("invalidated");
    facts.apply_verified_fact(&invalidated);
    assert!(!facts.snapshot.approval_restore_waiting());
    assert!(facts.snapshot.approval_card_snapshots().is_empty());
}

#[test]
fn restored_card_requires_its_own_build_validation_and_image_delivery() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "prepared", recovery_proposal());
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(facts.can_authorize(&binding));
    let restored = approval_restore_fact("proposal", "restored-build", 3);
    facts.apply_verified_fact(&restored);
    assert!(!facts.can_authorize(&binding));
    assert!(!facts.snapshot.approval_card_waiting());
    assert!(facts.snapshot.approval_restore_waiting());
    let mut artifact = reviewed_candidate(&binding);
    artifact.binding.build_id = "restored-build".into();
    artifact.valid = None;
    artifact.reviewed = false;
    facts.snapshot.artifacts.push(artifact);
    assert!(!facts.can_authorize(&binding));
    let artifact = facts.snapshot.artifacts.last_mut().unwrap();
    artifact.valid = Some(true);
    artifact.reviewed = true;
    assert!(!facts.can_authorize(&binding));
    facts
        .snapshot
        .review_images
        .push(review::test_image(&binding, "restored-build"));
    assert!(facts.can_authorize(&binding));
    assert!(facts.snapshot.approval_card_waiting());
}

#[test]
fn restore_before_replayed_revision_keeps_waiting_for_the_exact_recovered_version() {
    let mut facts = sequenced_facts();
    let mut old = recovery_proposal();
    old["proposal"]["revision"] = json!(1);
    deliver_fact(&mut facts, "old", old);
    let old_binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &old_binding);
    facts.remove_approval_card(&old_binding);
    let restored = approval_restore_fact("proposal", "review-proposal-2", 4);
    facts.apply_verified_fact(&restored);
    assert!(facts.snapshot.approval_restore_waiting());
    assert!(!facts.snapshot.approval_card_waiting());
    let mut replay = recovery_proposal();
    replay["fact_sequence"] = json!(3);
    deliver_fact(&mut facts, "replayed", replay);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert_eq!(binding.revision, 2);
    assert!(facts.snapshot.approval_restore_waiting());
    assert!(facts.snapshot.restored_approval_selection().is_none());
    assert!(!facts.can_authorize(&binding));
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert_eq!(
        facts.snapshot.restored_approval_selection(),
        Some((4, &binding))
    );
    assert!(facts.can_authorize(&binding));
}

#[test]
fn restore_waits_for_replayed_evidence_without_creating_approval_from_ready_text() {
    let mut facts = sequenced_facts();
    let restored = approval_restore_fact("proposal", "review-proposal-2", 3);
    assert_eq!(
        facts.apply_verified_fact(&restored),
        FactProjection::Applied
    );
    assert!(facts.snapshot.approval_restore_waiting());
    assert!(!facts.snapshot.approval_card_waiting());
    assert!(facts.snapshot.selected_approval_card().is_none());
    deliver_fact(&mut facts, "replayed-prepared", recovery_proposal());
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert!(!facts.can_authorize(&binding));
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert_eq!(
        facts.snapshot.restored_approval_selection(),
        Some((3, &binding))
    );
    assert!(facts.can_authorize(&binding));
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
    add_reviewed_candidate(&mut facts.snapshot, &binding);
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
    let active = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &active);
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
        add_reviewed_candidate(&mut facts.snapshot, &binding);
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
fn authorization_requires_validation_and_image_delivery_from_the_same_current_build() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "proposal", envelope());
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert!(!facts.can_authorize(&binding));
    let mut validation = artifact_envelope("a", 1, "built", Some(true));
    validation["artifact"]["review_image"] = json!({
        "delivery":"reference_only", "image_sha256":"a".repeat(64), "visual_analysis":"not_run"
    });
    deliver_fact(&mut facts, "render", validation.clone());
    assert!(!facts.can_authorize(&binding));
    validation["fact_sequence"] = json!(2);
    validation["artifact"]["review_image"]["delivery"] = json!("attached_png");
    deliver_fact(&mut facts, "image", validation.clone());
    assert!(facts.can_authorize(&binding));

    validation["fact_sequence"] = json!(3);
    validation["artifact"]["current"] = json!(false);
    deliver_fact(&mut facts, "stale", validation.clone());
    assert!(!facts.can_authorize(&binding));
    validation["fact_sequence"] = json!(4);
    validation["artifact"]["current"] = json!(true);
    validation["artifact"]["valid"] = json!(false);
    deliver_fact(&mut facts, "invalidated", validation);
    deliver_fact(
        &mut facts,
        "different-build",
        artifact_envelope("b", 5, "built", Some(true)),
    );
    assert!(facts.snapshot.artifacts[0].reviewed);
    assert_eq!(facts.snapshot.artifacts[1].valid, Some(true));
    assert!(
        !facts.can_authorize(&binding),
        "different builds must not combine their evidence"
    );
}

#[test]
fn candidate_review_delivery_requires_every_field_of_the_current_proposal_binding() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "proposal", envelope());
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    let artifact = reviewed_candidate(&binding);
    facts
        .snapshot
        .review_images
        .push(review::test_image(&binding, &artifact.binding.build_id));
    for field in [
        "proposal_id",
        "revision",
        "prepare_token",
        "authority_sha256",
        "input_sha256",
        "candidate",
        "state",
        "current",
        "valid",
        "reviewed",
    ] {
        let mut changed = artifact.clone();
        match field {
            "proposal_id" => changed.binding.proposal_id = "foreign".into(),
            "revision" => changed.binding.revision += 1,
            "prepare_token" => changed.binding.prepare_token = "foreign".into(),
            "authority_sha256" => changed.binding.authority_sha256 = "foreign".into(),
            "input_sha256" => changed.binding.input_sha256 = "foreign".into(),
            "candidate" => changed.binding.candidate = false,
            "state" => changed.state = ArtifactState::Failed,
            "current" => changed.current = None,
            "valid" => changed.valid = None,
            "reviewed" => changed.reviewed = false,
            _ => unreachable!(),
        }
        facts.snapshot.artifacts = vec![changed];
        assert!(!facts.can_authorize(&binding), "{field}");
    }
    facts.snapshot.artifacts = vec![artifact];
    assert!(facts.can_authorize(&binding));
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

#[test]
fn a_delayed_drawing_audit_cannot_restore_identification_for_a_previous_scene() {
    let mut facts = sequenced_facts();
    facts.apply_verified_fact(&recovery_proposal());
    let mut audit = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"drawing_audit","fact_sequence":90,"ok":true,"drawing_audit":{
        "audit_id":"d".repeat(64),"scene_sha256":"a".repeat(64),"candidate_count":0,
        "judged_count":0,"ai_review":"not_run","scene_application":"not_run",
        "offset":0,"next_offset":null,"candidates":[],
        "stair_linework":{"line_count":0,"region_count":0,"reviewed_region_count":0}}});
    assert_eq!(facts.apply_verified_fact(&audit), FactProjection::Applied);
    assert_eq!(
        facts.apply_verified_fact(&operation_envelope("applied", "not_required", 100)),
        FactProjection::Applied
    );
    assert!(facts.snapshot.drawing_audit.is_none());
    audit["fact_sequence"] = json!(99);
    assert_eq!(
        facts.apply_verified_fact(&audit),
        FactProjection::Superseded
    );
    assert!(facts.snapshot.drawing_audit.is_none());
    audit["fact_sequence"] = json!(101);
    audit["drawing_audit"]["scene_sha256"] = json!("b".repeat(64));
    assert_eq!(facts.apply_verified_fact(&audit), FactProjection::Applied);
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
    add_reviewed_candidate(&mut facts.snapshot, &fact.binding);
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
    let mut following = element;
    following["id"] = json!("stair.1.step.02");
    following["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("source_instance_id");
    value["proposal"]["draft"]["operations"]
        .as_array_mut()
        .unwrap()
        .push(
            json!({"collection":"elements","basis":"assumed","value_json":following.to_string()}),
        );
    assert!(proposal_fact(&value).is_some());
}

#[test]
fn schematic_stair_parts_preserve_assumed_rise_and_unverified_connection_for_approval() {
    let mut value = vertical_proposal();
    value["proposal"]["scope"] = json!({"floor_id":"floor.lower"});
    value["proposal"]["component_plan"]["connections"][0] = json!({
        "component_id":"stair.1", "source_instance_id":"source.stair", "kind":"stair",
        "mode":"single_floor_schematic", "served_floors":["floor.lower"],
        "openings":[], "dimension_basis":"assumed", "rise_m":3.6
    });
    let mut step: Value = serde_json::from_str(
        value["proposal"]["draft"]["operations"][0]["value_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    step["metadata"]["relation_validator"] = json!("vertical-schematic/1");
    step["metadata"]["modeling_mode"] = json!("single_floor_schematic");
    step["metadata"]["connection_status"] = json!("unverified");
    step["metadata"]["geometry_status"] = json!("schematic");
    step["metadata"]["solid_representation"] =
        json!("closed massing from the current floor; suspended stair structure not modeled");
    let mut landing = step.clone();
    landing["id"] = json!("stair.1.landing.01");
    landing["role"] = json!("landing");
    landing["height"] = json!(1.8);
    value["proposal"]["draft"]["operations"] = json!([
        {"collection":"elements","basis":"assumed","value_json":step.to_string()},
        {"collection":"elements","basis":"assumed","value_json":landing.to_string()}
    ]);
    let fact = proposal_fact(&value).unwrap();
    assert!(fact.ready);
    assert!(fact.objects[1].contains("单层示意楼梯 · 层高 3.6 m · assumed · 连接待核验"));
    assert!(fact.objects[2].contains("suspended stair structure not modeled"));
    assert!(fact.objects[4].contains("stair.1.landing.01 · landing"));
    assert!(!fact.objects.iter().any(|text| text.contains("既有洞口")));
    let mut facts = sequenced_facts();
    facts.snapshot.proposal = Some(fact.clone());
    assert!(!facts.can_authorize(&fact.binding));
    add_reviewed_candidate(&mut facts.snapshot, &fact.binding);
    assert!(facts.can_authorize(&fact.binding));

    for (key, invalid) in [
        ("mode", json!("two_floor")),
        ("kind", json!("shaft")),
        ("dimension_basis", json!("source")),
        ("rise_m", json!(0)),
        ("rise_m", json!("3.6")),
        ("served_floors", json!(["floor.lower", "floor.upper"])),
        ("served_floors", json!(["floor.other"])),
        ("openings", json!([{"floor_id":"floor.upper"}])),
        ("source_instance_id", json!("source.other")),
    ] {
        let mut malformed = value.clone();
        malformed["proposal"]["component_plan"]["connections"][0][key] = invalid;
        assert!(proposal_fact(&malformed).is_none(), "{key}");
    }
    for (key, invalid) in [
        ("relation_validator", json!("unknown")),
        ("modeling_mode", json!("two_floor")),
        ("connection_status", json!("verified")),
        ("step_dimension_basis", json!("source")),
        ("geometry_status", json!("verified")),
        ("solid_representation", Value::Null),
        ("source_instance_id", json!("source.other")),
    ] {
        let mut malformed = value.clone();
        let mut invalid_landing = landing.clone();
        invalid_landing["metadata"][key] = invalid;
        malformed["proposal"]["draft"]["operations"][1]["value_json"] =
            json!(invalid_landing.to_string());
        assert!(proposal_fact(&malformed).is_none(), "landing {key}");
    }
    let mut legacy_landing = value;
    landing["metadata"]["relation_validator"] = json!("vertical-connection/2");
    legacy_landing["proposal"]["draft"]["operations"][1]["value_json"] = json!(landing.to_string());
    assert!(proposal_fact(&legacy_landing).is_none());
}

#[test]
fn schematic_shaft_massing_and_cad_side_panels_require_bound_sources() {
    for role in ["shaft_wall", "partition"] {
        let mut value = vertical_proposal();
        value["proposal"]["scope"] = json!({"floor_id":"floor.lower"});
        value["proposal"]["vertical_sources"]["source.stair"] = json!({
            "source_id":"source.stair", "component_id":"stair.1", "floor_id":"floor.lower",
            "source_sha256":"a".repeat(64), "geometry_sha256":"b".repeat(64),
            "origin_m":[2,2], "linework_m":[[[0,0],[1,0]],[[0,0.1],[1,0.1]]]
        });
        let shaft = role == "shaft_wall";
        value["proposal"]["component_plan"]["connections"][0] = json!({
            "component_id":"stair.1", "source_instance_id":"source.stair",
            "kind":if shaft {"shaft"} else {"stair"},
            "mode":"single_floor_schematic", "served_floors":["floor.lower"],
            "openings":[], "dimension_basis":"assumed", "rise_m":3.6,
            "shaft_representation":"exterior_massing"
        });
        let mut element: Value = serde_json::from_str(
            value["proposal"]["draft"]["operations"][0]["value_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        element["id"] = json!(if shaft {
            "stair.1.enclosure"
        } else {
            "stair.1.flight.01.side.01"
        });
        element["role"] = json!(role);
        element["height"] = json!(3.6);
        element["holes"] = json!([]);
        element["metadata"] = json!({
            "component_id":"stair.1", "source_instance_id":"source.stair",
            "source_sha256":"a".repeat(64), "source_geometry_sha256":"b".repeat(64),
            "relation_validator":"vertical-schematic/1", "modeling_mode":"single_floor_schematic",
            "connection_status":"unverified", "step_dimension_basis":"assumed",
            "geometry_status":"schematic", "circulation_type":if shaft {"elevator"} else {"stair"},
            "vertical_spec_owner":if shaft {"stair.1.enclosure"} else {"stair.1.flight.01.step.001"},
            "solid_representation":if shaft {"exterior massing only; cavity and entrance not inferred"} else {"stepped tread surface with explicit sloping soffit"},
            "not_checked":"served floors; slab openings; vertical connection; headroom; structural design; building-code compliance"
        });
        if !shaft {
            element["metadata"]["side_panel_line_indices"] = json!("[0,1]");
            element["stair_soffit"] =
                json!({"start":{"x":2,"y":2},"end":{"x":2.3,"y":2},"start_z":0,"end_z":0.1});
            element["stair_top"] =
                json!({"start":{"x":2,"y":2},"end":{"x":2.3,"y":2},"start_z":0.9,"end_z":1});
        }
        value["proposal"]["draft"]["operations"][0]["value_json"] = json!(element.to_string());
        value["proposal"]["draft"]["operations"][0]["op"] = json!("add");
        value["proposal"]["draft"]["operations"][0]["target_id"] = element["id"].clone();
        value["proposal"]["draft"]["operations"][0]["floor_id"] = json!("floor.lower");
        value["proposal"]["draft"]["operations"][0]["source_ids"] = json!(["source.stair"]);
        let fact = proposal_fact(&value).unwrap();
        assert!(fact.ready);
        assert!(fact.objects.iter().any(|row| row.contains(if shaft {
            "电梯外部体量"
        } else {
            "CAD 侧板线索引"
        })));
        let mut facts = sequenced_facts();
        facts.snapshot.proposal = Some(fact.clone());
        assert!(!facts.can_authorize(&fact.binding));
        add_reviewed_candidate(&mut facts.snapshot, &fact.binding);
        assert!(facts.can_authorize(&fact.binding));
        for (pointer, invalid) in [
            (
                "/proposal/component_plan/connections/0/mode",
                json!("two_floor"),
            ),
            (
                "/proposal/component_plan/connections/0/openings",
                json!([{"floor_id":"floor.upper"}]),
            ),
            (
                "/proposal/vertical_sources/source.stair/geometry_sha256",
                json!("c".repeat(64)),
            ),
            (
                "/proposal/vertical_sources/source.stair/floor_id",
                json!("floor.other"),
            ),
            ("/proposal/draft/operations/0/op", json!("remove")),
            ("/proposal/draft/operations/0/target_id", json!("other")),
            (
                "/proposal/draft/operations/0/floor_id",
                json!("floor.other"),
            ),
            ("/proposal/draft/operations/0/source_ids", json!([])),
        ] {
            let mut malformed = value.clone();
            *malformed.pointer_mut(pointer).unwrap() = invalid;
            assert!(proposal_fact(&malformed).is_none(), "{role}: {pointer}");
        }
        for (key, invalid) in if shaft {
            vec![
                ("role", json!("stair_step")),
                ("height", json!(0.5)),
                ("stair_top", json!({})),
                (
                    "holes",
                    json!([[{"x":2.1,"y":2.1},{"x":2.2,"y":2.1},{"x":2.2,"y":2.2}]]),
                ),
            ]
        } else {
            vec![
                ("stair_soffit", Value::Null),
                ("stair_top", element["stair_soffit"].clone()),
            ]
        } {
            let mut malformed = value.clone();
            let mut part = element.clone();
            part[key] = invalid;
            malformed["proposal"]["draft"]["operations"][0]["value_json"] = json!(part.to_string());
            assert!(proposal_fact(&malformed).is_none(), "{role}: {key}");
        }
        let mut malformed = value.clone();
        let mut part = element.clone();
        if shaft {
            part["metadata"]["circulation_type"] = json!("stair");
        } else {
            part["metadata"]["side_panel_line_indices"] = json!("[0,9]");
        }
        malformed["proposal"]["draft"]["operations"][0]["value_json"] = json!(part.to_string());
        assert!(proposal_fact(&malformed).is_none(), "{role}: evidence");
        if !shaft {
            let mut same_polyline = value.clone();
            let mut part = element.clone();
            part["metadata"]["side_panel_line_indices"] = json!("[0,0]");
            same_polyline["proposal"]["vertical_sources"]["source.stair"]["linework_m"] =
                json!([[[0, 0], [1, 0], [1, 0.1], [0, 0.1], [0, 0]]]);
            same_polyline["proposal"]["draft"]["operations"][0]["value_json"] =
                json!(part.to_string());
            assert!(proposal_fact(&same_polyline).is_some());
            for key in ["stair_soffit", "stair_top"] {
                let mut part = element.clone();
                part[key]["start"]["x"] = json!(2.1);
                part[key]["end"]["x"] = json!(2.2);
                let mut short = value.clone();
                short["proposal"]["draft"]["operations"][0]["value_json"] = json!(part.to_string());
                assert!(proposal_fact(&short).is_none(), "{key} span");
            }
        }
    }
}

fn reference_stair_proposal() -> Value {
    let mut value = envelope();
    value["source_current"] = json!(true);
    value["fact_sequence"] = json!(10);
    value["proposal"]["scope"] = json!({"floor_id":"floor.lower"});
    let connection = json!({
        "component_id":"stair.1", "source_instance_id":"source.stair", "kind":"stair",
        "mode":"single_floor_schematic", "served_floors":["floor.lower"],
        "openings":[], "dimension_basis":"assumed", "rise_m":2.0,
        "geometry_basis":"region_bounds", "reference_sha256":"a".repeat(64),
        "limitations":"Only the visible CAD surfaces; elevations and soffit remain assumed",
        "reference_surfaces":[
            {"vertices_m":[[0,0,1],[1,0,1],[1,1,1],[0,1,1]],"role":"stair_step",
                "thickness_m":0.2,"source_ids":["cad.tread"],
                "soffit_m":{"start":{"x":0,"y":0},"end":{"x":0,"y":1},"start_z":0.7,"end_z":0.8}},
            {"vertices_m":[[0,1,1.5],[1,1,1.5],[1,2,2],[0,2,2]],"role":"landing",
                "thickness_m":0.2,"source_ids":["cad.landing"]}
        ], "steps":null,"riser_m":null
    });
    value["proposal"]["component_plan"] = json!({"connections":[connection.clone()]});
    value["proposal"]["vertical_sources"] = json!({"source.stair":{
        "source_id":"source.stair","component_id":"stair.1","floor_id":"floor.lower",
        "origin_m":[2,3],"scale":[1,1],"rotation_z_deg":0,"instance_path":[],
        "source_sha256":"b".repeat(64),"geometry_sha256":"c".repeat(64)
    }});
    let mut operations = Vec::new();
    for (index, surface) in connection["reference_surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let id = format!("stair.1.surface.{:03}", index + 1);
        let mut metadata = json!({
            "relation_validator":"vertical-schematic/1","component_id":"stair.1",
            "source_instance_id":"source.stair","source_sha256":"b".repeat(64),
            "source_geometry_sha256":"c".repeat(64),"source_instance_path":"",
            "modeling_mode":"single_floor_schematic","connection_status":"unverified",
            "step_dimension_basis":"assumed","geometry_status":"reference_informed_schematic",
            "geometry_basis":"assumed_region_bounds","reference_sha256":"a".repeat(64),
            "reference_status":"caller-supplied trial geometry; reference hash is an evidence citation",
            "vertical_spec_owner":"stair.1.surface.001",
            "solid_representation":"stepped tread surface with explicit sloping soffit",
            "not_checked":"served floors; slab openings; vertical connection; headroom; structural design; building-code compliance",
            "limitations":connection["limitations"]
        });
        if index == 0 {
            metadata["vertical_spec"] = json!(connection.to_string());
        } else {
            metadata["reason_ref"] = json!("stair.1.surface.001");
        }
        let mut element = json!({
            "id":id,"role":surface["role"],"metadata":metadata,"base_z":10,"height":if index==0 {1.0} else {2.0},
            "boundary":surface["vertices_m"].as_array().unwrap().iter().map(|p| json!({"x":2.0+p[0].as_f64().unwrap(),"y":3.0+p[1].as_f64().unwrap()})).collect::<Vec<_>>(),
            "holes":[],"source_ids":["source.stair",surface["source_ids"][0]],
            "stair_soffit":{"start":{"x":2,"y":3+index},"end":{"x":2,"y":4+index},
                "start_z":if index==0 {10.7} else {11.3},"end_z":if index==0 {10.8} else {11.8}}
        });
        if index == 1 {
            element["stair_top"] =
                json!({"start":{"x":2,"y":4},"end":{"x":2,"y":5},"start_z":11.5,"end_z":12});
        }
        operations.push(json!({"op":"add","collection":"elements","target_id":id,
            "floor_id":"floor.lower","basis":"assumed","source_ids":element["source_ids"],
            "value_json":element.to_string()}));
    }
    value["proposal"]["draft"]["operations"] = json!(operations);
    value
}

#[test]
fn reference_stair_keeps_an_independent_old_card_and_preserves_approval_gates() {
    let value = reference_stair_proposal();
    let mut facts = sequenced_facts();
    let mut old = envelope();
    old["fact_sequence"] = json!(9);
    old["proposal"]["proposal_id"] = json!("old-card");
    old["proposal"]["revision"] = json!(8);
    assert_eq!(facts.apply_verified_fact(&old), FactProjection::Applied);
    let old_binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &old_binding);
    assert!(facts.can_authorize(&old_binding));
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    let proposal = facts.snapshot.proposal.as_ref().unwrap();
    let binding = proposal.binding.clone();
    assert_eq!(facts.snapshot.approval_card_snapshots().len(), 2);
    let detail = proposal.objects.join("\n");
    for expected in [
        "参考",
        &"a".repeat(64),
        "仅证据引用",
        "连接待核验",
        "局部坐标",
        "相对楼面",
        "最高表面相对楼面 2.0 m",
        "斜底",
        "10.7",
        "stair.1.surface.002",
        "Only the visible CAD surfaces",
    ] {
        assert!(detail.contains(expected), "missing {expected}");
    }
    assert!(proposal.ready);
    assert!(facts.can_authorize(&old_binding));
    assert!(!facts.can_authorize(&binding));
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(facts.can_authorize(&binding));
    assert!(facts.snapshot.application.is_none());
    assert!(!facts.snapshot.authorizing);
}

#[test]
fn reference_stair_rejects_unbound_sources_states_and_reference_contracts() {
    let original = reference_stair_proposal();
    for (key, invalid) in [
        ("reference_sha256", json!("invalid")),
        ("reference_sha256", json!("d".repeat(64))),
        ("geometry_basis", json!("cad_treads")),
        ("limitations", Value::Null),
        ("flights", json!([{}])),
        ("landings", json!([{}])),
        ("steps", json!(4)),
        ("riser_m", json!(0.15)),
        ("rise_m", json!(3.0)),
        ("source_instance_id", json!("source.other")),
        ("dimension_basis", json!("user_confirmed")),
    ] {
        let mut bad = original.clone();
        bad["proposal"]["component_plan"]["connections"][0][key] = invalid;
        let mut owner: Value = serde_json::from_str(
            bad["proposal"]["draft"]["operations"][0]["value_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        owner["metadata"]["vertical_spec"] =
            json!(bad["proposal"]["component_plan"]["connections"][0].to_string());
        bad["proposal"]["draft"]["operations"][0]["value_json"] = json!(owner.to_string());
        assert!(proposal_fact(&bad).is_none(), "connection {key}");
    }
    for (key, invalid) in [
        ("relation_validator", json!("vertical-schematic/2")),
        ("geometry_status", json!("verified")),
        ("geometry_status", json!("schematic")),
        ("reference_status", json!("verified")),
        ("reference_sha256", json!("d".repeat(64))),
        ("source_instance_id", json!("source.other")),
        ("source_sha256", json!("d".repeat(64))),
        ("source_geometry_sha256", json!("d".repeat(64))),
        ("source_instance_path", json!("another/instance")),
        ("vertical_spec_owner", json!("other.surface.001")),
        ("connection_status", json!("verified")),
    ] {
        let mut bad = original.clone();
        let mut part: Value = serde_json::from_str(
            bad["proposal"]["draft"]["operations"][1]["value_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        part["metadata"][key] = invalid;
        bad["proposal"]["draft"]["operations"][1]["value_json"] = json!(part.to_string());
        assert!(proposal_fact(&bad).is_none(), "part {key}");
    }
    for fault in [
        "target",
        "floor",
        "missing_part",
        "duplicate_part",
        "soffit",
        "soffit_above_top",
        "top",
        "source",
        "source_identity",
        "surface_role",
        "surface_vertices",
        "surface_soffit",
    ] {
        let mut bad = original.clone();
        let mut part: Value = serde_json::from_str(
            bad["proposal"]["draft"]["operations"][1]["value_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        match fault {
            "target" => bad["proposal"]["draft"]["operations"][1]["target_id"] = json!("other"),
            "floor" => bad["proposal"]["draft"]["operations"][1]["floor_id"] = json!("floor.other"),
            "missing_part" => {
                bad["proposal"]["draft"]["operations"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
            }
            "duplicate_part" => {
                let duplicate = bad["proposal"]["draft"]["operations"][1].clone();
                bad["proposal"]["draft"]["operations"]
                    .as_array_mut()
                    .unwrap()
                    .push(duplicate);
            }
            "soffit" => part["stair_soffit"]["start_z"] = json!("assumed"),
            "soffit_above_top" => part["stair_soffit"]["start_z"] = json!(15),
            "top" => part["stair_top"]["end"] = part["stair_top"]["start"].clone(),
            "source" => part["source_ids"] = json!(["source.other"]),
            "source_identity" => {
                bad["proposal"]["vertical_sources"]["source.stair"]["source_id"] =
                    json!("source.other")
            }
            "surface_role" => {
                bad["proposal"]["component_plan"]["connections"][0]["reference_surfaces"][1]
                    ["role"] = json!("shaft_wall")
            }
            "surface_vertices" => {
                bad["proposal"]["component_plan"]["connections"][0]["reference_surfaces"][1]
                    ["vertices_m"][0][2] = json!("2")
            }
            "surface_soffit" => {
                bad["proposal"]["component_plan"]["connections"][0]["reference_surfaces"][0]
                    ["soffit_m"]["end_z"] = json!(5)
            }
            _ => unreachable!(),
        }
        if !matches!(fault, "missing_part" | "duplicate_part") {
            bad["proposal"]["draft"]["operations"][1]["value_json"] = json!(part.to_string());
        }
        if fault.starts_with("surface_") {
            let mut owner: Value = serde_json::from_str(
                bad["proposal"]["draft"]["operations"][0]["value_json"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            owner["metadata"]["vertical_spec"] =
                json!(bad["proposal"]["component_plan"]["connections"][0].to_string());
            bad["proposal"]["draft"]["operations"][0]["value_json"] = json!(owner.to_string());
        }
        assert!(proposal_fact(&bad).is_none(), "{fault}");
    }
}

#[test]
fn reference_stair_keeps_partition_holes_and_accepts_parts_before_the_spec_owner() {
    let mut value = reference_stair_proposal();
    value["proposal"]["component_plan"]["connections"][0]["reference_surfaces"][1]["role"] =
        json!("partition");
    value["proposal"]["component_plan"]["connections"][0]["reference_surfaces"][1]["holes_m"] =
        json!([[[0.2, 1.2], [0.4, 1.2], [0.3, 1.4]]]);
    let mut owner: Value = serde_json::from_str(
        value["proposal"]["draft"]["operations"][0]["value_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    owner["metadata"]["vertical_spec"] =
        json!(value["proposal"]["component_plan"]["connections"][0].to_string());
    value["proposal"]["draft"]["operations"][0]["value_json"] = json!(owner.to_string());
    let mut part: Value = serde_json::from_str(
        value["proposal"]["draft"]["operations"][1]["value_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    part["role"] = json!("partition");
    part["holes"] = json!([[{"x":2.2,"y":4.2},{"x":2.4,"y":4.2},{"x":2.3,"y":4.4}]]);
    value["proposal"]["draft"]["operations"][1]["value_json"] = json!(part.to_string());
    value["proposal"]["draft"]["operations"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let proposal = proposal_fact(&value).unwrap();
    assert!(proposal
        .objects
        .iter()
        .any(|line| line.contains("partition")));
    assert!(proposal
        .objects
        .iter()
        .any(|line| line.contains("参考表面孔洞")));
}

#[tokio::test]
#[ignore = "read-only reference stair replay; set DOMAIN_FACT_REPLAY to the sealed facts directory"]
async fn replay_reference_stair_restores_current_binding_after_validation_and_delivery() {
    let root = std::env::var_os("DOMAIN_FACT_REPLAY").expect("DOMAIN_FACT_REPLAY");
    let sequences = [
        2, 4, 16, 371, 730, 731, 732, 733, 734, 735, 736, 737, 738, 739, 741, 742, 743, 744, 745,
        746, 747, 748,
    ];
    let mut values = BTreeMap::new();
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        let raw = std::fs::read(&path).unwrap();
        let value: Value = serde_json::from_slice(&raw).unwrap();
        let sequence = value["fact_sequence"].as_u64().unwrap();
        if sequences.contains(&sequence) && value["kind"] != "fact_page" {
            assert_eq!(
                path.file_stem().unwrap().to_str().unwrap(),
                format!("{:x}", Sha256::digest(&raw))
            );
            assert!(values.insert(sequence, value).is_none());
        }
    }
    assert_eq!(values.len(), sequences.len());
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    setup.run_id = values[&730]["run_id"].as_str().unwrap().into();
    setup.context_sha256 = values[&730]["context_sha256"].as_str().unwrap().into();
    let directory = PathBuf::from(root);
    setup.repository = directory.ancestors().nth(5).unwrap().to_path_buf();
    setup.context_path = directory
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("context.json");
    // 通过生产扫描器展开封存分页，不能把 manifest 当作完整事实投影。
    values.clear();
    receipt::scan(&setup, 0, &mut receipt::FactFiles::new(), |value| {
        let sequence = value["fact_sequence"].as_u64().unwrap();
        if sequences.contains(&sequence) {
            assert!(values.insert(sequence, value).is_none());
        }
        true
    });
    assert_eq!(values.len(), sequences.len());
    let mut facts = sequenced_facts();
    facts.setup = Some(setup);
    let expected = ProposalBinding {
        run_id: values[&730]["run_id"].as_str().unwrap().into(),
        context_sha256: values[&730]["context_sha256"].as_str().unwrap().into(),
        proposal_id: "05740d9482ee45859a7c1bed6a7241a6".into(),
        revision: 1,
        prepare_token: values[&730]["proposal"]["prepare_token"]
            .as_str()
            .unwrap()
            .into(),
        base_sha256: "6edbc945925bbe1ec07bb2a597e82a84e0749858a77902b1959455a4e4283fbf".into(),
        candidate_sha256: "f7ff1064dc37ef0cd2ee67586c4d2579961abb919b93b8288e60be3ecb0a987c".into(),
    };
    for (sequence, value) in values {
        let result = facts.apply_verified_fact(&value);
        eprintln!(
            "seq={sequence} kind={} projection={result:?} authorize={}",
            value["kind"],
            facts.can_authorize(&expected)
        );
        assert_eq!(
            result,
            if sequence == 747 {
                FactProjection::Unrelated
            } else {
                FactProjection::Applied
            },
            "sequence {sequence}: {:?}",
            facts.projection_failure
        );
        if sequence == 371 {
            assert_eq!(
                facts.snapshot.proposal.as_ref().unwrap().binding.revision,
                8
            );
        }
        if sequence >= 730 {
            assert_eq!(facts.snapshot.proposal.as_ref().unwrap().binding, expected);
            assert!(!facts.can_authorize(&expected));
        }
        if sequence == 733 {
            assert!(facts
                .snapshot
                .artifacts
                .iter()
                .any(|artifact| artifact.valid == Some(true)
                    && artifact.current == Some(true)
                    && artifact.binding.input_sha256 == expected.candidate_sha256));
        }
    }
    assert!(!facts.snapshot.first_use_satisfied());
    assert_eq!(
        facts.snapshot.first_use_review.as_ref().unwrap().status,
        "ready"
    );
    assert!(facts.snapshot.rejected_facts.is_empty());
    assert!(!facts.snapshot.review_images.is_empty());
    for image in &mut facts.snapshot.review_images {
        review::verify_image(&image.binding).await.unwrap();
        image.file_state = ImageFileState::Available;
    }
    assert!(facts.snapshot.first_use_satisfied());
    assert!(facts.can_authorize(&expected));
    let mut wrong = expected.clone();
    wrong.revision += 1;
    assert!(!facts.can_authorize(&wrong));
    assert!(facts.snapshot.application.is_none());
    assert!(!facts.snapshot.authorizing);
    eprintln!("verified image identities={} current proposal={} r{} authorize=true; no authorization sent", facts.snapshot.review_images.len(), expected.proposal_id, expected.revision);
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
fn resumed_domain_replays_sealed_sequence_baseline_before_old_receipts() {
    let root = std::env::temp_dir().join(format!(
        "domain-baseline-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let directory = root.join("outputs/domain/run/project");
    std::fs::create_dir_all(directory.join("facts")).unwrap();
    std::fs::write(root.join("context.json"), b"{}").unwrap();
    let hash = format!("{:x}", Sha256::digest(b"{}"));
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    setup.repository = root.clone();
    setup.context_path = root.join("context.json");
    setup.context_sha256 = hash.clone();
    let scope = project_scope(150, "f08", &"d".repeat(64));
    let mut groups = grouped_source_view(159);
    groups["project_scope"] = scope["project_scope"].clone();
    let mut artifact = artifact_envelope("job", 172, "built", None);
    artifact["artifact"]["proposal_id"] = scope["proposal"]["proposal_id"].clone();
    artifact["project_scope"] = scope["project_scope"].clone();
    let mut validation = artifact.clone();
    validation["kind"] = json!("artifact_validation");
    validation["fact_sequence"] = json!(178);
    validation["artifact"]["valid"] = json!(true);
    let mut newer_artifact = artifact.clone();
    newer_artifact["fact_sequence"] = json!(181);
    let mut values = vec![
        project_catalog(140),
        project_selection(145, true),
        scope,
        groups,
        artifact,
        validation,
        newer_artifact,
    ];
    let mut index = serde_json::Map::new();
    for value in &mut values {
        value["context_sha256"] = json!(hash);
        value["ok"] = json!(true);
        let raw = serde_json::to_vec(value).unwrap();
        let sha = format!("{:x}", Sha256::digest(&raw));
        std::fs::write(directory.join("facts").join(format!("{sha}.json")), raw).unwrap();
        index.insert(sha, value["fact_sequence"].clone());
    }
    std::fs::write(
        directory.join("fact-sequences.json"),
        json!({"next":182,"facts":index}).to_string(),
    )
    .unwrap();
    let mut facts = DomainFacts::new(Some(setup));
    assert!(
        facts.snapshot.project_scope.is_some(),
        "persisted scope was not restored"
    );
    assert_eq!(facts.snapshot.artifacts[0].valid, Some(true));
    // Delayed stable receipts refer to the previous session's numbers.
    for index in [5, 4, 3, 5, 3] {
        assert_ne!(
            facts.apply_verified_fact(&values[index]),
            FactProjection::Invalid
        );
        let mut receipt = private_receipt(&values[index]);
        receipt.context_sha256 = hash.clone();
        facts.synchronization_finished(&receipt);
    }
    facts.synchronization_queue_drained();
    assert!(facts.snapshot.fact_sync_notice.is_none());
    assert!(facts.snapshot.groups.is_some());
    assert_eq!(facts.snapshot.artifacts[0].valid, Some(true));
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert!(
        !facts.can_authorize(&binding),
        "restoring facts must not invent a delivered review image"
    );
    let mut next_scope = project_scope(190, "f09.b", &"e".repeat(64));
    next_scope["context_sha256"] = json!(hash);
    assert_eq!(
        facts.apply_verified_fact(&next_scope),
        FactProjection::Applied
    );
    for index in [3, 4, 5] {
        assert_eq!(
            facts.apply_verified_fact(&values[index]),
            FactProjection::Superseded
        );
    }
    assert_eq!(
        facts.snapshot.project_scope.as_ref().unwrap().floor_key,
        "f09.b"
    );
    assert!(facts.snapshot.artifacts.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn resumed_domain_replays_baseline_when_history_exceeds_former_byte_cap() {
    // A real floor scope held 187 facts totalling ~109 MB; resume must not give up.
    let root = std::env::temp_dir().join(format!(
        "domain-baseline-large-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let directory = root.join("outputs/domain/run/project");
    std::fs::create_dir_all(directory.join("facts")).unwrap();
    std::fs::write(root.join("context.json"), b"{}").unwrap();
    let hash = format!("{:x}", Sha256::digest(b"{}"));
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    setup.repository = root.clone();
    setup.context_path = root.join("context.json");
    setup.context_sha256 = hash.clone();
    let scope = project_scope(150, "f08", &"d".repeat(64));
    let mut artifact = artifact_envelope("job", 172, "built", None);
    artifact["artifact"]["proposal_id"] = scope["proposal"]["proposal_id"].clone();
    artifact["project_scope"] = scope["project_scope"].clone();
    let mut validation = artifact.clone();
    validation["kind"] = json!("artifact_validation");
    validation["fact_sequence"] = json!(178);
    validation["artifact"]["valid"] = json!(true);
    let padding = "x".repeat(260 * 1024);
    let mut values: Vec<Value> = (1..140)
        .map(|sequence| {
            json!({"contract":"modeling-fact-receipt/v1","run_id":"run","kind":"padding",
                "fact_sequence":sequence,"padding":padding})
        })
        .collect();
    values.extend([
        project_catalog(140),
        project_selection(145, true),
        scope,
        artifact,
        validation,
    ]);
    let mut index = serde_json::Map::new();
    let mut total = 0;
    for value in &mut values {
        value["context_sha256"] = json!(hash);
        value["ok"] = json!(true);
        let raw = serde_json::to_vec(value).unwrap();
        total += raw.len();
        let sha = format!("{:x}", Sha256::digest(&raw));
        std::fs::write(directory.join("facts").join(format!("{sha}.json")), raw).unwrap();
        // The modeler keys its index by the semantic result digest, not the file name.
        index.insert(
            format!("{:x}", Sha256::digest(format!("semantic-{sha}"))),
            value["fact_sequence"].clone(),
        );
    }
    assert!(
        total > 32 * 1024 * 1024,
        "fixture must exceed the former cap"
    );
    std::fs::write(
        directory.join("fact-sequences.json"),
        json!({"next":179,"facts":index}).to_string(),
    )
    .unwrap();
    let facts = DomainFacts::new(Some(setup));
    assert!(
        facts.snapshot.project_scope.is_some(),
        "persisted scope was not restored from a large history"
    );
    assert_eq!(facts.snapshot.artifacts[0].valid, Some(true));
    std::fs::remove_dir_all(root).unwrap();
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
    add_reviewed_candidate(&mut facts.snapshot, &binding);
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
    let active = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &active);
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
    add_reviewed_candidate(&mut facts.snapshot, &binding);
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
    add_reviewed_candidate(&mut facts.snapshot, &binding);
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
    add_reviewed_candidate(&mut facts.snapshot, &binding);
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
        FactProjection::Deferred
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
    assert!(config.mcp_servers.is_empty());
    assert_eq!(
        crate::backend::acp_protocol::encode_mcp_servers(&config.mcp_servers),
        json!([])
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
    if cfg!(windows) {
        std::fs::create_dir_all(root.join("py")).unwrap();
        std::fs::write(root.join("py/python.exe"), "portable interpreter").unwrap();
        let portable = configure(Config {
            cwd: root.clone(),
            domain_project: Some("projects/test".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            portable.domain_setup.unwrap().python,
            normalized_path(&root).unwrap().join("py/python.exe")
        );
    }
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
fn current_catalog_recovers_persisted_selection_before_floor_scope_without_authorizing() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    let mut catalog = project_catalog(1);
    catalog["recovery_index"] =
        json!({"catalog_current":true,"selection":project_selection(2, true)["selection"]});
    assert_eq!(facts.apply_verified_fact(&catalog), FactProjection::Applied);
    assert!(
        facts.snapshot.selection.is_some(),
        "persisted selection missing from Core after recovery"
    );
    assert!(facts.snapshot.proposal.is_none());
    assert!(facts.snapshot.application.is_none());
    assert!(!facts.snapshot.authorizing);
    assert_eq!(
        facts.apply_verified_fact(&project_scope(3, "f08", &"d".repeat(64))),
        FactProjection::Applied
    );
}

#[test]
fn catalog_recovery_rejects_foreign_sources_and_does_not_replace_a_newer_selection() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    for key in ["source_sha256", "catalog_id"] {
        let mut facts = DomainFacts::new(Some(setup.clone()));
        let mut catalog = project_catalog(1);
        let mut selection = project_selection(2, true)["selection"].clone();
        selection[key] = json!("f".repeat(64));
        catalog["recovery_index"] = json!({"catalog_current":true,"selection":selection});
        assert_eq!(facts.apply_verified_fact(&catalog), FactProjection::Invalid);
        assert!(facts.snapshot.selection.is_none());
    }
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    let mut newer = project_selection(4, true);
    newer["selection"]["selection_id"] = json!("e".repeat(64));
    facts.apply_verified_fact(&newer);
    let mut catalog = project_catalog(5);
    catalog["recovery_index"] =
        json!({"catalog_current":true,"selection":project_selection(2, true)["selection"]});
    assert_eq!(facts.apply_verified_fact(&catalog), FactProjection::Applied);
    assert_eq!(
        facts.snapshot.selection.as_ref().unwrap().selection_id,
        "e".repeat(64)
    );
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

fn opening_completion_scope() -> Value {
    let mut scope = project_scope(3, "f08", &"d".repeat(64));
    scope["project_structure"] = json!(false);
    scope["project_scope"]["activation_revision"] = json!(1);
    scope["project_scope"]["opening_completion_binding"] = json!({
        "contract":"project-opening-completion/1","parent_scope_id":"e".repeat(64),
        "bootstrap_proposal_id":"a".repeat(32),"base_sha256":"f".repeat(64),
        "source_sha256":"b".repeat(64),"opening_ids":["door.1"]
    });
    let assets = json!({"style":"door","leaf_count":1,"frame_width":0.05});
    scope["proposal"] = hosted_proposal()["proposal"].clone();
    scope["proposal"]["base_sha256"] = json!("f".repeat(64));
    scope["proposal"]["scope"]["floor_id"] = json!("floor.f08");
    scope["proposal"]["opening_completion_source"] = json!({
        "contract":"project-opening-completion/1","path":"proof.json","sha256":"1".repeat(64)
    });
    scope["proposal"]["component_plan"] = json!({"openings":[{
        "operation":"complete-entity","target_opening_id":"door.1","component_id":"door.1",
        "source_instance_id":"door.1","kind":"door","host_wall_id":"wall.host",
        "height":2.1,"sill_height":0.0,"assets":assets
    }]});
    scope["proposal"]
        .as_object_mut()
        .unwrap()
        .remove("hosted_sources");
    scope["proposal"]["draft"]["operations"] = json!([{
        "op":"set","target_id":"door.1","collection":"openings","field":"assets",
        "basis":"assumed","source_ids":["cad.door"],"value_json":assets.to_string()
    }]);
    scope
}

#[test]
fn opening_completion_first_use_allows_only_authorized_null_cad_source_hashes() {
    let mut scope = opening_completion_scope();
    scope["proposal"]["source_dependencies"]["proof.json"] = json!("1".repeat(64));
    let mut required = json!({
        "review_key":"2".repeat(64),"component_id":"door.1","component_ids":["door.1"],
        "category":"door","template_id":"typed.door","source_kind":"cad",
        "source_instance_id":"door.1","source_sha256":null,"source_geometry_sha256":null,
        "definition_sha256":"3".repeat(64),"arguments_sha256":"4".repeat(64),
        "source_bindings":[{"component_id":"door.1","source_instance_id":"door.1",
            "source_sha256":null,"source_geometry_sha256":null}]
    });
    scope["proposal"]["component_first_use_requirements"] = json!({
        "contract":"component-first-use/5","required":[required.clone()],"reused":[]
    });
    let projected = project::scope_fact(&scope).unwrap();
    assert!(review::requirements_for_scope(&scope["proposal"], &projected).is_some());

    required["source_instance_id"] = json!("foreign-door");
    scope["proposal"]["component_first_use_requirements"]["required"] = json!([required.clone()]);
    assert!(review::requirements_for_scope(&scope["proposal"], &projected).is_none());
    required["source_instance_id"] = json!("door.1");
    required["source_bindings"][0]["source_sha256"] = json!("5".repeat(64));
    scope["proposal"]["component_first_use_requirements"]["required"] = json!([required]);
    assert!(review::requirements_for_scope(&scope["proposal"], &projected).is_none());
    assert!(review::requirements(&scope["proposal"]).is_none());

    scope["proposal"]["component_first_use_requirements"]["required"][0]["source_sha256"] =
        json!("6".repeat(64));
    scope["proposal"]["component_first_use_requirements"]["required"][0]["source_bindings"][0]
        ["source_sha256"] = json!("6".repeat(64));
    assert!(review::requirements_for_scope(&scope["proposal"], &projected).is_none());
}

#[test]
fn opening_completion_activation_projects_all_160_openings_and_preserves_validation() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let mut previous = project_scope(2198, "f08", &"e".repeat(64));
    previous["project_scope"]["activation_revision"] = json!(88);
    assert_eq!(
        facts.apply_verified_fact(&previous),
        FactProjection::Applied
    );

    // Minimal shape from real seq 2200: revision 89, 160 bound openings, one
    // assets-only operation and complete-entity specification for each opening.
    let mut scope = opening_completion_scope();
    scope["fact_sequence"] = json!(2200);
    scope["project_scope"]["activation_revision"] = json!(89);
    let assets = json!({"style":"door","leaf_count":1,"leaf_span":null,
        "leaf_thickness":0.04,"glass_thickness":0.008,"mullion_width":0.04,
        "panel_max_width":1.2,"clearance":0.008});
    let mut ids = Vec::new();
    let mut operations = Vec::new();
    let mut specs = Vec::new();
    for index in 0..160 {
        let id = format!("opening.{index}");
        let mut op = scope["proposal"]["draft"]["operations"][0].clone();
        op["target_id"] = json!(id);
        op["value_json"] = json!(assets.to_string());
        let mut spec = scope["proposal"]["component_plan"]["openings"][0].clone();
        for key in ["component_id", "target_opening_id", "source_instance_id"] {
            spec[key] = json!(id);
        }
        spec["assets"] = assets.clone();
        ids.push(id);
        operations.push(op);
        specs.push(spec);
    }
    scope["project_scope"]["opening_completion_binding"]["opening_ids"] = json!(ids);
    scope["proposal"]["draft"]["operations"] = json!(operations);
    scope["proposal"]["component_plan"]["openings"] = json!(specs);
    let (review_proposal, _) = first_use_fixture();
    let mut requirement =
        review_proposal["proposal"]["component_first_use_requirements"]["required"][0].clone();
    requirement["category"] = json!("door");
    requirement["template_id"] = json!("typed.door");
    requirement["component_id"] = json!("opening.0");
    requirement["source_instance_id"] = json!("opening.0");
    requirement["component_ids"] = json!(["opening.0", "opening.159"]);
    requirement["style_signature_sha256"] = json!("c".repeat(64));
    requirement["source_dimension_ranges_m"] = json!({"width":[0.85,1.04],"height":[2.1,2.1]});
    for (index, id) in ["opening.0", "opening.159"].into_iter().enumerate() {
        requirement["source_bindings"][index]["component_id"] = json!(id);
        requirement["source_bindings"][index]["source_instance_id"] = json!(id);
    }
    requirement["source_bindings"][1]["source_geometry_sha256"] = json!("6".repeat(64));
    scope["proposal"]["component_first_use_requirements"] = json!({
        "contract":"component-first-use/4","required":[requirement],"reused":[]});
    // Activation freezes requirements; a proposal read without ready evidence stays blocked.
    scope["component_first_use_review"] = json!({"status":"blocked","current":true});
    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert_eq!(facts.snapshot.proposal.as_ref().unwrap().objects.len(), 320);
    assert!(facts.snapshot.proposal.as_ref().unwrap().objects[318].contains("opening.159"));
    assert!(!facts.can_authorize(&binding));
    let mut proposal = scope.clone();
    proposal["kind"] = json!("proposal");
    proposal["fact_sequence"] = json!(2202);
    assert_eq!(
        facts.apply_verified_fact(&proposal),
        FactProjection::Applied
    );
    let mut artifact = artifact_envelope("completion", 2203, "built", Some(true));
    artifact["project_scope"] = scope["project_scope"].clone();
    artifact["project_structure"] = json!(false);
    artifact["artifact"]["proposal_id"] = json!(binding.proposal_id);
    artifact["artifact"]["revision"] = json!(binding.revision);
    artifact["artifact"]["prepare_token"] = json!(binding.prepare_token);
    artifact["artifact"]["authority_sha256"] = json!(binding.base_sha256);
    artifact["artifact"]["input_sha256"] = json!(binding.candidate_sha256);
    assert_eq!(
        facts.apply_verified_fact(&artifact),
        FactProjection::Applied
    );
    assert!(!facts.can_authorize(&binding));
    for fault in [
        "revision",
        "selection",
        "scope",
        "duplicate",
        "target",
        "assets",
        "field",
        "proof",
    ] {
        let mut bad = proposal.clone();
        bad["fact_sequence"] = json!(2300);
        match fault {
            "revision" => {
                bad["project_scope"]
                    .as_object_mut()
                    .unwrap()
                    .remove("activation_revision");
            }
            "selection" => bad["project_scope"]["selection_id"] = json!("9".repeat(64)),
            "scope" => bad["project_scope"]["scope_id"] = json!("9".repeat(64)),
            "duplicate" => {
                bad["project_scope"]["opening_completion_binding"]["opening_ids"][159] =
                    json!("opening.0")
            }
            "target" => bad["proposal"]["draft"]["operations"][159]["target_id"] = json!("foreign"),
            "assets" => {
                bad["proposal"]["component_plan"]["openings"][159]["assets"]["leaf_count"] =
                    json!(2)
            }
            "field" => bad["proposal"]["draft"]["operations"][159]["field"] = json!("width"),
            "proof" => bad["proposal"]["opening_completion_source"]["sha256"] = Value::Null,
            _ => unreachable!(),
        }
        assert_eq!(
            facts.apply_verified_fact(&bad),
            FactProjection::Invalid,
            "{fault}"
        );
        assert_eq!(facts.snapshot.proposal.as_ref().unwrap().binding, binding);
        assert!(!facts.can_authorize(&binding));
    }
}

#[test]
fn opening_completion_scope_preserves_exact_assets_only_binding_and_new_approval() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let scope = opening_completion_scope();
    let mut floor_proposal = scope.clone();
    floor_proposal["kind"] = json!("proposal");
    assert_eq!(
        sequenced_facts().apply_verified_fact(&floor_proposal),
        FactProjection::Invalid
    );
    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Applied);
    assert!(facts.snapshot.proposal.as_ref().unwrap().objects[0].contains("既有洞口 door.1"));
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    assert!(!facts.can_authorize(&binding));
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(facts.can_authorize(&binding));
    for fault in [
        "source",
        "target",
        "base",
        "field",
        "proof",
        "structure",
        "duplicate",
        "source_instance",
    ] {
        let mut bad = scope.clone();
        bad["fact_sequence"] = json!(100);
        match fault {
            "source" => {
                bad["project_scope"]["opening_completion_binding"]["source_sha256"] =
                    json!("2".repeat(64))
            }
            "target" => {
                bad["proposal"]["draft"]["operations"][0]["target_id"] = json!("door.other")
            }
            "base" => bad["proposal"]["base_sha256"] = json!("3".repeat(64)),
            "field" => bad["proposal"]["draft"]["operations"][0]["field"] = json!("width"),
            "proof" => bad["proposal"]["opening_completion_source"]["sha256"] = Value::Null,
            "structure" => bad["project_structure"] = json!(true),
            "duplicate" => {
                bad["project_scope"]["opening_completion_binding"]["opening_ids"] =
                    json!(["door.1", "door.1"])
            }
            "source_instance" => {
                bad["proposal"]["component_plan"]["openings"][0]["source_instance_id"] =
                    json!("foreign")
            }
            _ => unreachable!(),
        }
        assert_eq!(
            facts.apply_verified_fact(&bad),
            FactProjection::Invalid,
            "{fault}"
        );
        assert_eq!(facts.snapshot.proposal.as_ref().unwrap().binding, binding);
    }
    let mut changed = scope.clone();
    changed["kind"] = json!("proposal");
    changed["fact_sequence"] = json!(4);
    changed["project_scope"]["opening_completion_binding"]["opening_ids"] = json!(["door.other"]);
    assert_eq!(facts.apply_verified_fact(&changed), FactProjection::Invalid);
    let mut unbound = scope.clone();
    unbound.as_object_mut().unwrap().remove("project_scope");
    assert!(proposal_fact(&unbound).is_none());
    for style in ["window", "curtain_wall"] {
        let mut window = scope.clone();
        window["proposal"]["component_plan"]["openings"][0]["kind"] = json!("window");
        window["proposal"]["component_plan"]["openings"][0]["assets"]["style"] = json!(style);
        window["proposal"]["draft"]["operations"][0]["value_json"] =
            json!(window["proposal"]["component_plan"]["openings"][0]["assets"].to_string());
        assert!(proposal_fact(&window).unwrap().objects[0].contains("window 实体补全"));
    }
}

#[test]
fn floor_and_opening_completion_switches_project_bound_reviews_live_and_after_resume() {
    let root = std::env::temp_dir().join(format!(
        "domain-opening-switch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let directory = root.join("outputs/domain/run/project");
    std::fs::create_dir_all(directory.join("facts")).unwrap();
    std::fs::write(root.join("context.json"), b"{}").unwrap();
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    setup.repository = root.clone();
    setup.context_path = root.join("context.json");
    setup.context_sha256 = format!("{:x}", Sha256::digest(b"{}"));
    let mut facts = DomainFacts::new(Some(setup.clone()));
    let (mut requirements, evidence) = first_use_fixture();
    let item = &mut requirements["proposal"]["component_first_use_requirements"]["required"][0];
    item["component_id"] = json!("door.1");
    item["component_ids"] = json!(["door.1"]);
    item["category"] = json!("door");
    item["template_id"] = json!("typed.door");
    item["source_bindings"].as_array_mut().unwrap().truncate(1);
    item["source_bindings"][0]["component_id"] = json!("door.1");
    let requirement = requirements["proposal"]["component_first_use_requirements"].clone();
    let mut history = Vec::new();
    let mut index = serde_json::Map::new();
    for activation in 1..=4 {
        let sequence = activation * 10;
        let mut scope = if activation % 2 == 0 {
            opening_completion_scope()
        } else {
            project_scope(sequence, "f08", &"e".repeat(64))
        };
        scope["project_scope"]["activation_revision"] = json!(activation);
        scope["fact_sequence"] = json!(sequence);
        scope["proposal"]["component_first_use_requirements"] = requirement.clone();
        let mut proposal = scope.clone();
        proposal["kind"] = json!("proposal");
        proposal["fact_sequence"] = json!(sequence + 1);
        let mut artifact = artifact_envelope("preview", sequence + 2, "built", None);
        for key in ["proposal_id", "revision", "prepare_token"] {
            artifact["artifact"][key] = proposal["proposal"][key].clone();
        }
        artifact["artifact"]["authority_sha256"] = proposal["proposal"]["base_sha256"].clone();
        artifact["artifact"]["input_sha256"] = proposal["proposal"]["candidate_sha256"].clone();
        let mut validation = artifact.clone();
        validation["kind"] = json!("artifact_validation");
        validation["fact_sequence"] = json!(sequence + 3);
        validation["artifact"]["valid"] = json!(true);
        validation["artifact"]["review_image"] = json!({"delivery":"attached_png","image_sha256":"a".repeat(64),"visual_analysis":"not_run"});
        validation["artifact"]["review_image"]["image_path"] = json!(root.join("candidate.png"));
        validation["artifact"]["review_image"]["dimensions_px"] = json!([1, 1]);
        validation["artifact"]["review_image"]["request"] = json!({
            "build_id":"preview","proposal_id":proposal["proposal"]["proposal_id"],
            "revision":proposal["proposal"]["revision"],"prepare_token":proposal["proposal"]["prepare_token"],
            "input_sha256":proposal["proposal"]["candidate_sha256"],"blend_sha256":"b".repeat(64),"focus_m":null});
        let mut review = evidence.clone();
        review["kind"] = json!("component_first_use_review");
        review["contract"] = json!(CONTRACT);
        review["fact_sequence"] = json!(sequence + 4);
        let owner = &mut review["component_first_use_review"];
        for key in [
            "proposal_id",
            "revision",
            "prepare_token",
            "base_sha256",
            "candidate_sha256",
        ] {
            owner[key] = proposal["proposal"][key].clone();
            owner["manifest"][key] = proposal["proposal"][key].clone();
        }
        owner["manifest"]["input_sha256"] = proposal["proposal"]["candidate_sha256"].clone();
        owner["manifest"]["requirements_sha256"] = json!(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&requirement).unwrap())
        ));
        for (key, value) in requirement["required"][0].as_object().unwrap() {
            owner["manifest"]["items"][0][key] = value.clone();
        }
        let mut values = if activation == 1 {
            vec![project_catalog(1), project_selection(2, true)]
        } else {
            Vec::new()
        };
        values.extend([
            scope.clone(),
            proposal.clone(),
            artifact,
            validation,
            review,
        ]);
        for value in &mut values {
            value["run_id"] = json!("run");
            value["context_sha256"] = json!(setup.context_sha256);
            value["ok"] = json!(true);
            if value["fact_sequence"].as_u64().unwrap() > sequence {
                value["project_scope"] = scope["project_scope"].clone();
                value["project_structure"] = scope["project_structure"].clone();
            }
            assert_eq!(
                facts.apply_verified_fact(value),
                FactProjection::Applied,
                "activation {activation}, kind {}",
                value["kind"]
            );
            let raw = serde_json::to_vec(value).unwrap();
            let sha = format!("{:x}", Sha256::digest(&raw));
            std::fs::write(directory.join("facts").join(format!("{sha}.json")), raw).unwrap();
            index.insert(sha, value["fact_sequence"].clone());
        }
        std::fs::write(
            directory.join("fact-sequences.json"),
            json!({"next":sequence+5,"facts":index}).to_string(),
        )
        .unwrap();
        let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
        assert!(
            !facts.can_authorize(&binding),
            "first-use images need local verification"
        );
        for image in &mut facts.snapshot.review_images {
            image.file_state = ImageFileState::Available;
        }
        assert!(facts.can_authorize(&binding));
        for old in &history {
            assert_eq!(facts.apply_verified_fact(old), FactProjection::Superseded);
        }
        let mut resumed = DomainFacts::new(Some(setup.clone()));
        assert_eq!(resumed.snapshot.project_scope, facts.snapshot.project_scope);
        assert_eq!(resumed.snapshot.proposal.as_ref().unwrap().binding, binding);
        assert_eq!(resumed.snapshot.artifacts[0].valid, Some(true));
        assert_eq!(
            resumed.snapshot.first_use_review.as_ref().unwrap().status,
            "ready"
        );
        assert!(!resumed.can_authorize(&binding));
        // Fresh proposal/review reads and locally verified images restore eligibility.
        assert_eq!(
            resumed.apply_verified_fact(&values[values.len() - 4]),
            FactProjection::Applied
        );
        assert_eq!(
            resumed.apply_verified_fact(values.last().unwrap()),
            FactProjection::Applied
        );
        for image in &mut resumed.snapshot.review_images {
            image.file_state = ImageFileState::Available;
        }
        assert!(resumed.can_authorize(&binding));
        for old in &history {
            assert_eq!(resumed.apply_verified_fact(old), FactProjection::Superseded);
        }
        history = values
            .into_iter()
            .filter(|value| value.get("project_scope").is_some())
            .collect();
    }
    std::fs::remove_dir_all(root).unwrap();
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
        FactProjection::Deferred
    );
    facts.synchronization_failed("proposal", 5, "floor scope response was lost".into());
    assert!(facts.snapshot.proposal.is_none());
    assert!(!facts.can_authorize(&binding));

    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Applied);
    add_reviewed_candidate(&mut facts.snapshot, &binding);
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
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(facts.can_authorize(&binding));
    assert!(facts.snapshot.proposal.as_ref().unwrap().objects[0].contains("墙"));
    let second = project_scope(6, "f09.b", &"e".repeat(64));
    assert_eq!(facts.apply_verified_fact(&second), FactProjection::Applied);
    assert!(!facts.can_authorize(&binding));
    let current = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &current);
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
    assert!(!facts.can_authorize(&current));
    assert!(facts
        .snapshot
        .fact_sync_notice
        .as_deref()
        .unwrap()
        .contains("seq 999"));
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
    add_reviewed_candidate(&mut facts.snapshot, &binding);
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
fn stair_linework_summary_is_page_independent_and_missing_legacy_evidence_stays_unknown() {
    let mut value = json!({"ok":true,"drawing_audit":{
        "audit_id":"a".repeat(64),"scene_sha256":"b".repeat(64),
        "candidate_count":3,"judged_count":0,"ai_review":"not_run",
        "scene_application":"not_run","offset":0,"next_offset":1,
        "candidates":[{"id":"gap.1","detector":"gap","message":"gap"}]}});
    assert!(project::drawing_audit_fact(&value)
        .unwrap()
        .stair_linework
        .is_none());
    value["drawing_audit"]["stair_linework"] = json!({
        "line_count":1216,"region_count":2,"reviewed_region_count":0
    });
    let first = project::drawing_audit_fact(&value).unwrap();
    value["drawing_audit"]["offset"] = json!(1);
    value["drawing_audit"]["next_offset"] = Value::Null;
    value["drawing_audit"]["candidates"] = json!([
        {"id":"stair.1","detector":"stair_linework_reference","message":"plan region","bbox_m":[1,2,3,4]},
        {"id":"stair.2","detector":"stair_linework_reference","message":"plan region","bbox_m":[5,6,7,8]}
    ]);
    assert_eq!(
        project::drawing_audit_fact(&value).unwrap().stair_linework,
        first.stair_linework
    );
    assert!(project::drawing_audit_fact(&value).unwrap().rows[0]
        .contains("位置 (1.00, 2.00)—(3.00, 4.00) 米"));
    for (field, count) in [
        ("region_count", 0),
        ("region_count", 4),
        ("line_count", 1),
        ("reviewed_region_count", 1),
    ] {
        let mut wrong = value.clone();
        wrong["drawing_audit"]["stair_linework"][field] = json!(count);
        assert!(
            project::drawing_audit_fact(&wrong).is_none(),
            "{field}={count}"
        );
    }
    value["drawing_audit"]["candidates"][0]["bbox_m"] = json!([3, 2, 1, 4]);
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
        "source_discovery":{"state":"unknown","missing_floors":["f08","f09.b"],
            "unresolved_member_count":0,"unclassified_candidate_count":0,"uncovered_count":0,
            "diagnostic_member_count":0,"stale_selection_count":0,"region_count":0,
            "unreviewed_region_count":0,"uncertain_region_count":0,"stale_review_count":0,
            "orphaned_stale_review_count":0},
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

fn complete_building_report(sequence: u64) -> Value {
    let mut value = project_building_report(sequence);
    let record = &mut value["project_building"];
    for floor in record["inputs"]["floors"].as_array_mut().unwrap() {
        floor["source_counts"] = json!({"built":1});
        floor["drawing_review"] = json!({"candidate_count":0,"judged_count":0,"verdict_counts":{}});
        floor["scan_id"] = json!("a".repeat(64));
        floor["source_view_sha256"] = json!("b".repeat(64));
        floor["linework_summary"] = json!({"contract_version":"source-linework/1",
            "member_count":1,"selected_member_count":1,"unselected_member_count":0,
            "candidate_count":1,"unclassified_candidate_count":0,"region_count":1,
            "annotation_count":0,"uncovered_count":0,"diagnostic_member_count":0,
            "stale_selection_count":0});
        floor["source_region_summary"] = json!({"contract_version":"source-region-review/1",
            "region_count":1,"reviewed_region_count":1,"unreviewed_region_count":0,
            "uncertain_region_count":0,"stale_review_count":0,"orphaned_stale_review_count":0});
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
    record["report"]["source_discovery"]["state"] = json!("complete");
    record["report"]["source_discovery"]["missing_floors"] = json!([]);
    record["report"]["source_discovery"]["region_count"] = json!(2);
    value
}

#[test]
fn technical_building_checks_require_bound_connection_proof_and_keep_human_review_unrun() {
    let mut value = complete_building_report(3);
    let selection = project::selection_fact(&project_selection(2, true)).unwrap();
    let fact = project::building_fact(&value, &selection).unwrap();
    assert_eq!(fact.report.as_ref().unwrap().technical_status, "complete");
    assert_eq!(fact.report.as_ref().unwrap().state, "attention");
    value["project_building"]["inputs"]["connection_sources_sha256"] = Value::Null;
    assert!(project::building_fact(&value, &selection).is_none());
}

#[test]
fn building_reports_require_full_source_discovery_even_when_every_candidate_is_built() {
    let selection = project::selection_fact(&project_selection(2, true)).unwrap();
    for field in [
        "unreviewed_region_count",
        "uncertain_region_count",
        "stale_review_count",
        "orphaned_stale_review_count",
        "unclassified_candidate_count",
        "uncovered_count",
        "diagnostic_member_count",
        "stale_selection_count",
        "unselected_member_count",
        "missing_region_summary",
        "missing_summaries",
    ] {
        let mut value = complete_building_report(3);
        let record = &mut value["project_building"];
        let floor = &mut record["inputs"]["floors"][0];
        if field.starts_with("missing_") {
            floor
                .as_object_mut()
                .unwrap()
                .remove("source_region_summary");
            if field == "missing_summaries" {
                floor.as_object_mut().unwrap().remove("linework_summary");
            }
        } else if floor["source_region_summary"].get(field).is_some() {
            floor["source_region_summary"][field] = json!(1);
            if matches!(field, "unreviewed_region_count" | "stale_review_count") {
                floor["source_region_summary"]["reviewed_region_count"] = json!(0);
                floor["source_region_summary"]["unreviewed_region_count"] = json!(1);
            }
        } else {
            floor["linework_summary"][field] = json!(1);
            if field == "unselected_member_count" {
                floor["linework_summary"]["selected_member_count"] = json!(0);
                floor["linework_summary"]["candidate_count"] = json!(0);
            }
        }
        assert!(
            project::building_fact(&value, &selection).is_none(),
            "{field}: complete cannot pass"
        );
        let record = &mut value["project_building"];
        record["source_reconciliation"] = json!("attention");
        record["report"]["technical_status"] = json!("attention");
        let discovery = &mut record["report"]["source_discovery"];
        if field.starts_with("missing_") || field == "unselected_member_count" {
            discovery["state"] = json!("unknown");
            discovery["missing_floors"] = json!(["f08"]);
        } else {
            discovery["state"] = json!("attention");
        }
        if field.starts_with("missing_") {
            discovery["region_count"] = json!(1);
        } else if field == "unselected_member_count" {
            discovery["unresolved_member_count"] = json!(1);
        } else {
            discovery[field] = json!(1);
            if field == "stale_review_count" {
                discovery["unreviewed_region_count"] = json!(1);
            }
        }
        assert!(
            project::building_fact(&value, &selection).is_some(),
            "{field}: retain explicit gap"
        );
        value["project_building"]["report"]["source_discovery"]["state"] = json!("complete");
        assert!(
            project::building_fact(&value, &selection).is_none(),
            "{field}: reject forged totals"
        );
    }
}

#[test]
fn missing_discovery_cannot_restore_a_previous_complete_building_report() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    assert_eq!(
        facts.apply_verified_fact(&complete_building_report(3)),
        FactProjection::Applied
    );
    assert_eq!(
        facts.snapshot.buildings[0]
            .report
            .as_ref()
            .unwrap()
            .technical_status,
        "complete"
    );
    assert_eq!(
        facts.apply_verified_fact(&project_building(4, "reported", "built", None)),
        FactProjection::Applied
    );
    assert!(facts.snapshot.buildings[0].report.is_none());
    assert_eq!(
        facts.snapshot.buildings[0].source_discovery.state,
        "unknown"
    );
}

#[test]
fn no_object_candidates_or_complete_structure_proof_cannot_hide_unread_regions() {
    let mut value = complete_building_report(3);
    let record = &mut value["project_building"];
    for floor in record["inputs"]["floors"].as_array_mut().unwrap() {
        floor["source_counts"] = json!({});
        floor["source_total"] = json!(0);
        floor["linework_summary"]["selected_member_count"] = json!(0);
        floor["linework_summary"]["unselected_member_count"] = json!(1);
        floor["linework_summary"]["candidate_count"] = json!(0);
        floor["existing_structure_summary"] = json!({"contract_version":"source-structure-coverage/1",
            "member_count":1,"complete_member_count":1,"partial_member_count":0,
            "reference_only_member_count":0,"unproven_member_count":0,"unselected_complete_member_count":1});
    }
    record["report"]["source_counts"] = json!({});
    record["report"]["source_total"] = json!(0);
    let selection = project::selection_fact(&project_selection(2, true)).unwrap();
    assert!(project::building_fact(&value, &selection).is_some());
    let record = &mut value["project_building"];
    record["inputs"]["floors"][0]["source_region_summary"]["reviewed_region_count"] = json!(0);
    record["inputs"]["floors"][0]["source_region_summary"]["unreviewed_region_count"] = json!(1);
    record["source_reconciliation"] = json!("attention");
    record["report"]["technical_status"] = json!("attention");
    record["report"]["source_discovery"]["state"] = json!("attention");
    record["report"]["source_discovery"]["unreviewed_region_count"] = json!(1);
    let fact = project::building_fact(&value, &selection).unwrap();
    assert_eq!(fact.source_discovery.unresolved_member_count, 0);
    assert_eq!(fact.source_discovery.unreviewed_region_count, 1);
    value["project_building"]["report"]["technical_status"] = json!("complete");
    assert!(project::building_fact(&value, &selection).is_none());
}

#[test]
fn building_reports_distinguish_structure_representation_and_require_matching_totals() {
    let mut value = complete_building_report(3);
    let floor = &mut value["project_building"]["inputs"]["floors"][0];
    floor["source_region_summary"]["reviewed_region_count"] = json!(0);
    floor["source_region_summary"]["structure_represented_region_count"] = json!(1);
    floor["existing_structure_summary"] = json!({"contract_version":"source-structure-coverage/1",
        "member_count":1,"complete_member_count":1,"partial_member_count":0,
        "reference_only_member_count":0,"unproven_member_count":0,"unselected_complete_member_count":0});
    value["project_building"]["report"]["source_discovery"]["structure_represented_region_count"] =
        json!(1);
    let selection = project::selection_fact(&project_selection(2, true)).unwrap();
    let fact = project::building_fact(&value, &selection).unwrap();
    assert_eq!(fact.source_discovery.structure_represented_region_count, 1);
    assert_eq!(fact.source_discovery.state, "complete");
    assert_eq!(fact.report.unwrap().technical_status, "complete");
    for changed in [json!(0), json!(-1), json!(1.5), json!("1"), Value::Null] {
        let mut bad = value.clone();
        bad["project_building"]["report"]["source_discovery"]
            ["structure_represented_region_count"] = changed;
        assert!(project::building_fact(&bad, &selection).is_none());
    }
    let mut legacy_report = value.clone();
    legacy_report["project_building"]["report"]["source_discovery"]
        .as_object_mut()
        .unwrap()
        .remove("structure_represented_region_count");
    assert!(project::building_fact(&legacy_report, &selection).is_none());
    let mut missing_proof = value;
    missing_proof["project_building"]["inputs"]["floors"][0]
        .as_object_mut()
        .unwrap()
        .remove("existing_structure_summary");
    assert!(project::building_fact(&missing_proof, &selection).is_none());
    assert!(project::building_fact(&complete_building_report(3), &selection).is_some());
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
    // A job still sealing facts for an earlier activation is stale, not corrupt: it is
    // skipped (no sync error) and never becomes current state.
    let mut late = first.clone();
    late["kind"] = json!("proposal");
    late["fact_sequence"] = json!(100);
    assert_eq!(facts.apply_verified_fact(&late), FactProjection::Superseded);
    assert_eq!(
        facts
            .snapshot
            .project_scope
            .as_ref()
            .unwrap()
            .activation_revision,
        Some(3)
    );
    late["kind"] = json!("project_scope");
    assert_eq!(facts.apply_verified_fact(&late), FactProjection::Invalid);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);
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
    assert!(!facts.can_authorize(&binding));
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
    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Deferred);
    facts.apply_verified_fact(&project_building(3, "base", "built", Some(true)));
    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);
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
            if key == "build_id" {
                FactProjection::Deferred
            } else {
                FactProjection::Invalid
            },
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
        FactProjection::Deferred
    );
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let scope = project_layer_choice_scope(4);
    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Deferred);
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
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(!facts.can_authorize(&binding));
    assert!(facts
        .snapshot
        .fact_sync_notice
        .as_deref()
        .unwrap()
        .contains("seq 99"));
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
    let old = json!({"id":"bed.1.body","role":"furniture","origin":[2,2,0],"source_ids":[],
        "primitive_id":"primitive.old-frame","arguments":{"height":0.6},"rotation_z_deg":15,"scale":[-1,1,1]});
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
        "原 primitive primitive.old-frame",
        "原参数 {\"height\":0.6}",
        "原旋转 15",
        "原缩放 [-1,1,1]",
    ] {
        assert!(
            fact.objects.iter().any(|row| row.contains(expected)),
            "{expected}"
        );
    }
    let mut facts = sequenced_facts();
    facts.snapshot.proposal = Some(fact.clone());
    add_reviewed_candidate(&mut facts.snapshot, &fact.binding);
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
    assert!(fact
        .objects
        .iter()
        .any(|row| row.contains("原 primitive primitive.old-frame")));
    value["proposal"]["draft"]["operations"] = json!([]);
    assert!(proposal_fact(&value).is_none());
}

#[test]
fn edit_projection_rejects_invalid_original_shape_and_pose() {
    for (field, invalid) in [
        ("primitive_id", Value::Null),
        ("arguments", json!([])),
        ("arguments", json!({"height":"tall"})),
        ("origin", json!([0, 0])),
        ("rotation_z_deg", json!("15")),
        ("scale", json!([1, 1])),
    ] {
        let mut value = edit_envelope();
        value["proposal"]["component_edit"]["selected_parts"]["bed.1"][0][field] = invalid;
        assert!(proposal_fact(&value).is_none(), "{field}");
    }
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
fn edit_projection_accepts_the_modeler_cap_of_two_hundred_components() {
    let build = |count: usize| {
        let mut value = edit_envelope();
        let mut parts = Vec::new();
        let mut selected = Vec::new();
        let mut baseline = serde_json::Map::new();
        for i in 0..count {
            let component = format!("bed.{i}");
            selected.push(component.clone());
            let id = format!("{component}.p0");
            baseline.insert(
                component,
                json!([{"id":id,"role":"furniture","origin":[2,2,0],"source_ids":[],
                    "primitive_id":"primitive.old-frame","arguments":{},"rotation_z_deg":0,"scale":[1,1,1]}]),
            );
            parts.push(
                json!({"op":"remove","target_id":id,"floor_id":"floor.ground","collection":"solids",
                "basis":"assumed","source_ids":[],"reason":"whole selected removal"}),
            );
        }
        value["proposal"]["component_edit"]["plan"]["selected_component_ids"] = json!(selected);
        value["proposal"]["component_edit"]["plan"]["remove_component_ids"] = json!(selected);
        value["proposal"]["component_edit"]["plan"]["replacements"]["components"] = json!([]);
        value["proposal"]["component_edit"]["selected_parts"] = json!(baseline);
        value["proposal"]["component_edit"]["feedback"]["object_ids"] = json!(selected);
        value["proposal"]["feedback"]["object_ids"] = json!(selected);
        value["proposal"]["draft"]["operations"] = json!(parts);
        value
    };
    // 阶段 5 椅子模板升级一次选中 165 个实例；与模型器上限 200 对齐。
    assert!(proposal_fact(&build(165)).is_some());
    assert!(proposal_fact(&build(200)).is_some());
    assert!(proposal_fact(&build(201)).is_none());
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
            originals.push(json!({"id":id,"role":"furniture","origin":[2,2,0],"source_ids":[],
                "primitive_id":"primitive.old-frame","arguments":{},"rotation_z_deg":0,"scale":[1,1,1]}));
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

#[test]
fn source_policy_change_clears_old_views_and_rejects_late_legacy_facts() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "proposal", envelope());
    let proposal = facts.snapshot.proposal.clone();
    deliver_fact(&mut facts, "old-groups", grouped_source_view(4));
    let cap = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"capabilities","fact_sequence":5,"capabilities":{"source_coincidence_review":{"contract_version":"source-coincidence/1"}}});
    facts.apply_verified_fact(&cap);
    assert!(facts.snapshot.groups.is_none());
    assert!(facts.snapshot.ledger.is_none());
    assert_eq!(facts.snapshot.proposal, proposal);
    assert_eq!(
        facts.apply_verified_fact(&grouped_source_view(100)),
        FactProjection::Superseded
    );
    let mut fresh = grouped_source_view(6);
    fresh["source_coincidence_contract_version"] = json!("source-coincidence/1");
    assert_eq!(facts.apply_verified_fact(&fresh), FactProjection::Applied);
    assert!(facts.snapshot.groups.is_some());
}

#[test]
fn source_policy_matching_cached_view_survives_newer_capabilities() {
    let mut facts = sequenced_facts();
    let mut groups = grouped_source_view(3);
    groups["source_coincidence_contract_version"] = json!("source-coincidence/1");
    assert_eq!(facts.apply_verified_fact(&groups), FactProjection::Applied);
    let cap = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"capabilities","fact_sequence":5,"capabilities":{"source_coincidence_review":{"contract_version":"source-coincidence/1"}}});
    facts.apply_verified_fact(&cap);
    assert!(facts.snapshot.groups.is_some());
    let mut ledger = instance_source_view(6);
    ledger["source_coincidence_contract_version"] = json!("source-coincidence/1");
    assert_eq!(facts.apply_verified_fact(&ledger), FactProjection::Applied);
    assert!(facts.snapshot.groups.is_none());
    assert!(facts.snapshot.ledger.is_some());
}

#[test]
fn malformed_source_policy_cannot_clear_or_poison_a_verified_view() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "groups", grouped_source_view(4));
    let cap = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"capabilities","fact_sequence":100,"capabilities":{"source_coincidence_review":{"contract_version":false}}});
    assert_eq!(facts.apply_verified_fact(&cap), FactProjection::Invalid);
    assert!(facts.snapshot.groups.is_some());
    assert_eq!(
        facts.apply_verified_fact(&instance_source_view(5)),
        FactProjection::Applied
    );
}

#[test]
fn superseded_source_policy_receipt_cannot_become_the_displayed_fact() {
    let mut facts = sequenced_facts();
    let cap = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"capabilities","fact_sequence":5,"capabilities":{"source_coincidence_review":{"contract_version":"source-coincidence/1"}}});
    assert_eq!(facts.apply_verified_fact(&cap), FactProjection::Applied);
    facts.synchronization_finished(&private_receipt(&cap));
    let older = grouped_source_view(100);
    assert_eq!(
        facts.apply_verified_fact(&older),
        FactProjection::Superseded
    );
    facts.synchronization_finished(&private_receipt(&older));
    assert_eq!(facts.snapshot.verified_fact.as_ref().unwrap().sequence, 5);
    assert!(facts.snapshot.groups.is_none());
    assert!(facts.snapshot.fact_sync_notice.is_none());
}

#[test]
fn newer_page_of_the_same_source_state_keeps_both_shapes_and_header() {
    let mut facts = sequenced_facts();
    let groups = sealed_source_view(grouped_source_view(3), 'a');
    assert_eq!(facts.apply_verified_fact(&groups), FactProjection::Applied);
    let page = sealed_source_view(instance_source_view(9), 'a');
    assert_eq!(facts.apply_verified_fact(&page), FactProjection::Applied);
    facts.synchronization_finished(&private_receipt(&page));
    // Same sealed view: both shapes stay; replaying a present shape is stale.
    assert!(facts.snapshot.groups.is_some());
    assert!(facts.snapshot.ledger.is_some());
    assert_eq!(
        facts.apply_verified_fact(&groups),
        FactProjection::Superseded
    );
    facts.synchronization_finished(&private_receipt(&groups));
    assert_eq!(
        facts.snapshot.verified_fact.as_ref().unwrap().kind,
        "source_ledger"
    );
    assert_eq!(facts.snapshot.verified_fact.as_ref().unwrap().sequence, 9);
    // The presentation choice must not lower the watermark for different source state.
    let older_other_state = sealed_source_view(instance_source_view(8), 'b');
    assert_eq!(
        facts.apply_verified_fact(&older_other_state),
        FactProjection::Superseded
    );
    assert!(facts.snapshot.groups.is_some());
}

#[test]
fn source_view_stamp_declaration_clears_legacy_views_and_rejects_late_unstamped_data() {
    let mut facts = sequenced_facts();
    deliver_fact(&mut facts, "old-groups", grouped_source_view(4));
    let cap = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx", "kind":"capabilities",
        "fact_sequence":5,"capabilities":{"source_view":{"contract_version":"source-view/1"}}});
    assert_eq!(facts.apply_verified_fact(&cap), FactProjection::Applied);
    assert!(facts.snapshot.groups.is_none());
    assert_eq!(
        facts.apply_verified_fact(&grouped_source_view(100)),
        FactProjection::Superseded
    );
    assert_eq!(
        facts.apply_verified_fact(&sealed_source_view(grouped_source_view(6), 'a')),
        FactProjection::Applied
    );
    assert!(facts.snapshot.groups.is_some());
}

#[test]
fn malformed_source_view_stamp_cannot_poison_state_or_lower_its_watermark() {
    let mut facts = sequenced_facts();
    let groups = sealed_source_view(grouped_source_view(4), 'a');
    assert_eq!(facts.apply_verified_fact(&groups), FactProjection::Applied);
    let mut bad = sealed_source_view(instance_source_view(100), 'a');
    bad["source_view_sha256"] = json!("invalid");
    assert_eq!(facts.apply_verified_fact(&bad), FactProjection::Invalid);
    assert!(facts.snapshot.groups.is_some());
    assert_eq!(
        facts.apply_verified_fact(&sealed_source_view(instance_source_view(5), 'b')),
        FactProjection::Applied
    );
    assert_eq!(
        facts.apply_verified_fact(&groups),
        FactProjection::Superseded
    );
}

#[test]
fn source_view_capability_preserves_stamped_cache_and_stale_receipt_cannot_replace_header() {
    let mut facts = sequenced_facts();
    let groups = sealed_source_view(grouped_source_view(4), 'a');
    assert_eq!(facts.apply_verified_fact(&groups), FactProjection::Applied);
    let cap = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx", "kind":"capabilities",
        "fact_sequence":5,"capabilities":{"source_view":{"contract_version":"source-view/1"}}});
    assert_eq!(facts.apply_verified_fact(&cap), FactProjection::Applied);
    assert!(facts.snapshot.groups.is_some());
    let page = sealed_source_view(instance_source_view(6), 'b');
    assert_eq!(facts.apply_verified_fact(&page), FactProjection::Applied);
    facts.synchronization_finished(&private_receipt(&page));
    assert_eq!(
        facts.apply_verified_fact(&groups),
        FactProjection::Superseded
    );
    facts.synchronization_finished(&private_receipt(&groups));
    assert_eq!(facts.snapshot.verified_fact.as_ref().unwrap().sequence, 6);
    assert!(facts.snapshot.ledger.is_some());
}

fn linework_source_view(mut value: Value, hash: char) -> Value {
    value = sealed_source_view(value, hash);
    value["source_view_contract_version"] = json!("source-view/2");
    value["linework_summary"] = json!({
        "contract_version":"source-linework/1", "member_count":8,
        "selected_member_count":5,"unselected_member_count":3,"candidate_count":2,
        "unclassified_candidate_count":1,"region_count":4,"annotation_count":6,
        "uncovered_count":2,"diagnostic_member_count":1,"stale_selection_count":1
    });
    value
}

fn opening_inventory_view(mut value: Value) -> Value {
    value["opening_summary"] = json!({
        "contract":"approved-opening-inventory/1", "inventory_status":"verified",
        "total_count":4,"verified_aperture_count":3,"missing_entity_count":2,
        "complete_entity_count":1,"unverified_count":2
    });
    value
}

fn existing_structure_view(mut value: Value) -> Value {
    value["existing_structure_summary"] = json!({
        "contract_version":"source-structure-coverage/1", "member_count":8,
        "complete_member_count":3,"partial_member_count":1,
        "reference_only_member_count":2,"unproven_member_count":2,
        "unselected_complete_member_count":2
    });
    value
}

fn source_region_review_view(mut value: Value) -> Value {
    value["source_region_summary"] = json!({
        "contract_version":"source-region-review/1","region_count":4,
        "reviewed_region_count":2,"unreviewed_region_count":2,
        "uncertain_region_count":1,"stale_review_count":1,
        "orphaned_stale_review_count":5
    });
    value
}

#[test]
fn region_review_v2_accepts_skipped_unknown_counted_as_reviewed() {
    let base = source_region_review_view(existing_structure_view(linework_source_view(
        instance_source_view(4),
        'a',
    )));
    let mut value = base.clone();
    value["source_region_summary"]["contract_version"] = json!("source-region-review/2");
    value["source_region_summary"]["skipped_unknown_region_count"] = json!(1);
    assert_eq!(
        sequenced_facts().apply_verified_fact(&value),
        FactProjection::Applied
    );
    // Uncertain plus skipped cannot exceed the reviewed regions.
    value["source_region_summary"]["skipped_unknown_region_count"] = json!(2);
    assert_eq!(
        sequenced_facts().apply_verified_fact(&value),
        FactProjection::Invalid
    );
}

#[test]
fn floor_progress_uses_sealed_complete_counts_and_invalidates_with_the_source_view() {
    let mut facts = sequenced_facts();
    let mut value = source_region_review_view(existing_structure_view(linework_source_view(
        instance_source_view(4),
        'a',
    )));
    value["floor_progress"] = json!({
        "contract":"floor-progress/1", "scan_id":value["scan_id"],
        "scene_sha256":value["scene_sha256"],
        "categories":[{"category":"chair","built":322,"pending":100,"blocked":7,"unidentified":0}],
        "drawing_coverage":{"covered":50,"total":100,"unit":"图块实例和散线区域来源单位"},
        "unreviewed_regions":2
    });
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    let progress = facts.snapshot.current_floor_progress().unwrap().clone();
    assert_eq!(progress.categories[0].built, 322);
    assert!(facts.snapshot.floor_progress_v2.is_none());
    let mut bad = value.clone();
    bad["fact_sequence"] = json!(99);
    bad["floor_progress"]["scene_sha256"] = json!("other");
    assert_eq!(facts.apply_verified_fact(&bad), FactProjection::Invalid);
    assert_eq!(facts.snapshot.current_floor_progress(), Some(&progress));
    bad["floor_progress"] = value["floor_progress"].clone();
    bad["floor_progress"]["drawing_coverage"]["covered"] = json!(101);
    assert_eq!(facts.apply_verified_fact(&bad), FactProjection::Invalid);
    let mut next = value.clone();
    next["fact_sequence"] = json!(5);
    next.as_object_mut().unwrap().remove("floor_progress");
    assert_eq!(facts.apply_verified_fact(&next), FactProjection::Applied);
    assert!(facts.snapshot.current_floor_progress().is_none());
    assert_eq!(
        facts.apply_verified_fact(&value),
        FactProjection::Superseded
    );
    assert!(facts.snapshot.current_floor_progress().is_none());
}

#[test]
fn floor_progress_v2_projects_completion_and_rejects_malformed_fields() {
    let mut facts = sequenced_facts();
    let mut value = source_region_review_view(existing_structure_view(linework_source_view(
        instance_source_view(4),
        'a',
    )));
    value["floor_progress"] = json!({
        "contract":"floor-progress/2", "scan_id":value["scan_id"],
        "scene_sha256":value["scene_sha256"],
        "categories":[{"category":"chair","built":10,"pending":331,"blocked":7,"unidentified":3455}],
        "drawing_coverage":{"covered":50,"total":100,"unit":"来源成员"},
        "unreviewed_regions":2,
        "completion":false,
        "completion_blockers":["unreviewed_regions","pending_sources","unidentified_sources"],
        "disposition_counts":{"built":10,"schematic_built":13,"skipped_unextractable":385,"future_disposition":2},
        "cross_floor_unverified":13,
        "deferrals":[{"source_id":"a","kind":"collision"}],
        "skips":[{"source_id":"b","kind":"unextractable"}],
        "legacy_items":[{"source_id":"c","kind":"2d_reference"}]
    });
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    let v2 = facts.snapshot.floor_progress_v2.as_ref().unwrap();
    assert!(!v2.completion);
    assert_eq!(v2.completion_blockers.len(), 3);
    assert_eq!(v2.disposition_counts["future_disposition"], 2);
    assert_eq!(v2.cross_floor_unverified, 13);
    assert_eq!((v2.deferrals, v2.skips, v2.legacy_items), (1, 1, 1));
    for (key, bad_value) in [
        ("completion", json!("false")),
        ("completion_blockers", json!([7])),
        ("disposition_counts", json!({"built":-1})),
        ("cross_floor_unverified", json!(-1)),
        ("deferrals", json!({})),
        ("skips", json!(["bad"])),
        ("legacy_items", json!(null)),
    ] {
        let mut bad = value.clone();
        bad["floor_progress"][key] = bad_value;
        assert_eq!(
            facts.apply_verified_fact(&bad),
            FactProjection::Invalid,
            "{key}"
        );
    }
}

#[test]
fn source_region_reviews_share_current_source_shapes_and_keep_missing_or_stale_summary_unknown() {
    let mut facts = sequenced_facts();
    for (sequence, kind) in [
        (4, "source_groups"),
        (5, "source_ledger"),
        (6, "source_linework"),
        (7, "source_linework_selection"),
    ] {
        let mut value = source_region_review_view(linework_source_view(
            if kind == "source_groups" {
                grouped_source_view(sequence)
            } else {
                instance_source_view(sequence)
            },
            'a',
        ));
        value["kind"] = json!(kind);
        assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
        let regions = facts.snapshot.current_source_regions().unwrap();
        assert_eq!(regions.region_count, 4);
        assert_eq!(regions.reviewed_region_count, 2);
        assert_eq!(regions.unreviewed_region_count, 2);
        assert_eq!(regions.structure_represented_region_count, 0);
        assert_eq!(regions.uncertain_region_count, 1);
        assert_eq!(regions.stale_review_count, 1);
        assert_eq!(regions.orphaned_stale_review_count, 5);
    }
    let current = facts.snapshot.source_regions.clone();
    let stale = source_region_review_view(linework_source_view(grouped_source_view(5), 'b'));
    assert_eq!(
        facts.apply_verified_fact(&stale),
        FactProjection::Superseded
    );
    assert_eq!(facts.snapshot.source_regions, current);
    assert_eq!(
        facts.apply_verified_fact(&linework_source_view(grouped_source_view(8), 'c')),
        FactProjection::Applied
    );
    assert!(facts.snapshot.source_regions.is_none());
    facts.clear_source_view();
    assert!(facts.snapshot.source_regions.is_none());
}

#[test]
fn malformed_source_region_review_totals_cannot_advance_source_watermark() {
    let mut facts = sequenced_facts();
    let current = source_region_review_view(linework_source_view(grouped_source_view(4), 'a'));
    facts.apply_verified_fact(&current);
    let previous = facts.snapshot.source_regions.clone();
    for (field, value) in [
        ("contract_version", json!("unknown")),
        ("region_count", json!(5)),
        ("reviewed_region_count", json!(-1)),
        ("reviewed_region_count", json!(3)),
        ("unreviewed_region_count", json!(u64::MAX)),
        ("structure_represented_region_count", json!(-1)),
        ("structure_represented_region_count", json!(1.5)),
        ("structure_represented_region_count", json!("1")),
        ("structure_represented_region_count", json!(true)),
        ("structure_represented_region_count", Value::Null),
        ("structure_represented_region_count", json!(u64::MAX)),
        ("uncertain_region_count", json!(3)),
        ("stale_review_count", json!(3)),
        ("stale_review_count", Value::Null),
        ("orphaned_stale_review_count", json!(-1)),
        ("orphaned_stale_review_count", json!(1.5)),
        ("orphaned_stale_review_count", json!("1")),
        ("orphaned_stale_review_count", Value::Null),
    ] {
        let mut bad =
            source_region_review_view(linework_source_view(instance_source_view(100), 'b'));
        bad["source_region_summary"][field] = value;
        assert_eq!(
            facts.apply_verified_fact(&bad),
            FactProjection::Invalid,
            "{field}"
        );
        assert_eq!(facts.snapshot.source_regions, previous);
    }
    let mut missing_field = current.clone();
    missing_field["source_region_summary"]
        .as_object_mut()
        .unwrap()
        .remove("orphaned_stale_review_count");
    assert_eq!(
        facts.apply_verified_fact(&missing_field),
        FactProjection::Invalid
    );
    assert_eq!(facts.snapshot.source_regions, previous);
    let mut no_stamp = current.clone();
    no_stamp
        .as_object_mut()
        .unwrap()
        .remove("source_view_sha256");
    no_stamp
        .as_object_mut()
        .unwrap()
        .remove("source_view_contract_version");
    assert_eq!(
        facts.apply_verified_fact(&no_stamp),
        FactProjection::Invalid
    );
    assert_eq!(
        facts.apply_verified_fact(&source_region_review_view(linework_source_view(
            instance_source_view(5),
            'b'
        ))),
        FactProjection::Applied
    );
}

#[test]
fn structure_represented_regions_require_current_complete_structure_summary() {
    let mut facts = sequenced_facts();
    let mut value = existing_structure_view(source_region_review_view(linework_source_view(
        grouped_source_view(4),
        'a',
    )));
    value["source_region_summary"]["unreviewed_region_count"] = json!(1);
    value["source_region_summary"]["structure_represented_region_count"] = json!(1);
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    let previous = facts.snapshot.source_regions.clone();
    assert_eq!(
        previous
            .as_ref()
            .unwrap()
            .structure_represented_region_count,
        1
    );
    let mut missing = value.clone();
    missing["fact_sequence"] = json!(100);
    missing
        .as_object_mut()
        .unwrap()
        .remove("existing_structure_summary");
    assert_eq!(facts.apply_verified_fact(&missing), FactProjection::Invalid);
    let mut partial_only = value.clone();
    partial_only["fact_sequence"] = json!(100);
    partial_only["existing_structure_summary"]["complete_member_count"] = json!(0);
    partial_only["existing_structure_summary"]["partial_member_count"] = json!(4);
    partial_only["existing_structure_summary"]["unselected_complete_member_count"] = json!(0);
    assert_eq!(
        facts.apply_verified_fact(&partial_only),
        FactProjection::Invalid
    );
    assert_eq!(facts.snapshot.source_regions, previous);
    facts.snapshot.existing_structure.as_mut().unwrap().scan_id = "stale".into();
    assert!(facts.snapshot.current_source_regions().is_none());
}

#[test]
fn source_region_review_cannot_restore_a_previous_floor_scope() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let first = project_scope(3, "f08", &"d".repeat(64));
    facts.apply_verified_fact(&first);
    let mut regions = source_region_review_view(linework_source_view(grouped_source_view(4), 'a'));
    regions["project_scope"] = first["project_scope"].clone();
    assert_eq!(facts.apply_verified_fact(&regions), FactProjection::Applied);
    assert!(facts.snapshot.source_regions.is_some());
    facts.apply_verified_fact(&project_scope(5, "f09.b", &"e".repeat(64)));
    assert!(facts.snapshot.source_regions.is_none());
    regions["fact_sequence"] = json!(100);
    assert_eq!(facts.apply_verified_fact(&regions), FactProjection::Invalid);
    assert!(facts.snapshot.source_regions.is_none());
}

#[test]
fn existing_structure_coverage_is_shared_by_source_shapes_without_selecting_members() {
    let mut facts = sequenced_facts();
    for (sequence, kind) in [
        (4, "source_groups"),
        (5, "source_ledger"),
        (6, "source_linework"),
        (7, "source_linework_selection"),
    ] {
        let mut value = existing_structure_view(linework_source_view(
            if kind == "source_groups" {
                grouped_source_view(sequence)
            } else {
                instance_source_view(sequence)
            },
            'a',
        ));
        value["kind"] = json!(kind);
        assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
        let coverage = facts.snapshot.current_existing_structure().unwrap();
        assert_eq!(coverage.member_count, 8);
        assert_eq!(coverage.complete_member_count, 3);
        assert_eq!(coverage.partial_member_count, 1);
        assert_eq!(coverage.reference_only_member_count, 2);
        assert_eq!(coverage.unproven_member_count, 2);
        assert_eq!(coverage.unselected_complete_member_count, 2);
        assert_eq!(
            facts
                .snapshot
                .linework
                .as_ref()
                .unwrap()
                .unselected_member_count,
            3
        );
        assert_eq!(
            facts
                .snapshot
                .linework
                .as_ref()
                .unwrap()
                .selected_member_count,
            5
        );
    }
    let stale = existing_structure_view(linework_source_view(grouped_source_view(5), 'b'));
    assert_eq!(
        facts.apply_verified_fact(&stale),
        FactProjection::Superseded
    );
    let fresh_missing = linework_source_view(grouped_source_view(8), 'c');
    assert_eq!(
        facts.apply_verified_fact(&fresh_missing),
        FactProjection::Applied
    );
    assert!(facts.snapshot.existing_structure.is_none());
}

#[test]
fn structure_coverage_rejects_tampered_counts_without_advancing_source_watermark() {
    let mut facts = sequenced_facts();
    let current = existing_structure_view(linework_source_view(grouped_source_view(4), 'a'));
    facts.apply_verified_fact(&current);
    let previous = facts.snapshot.existing_structure.clone();
    for (field, value) in [
        ("contract_version", json!("unknown")),
        ("member_count", json!(9)),
        ("complete_member_count", json!(-1)),
        ("partial_member_count", json!(0)),
        ("reference_only_member_count", json!(3)),
        ("unproven_member_count", json!(u64::MAX)),
        ("unselected_complete_member_count", json!(4)),
        ("unselected_complete_member_count", Value::Null),
    ] {
        let mut bad = existing_structure_view(linework_source_view(instance_source_view(100), 'b'));
        bad["existing_structure_summary"][field] = value;
        assert_eq!(
            facts.apply_verified_fact(&bad),
            FactProjection::Invalid,
            "{field}"
        );
        assert_eq!(facts.snapshot.existing_structure, previous);
    }
    let mut no_unselected = current.clone();
    no_unselected["fact_sequence"] = json!(100);
    no_unselected["linework_summary"]["unselected_member_count"] = json!(1);
    no_unselected["linework_summary"]["selected_member_count"] = json!(7);
    assert_eq!(
        facts.apply_verified_fact(&no_unselected),
        FactProjection::Invalid
    );
    let mut no_seal = current.clone();
    no_seal
        .as_object_mut()
        .unwrap()
        .remove("source_view_sha256");
    no_seal
        .as_object_mut()
        .unwrap()
        .remove("source_view_contract_version");
    assert_eq!(facts.apply_verified_fact(&no_seal), FactProjection::Invalid);
    assert_eq!(
        facts.apply_verified_fact(&existing_structure_view(linework_source_view(
            instance_source_view(5),
            'b'
        ))),
        FactProjection::Applied
    );
    facts.clear_source_view();
    assert!(facts.snapshot.existing_structure.is_none());
}

#[test]
fn structure_coverage_cannot_cross_floor_scope_or_activation() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let first = project_scope(3, "f08", &"d".repeat(64));
    facts.apply_verified_fact(&first);
    let mut value = existing_structure_view(linework_source_view(grouped_source_view(4), 'a'));
    value["project_scope"] = first["project_scope"].clone();
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    assert!(facts.snapshot.existing_structure.is_some());
    facts.apply_verified_fact(&project_scope(5, "f09.b", &"e".repeat(64)));
    assert!(facts.snapshot.existing_structure.is_none());
    value["fact_sequence"] = json!(100);
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Invalid);
    assert!(facts.snapshot.existing_structure.is_none());
}

#[test]
fn approved_opening_inventory_is_projected_from_every_source_shape_and_capabilities() {
    let mut facts = sequenced_facts();
    for (sequence, kind) in [
        (4, "source_groups"),
        (5, "source_ledger"),
        (6, "source_linework"),
        (7, "source_linework_selection"),
        (8, "capabilities"),
    ] {
        let mut value = opening_inventory_view(linework_source_view(
            if kind == "source_groups" {
                grouped_source_view(sequence)
            } else {
                instance_source_view(sequence)
            },
            'a',
        ));
        value["kind"] = json!(kind);
        if kind == "capabilities" {
            value.as_object_mut().unwrap().remove("scan_id");
        }
        assert_eq!(
            facts.apply_verified_fact(&value),
            FactProjection::Applied,
            "{kind}"
        );
        let inventory = facts.snapshot.openings.as_ref().unwrap();
        assert_eq!(inventory.total_count, 4);
        assert_eq!(inventory.verified_aperture_count, 3);
        assert_eq!(inventory.missing_entity_count, 2);
        assert_eq!(inventory.complete_entity_count, 1);
        assert_eq!(inventory.unverified_count, 2);
        assert!(!inventory.complete());
        assert_eq!(inventory.scan_id.as_deref(), value["scan_id"].as_str());
        assert_eq!(
            inventory.scene_sha256,
            value["scene_sha256"].as_str().unwrap()
        );
    }
    facts.clear_project_scope();
    assert!(facts.snapshot.openings.is_none());
}

#[test]
fn invalid_opening_inventory_cannot_poison_counts_or_sequence_watermarks() {
    let mut facts = sequenced_facts();
    let current = opening_inventory_view(linework_source_view(grouped_source_view(4), 'a'));
    facts.apply_verified_fact(&current);
    let previous = facts.snapshot.openings.clone();
    for (field, value) in [
        ("contract", json!("unknown")),
        ("inventory_status", json!("unknown")),
        ("total_count", json!(-1)),
        ("total_count", json!(2)),
        ("verified_aperture_count", json!(5)),
        ("missing_entity_count", json!(4)),
        ("complete_entity_count", json!(3)),
        ("unverified_count", json!(5)),
        ("unverified_count", Value::Null),
    ] {
        let mut bad = opening_inventory_view(linework_source_view(instance_source_view(100), 'b'));
        bad["opening_summary"][field] = value;
        assert_eq!(
            facts.apply_verified_fact(&bad),
            FactProjection::Invalid,
            "{field}"
        );
        assert_eq!(facts.snapshot.openings, previous);
    }
    let mut missing_binding = current.clone();
    missing_binding["kind"] = json!("capabilities");
    missing_binding
        .as_object_mut()
        .unwrap()
        .remove("scene_sha256");
    assert_eq!(
        facts.apply_verified_fact(&missing_binding),
        FactProjection::Invalid
    );
    assert_eq!(
        facts.apply_verified_fact(&opening_inventory_view(linework_source_view(
            instance_source_view(5),
            'b'
        ))),
        FactProjection::Applied
    );
}

#[test]
fn delayed_capabilities_or_recovered_source_shapes_cannot_restore_older_opening_inventory() {
    let mut facts = sequenced_facts();
    let mut latest = opening_inventory_view(linework_source_view(grouped_source_view(8), 'a'));
    latest["opening_summary"]["missing_entity_count"] = json!(1);
    facts.apply_verified_fact(&latest);
    let mut stale_cap = opening_inventory_view(linework_source_view(instance_source_view(7), 'a'));
    stale_cap["kind"] = json!("capabilities");
    assert_eq!(
        facts.apply_verified_fact(&stale_cap),
        FactProjection::Superseded
    );
    let recovered = opening_inventory_view(linework_source_view(instance_source_view(6), 'a'));
    assert_eq!(
        facts.apply_verified_fact(&recovered),
        FactProjection::Applied
    );
    assert_eq!(
        facts
            .snapshot
            .openings
            .as_ref()
            .unwrap()
            .missing_entity_count,
        1
    );
    assert_eq!(facts.fact_sequences["opening_inventory"], 8);
    let mut cap = opening_inventory_view(linework_source_view(instance_source_view(9), 'a'));
    cap["kind"] = json!("capabilities");
    cap["opening_summary"]["missing_entity_count"] = json!(0);
    facts.apply_verified_fact(&cap);
    assert_eq!(
        facts
            .snapshot
            .openings
            .as_ref()
            .unwrap()
            .missing_entity_count,
        0
    );
    let no_inventory = linework_source_view(grouped_source_view(10), 'b');
    assert_eq!(
        facts.apply_verified_fact(&no_inventory),
        FactProjection::Applied
    );
    assert!(facts.snapshot.openings.is_none());
    assert_eq!(facts.apply_verified_fact(&cap), FactProjection::Superseded);
}

#[test]
fn linework_coverage_is_scope_wide_and_shared_by_all_sealed_source_shapes() {
    let mut facts = sequenced_facts();
    let groups = linework_source_view(grouped_source_view(4), 'a');
    assert_eq!(facts.apply_verified_fact(&groups), FactProjection::Applied);
    let summary = facts.snapshot.linework.clone().unwrap();
    assert_eq!(summary.member_count, 8);
    assert_eq!(summary.unselected_member_count, 3);
    assert_eq!(summary.uncovered_count, 2);
    for (sequence, kind) in [(5, "source_linework"), (6, "source_linework_selection")] {
        let mut page = linework_source_view(instance_source_view(sequence), 'a');
        page["kind"] = json!(kind);
        page["members"] = json!([]);
        page["offset"] = json!(40);
        assert_eq!(facts.apply_verified_fact(&page), FactProjection::Applied);
        facts.synchronization_finished(&private_receipt(&page));
        assert_eq!(facts.snapshot.linework.as_ref(), Some(&summary));
        assert!(facts.snapshot.groups.is_some());
        assert_eq!(facts.snapshot.verified_fact.as_ref().unwrap().kind, kind);
    }
}

#[test]
fn changed_linework_selection_invalidates_groups_and_old_pages_cannot_restore_them() {
    let mut facts = sequenced_facts();
    let groups = linework_source_view(grouped_source_view(4), 'a');
    facts.apply_verified_fact(&groups);
    let mut selection = linework_source_view(instance_source_view(6), 'c');
    selection["kind"] = json!("source_linework_selection");
    selection["linework_summary"]["selected_member_count"] = json!(8);
    selection["linework_summary"]["unselected_member_count"] = json!(0);
    assert_eq!(
        facts.apply_verified_fact(&selection),
        FactProjection::Applied
    );
    assert!(facts.snapshot.groups.is_none());
    assert_eq!(
        facts
            .snapshot
            .linework
            .as_ref()
            .unwrap()
            .selected_member_count,
        8
    );
    assert_eq!(
        facts.apply_verified_fact(&groups),
        FactProjection::Superseded
    );
    let fresh = linework_source_view(grouped_source_view(7), 'd');
    assert_eq!(facts.apply_verified_fact(&fresh), FactProjection::Applied);
    assert_eq!(
        facts.apply_verified_fact(&selection),
        FactProjection::Superseded
    );
    assert_eq!(
        facts
            .snapshot
            .linework
            .as_ref()
            .unwrap()
            .selected_member_count,
        5
    );
}

#[test]
fn invalid_linework_totals_cannot_replace_facts_or_advance_the_source_watermark() {
    let mut facts = sequenced_facts();
    let current = linework_source_view(grouped_source_view(4), 'a');
    facts.apply_verified_fact(&current);
    for (field, count) in [
        ("selected_member_count", 9),
        ("unselected_member_count", 4),
        ("candidate_count", 6),
        ("unclassified_candidate_count", 3),
        ("diagnostic_member_count", 9),
    ] {
        let mut bad = linework_source_view(instance_source_view(100), 'b');
        bad["linework_summary"][field] = json!(count);
        assert_eq!(
            facts.apply_verified_fact(&bad),
            FactProjection::Invalid,
            "{field}"
        );
        assert!(facts.snapshot.groups.is_some());
    }
    let mut missing = linework_source_view(instance_source_view(100), 'b');
    missing.as_object_mut().unwrap().remove("linework_summary");
    assert_eq!(facts.apply_verified_fact(&missing), FactProjection::Invalid);
    missing = linework_source_view(instance_source_view(100), 'b');
    missing["linework_summary"]["stale_selection_count"] = json!(-1);
    assert_eq!(facts.apply_verified_fact(&missing), FactProjection::Invalid);
    assert_eq!(
        facts.apply_verified_fact(&linework_source_view(instance_source_view(5), 'b')),
        FactProjection::Applied
    );
}

#[test]
fn source_view_upgrade_requires_general_coverage_and_clears_legacy_shapes() {
    let mut facts = sequenced_facts();
    facts.apply_verified_fact(&sealed_source_view(grouped_source_view(3), 'a'));
    let capability = json!({"contract":CONTRACT,"kind":"capabilities","run_id":"run", "context_sha256":"ctx",
        "fact_sequence":4,"capabilities":{"source_view":{"contract_version":"source-view/2"}}});
    assert_eq!(
        facts.apply_verified_fact(&capability),
        FactProjection::Applied
    );
    assert!(facts.snapshot.groups.is_none());
    assert_eq!(
        facts.apply_verified_fact(&sealed_source_view(grouped_source_view(100), 'b')),
        FactProjection::Superseded
    );
    assert_eq!(
        facts.apply_verified_fact(&linework_source_view(grouped_source_view(5), 'b')),
        FactProjection::Applied
    );
    facts.clear_project_scope();
    assert!(facts.snapshot.linework.is_none());
    assert!(facts.snapshot.groups.is_none());
}

#[test]
fn linework_groups_accept_an_empty_block_only_with_explicit_evidence_kind() {
    let mut value = linework_source_view(grouped_source_view(4), 'a');
    value["groups"][0]["block"] = json!("");
    value["groups"][0]["evidence_kind"] = json!("linework");
    let groups = source_groups_fact(&value).unwrap();
    assert!(groups.rows[0].contains("AI 选择的线稿对象"));
    value["groups"][0]["evidence_kind"] = json!("insert");
    assert!(source_groups_fact(&value).is_none());
}

#[test]
fn source_views_preserve_ai_non_component_reasons_without_hiding_later_reinterpretation() {
    let mut groups = grouped_source_view(4);
    groups["assessed_groups"] = json!(1);
    groups["groups"][0]["assessment"] = json!({
        "category":null,"non_component":true,"basis":"assumed","reason":"图例索引，非楼层实体"
    });
    for instance in groups["groups"][0]["instances"].as_array_mut().unwrap() {
        instance["non_component"] = json!(true);
    }
    let projection = source_groups_fact(&groups).unwrap();
    assert!(projection.rows.iter().any(|row| {
        row.contains("AI 判为非构件 · assumed") && row.contains("图例索引，非楼层实体")
    }));
    groups["groups"][0]["instances"][1]["non_component"] = json!(false);
    let reinterpreted = source_groups_fact(&groups).unwrap();
    assert!(reinterpreted
        .rows
        .iter()
        .all(|row| !row.contains("AI 判为非构件")));
    assert!(reinterpreted
        .rows
        .iter()
        .any(|row| row.contains("图例索引")));

    let mut ledger = instance_source_view(5);
    let row = &mut ledger["objects"][0];
    row["category"] = Value::Null;
    row["state"] = json!("rejected");
    row["non_component"] = json!(true);
    row["classification_basis"] = json!("assumed");
    row["classification_reason"] = json!("图例索引，非楼层实体");
    let projection = source_ledger_fact(&ledger).unwrap();
    assert!(projection
        .rows
        .iter()
        .any(|row| row.contains("判为非构件 · assumed · 依据：图例索引")));
    ledger["objects"][0]["classification_reason"] = json!("");
    assert!(source_ledger_fact(&ledger).is_none());
}

#[test]
fn source_ledger_keeps_aperture_validation_separate_from_missing_opening_entities() {
    let mut value = instance_source_view(5);
    let row = &mut value["objects"][0];
    row["category"] = json!("door");
    row["state"] = json!("pending_confirmation");
    row["aperture_status"] = json!("verified");
    row["entity_status"] = json!("missing");
    let projection = source_ledger_fact(&value).unwrap();
    assert!(projection
        .rows
        .iter()
        .any(|row| row == "洞口 已核验 · 门窗实体 缺失"));
    assert_eq!(projection.counts.get("pending_confirmation"), Some(&1));
    assert!(!projection.counts.contains_key("built"));
    value["objects"][0]["entity_status"] = json!("complete");
    assert!(source_ledger_fact(&value)
        .unwrap()
        .rows
        .iter()
        .any(|row| row == "洞口 已核验 · 门窗实体 已建"));
    value["objects"][0]["entity_status"] = json!("unknown");
    assert!(source_ledger_fact(&value).is_none());
}

fn first_use_fixture() -> (Value, Value) {
    let mut proposal = envelope();
    let item = json!({"review_key":"1".repeat(64),"component_id":"chair.1","component_ids":["chair.1","chair.2"],
        "category":"chair","template_id":"chair.template","definition_sha256":"2".repeat(64),
        "arguments_sha256":"3".repeat(64),"source_kind":"cad","source_instance_id":"source.1",
        "source_sha256":"4".repeat(64),"source_geometry_sha256":"5".repeat(64),
        "source_bindings":[{"component_id":"chair.1","source_instance_id":"source.1","source_sha256":"4".repeat(64),"source_geometry_sha256":"5".repeat(64)},
            {"component_id":"chair.2","source_instance_id":"source.2","source_sha256":"4".repeat(64),"source_geometry_sha256":"5".repeat(64)}]});
    proposal["proposal"]["component_first_use_requirements"] =
        json!({"contract":"component-first-use/1","required":[item.clone()],"reused":[]});
    let mut rendered = item;
    rendered["part_ids"] = json!(["chair.1.seat", "chair.1.base"]);
    rendered["renderer"] = json!({"engine":"BLENDER_WORKBENCH","script_sha256":"6".repeat(64),"blender_version":[4,5,0]});
    rendered["cad_evidence_sha256"] = json!("7".repeat(64));
    rendered["dimensions"] = json!({"cad_xy_m":[0.6,0.5],"model_xyz_m":[0.6,0.5,0.83],"parameters":{"height":0.83},"assumed":[{"message":"height assumed 0.83m"}]});
    rendered["images"] = json!([{"view":"comparison","image_path":std::env::temp_dir().join("first-use.png"),"image_sha256":"8".repeat(64),"dimensions_px":[2400,1800]}]);
    let manifest = json!({"contract":"component-first-use-review/1","proposal_id":"proposal","revision":2,"prepare_token":"token","base_sha256":"base","candidate_sha256":"candidate",
        "requirements_sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&proposal["proposal"]["component_first_use_requirements"]).unwrap())),
        "build_id":"first-use-build","input_sha256":"candidate","blend_sha256":"9".repeat(64),"items":[rendered]});
    let review = json!({"component_first_use_review":{"status":"ready","current":true,"review_set_sha256":"a".repeat(64),"manifest":manifest}});
    (proposal, review)
}

fn first_use_v6_fixture() -> Value {
    // First required group from 740d08876ea94977b3ec127b949c2858.json;
    // retain its identities, direction, installation argument hash and style group.
    let (mut proposal, mut evidence) = first_use_fixture();
    let item = json!({
        "review_key":"043210a7a7a6f11ba46f7bc4e12852bf3879cd5af4ef71bcf75f36d15103e3a3",
        "component_id":"component.cad.f02.1402E2.130359",
        "source_instance_id":"source.cad.f02.instance.1402E2.130359",
        "source_sha256":"5051e4b3d45c2752e2f539eabf4fc57e513fb34fbee664f8c2a4918a4b6c9cd5",
        "source_geometry_sha256":"bd1c92f2386061e5a4c02f23edce84f83aff7b3fb87e3aa271fa8eceafbce307",
        "category":"infusion_chair",
        "template_id":"template.cad-minimal-v2.1a593ea8684ed3f1abb06189ec00b1e0",
        "definition_sha256":"8fe5a0a7b201bcb0fe732a290905bdeabb81c1eb45dc030d141a0f8ed118ae89",
        "arguments_sha256":"8dfb3ef6bd7d8c258f411bd6aa4f5eb1e5499f94389802323b62cdd149c15739",
        "style_signature_sha256":"15b47e9da1c646214306654af10a602fc1a2e42524b07f011b6f476cedb6e332",
        "source_kind":"cad","source_direction":[false,false,180.0],
        "component_ids":["component.cad.f02.1402E2.130359"],
        "source_bindings":[{"component_id":"component.cad.f02.1402E2.130359",
            "source_instance_id":"source.cad.f02.instance.1402E2.130359",
            "source_sha256":"5051e4b3d45c2752e2f539eabf4fc57e513fb34fbee664f8c2a4918a4b6c9cd5",
            "source_geometry_sha256":"bd1c92f2386061e5a4c02f23edce84f83aff7b3fb87e3aa271fa8eceafbce307"}],
        "source_dimension_ranges_m":{"cad_x":[0.8,0.8],"cad_y":[0.5,0.5]}
    });
    proposal["proposal"]["component_first_use_requirements"] = json!({
        "contract":"component-first-use/6","comparison_contract":"component-first-use-comparison/1",
        "required":[item.clone()],"reused":[]});
    proposal["proposal"]["component_plan"] = json!({
        "templates":[{"id":item["template_id"],"metadata":{"template_kind":"cad_minimal_form"}}],
        "components":[{"component_id":item["component_id"],"template_id":item["template_id"],
            "source_instance_id":item["source_instance_id"],"placement_basis":"source",
            "scale":[1.0,1.0,1.0],"rotation_z_deg":180.0}]});
    let manifest = &mut evidence["component_first_use_review"]["manifest"];
    manifest["requirements_sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&proposal["proposal"]["component_first_use_requirements"]).unwrap()
        )
    ));
    let rendered = &mut manifest["items"][0];
    for (key, value) in item.as_object().unwrap() {
        rendered[key] = value.clone();
    }
    rendered["renderer"] = json!({"engine":"REVIEW_WEB_3D"});
    rendered["review_web"] = json!({"review_type":"component_first_use_3d","version":"2",
        "html_path":std::env::temp_dir().join("first-use-web/index.html"),
        "data_sha256":"b".repeat(64),"manifest_sha256":"c".repeat(64),"artifact_sha256":"d".repeat(64),
        "manifest":{"data_sha256":"b".repeat(64),"artifact_sha256":"d".repeat(64),"image_sha256":"8".repeat(64)}});
    proposal["component_first_use_review"] = evidence["component_first_use_review"].clone();
    proposal
}

#[test]
fn first_use_v6_binds_frozen_comparison_and_review_set_before_confirmation() {
    let value = first_use_v6_fixture();
    let mut facts = sequenced_facts();
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    let image = &facts.snapshot.review_images[0];
    let comparison = image
        .binding
        .web_review
        .as_ref()
        .unwrap()
        .first_use_comparison
        .as_ref()
        .unwrap();
    assert_eq!(comparison.proposal_sha256, binding.candidate_sha256);
    assert!(comparison.cad_required);
    assert_eq!(comparison.part_ids, vec!["chair.1.seat", "chair.1.base"]);
    assert_eq!(
        comparison.dimensions,
        value["component_first_use_review"]["manifest"]["items"][0]["dimensions"]
    );
    assert!(!facts.can_authorize(&binding));
    facts.snapshot.review_images[0].file_state = ImageFileState::Available;
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(facts.can_authorize(&binding));
    assert_eq!(
        facts.snapshot.confirmation().review_set_sha256,
        Some("a".repeat(64))
    );
    facts.snapshot.review_images[0].binding.review_set_sha256 = Some("f".repeat(64));
    assert!(!facts.can_authorize(&binding));
    for path in [
        "/proposal/component_first_use_requirements/comparison_contract",
        "/proposal/component_plan/components/0/rotation_z_deg",
        "/component_first_use_review/manifest/requirements_sha256",
        "/component_first_use_review/manifest/items/0/renderer/engine",
        "/component_first_use_review/manifest/items/0/arguments_sha256",
        "/component_first_use_review/manifest/items/0/source_bindings",
        "/component_first_use_review/manifest/items/0/source_direction",
    ] {
        let mut bad = value.clone();
        *bad.pointer_mut(path).unwrap() = json!("changed");
        assert_eq!(
            sequenced_facts().apply_verified_fact(&bad),
            FactProjection::Invalid,
            "{path}"
        );
    }
    let mut referenced = value.clone();
    let dimensions =
        &mut referenced["component_first_use_review"]["manifest"]["items"][0]["dimensions"];
    dimensions.as_object_mut().unwrap().remove("assumed");
    dimensions["assumed_ref"] = json!({"sha256":"e".repeat(64),"count":1000});
    dimensions["assumed_rows"] = json!(["封存假设，重复项合并显示"]);
    assert_eq!(
        sequenced_facts().apply_verified_fact(&referenced),
        FactProjection::Applied
    );
    for (field, bad_value) in [
        ("assumed", json!([])),
        ("assumed_ref", json!({"sha256":"bad","count":1000})),
        ("assumed_rows", json!([12])),
    ] {
        let mut bad = referenced.clone();
        bad["component_first_use_review"]["manifest"]["items"][0]["dimensions"][field] = bad_value;
        assert_eq!(
            sequenced_facts().apply_verified_fact(&bad),
            FactProjection::Invalid,
            "{field}"
        );
    }
}

#[test]
fn first_use_v6_keeps_all_136_groups_and_the_candidate_image() {
    let mut value = first_use_v6_fixture();
    let required = value["proposal"]["component_first_use_requirements"]["required"][0].clone();
    let rendered = value["component_first_use_review"]["manifest"]["items"][0].clone();
    let placement = value["proposal"]["component_plan"]["components"][0].clone();
    let mut requirements = Vec::new();
    let mut items = Vec::new();
    let mut placements = Vec::new();
    for n in 0..136 {
        let mut requirement = required.clone();
        let id = format!("component.{n}");
        requirement["review_key"] = json!(format!("{n:064x}"));
        requirement["component_id"] = json!(id);
        requirement["component_ids"] = json!([id]);
        requirement["source_bindings"][0]["component_id"] = json!(id);
        let mut item = rendered.clone();
        for (key, value) in requirement.as_object().unwrap() {
            item[key] = value.clone();
        }
        let mut placement = placement.clone();
        placement["component_id"] = json!(id);
        requirements.push(requirement);
        items.push(item);
        placements.push(placement);
    }
    value["proposal"]["component_plan"]["components"] = json!(placements);
    value["proposal"]["component_first_use_requirements"]["required"] = json!(requirements);
    value["component_first_use_review"]["manifest"]["items"] = json!(items);
    value["component_first_use_review"]["manifest"]["requirements_sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&value["proposal"]["component_first_use_requirements"]).unwrap()
        )
    ));
    let mut facts = sequenced_facts();
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    assert_eq!(facts.snapshot.review_images.len(), 136);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert_eq!(facts.snapshot.review_images.len(), 137);
    assert!(!facts.can_authorize(&binding));
    for image in &mut facts.snapshot.review_images {
        image.file_state = ImageFileState::Available;
    }
    assert!(facts.can_authorize(&binding));
}

#[tokio::test]
async fn first_use_v6_authorization_passes_the_confirmed_set_and_rejects_a_different_grant() {
    let directory =
        std::env::temp_dir().join(format!("tui-v6-authorization-{}", std::process::id()));
    std::fs::create_dir_all(directory.join("blender_agent/cli")).unwrap();
    std::fs::write(directory.join("context.json"), b"{}").unwrap();
    std::fs::write(directory.join("blender_agent/__init__.py"), "").unwrap();
    std::fs::write(directory.join("blender_agent/cli/__init__.py"), "").unwrap();
    std::fs::write(
        directory.join("blender_agent/cli/main.py"),
        r#"
import sys,json
from pathlib import Path
a=sys.argv
def arg(key): return a[a.index(key)+1]
assert arg('--expected-review-set-sha256') == 'a'*64
print(json.dumps({'ok':True,'kind':'authorization','run_id':'run',
 'context_sha256':__import__('hashlib').sha256(b'{}').hexdigest(),
 'grant':{'authorization_id':'v6-grant','proposal_id':arg('--proposal'),
 'revision':int(arg('--revision')),'prepare_token':arg('--prepare-token'),
 'review_set_sha256':('b' if Path('wrong-grant').exists() else 'a')*64}}))
"#,
    )
    .unwrap();
    let mut setup = sequenced_facts().setup.unwrap();
    setup.repository = directory.clone();
    setup.python = receipt::tests::python();
    setup.context_path = directory.join("context.json");
    setup.context_sha256 = format!("{:x}", Sha256::digest(b"{}"));
    let mut value = first_use_v6_fixture();
    value["context_sha256"] = json!(setup.context_sha256);
    let mut facts = DomainFacts::new(Some(setup.clone()));
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    let confirmation = facts.snapshot.confirmation();
    let binding = confirmation.proposal;
    let text = authorize(
        setup.clone(),
        binding.clone(),
        confirmation.review_set_sha256.clone(),
        Vec::new(),
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(payload["review_set_sha256"], "a".repeat(64));
    assert_eq!(payload["proposal"], serde_json::to_value(&binding).unwrap());
    assert_eq!(
        payload["apply_change_arguments"]["authorization_id"],
        "v6-grant"
    );
    std::fs::write(directory.join("wrong-grant"), b"").unwrap();
    assert!(
        authorize(setup, binding, confirmation.review_set_sha256, Vec::new())
            .await
            .is_err()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

fn first_use_scope_fixture() -> Value {
    let (proposal, evidence) = first_use_fixture();
    let mut scope = project_scope(3, "f08", &"d".repeat(64));
    scope["proposal"]["component_first_use_requirements"] =
        proposal["proposal"]["component_first_use_requirements"].clone();
    scope["component_first_use_review"] = evidence["component_first_use_review"].clone();
    scope["component_first_use_review"]["manifest"]["proposal_id"] =
        scope["proposal"]["proposal_id"].clone();
    scope
}

#[test]
fn project_scope_restores_first_use_review_without_new_image_delivery() {
    let mut facts = sequenced_facts();
    facts.setup.as_mut().unwrap().mode = DomainMode::Project;
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let scope = first_use_scope_fixture();
    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    let review = facts.snapshot.first_use_review.as_ref().unwrap();
    assert_eq!(review.status, "ready");
    assert_eq!(review.review_set_sha256, Some("a".repeat(64)));
    assert_eq!(review.required_count, 1);
    assert_eq!(facts.snapshot.review_images.len(), 1);
    assert_eq!(
        facts.snapshot.review_images[0].file_state,
        ImageFileState::Pending
    );
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(!facts.can_authorize(&binding));
    facts.snapshot.review_images[0].file_state = ImageFileState::Available;
    assert!(facts.can_authorize(&binding));
    assert!(!facts.snapshot.authorizing);
    assert!(facts.snapshot.application.is_none());
}

#[test]
fn project_scope_repeated_after_ready_proposal_preserves_verified_image_state() {
    let mut facts = sequenced_facts();
    facts.setup.as_mut().unwrap().mode = DomainMode::Project;
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let scope = first_use_scope_fixture();
    let mut initial = scope.clone();
    initial
        .as_object_mut()
        .unwrap()
        .remove("component_first_use_review");
    assert_eq!(facts.apply_verified_fact(&initial), FactProjection::Applied);
    assert!(!facts.snapshot.first_use_satisfied());
    let mut proposal = scope.clone();
    proposal["kind"] = json!("proposal");
    proposal["fact_sequence"] = json!(4);
    assert_eq!(
        facts.apply_verified_fact(&proposal),
        FactProjection::Applied
    );
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    facts.snapshot.review_images[0].file_state = ImageFileState::Available;
    facts.snapshot.review_images[0].externally_opened = true;
    facts.snapshot.review_images[0].notice = Some("verified locally".into());
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(facts.can_authorize(&binding));
    for _ in 0..2 {
        assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Applied);
        assert_eq!(
            facts.snapshot.first_use_review.as_ref().unwrap().status,
            "ready"
        );
        assert_eq!(facts.snapshot.review_images.len(), 2);
        let image = &facts.snapshot.review_images[0];
        assert_eq!(image.file_state, ImageFileState::Available);
        assert!(image.externally_opened);
        assert_eq!(image.notice.as_deref(), Some("verified locally"));
        assert!(facts.can_authorize(&binding));
    }
    let mut blocked = scope.clone();
    blocked["fact_sequence"] = json!(5);
    blocked["component_first_use_review"]["current"] = json!(false);
    assert_eq!(facts.apply_verified_fact(&blocked), FactProjection::Applied);
    assert!(!facts.snapshot.first_use_satisfied());
    assert!(!facts.can_authorize(&binding));
}

#[test]
fn project_scope_rejects_invalid_embedded_first_use_manifest_before_activation() {
    for field in [
        "proposal_id",
        "revision",
        "prepare_token",
        "base_sha256",
        "candidate_sha256",
        "input_sha256",
        "requirements_sha256",
        "items",
    ] {
        let mut facts = sequenced_facts();
        facts.setup.as_mut().unwrap().mode = DomainMode::Project;
        facts.apply_verified_fact(&project_catalog(1));
        facts.apply_verified_fact(&project_selection(2, true));
        let mut scope = first_use_scope_fixture();
        scope["component_first_use_review"]["manifest"][field] = json!("foreign");
        assert_eq!(
            facts.apply_verified_fact(&scope),
            FactProjection::Invalid,
            "{field}"
        );
        assert!(facts.snapshot.project_scope.is_none(), "{field}");
        assert!(facts.snapshot.proposal.is_none(), "{field}");
        assert!(facts.snapshot.review_images.is_empty(), "{field}");
    }
}

#[test]
fn project_scope_baseline_restores_ready_review_and_rejects_bad_manifest() {
    let root = std::env::temp_dir().join(format!(
        "domain-scope-first-use-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let directory = root.join("outputs/domain/run/project");
    std::fs::create_dir_all(directory.join("facts")).unwrap();
    std::fs::write(root.join("context.json"), b"{}").unwrap();
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    setup.repository = root.clone();
    setup.context_path = root.join("context.json");
    setup.context_sha256 = format!("{:x}", Sha256::digest(b"{}"));
    let mut values = vec![
        project_catalog(1),
        project_selection(2, true),
        first_use_scope_fixture(),
    ];
    let mut bad = values[2].clone();
    bad["fact_sequence"] = json!(4);
    bad["component_first_use_review"]["manifest"]["candidate_sha256"] = json!("foreign");
    values.push(bad);
    let mut index = serde_json::Map::new();
    for value in &mut values {
        value["context_sha256"] = json!(setup.context_sha256);
        let raw = serde_json::to_vec(value).unwrap();
        let sha = format!("{:x}", Sha256::digest(&raw));
        std::fs::write(directory.join("facts").join(format!("{sha}.json")), raw).unwrap();
        index.insert(sha, value["fact_sequence"].clone());
        std::fs::write(
            directory.join("fact-sequences.json"),
            json!({"next":5,"facts":index}).to_string(),
        )
        .unwrap();
        if value["fact_sequence"].as_u64().unwrap() < 3 {
            continue;
        }
        let resumed = DomainFacts::new(Some(setup.clone()));
        assert_eq!(
            resumed.snapshot.first_use_review.as_ref().unwrap().status,
            "ready"
        );
        assert_eq!(resumed.snapshot.review_images.len(), 1);
        assert_eq!(
            resumed.snapshot.review_images[0].file_state,
            ImageFileState::Pending
        );
        let binding = &resumed.snapshot.proposal.as_ref().unwrap().binding;
        assert!(!resumed.can_authorize(binding));
        assert!(!resumed.snapshot.authorizing);
        assert_eq!(
            resumed.snapshot.rejected_facts.len(),
            usize::from(value["fact_sequence"] == 4)
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

fn structure_revision_fixture() -> (Value, Value) {
    let (mut proposal, mut evidence) = first_use_fixture();
    let certificate = json!({"contract":"cad-structure-revision/1","path":"sealed-certificate.json","sha256":"c".repeat(64)});
    proposal["project_structure"] = json!(true);
    proposal["proposal"]["structure_revision_source"] = certificate.clone();
    proposal["proposal"]["source_dependencies"] = json!({"sealed-certificate.json":"c".repeat(64)});
    proposal["proposal"]["scope"] = json!({"floor_id":"floor","structural_additions":true});
    proposal["proposal"]["draft"]["operations"] = json!([
        {"op":"remove","collection":"walls","target_id":"wall.old","floor_id":"floor","basis":"assumed","source_ids":["cad.1"],"reason":"合并连续墙"},
        {"op":"add","collection":"walls","target_id":"wall.new","floor_id":"floor","basis":"assumed","source_ids":["cad.1"],"reason":"合并连续墙",
         "value_json":json!({"id":"wall.new","start":{"x":0,"y":0},"end":{"x":3,"y":0},"height":3,"thickness":0.2,"source_ids":["cad.1"]}).to_string()}
    ]);
    let binding = json!({"contract":"cad-void-candidate/1","floor_id":"floor","source_sha256":"4".repeat(64),
        "config_sha256":"5".repeat(64),"source_ids":["cad.void"],"kind":"closed_region","geometry_sha256":"6".repeat(64)});
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&binding).unwrap())
    );
    let id = format!("void.{}", &digest[..24]);
    let question =
        json!({"candidate_id":id,"binding":binding,"question":"是否挑空？","status":"keep_slab"});
    let decision = json!({"contract":"cad-void-decision/1","candidate_id":id,"binding":binding,
        "choice":"keep_slab","actor":"user","reason":"确认为楼板","user_statement":"保留楼板"});
    proposal["structure_revision"] = json!({"host_mapping":{"wall.old":"wall.new"},"pending":[],"merged_runs":[],"closed_joints":[],"cad_bridges":[],
        "slab":{"verification_questions":[question.clone()],"void_decisions":[decision]}});
    proposal["verification_questions"] = json!([question]);
    let arguments = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!({"certificate":certificate,"base":"base"})).unwrap()
        )
    );
    let binding = json!({"component_id":"structure.revision","source_instance_id":"structure.revision",
        "source_sha256":"4".repeat(64),"source_geometry_sha256":"5".repeat(64)});
    let wall: Value = serde_json::from_str(
        proposal["proposal"]["draft"]["operations"][1]["value_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let definition = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&json!([wall.clone()])).unwrap())
    );
    let mut key = binding.clone();
    key["definition"] = json!(definition);
    key["arguments"] = json!(arguments);
    let key = format!("{:x}", Sha256::digest(serde_json::to_vec(&key).unwrap()));
    let item = json!({"component_id":"structure.revision","source_instance_id":"structure.revision",
        "source_sha256":"4".repeat(64),"source_geometry_sha256":"5".repeat(64),"review_key":key,
        "definition_sha256":definition,"arguments_sha256":arguments,"source_kind":"cad",
        "category":"structure_revision","template_id":"typed.structure_revision","component_ids":["wall.new"],"entity_ids":["wall.new"],"source_bindings":[binding]});
    proposal["proposal"]["component_first_use_requirements"] = json!({"contract":"component-first-use/structure-revision-1","required":[item.clone()],"reused":[]});
    let rendered = &mut evidence["component_first_use_review"]["manifest"]["items"][0];
    for (key, value) in item.as_object().unwrap() {
        rendered[key] = value.clone();
    }
    rendered["part_ids"] = json!(["wall.new"]);
    rendered["dimensions"]["parts"] = json!([wall]);
    evidence["component_first_use_review"]["manifest"]["requirements_sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&proposal["proposal"]["component_first_use_requirements"]).unwrap()
        )
    ));
    (proposal, evidence)
}

#[test]
fn structural_revision_projects_exact_operations_hosts_void_decisions_and_review() {
    let (proposal, evidence) = structure_revision_fixture();
    let mut facts = sequenced_facts();
    assert_eq!(
        facts.apply_verified_fact(&proposal),
        FactProjection::Applied
    );
    let card = facts.snapshot.proposal.as_ref().unwrap();
    assert!(card.objects.iter().any(|s| s.contains("宿主映射 1")));
    assert!(card
        .objects
        .iter()
        .any(|s| s.contains("移除 walls/wall.old")));
    assert!(card
        .objects
        .iter()
        .any(|s| s.contains("新增 walls/wall.new")));
    assert!(card.objects.iter().any(|s| s.contains("保留楼板")));
    let revision = facts.snapshot.structure_revision.as_ref().unwrap();
    assert_eq!(revision.proposal, card.binding);
    assert_eq!(revision.host_mapping_count, 1);
    assert_eq!(revision.changes.len(), 2);
    assert!(revision.void_decisions[0].contains("保留楼板"));
    let required = review::requirements(&proposal["proposal"]).unwrap();
    let (review, mut images) = review::project(&evidence, &card.binding, &required).unwrap();
    assert!(!review.satisfied(&images));
    images[0].file_state = ImageFileState::Available;
    assert!(review.satisfied(&images));
    for field in [
        "revision",
        "prepare_token",
        "base_sha256",
        "candidate_sha256",
        "requirements_sha256",
    ] {
        let mut changed = evidence.clone();
        changed["component_first_use_review"]["manifest"][field] = json!("tampered");
        assert!(
            review::project(&changed, &card.binding, &required).is_none(),
            "{field}"
        );
    }
    let mut next = envelope();
    next["proposal"]["proposal_id"] = json!("next-proposal");
    next["proposal"]["prepare_token"] = json!("next-token");
    assert_eq!(facts.apply_verified_fact(&next), FactProjection::Applied);
    assert!(facts.snapshot.structure_revision.is_none());
}

#[test]
fn structural_revision_rejects_changed_certificates_operations_and_void_bindings() {
    let (proposal, _) = structure_revision_fixture();
    for pointer in [
        "/proposal/structure_revision_source/sha256",
        "/proposal/structure_revision_source/contract",
        "/proposal/draft/operations/0/floor_id",
        "/proposal/draft/operations/1/basis",
        "/proposal/component_first_use_requirements/contract",
        "/proposal/component_first_use_requirements/required/0/arguments_sha256",
        "/proposal/component_first_use_requirements/required/0/entity_ids/0",
        "/structure_revision/slab/void_decisions/0/choice",
        "/structure_revision/slab/void_decisions/0/binding/geometry_sha256",
        "/verification_questions/0/status",
    ] {
        let mut changed = proposal.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("tampered");
        assert_eq!(
            sequenced_facts().apply_verified_fact(&changed),
            FactProjection::Invalid,
            "{pointer}"
        );
    }
    let mut unresolved = proposal.clone();
    unresolved["structure_revision"]["slab"]["void_decisions"] = json!([]);
    unresolved["structure_revision"]["slab"]["verification_questions"][0]["status"] =
        json!("unanswered");
    unresolved["verification_questions"] =
        unresolved["structure_revision"]["slab"]["verification_questions"].clone();
    assert_eq!(
        sequenced_facts().apply_verified_fact(&unresolved),
        FactProjection::Invalid
    );
    unresolved["proposal"]["pending"] = json!([{"message":"awaiting void decision"}]);
    unresolved["proposal"]["state"] = json!("blocked");
    let mut facts = sequenced_facts();
    assert_eq!(
        facts.apply_verified_fact(&unresolved),
        FactProjection::Applied
    );
    assert!(!facts.snapshot.proposal.unwrap().ready);
}

#[test]
fn structural_audit_and_decision_text_remain_inside_the_receipt_hash() {
    let (proposal, _) = structure_revision_fixture();
    let mut proposal = proposal;
    proposal["fact_sequence"] = json!(1);
    proposal["ok"] = json!(true);
    let root = std::env::temp_dir().join(format!(
        "structure-receipt-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("certificate.json");
    proposal["proposal"]["base_sha256"] = json!("b".repeat(64));
    let proof = json!({"contract":"cad-structure-revision/1","base_sha256":"b".repeat(64),"dependencies":{},
        "audit":proposal["structure_revision"]});
    let proof_raw = serde_json::to_vec(&proof).unwrap();
    std::fs::write(&path, &proof_raw).unwrap();
    let proof_sha = format!("{:x}", Sha256::digest(&proof_raw));
    proposal["proposal"]["structure_revision_source"]["path"] = json!(path);
    proposal["proposal"]["structure_revision_source"]["sha256"] = json!(proof_sha);
    proposal["proposal"]["source_dependencies"] = json!({path.to_str().unwrap():proof_sha});
    let raw = serde_json::to_vec(&proposal).unwrap();
    let binding: serde_json::Map<String, Value> = [
        "proposal_id",
        "revision",
        "prepare_token",
        "base_sha256",
        "candidate_sha256",
    ]
    .into_iter()
    .map(|key| (key.into(), proposal["proposal"][key].clone()))
    .collect();
    let receipt = receipt::FactReceipt {
        contract: "modeling-fact-receipt/v1".into(),
        run_id: "run".into(),
        context_sha256: "ctx".into(),
        kind: "proposal".into(),
        ok: true,
        fact_sequence: 1,
        fact_bytes: raw.len(),
        fact_sha256: format!("{:x}", Sha256::digest(&raw)),
        binding: Value::Object(binding),
    };
    let mut setup = sequenced_facts().setup.unwrap();
    setup.repository = root.clone();
    assert!(receipt::verify(&setup, &receipt, &raw).is_ok());
    for pointer in [
        "/structure_revision/host_mapping/wall.old",
        "/structure_revision/slab/void_decisions/0/reason",
        "/structure_revision/slab/void_decisions/0/user_statement",
        "/proposal/draft/operations/1/value_json",
    ] {
        let mut changed = proposal.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("changed");
        assert!(
            receipt::verify(&setup, &receipt, &serde_json::to_vec(&changed).unwrap()).is_err(),
            "{pointer}"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn structural_review_rejects_changed_geometry_or_missing_and_foreign_meshes() {
    let (proposal, evidence) = structure_revision_fixture();
    let binding = proposal_fact(&proposal).unwrap().binding;
    let requirement = review::requirements(&proposal["proposal"]).unwrap();
    for ids in [
        json!(["unrelated.mesh"]),
        json!([]),
        json!(["wall.new", "foreign.mesh"]),
    ] {
        let mut bad = evidence.clone();
        bad["component_first_use_review"]["manifest"]["items"][0]["part_ids"] = ids;
        assert!(review::project(&bad, &binding, &requirement).is_none());
    }
    let mut bad = evidence.clone();
    bad["component_first_use_review"]["manifest"]["items"][0]["dimensions"]["parts"][0]["end"]
        ["x"] = json!(30);
    assert!(review::project(&bad, &binding, &requirement).is_none());
    let mut changed = proposal.clone();
    let op = &mut changed["proposal"]["draft"]["operations"][1];
    let mut wall: Value = serde_json::from_str(op["value_json"].as_str().unwrap()).unwrap();
    wall["end"]["x"] = json!(30);
    op["value_json"] = json!(wall.to_string());
    changed["component_first_use_review"] = evidence["component_first_use_review"].clone();
    assert_eq!(
        sequenced_facts().apply_verified_fact(&changed),
        FactProjection::Invalid
    );
}

#[test]
fn structural_review_accepts_more_than_sixty_four_typed_parts() {
    let (mut proposal, mut evidence) = structure_revision_fixture();
    let template = proposal["proposal"]["draft"]["operations"][1].clone();
    let mut operations = vec![proposal["proposal"]["draft"]["operations"][0].clone()];
    let mut parts = Vec::new();
    let mut ids = Vec::new();
    for index in 0..70 {
        let id = format!("wall.{index:03}");
        let mut op = template.clone();
        let mut wall: Value = serde_json::from_str(op["value_json"].as_str().unwrap()).unwrap();
        wall["id"] = json!(id);
        op["target_id"] = json!(id);
        op["value_json"] = json!(wall.to_string());
        operations.push(op);
        parts.push(wall);
        ids.push(id);
    }
    proposal["proposal"]["draft"]["operations"] = json!(operations);
    let item = &mut proposal["proposal"]["component_first_use_requirements"]["required"][0];
    item["entity_ids"] = json!(ids);
    item["component_ids"] = json!(ids);
    item["definition_sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&parts).unwrap())
    ));
    let mut key = item["source_bindings"][0].clone();
    key["definition"] = item["definition_sha256"].clone();
    key["arguments"] = item["arguments_sha256"].clone();
    item["review_key"] = json!(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&key).unwrap())
    ));
    let rendered = &mut evidence["component_first_use_review"]["manifest"]["items"][0];
    for (key, value) in item.as_object().unwrap() {
        rendered[key] = value.clone();
    }
    rendered["part_ids"] = json!(ids);
    rendered["dimensions"]["parts"] = json!(parts);
    evidence["component_first_use_review"]["manifest"]["requirements_sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&proposal["proposal"]["component_first_use_requirements"]).unwrap()
        )
    ));
    let required = review::requirements(&proposal["proposal"]).unwrap();
    let binding = proposal_fact(&proposal).unwrap().binding;
    let (_, images) = review::project(&evidence, &binding, &required).unwrap();
    assert_eq!(images.len(), 1);
}

#[test]
fn web_first_use_projects_sealed_package_and_rejects_changed_evidence() {
    let (mut proposal, mut evidence) = first_use_fixture();
    let item = &mut evidence["component_first_use_review"]["manifest"]["items"][0];
    item["renderer"] = json!({"engine":"REVIEW_WEB_3D","backend":"chromium"});
    item["review_web"] = json!({
        "review_type":"component_first_use_3d","version":"2",
        "html_path":std::env::temp_dir().join("first-use-web/index.html"),
        "data_sha256":"b".repeat(64),"manifest_sha256":"c".repeat(64),
        "artifact_sha256":"d".repeat(64),
        "manifest":{"data_sha256":"b".repeat(64),"artifact_sha256":"d".repeat(64),
            "image_sha256":"8".repeat(64)}
    });
    let requirement = review::requirements(&proposal["proposal"]).unwrap();
    let binding = proposal_fact(&proposal).unwrap().binding;
    let (ready, mut images) = review::project(&evidence, &binding, &requirement).unwrap();
    let package = images[0].binding.web_review.as_ref().unwrap();
    let detail = images[0].detail.as_ref().unwrap();
    for value in [
        "component_first_use_3d",
        "REVIEW_WEB_3D",
        &"8".repeat(64),
        &"b".repeat(64),
        &"c".repeat(64),
        &"d".repeat(64),
        &"a".repeat(64),
    ] {
        assert!(detail.contains(value), "{value}");
    }
    assert_eq!(
        package.html_path,
        std::env::temp_dir().join("first-use-web/index.html")
    );
    assert_eq!(package.png_path, std::env::temp_dir().join("first-use.png"));
    assert_eq!(package.data_sha256, "b".repeat(64));
    assert_eq!(package.manifest_sha256, Some("c".repeat(64)));
    assert_eq!(package.artifact_sha256, Some("d".repeat(64)));
    assert_eq!(package.url_hash.as_deref(), Some("view=perspective"));
    assert_eq!(images[0].binding.proposal, binding);
    assert_eq!(images[0].binding.review_set_sha256, ready.review_set_sha256);
    let mut native = evidence.clone();
    let native_manifest = &mut native["component_first_use_review"]["manifest"];
    native_manifest
        .as_object_mut()
        .unwrap()
        .remove("blend_sha256");
    native_manifest["glb_sha256"] = json!("d".repeat(64));
    let (_, native_images) = review::project(&native, &binding, &requirement).unwrap();
    let native_detail = native_images[0].detail.as_ref().unwrap();
    assert!(native_detail.contains(&format!("GLB SHA256：{}", "d".repeat(64))));
    assert!(!native_detail.contains("BLEND SHA256"));
    for invalid in [json!("e".repeat(64)), json!("invalid"), Value::Null] {
        let mut changed = native.clone();
        changed["component_first_use_review"]["manifest"]["glb_sha256"] = invalid;
        assert!(review::project(&changed, &binding, &requirement).is_none());
    }
    for engine in ["PIL_2D_OVERLAY", "BLENDER_WORKBENCH"] {
        let mut changed = native.clone();
        changed["component_first_use_review"]["manifest"]["items"][0]["renderer"]["engine"] =
            json!(engine);
        assert!(review::project(&changed, &binding, &requirement).is_none());
    }
    let mut changed = native.clone();
    changed["component_first_use_review"]["manifest"]
        .as_object_mut()
        .unwrap()
        .remove("glb_sha256");
    assert!(review::project(&changed, &binding, &requirement).is_none());
    images[0].file_state = ImageFileState::Available;
    let mut facts = sequenced_facts();
    facts.snapshot.review_images = images;
    let mut changed = evidence.clone();
    changed["component_first_use_review"]["manifest"]["items"][0]["review_web"]
        ["manifest_sha256"] = json!("e".repeat(64));
    let (_, images) = review::project(&changed, &binding, &requirement).unwrap();
    facts.replace_review_images(images, true);
    assert_eq!(
        facts.snapshot.review_images[0].file_state,
        ImageFileState::Pending
    );
    assert!(!ready.satisfied(&facts.snapshot.review_images));

    for (field, invalid) in [
        ("review_type", json!("component_form_3d")),
        ("version", json!("3")),
        ("data_sha256", json!("invalid")),
        ("manifest_sha256", Value::Null),
        ("artifact_sha256", json!("invalid")),
        ("png_path", json!(std::env::temp_dir().join("other.png"))),
        ("url_hash", json!("view=top")),
        ("html_path", json!("https://example.com/index.html")),
        ("html_path", json!(r"\\server\review\index.html")),
        ("html_path", json!("relative/index.html")),
    ] {
        let mut changed = evidence.clone();
        changed["component_first_use_review"]["manifest"]["items"][0]["review_web"][field] =
            invalid;
        assert!(
            review::project(&changed, &binding, &requirement).is_none(),
            "{field}"
        );
    }
    for field in ["data_sha256", "artifact_sha256", "image_sha256"] {
        let mut changed = evidence.clone();
        changed["component_first_use_review"]["manifest"]["items"][0]["review_web"]["manifest"]
            [field] = json!("e".repeat(64));
        assert!(
            review::project(&changed, &binding, &requirement).is_none(),
            "{field}"
        );
    }
    let mut missing = evidence.clone();
    missing["component_first_use_review"]["manifest"]["items"][0]
        .as_object_mut()
        .unwrap()
        .remove("review_web");
    assert!(review::project(&missing, &binding, &requirement).is_none());

    proposal["component_first_use_review"] = evidence["component_first_use_review"].clone();
    proposal["fact_sequence"] = json!(1);
    proposal["ok"] = json!(true);
    let receipt = private_receipt(&proposal);
    let setup = facts.setup.as_ref().unwrap();
    assert!(receipt::verify(setup, &receipt, &serde_json::to_vec(&proposal).unwrap()).is_ok());
    for field in [
        "html_path",
        "data_sha256",
        "manifest_sha256",
        "artifact_sha256",
    ] {
        let mut changed = proposal.clone();
        changed["component_first_use_review"]["manifest"]["items"][0]["review_web"][field] =
            json!("f".repeat(64));
        assert!(receipt::verify(setup, &receipt, &serde_json::to_vec(&changed).unwrap()).is_err());
    }
}

#[test]
fn web_artifact_binds_glb_and_python_spelled_focus_to_current_proposal() {
    let (proposal, evidence) = first_use_fixture();
    let binding = proposal_fact(&proposal).unwrap().binding;
    let mut value = artifact_envelope("preview", 2, "built", Some(true));
    value["artifact"]["review_image"] =
        evidence["component_first_use_review"]["manifest"]["items"][0]["images"][0].clone();
    value["artifact"]["review_image"]["delivery"] = json!("attached_png");
    value["artifact"]["review_image"]["renderer"] = json!("chromium");
    value["artifact"]["review_image"]["request"] = json!({
        "build_id":"preview","proposal_id":"proposal","revision":2,"prepare_token":"token",
        "input_sha256":"candidate","blend_sha256":"b".repeat(64),"glb_sha256":"c".repeat(64),
        "view":"front","focus_m":[0,0.000001,2,3]
    });
    value["artifact"]["review_image"]["review_web"] = json!({
        "review_type":"component_form_3d","version":"2",
        "html_path":std::env::temp_dir().join("artifact-web/index.html"),
        "data_sha256":"d".repeat(64),"manifest_sha256":"e".repeat(64),
        "artifact_sha256":"c".repeat(64)
    });
    let image = review::artifact_image(&value, &binding).unwrap();
    let detail = image.detail.as_ref().unwrap();
    assert!(detail.contains("component_form_3d"));
    assert!(detail.contains("chromium"));
    for digit in ['8', 'b', 'c', 'd', 'e'] {
        assert!(detail.contains(&digit.to_string().repeat(64)));
    }
    let package = image.binding.web_review.unwrap();
    assert_eq!(
        package.url_hash.as_deref(),
        Some("view=front&focus=0.0,1e-06,2.0,3.0")
    );
    assert_eq!(package.artifact_sha256, Some("c".repeat(64)));
    assert_eq!(image.binding.proposal, binding);
    assert_eq!(image.binding.review_key, "preview");
    assert_eq!(image.binding.review_set_sha256, None);
    let mut native = value.clone();
    native["artifact"]["review_image"]["request"]
        .as_object_mut()
        .unwrap()
        .remove("blend_sha256");
    let native_image = review::artifact_image(&native, &binding).unwrap();
    assert!(native_image.detail.as_ref().unwrap().contains("GLB SHA256"));
    assert!(!native_image
        .detail
        .as_ref()
        .unwrap()
        .contains("BLEND SHA256"));
    let mut changed = native.clone();
    changed["artifact"]["review_image"]["request"]["glb_sha256"] = json!("f".repeat(64));
    assert!(review::artifact_image(&changed, &binding).is_none());
    changed = native.clone();
    changed["artifact"]["review_image"]
        .as_object_mut()
        .unwrap()
        .remove("review_web");
    assert!(review::artifact_image(&changed, &binding).is_none());
    changed = native;
    changed["artifact"]["review_image"]["request"]
        .as_object_mut()
        .unwrap()
        .remove("glb_sha256");
    assert!(review::artifact_image(&changed, &binding).is_none());
    for (field, invalid) in [
        ("glb_sha256", json!("f".repeat(64))),
        ("revision", json!(3)),
        ("prepare_token", json!("foreign")),
        ("view", json!("missing_walls")),
        ("view", json!("perspective")),
        ("focus_m", json!([2, 0, 1, 3])),
        ("focus_m", json!([0, 1, 2])),
    ] {
        let mut changed = value.clone();
        changed["artifact"]["review_image"]["request"][field] = invalid;
        assert!(
            review::artifact_image(&changed, &binding).is_none(),
            "{field}"
        );
    }
    let mut facts = sequenced_facts();
    let mut prepared = proposal;
    prepared["fact_sequence"] = json!(1);
    assert_eq!(
        facts.apply_verified_fact(&prepared),
        FactProjection::Applied
    );
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    value["fact_sequence"] = json!(3);
    value["artifact"]["review_image"]["review_web"]["artifact_sha256"] = json!("f".repeat(64));
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Invalid);
    assert!(!facts.can_authorize(&binding));
}

#[test]
fn reference_evaluation_is_read_only_without_an_active_project_scope() {
    let mut facts = sequenced_facts();
    facts.setup.as_mut().unwrap().mode = DomainMode::Project;
    let mut value = envelope();
    value["kind"] = json!("reference_evaluation");
    value["fact_sequence"] = json!(1);
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Unrelated);
    assert!(facts.snapshot.proposal.is_none());
    assert!(facts.snapshot.review_images.is_empty());
    assert!(facts.snapshot.rejected_facts.is_empty());
    assert!(facts.deferred_sequences.is_empty());
}

#[test]
fn evaluation_receipt_uses_empty_owner_binding_and_seals_new_report_fields() {
    let facts = sequenced_facts();
    let setup = facts.setup.as_ref().unwrap();
    let mut value = json!({
        "contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
        "kind":"reference_evaluation","ok":true,"fact_sequence":1,
        "proposal_id":"proposal","scene_sha256":"b".repeat(64),
        "evaluation":{"input_sha256":"c".repeat(64),
            "artifacts":{"missing_wall_plan_png":"sealed/report.png"},
            "verification_questions":[],"diagnostics":[]}
    });
    let mut receipt = private_receipt(&value);
    receipt.binding = json!({});
    let raw = serde_json::to_vec(&value).unwrap();
    assert_eq!(receipt::verify(setup, &receipt, &raw).unwrap(), value);
    receipt.binding = json!({"proposal_id":"proposal"});
    assert!(receipt::verify(setup, &receipt, &raw).is_err());
    receipt.binding = json!({});
    value["evaluation"]["artifacts"]["missing_wall_plan_png"] = json!("changed/report.png");
    assert!(receipt::verify(setup, &receipt, &serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn first_use_requires_exact_template_arguments_sources_revision_and_opened_review_set() {
    let (proposal, evidence) = first_use_fixture();
    let requirement = review::requirements(&proposal["proposal"]).unwrap();
    let binding = proposal_fact(&proposal).unwrap().binding;
    let (ready, mut images) = review::project(&evidence, &binding, &requirement).unwrap();
    assert!(!ready.satisfied(&images));
    images[0].file_state = ImageFileState::Available;
    assert!(ready.satisfied(&images));
    images[0].externally_opened = true;
    assert!(ready.satisfied(&images));
    images[0].binding.review_set_sha256 = Some("b".repeat(64));
    assert!(!ready.satisfied(&images));
    for field in [
        "definition_sha256",
        "arguments_sha256",
        "source_sha256",
        "source_geometry_sha256",
        "source_instance_id",
        "template_id",
        "component_ids",
        "source_bindings",
    ] {
        let mut changed = evidence.clone();
        changed["component_first_use_review"]["manifest"]["items"][0][field] = json!("foreign");
        assert!(
            review::project(&changed, &binding, &requirement).is_none(),
            "{field}"
        );
    }
    for field in [
        "revision",
        "prepare_token",
        "base_sha256",
        "candidate_sha256",
        "input_sha256",
        "requirements_sha256",
    ] {
        let mut changed = evidence.clone();
        changed["component_first_use_review"]["manifest"][field] = json!("foreign");
        assert!(
            review::project(&changed, &binding, &requirement).is_none(),
            "{field}"
        );
    }
    let mut stale = evidence.clone();
    stale["component_first_use_review"]["current"] = json!(false);
    let (blocked, images) = review::project(&stale, &binding, &ready).unwrap();
    assert!(!blocked.satisfied(&images));
    assert!(images.is_empty());
}

#[test]
fn first_use_accepts_review_set_from_attached_delivery_record() {
    let (proposal, mut evidence) = first_use_fixture();
    let set = "a".repeat(64);
    evidence["component_first_use_review"]["review_set_sha256"] = Value::Null;
    evidence["component_first_use_review"]["delivery"] = json!({
        "delivery": "attached_png",
        "review_key": "1".repeat(64),
        "image_sha256": "8".repeat(64),
        "review_set_sha256": set,
        "request_id": "first-use-0"
    });
    let requirement = review::requirements(&proposal["proposal"]).unwrap();
    let (ready, images) = review::project(
        &evidence,
        &proposal_fact(&proposal).unwrap().binding,
        &requirement,
    )
    .unwrap();
    assert_eq!(ready.review_set_sha256, Some("a".repeat(64)));
    assert_eq!(images.len(), 1);
    evidence["component_first_use_review"]["review_set_sha256"] = json!("b".repeat(64));
    assert!(review::project(
        &evidence,
        &proposal_fact(&proposal).unwrap().binding,
        &requirement,
    )
    .is_none());
}

#[test]
fn first_use_accepts_six_item_batch_after_cumulative_progress_fact() {
    let (mut proposal, mut evidence) = first_use_fixture();
    let template = proposal["proposal"]["component_first_use_requirements"]["required"][0].clone();
    let rendered_template = evidence["component_first_use_review"]["manifest"]["items"][0].clone();
    let mut required = Vec::new();
    let mut items = Vec::new();
    for index in 0..6 {
        let id = format!("opening.batch.{index}");
        let source = format!("source.batch.{index}");
        let key = format!("{index:064x}");
        let image_sha = format!("{:064x}", index + 8);
        let mut requirement = template.clone();
        requirement["review_key"] = json!(key);
        requirement["component_id"] = json!(id);
        requirement["component_ids"] = json!([id]);
        requirement["source_instance_id"] = json!(source);
        requirement["source_bindings"] = json!([requirement["source_bindings"][0].clone()]);
        requirement["source_bindings"][0]["component_id"] = json!(id);
        requirement["source_bindings"][0]["source_instance_id"] = json!(source);
        let mut item = rendered_template.clone();
        item["review_key"] = json!(key);
        item["component_id"] = json!(id);
        item["component_ids"] = json!([id]);
        item["source_instance_id"] = json!(source);
        item["source_bindings"] = json!([item["source_bindings"][0].clone()]);
        item["source_bindings"][0]["component_id"] = json!(id);
        item["source_bindings"][0]["source_instance_id"] = json!(source);
        item["images"][0]["image_sha256"] = json!(image_sha);
        required.push(requirement);
        items.push(item);
    }
    proposal["proposal"]["component_first_use_requirements"]["required"] = json!(required);
    evidence["component_first_use_review"]["completed"] = json!(6);
    evidence["component_first_use_review"]["total"] = json!(6);
    evidence["component_first_use_review"]["manifest"]["items"] = json!(items);
    evidence["component_first_use_review"]["manifest"]["requirements_sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&proposal["proposal"]["component_first_use_requirements"]).unwrap()
        )
    ));
    let requirement = review::requirements(&proposal["proposal"]).unwrap();
    let (ready, images) = review::project(
        &evidence,
        &proposal_fact(&proposal).unwrap().binding,
        &requirement,
    )
    .unwrap();
    assert_eq!(ready.completed, Some(6));
    assert_eq!(ready.total, Some(6));
    assert_eq!(images.len(), 6);
}

#[test]
fn first_use_projection_failure_names_the_missing_review_set_field() {
    let (_, mut evidence) = first_use_fixture();
    evidence["component_first_use_review"]["review_set_sha256"] = Value::Null;
    let reason = review::project_failure_reason(&evidence);
    assert!(reason.contains("delivery.review_set_sha256"), "{reason}");
    evidence["component_first_use_review"]["status"] = json!("invented_status");
    let reason = review::project_failure_reason(&evidence);
    assert!(
        reason.contains("component_first_use_review.status"),
        "{reason}"
    );
}

#[test]
fn first_use_v3_accepts_group_ranges_and_blocked_manifest_entries() {
    let (mut proposal, mut evidence) = first_use_fixture();
    proposal["proposal"]["component_first_use_requirements"]["contract"] =
        json!("component-first-use/3");
    proposal["proposal"]["component_first_use_requirements"]["required"][0]
        ["source_dimension_ranges_m"] = json!({"cad_x":[0.6,0.6],"cad_y":[0.5,0.5]});
    let requirement = review::requirements(&proposal["proposal"]).unwrap();
    evidence["component_first_use_review"]["manifest"]["items"][0]["source_dimension_ranges_m"] =
        proposal["proposal"]["component_first_use_requirements"]["required"][0]
            ["source_dimension_ranges_m"]
            .clone();
    evidence["component_first_use_review"]["manifest"]["requirements_sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&proposal["proposal"]["component_first_use_requirements"]).unwrap()
        )
    ));
    evidence["component_first_use_review"]["manifest"]["blocked"] =
        json!([{"review_key":"missing"}]);
    let (ready, images) = review::project(
        &evidence,
        &proposal_fact(&proposal).unwrap().binding,
        &requirement,
    )
    .unwrap();
    assert_eq!(ready.blocked_count, 1);
    assert!(!ready.satisfied(&images));
}

#[test]
fn first_use_v5_and_v6_separate_source_directions_and_bind_the_manifest() {
    for (contract, template_kind) in [
        ("component-first-use/5", "schematic_extrusion"),
        ("component-first-use/5", "cad_minimal_form"),
        ("component-first-use/6", "schematic_extrusion"),
        ("component-first-use/6", "cad_minimal_form"),
    ] {
        let (mut proposal, mut evidence) = first_use_fixture();
        // Minimal seq 2387 structure: schematic template, source-bound placement,
        // and a normalized CAD direction (295.714285714... -> 295.714286).
        proposal["proposal"]["component_plan"] = json!({
        "templates":[{"id":"chair.template","metadata":{"template_kind":template_kind}}],
        "components":[
            {"component_id":"chair.1","template_id":"chair.template","source_instance_id":"source.1",
             "placement_basis":"source","scale":[1.0,1.0,1.0],"rotation_z_deg":-64.28571428571428},
            {"component_id":"chair.2","template_id":"chair.template","source_instance_id":"source.2",
             "placement_basis":"source","scale":[1.0,1.0,1.0],"rotation_z_deg":295.7142857142857}
        ]});
        let requirements = &mut proposal["proposal"]["component_first_use_requirements"];
        requirements["contract"] = json!(contract);
        requirements["required"][0]["source_direction"] = json!([false, false, 295.714286]);
        requirements["required"][0]["style_signature_sha256"] = json!("c".repeat(64));
        for (field, value) in requirements["required"][0].as_object().unwrap() {
            evidence["component_first_use_review"]["manifest"]["items"][0][field] = value.clone();
        }
        evidence["component_first_use_review"]["manifest"]["requirements_sha256"] = json!(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(requirements).unwrap())
        ));
        let requirement = review::requirements(&proposal["proposal"]).unwrap();
        let binding = proposal_fact(&proposal).unwrap().binding;
        let (ready, images) = review::project(&evidence, &binding, &requirement).unwrap();
        assert_eq!(ready.status, "ready");
        assert_eq!(images.len(), 1);
        for direction in [
            Value::Null,
            json!([]),
            json!([false, false]),
            json!([0, false, 295.714286]),
            json!([false, false, "295.714286"]),
            json!([false, false, -1]),
            json!([false, false, 360]),
            json!([true, false, 295.714286]),
            json!([false, false, 90.0]),
        ] {
            let mut bad = proposal.clone();
            bad["proposal"]["component_first_use_requirements"]["required"][0]
                ["source_direction"] = direction.clone();
            assert!(
                review::requirements(&bad["proposal"]).is_none(),
                "{direction}"
            );
            let mut changed = evidence.clone();
            changed["component_first_use_review"]["manifest"]["items"][0]["source_direction"] =
                direction;
            assert!(review::project(&changed, &binding, &requirement).is_none());
        }
        let mut missing = proposal.clone();
        missing["proposal"]["component_first_use_requirements"]["required"][0]
            .as_object_mut()
            .unwrap()
            .remove("source_direction");
        assert!(review::requirements(&missing["proposal"]).is_none());
        for (field, value) in [
            ("rotation_z_deg", json!(90.0)),
            ("scale", json!([-1.0, 1.0, 1.0])),
        ] {
            let mut bad = proposal.clone();
            bad["proposal"]["component_plan"]["components"][1][field] = value;
            assert!(review::requirements(&bad["proposal"]).is_none(), "{field}");
        }
        let mut bad = proposal.clone();
        bad["proposal"]["component_first_use_requirements"]["required"][0]["source_bindings"][1]
            ["source_geometry_sha256"] = json!("f".repeat(64));
        assert!(review::requirements(&bad["proposal"]).is_none());
        let mut changed = evidence.clone();
        changed["component_first_use_review"]["manifest"]["items"][0]["source_bindings"][1]
            ["source_geometry_sha256"] = json!("f".repeat(64));
        assert!(review::project(&changed, &binding, &requirement).is_none());
    }
}

#[test]
fn first_use_cad_edit_binds_source_direction_and_rendered_edit_pose_separately() {
    for basis in ["source", "user_request"] {
        let mut proposal = edit_envelope();
        let (fixture, mut evidence) = first_use_fixture();
        let mut item =
            fixture["proposal"]["component_first_use_requirements"]["required"][0].clone();
        item["component_id"] = json!("bed.1");
        item["component_ids"] = json!(["bed.1"]);
        item["template_id"] = json!("bed.template");
        item["category"] = json!("bed");
        item["source_direction"] = json!([true, false, 295.714286]);
        item["source_bindings"] = json!([item["source_bindings"][0].clone()]);
        item["source_bindings"][0]["component_id"] = json!("bed.1");
        let origin = if basis == "source" {
            json!([2, 2, 0])
        } else {
            json!([4, 2, 0])
        };
        let angle = if basis == "source" {
            -64.28571428571428
        } else {
            90.0
        };
        let p = &mut proposal["proposal"];
        p["component_edit"]["plan"]["replacements"] = json!({
            "templates":[{"id":"bed.template","metadata":{"template_kind":"cad_minimal_form"}}],
            "components":[{"component_id":"bed.1","template_id":"bed.template", "source_instance_id":"source.1",
                "placement_basis":basis,"origin":origin,"scale":[-1,1,1],"rotation_z_deg":angle}]
        });
        p["component_edit"]["sources"] = json!({"source.1":{
            "source_id":"source.1","component_id":"bed.1", "source_sha256":"4".repeat(64),
            "geometry_sha256":"5".repeat(64),"scale":[-1,1], "origin_m":[2,2],"rotation_z_deg":295.7142857142857
        }});
        p["component_edit"]["selected_parts"]["bed.1"][0]["source_ids"] = json!(["source.1"]);
        for op in p["draft"]["operations"].as_array_mut().unwrap() {
            op["source_ids"] = json!(["source.1"]);
        }
        let mut part: Value =
            serde_json::from_str(p["draft"]["operations"][1]["value_json"].as_str().unwrap())
                .unwrap();
        part["source_ids"] = json!(["source.1"]);
        part["origin"] = origin;
        part["rotation_z_deg"] = json!(angle);
        part["scale"] = json!([-1, 1, 1]);
        p["draft"]["operations"][1]["value_json"] = json!(part.to_string());
        p["component_first_use_requirements"] =
            json!({"contract":"component-first-use/5", "required":[item.clone()],"reused":[]});
        let manifest = &mut evidence["component_first_use_review"]["manifest"];
        for (key, value) in item.as_object().unwrap() {
            manifest["items"][0][key] = value.clone();
        }
        manifest["requirements_sha256"] = json!(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&p["component_first_use_requirements"]).unwrap())
        ));
        manifest["items"][0]["part_ids"] = json!(["bed.1.body"]);
        manifest["items"][0]["dimensions"]["parts"] = json!([part]);
        let requirement = review::requirements(&proposal["proposal"]).unwrap();
        let binding = proposal_fact(&proposal).unwrap().binding;
        let (ready, images) = review::project(&evidence, &binding, &requirement).unwrap();
        assert_eq!(ready.status, "ready");
        assert!(images[0].detail.as_ref().unwrap().contains("CAD来源方向"));
        for (field, invalid) in [
            ("rotation_z_deg", json!(90)),
            ("scale", json!([1, 1])),
            ("source_id", json!("other")),
            ("component_id", json!("bed.2")),
            ("source_sha256", json!("f".repeat(64))),
            ("geometry_sha256", json!("f".repeat(64))),
        ] {
            let mut bad = proposal.clone();
            bad["proposal"]["component_edit"]["sources"]["source.1"][field] = invalid;
            assert!(
                review::requirements(&bad["proposal"]).is_none(),
                "{basis}/{field}"
            );
        }
        for (field, invalid) in [
            ("template_id", json!("foreign.template")),
            ("source_instance_id", json!("source.other")),
            ("placement_basis", json!("proxy_test")),
            ("origin", json!([4, 2])),
            ("scale", json!([-2, 1, 1])),
        ] {
            let mut bad = proposal.clone();
            bad["proposal"]["component_edit"]["plan"]["replacements"]["components"][0][field] =
                invalid;
            assert!(
                review::requirements(&bad["proposal"]).is_none(),
                "{basis}/{field}"
            );
        }
        if basis == "source" {
            for (field, invalid) in [("rotation_z_deg", json!(90)), ("origin", json!([4, 2, 0]))] {
                let mut bad = proposal.clone();
                bad["proposal"]["component_edit"]["plan"]["replacements"]["components"][0][field] =
                    invalid;
                assert!(
                    review::requirements(&bad["proposal"]).is_none(),
                    "{basis}/{field}"
                );
            }
        }
        let mut missing = proposal.clone();
        missing["proposal"]["component_first_use_requirements"]["required"][0]
            .as_object_mut()
            .unwrap()
            .remove("source_direction");
        assert!(review::requirements(&missing["proposal"]).is_none());
        for (field, invalid) in [
            ("origin", json!([8, 2, 0])),
            ("rotation_z_deg", json!(180)),
            ("scale", json!([1, 1, 1])),
        ] {
            let mut bad = evidence.clone();
            bad["component_first_use_review"]["manifest"]["items"][0]["dimensions"]["parts"][0]
                [field] = invalid;
            assert!(
                review::project(&bad, &binding, &requirement).is_none(),
                "{basis}/{field}"
            );
        }
        let mut duplicate = proposal.clone();
        let placement = duplicate["proposal"]["component_edit"]["plan"]["replacements"]
            ["components"][0]
            .clone();
        duplicate["proposal"]["component_edit"]["plan"]["replacements"]["components"]
            .as_array_mut()
            .unwrap()
            .push(placement);
        assert!(review::requirements(&duplicate["proposal"]).is_none());
    }
}

#[test]
fn first_use_v4_style_groups_project_with_aperture_ranges() {
    for contract in [
        "component-first-use/4",
        "component-first-use/5",
        "component-first-use/6",
    ] {
        let (mut proposal, mut evidence) = first_use_fixture();
        let requirements = &mut proposal["proposal"]["component_first_use_requirements"];
        requirements["contract"] = json!(contract);
        requirements["required"][0]["style_signature_sha256"] = json!("c".repeat(64));
        requirements["required"][0]["source_dimension_ranges_m"] = json!({
            "cad_x":[0.9,1.5],"cad_y":[0.2,0.2],"width":[0.9,1.5],"height":[2.1,2.1],"sill_height":[0,0]
        });
        requirements["reused"] = json!([{"review_key":"a".repeat(64),"approved_review":{
        "proposal_id":"approved","review_set_sha256":"b".repeat(64),
        "domain_directory":"D:/runs/f02-opening-1/changes"}}]);
        requirements["required"][0]["category"] = json!("door");
        requirements["required"][0]["source_bindings"][1]["source_geometry_sha256"] =
            json!("6".repeat(64));
        let requirement = review::requirements(&proposal["proposal"]).unwrap();
        for (field, value) in proposal["proposal"]["component_first_use_requirements"]["required"]
            [0]
        .as_object()
        .unwrap()
        {
            evidence["component_first_use_review"]["manifest"]["items"][0][field] = value.clone();
        }
        evidence["component_first_use_review"]["manifest"]["requirements_sha256"] = json!(format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&proposal["proposal"]["component_first_use_requirements"])
                    .unwrap()
            )
        ));
        let (ready, images) = review::project(
            &evidence,
            &proposal_fact(&proposal).unwrap().binding,
            &requirement,
        )
        .unwrap();
        assert_eq!(ready.status, "ready");
        assert_eq!(ready.blocked_count, 0);
        assert!(!images.is_empty());
        for fault in [
            "hash",
            "representative",
            "source",
            "style",
            "contract",
            "category",
        ] {
            let mut bad = proposal.clone();
            let required = &mut bad["proposal"]["component_first_use_requirements"]["required"][0];
            match fault {
                "hash" => {
                    required["source_bindings"][1]["source_geometry_sha256"] = json!("invalid")
                }
                "representative" => {
                    required["source_bindings"][0]["source_geometry_sha256"] = json!("6".repeat(64))
                }
                "source" => required["source_bindings"][1]["source_sha256"] = json!("6".repeat(64)),
                "style" => required["style_signature_sha256"] = Value::Null,
                "category" => required["category"] = json!("chair"),
                "contract" => {
                    bad["proposal"]["component_first_use_requirements"]["contract"] =
                        json!("component-first-use/3")
                }
                _ => unreachable!(),
            }
            assert!(review::requirements(&bad["proposal"]).is_none(), "{fault}");
        }
        for field in [
            "source_bindings",
            "style_signature_sha256",
            "source_dimension_ranges_m",
        ] {
            let mut changed = evidence.clone();
            changed["component_first_use_review"]["manifest"]["items"][0][field] = Value::Null;
            assert!(
                review::project(
                    &changed,
                    &proposal_fact(&proposal).unwrap().binding,
                    &requirement
                )
                .is_none(),
                "{field}"
            );
        }
        let mut changed = evidence.clone();
        changed["component_first_use_review"]["manifest"]["items"][0]["source_bindings"][1]
            ["source_geometry_sha256"] = json!("7".repeat(64));
        assert!(review::project(
            &changed,
            &proposal_fact(&proposal).unwrap().binding,
            &requirement
        )
        .is_none());
    }
}

#[test]
fn first_use_2d_overlay_is_accepted_for_any_sealed_category() {
    for (category, accepted) in [
        ("door", true),
        ("fire_hydrant", true),
        ("office_chair", true),
    ] {
        let (mut proposal, mut evidence) = first_use_fixture();
        proposal["proposal"]["component_first_use_requirements"]["required"][0]["category"] =
            json!(category);
        let requirement = review::requirements(&proposal["proposal"]).unwrap();
        let item = &mut evidence["component_first_use_review"]["manifest"]["items"][0];
        item["category"] = json!(category);
        item["renderer"] = json!({"engine":"PIL_2D_OVERLAY","version":"1","font":"PIL-default"});
        evidence["component_first_use_review"]["manifest"]["requirements_sha256"] = json!(format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&proposal["proposal"]["component_first_use_requirements"])
                    .unwrap()
            )
        ));
        let projected = review::project(
            &evidence,
            &proposal_fact(&proposal).unwrap().binding,
            &requirement,
        );
        assert_eq!(projected.is_some(), accepted, "{category}");
    }
}

#[test]
fn first_use_async_progress_is_projected_without_images() {
    let (proposal, mut evidence) = first_use_fixture();
    evidence["component_first_use_review"] = json!({
        "status":"running", "current":true, "job_id":"job-1", "completed":12, "total":40,
        "proposal_id":"proposal", "revision":2, "prepare_token":"token",
        "base_sha256":"base", "candidate_sha256":"candidate"
    });
    let requirement = review::requirements(&proposal["proposal"]).unwrap();
    let (running, images) = review::project(
        &evidence,
        &proposal_fact(&proposal).unwrap().binding,
        &requirement,
    )
    .unwrap();
    assert_eq!(running.status, "running");
    assert_eq!(running.completed, Some(12));
    assert_eq!(running.total, Some(40));
    assert!(images.is_empty());
    assert!(!running.satisfied(&images));
}

#[test]
fn modeler_review_states_project_without_turning_pending_work_into_authorization() {
    // bam-integ b5bc642: component_first_use.py:1638-1667;
    // artifact_review_images.py:553-627,665-675 and artifact_worker.py:31-37.
    // domain_tools.py:4749-4823 emits the last six as approval_review.status;
    // keep observed recovery summaries safe if delivered in the first-use slot.
    let statuses = [
        "ready",
        "blocked",
        "not_required",
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
    for status in statuses {
        for required in [true, false] {
            let (mut proposal, evidence) = first_use_fixture();
            if !required {
                proposal["proposal"]["component_first_use_requirements"]["required"] = json!([]);
            }
            if (status == "ready" && !required) || (status == "not_required" && required) {
                continue;
            }
            let mut facts = sequenced_facts();
            assert_eq!(
                facts.apply_verified_fact(&proposal),
                FactProjection::Applied
            );
            let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
            add_reviewed_candidate(&mut facts.snapshot, &binding);
            if required {
                let mut ready = proposal.clone();
                ready["kind"] = json!("component_first_use_review");
                ready["component_first_use_review"] =
                    evidence["component_first_use_review"].clone();
                let review = &mut ready["component_first_use_review"];
                for key in [
                    "proposal_id",
                    "revision",
                    "prepare_token",
                    "base_sha256",
                    "candidate_sha256",
                ] {
                    review[key] = proposal["proposal"][key].clone();
                }
                assert_eq!(facts.apply_verified_fact(&ready), FactProjection::Applied);
                for image in &mut facts.snapshot.review_images {
                    image.file_state = ImageFileState::Available;
                }
                assert!(facts.can_authorize(&binding));
            }
            let mut update = proposal.clone();
            update["kind"] = json!("component_first_use_review");
            update["component_first_use_review"] = evidence["component_first_use_review"].clone();
            let review = &mut update["component_first_use_review"];
            for key in [
                "proposal_id",
                "revision",
                "prepare_token",
                "base_sha256",
                "candidate_sha256",
            ] {
                review[key] = proposal["proposal"][key].clone();
            }
            review["status"] = json!(status);
            if status != "ready" {
                review["manifest"] = Value::Null;
                review["review_set_sha256"] = Value::Null;
                review["completed"] = json!(0);
                review["total"] = json!(0);
            }
            assert_eq!(
                facts.apply_verified_fact(&update),
                FactProjection::Applied,
                "{status}, required={required}"
            );
            assert!(facts.snapshot.rejected_facts.is_empty(), "{status}");
            if status != "ready" && status != "not_required" {
                assert!(
                    !facts.can_authorize(&binding),
                    "{status}, required={required}"
                );
                assert_eq!(facts.snapshot.review_set_sha256(), None, "{status}");
                assert!(
                    facts
                        .snapshot
                        .review_images
                        .iter()
                        .all(|image| !image.required),
                    "{status}"
                );
            }
            let mut invented = update;
            invented["component_first_use_review"]["status"] = json!("invented_status");
            assert_eq!(
                facts.apply_verified_fact(&invented),
                FactProjection::Invalid
            );
            let mut foreign = proposal.clone();
            foreign["kind"] = json!("component_first_use_review");
            foreign["component_first_use_review"] =
                json!({"proposal_id":"foreign","status":status});
            assert_eq!(
                facts.apply_verified_fact(&foreign),
                FactProjection::Superseded
            );
        }
    }
}

#[test]
fn modeler_approval_recovery_summaries_do_not_replace_first_use_proof() {
    // bam-integ b5bc642 integrations/domain_tools.py:4749,4772,4794,4815-4823.
    for status in [
        "restore_applied_result",
        "proposal_not_approvable",
        "preview_required",
        "candidate_failed",
        "candidate_outcome_unknown",
        "restore_candidate_review",
    ] {
        let (mut proposal, _) = first_use_fixture();
        proposal["approval_review"] = json!({"contract":"approval-review/1","status":status,
            "approval_ready":false,"readiness_evaluation":"not_evaluated_by_inspector"});
        let mut facts = sequenced_facts();
        assert_eq!(
            facts.apply_verified_fact(&proposal),
            FactProjection::Applied,
            "{status}"
        );
        let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
        add_reviewed_candidate(&mut facts.snapshot, &binding);
        assert_eq!(
            facts.snapshot.first_use_review.as_ref().unwrap().status,
            "blocked"
        );
        assert!(!facts.can_authorize(&binding), "{status}");
    }
}

#[test]
fn modeler_execution_and_source_states_remain_projectable_without_granting_approval() {
    // Producer fixtures: bam-integ b5bc642. These lists deliberately live apart
    // from the consumers' accepted sets so a removed state is a regression.
    // orchestrator/changes.py:87 (PreparedChange.state).
    for state in [
        "prepared",
        "blocked",
        "superseded",
        "applied",
        "built",
        "unknown",
    ] {
        let mut facts = sequenced_facts();
        let mut value = envelope();
        value["proposal"]["state"] = json!(state);
        assert_eq!(
            facts.apply_verified_fact(&value),
            FactProjection::Applied,
            "proposal: {state}"
        );
        let proposal = facts.snapshot.proposal.as_ref().unwrap();
        assert_eq!(proposal.ready, state == "prepared", "{state}");
        assert!(!facts.can_authorize(&proposal.binding));
    }
    // orchestrator/change_artifacts.py:84-87,226,331-371. cancelled is a
    // pre-existing TUI compatibility value, not a current producer transition.
    for state in [
        "queued",
        "building",
        "unknown",
        "failed",
        "built",
        "cancelled",
    ] {
        for kind in ["artifact", "artifact_validation"] {
            let mut facts = sequenced_facts();
            assert_eq!(
                facts.apply_verified_fact(&envelope()),
                FactProjection::Applied
            );
            let mut value = artifact_envelope("enum-build", 2, state, Some(false));
            value["kind"] = json!(kind);
            assert_eq!(
                facts.apply_verified_fact(&value),
                FactProjection::Applied,
                "{kind}: {state}"
            );
            assert_eq!(facts.snapshot.artifacts.len(), 1);
            assert!(!facts.can_authorize(&facts.snapshot.proposal.as_ref().unwrap().binding));
        }
    }
    // changes.py:1774-1791,1823-1858; workflow.py:175,196,201.
    // application and operation share the same receipt; rejected is retained
    // compatibility, not a transition of the current application workflow.
    for kind in ["application", "operation"] {
        for state in ["applying", "applied", "built", "failed", "rejected"] {
            let mut facts = sequenced_facts();
            assert_eq!(
                facts.apply_verified_fact(&recovery_proposal()),
                FactProjection::Applied
            );
            let mut value = operation_envelope(
                state,
                if state == "applying" {
                    "candidate_present"
                } else {
                    "not_required"
                },
                2,
            );
            value["kind"] = json!(kind);
            assert_eq!(
                facts.apply_verified_fact(&value),
                FactProjection::Applied,
                "{kind}: {state}"
            );
            assert_eq!(facts.snapshot.operation.as_ref().unwrap().state, state);
            assert!(!facts.can_authorize(&facts.snapshot.proposal.as_ref().unwrap().binding));
        }
    }
    for recovery in [
        "candidate_present",
        "base_present",
        "conflict",
        "not_required",
        "reconciled_without_replay",
    ] {
        let mut facts = sequenced_facts();
        facts.apply_verified_fact(&recovery_proposal());
        assert_eq!(
            facts.apply_verified_fact(&operation_envelope("applied", recovery, 2)),
            FactProjection::Applied,
            "{recovery}"
        );
        assert_eq!(
            facts.snapshot.operation.as_ref().unwrap().recovery,
            recovery
        );
    }
    // integrations/tool_jobs.py:66,80,95,107,110,140-178: completed records
    // unwrap into the original business fact; queued is only a recovered input.
    for state in ["running", "unknown", "completed", "queued"] {
        let mut facts = sequenced_facts();
        let value = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx",
            "kind":"tool_operation","state":state,"op_id":"a".repeat(32),"replay_allowed":false});
        assert_eq!(
            facts.apply_verified_fact(&value),
            FactProjection::Unrelated,
            "{state}"
        );
        assert!(facts.snapshot.operation.is_none() && facts.snapshot.proposal.is_none());
    }
    // orchestrator/source_scan.py:1591-1800,1925-1933; source_groups.py:131-147.
    for state in [
        "built",
        "pending_addition",
        "pending_confirmation",
        "suspected_duplicate",
        "rejected",
        "capability_missing",
        "skipped",
    ] {
        let mut facts = sequenced_facts();
        let mut value = instance_source_view(1);
        value["objects"][0]["state"] = json!(state);
        assert_eq!(
            facts.apply_verified_fact(&value),
            FactProjection::Applied,
            "ledger: {state}"
        );
        assert!(facts
            .snapshot
            .ledger
            .as_ref()
            .unwrap()
            .counts
            .contains_key(state));
        assert!(facts.snapshot.proposal.is_none());
    }
    // source_scan.py:1617-1640: geometry binding is explicitly weaker than verification.
    for (aperture, label) in [
        ("verified", "已核验"),
        ("unverified", "待核验"),
        ("geometry_bound", "几何已绑定"),
    ] {
        for entity in ["complete", "missing", "unverified"] {
            let mut facts = sequenced_facts();
            let mut value = instance_source_view(1);
            value["objects"][0]["aperture_status"] = json!(aperture);
            value["objects"][0]["entity_status"] = json!(entity);
            assert_eq!(
                facts.apply_verified_fact(&value),
                FactProjection::Applied,
                "{aperture}/{entity}"
            );
            let rows = &facts.snapshot.ledger.as_ref().unwrap().rows;
            assert!(rows
                .iter()
                .any(|row| row.starts_with(&format!("洞口 {label} ·"))));
            if aperture == "geometry_bound" {
                assert!(rows.iter().all(|row| !row.contains("已核验")));
            }
            assert!(facts.snapshot.proposal.is_none());
        }
    }
}

#[test]
fn modeler_floor_progress_dispositions_and_blockers_are_read_only_summary_values() {
    // bam-integ b5bc642 integrations/domain_tools.py:464-487,611-621,641-699.
    let dispositions = [
        "deferred_collision",
        "skipped_unknown",
        "2d_reference",
        "schematic_built",
        "built",
        "pending",
        "unidentified",
        "skipped_unextractable",
    ];
    let blockers = [
        "unreviewed_regions",
        "uncertain_regions",
        "stale_regions",
        "pending_sources",
        "unidentified_sources",
    ];
    for contract in ["floor-progress/1", "floor-progress/2"] {
        for completion in [false, true] {
            let mut value =
                source_region_review_view(linework_source_view(instance_source_view(1), 'a'));
            if completion {
                value["source_region_summary"]["reviewed_region_count"] = json!(4);
                value["source_region_summary"]["unreviewed_region_count"] = json!(0);
                value["source_region_summary"]["uncertain_region_count"] = json!(0);
                value["source_region_summary"]["stale_review_count"] = json!(0);
            }
            let counts: serde_json::Map<String, Value> = dispositions
                .iter()
                .map(|state| {
                    (
                        (*state).into(),
                        json!(
                            if completion && matches!(*state, "pending" | "unidentified") {
                                0
                            } else {
                                1
                            }
                        ),
                    )
                })
                .collect();
            value["floor_progress"] = json!({"contract":contract,"scan_id":value["scan_id"],"scene_sha256":value["scene_sha256"],
                "categories":[{"category":"chair","built":1,"pending":if completion {0} else {1},"blocked":if completion {0} else {1},"unidentified":if completion {0} else {1}}],
                "drawing_coverage":{"covered":if completion {2} else {1},"total":2,"unit":"来源单位"},"unreviewed_regions":if completion {0} else {2},
                "completion":completion,"completion_blockers":if completion {json!([])} else {json!(blockers)},
                "disposition_counts":counts,"cross_floor_unverified":1,"deferrals":[],"skips":[],"legacy_items":[]});
            let mut facts = sequenced_facts();
            assert_eq!(
                facts.apply_verified_fact(&value),
                FactProjection::Applied,
                "{contract}/{completion}"
            );
            if contract == "floor-progress/2" {
                let progress = facts.snapshot.floor_progress_v2.as_ref().unwrap();
                assert_eq!(progress.completion, completion);
                assert_eq!(
                    progress.completion_blockers.len(),
                    if completion { 0 } else { blockers.len() }
                );
                assert!(dispositions
                    .iter()
                    .all(|state| progress.disposition_counts.contains_key(*state)));
            }
            assert!(facts.snapshot.proposal.is_none() && facts.snapshot.operation.is_none());
        }
    }
}

#[test]
fn modeler_project_and_drawing_states_remain_evidence_without_scene_authorization() {
    // bam-integ b5bc642: project_tools.py:439-469;
    // project_location.py:74-100; project_buildings.py:458-563,617-660;
    // building_report.py:149; drawing_reviews.py:24,123-125,221-230.
    for state in ["selected", "needs_selection"] {
        let mut setup = sequenced_facts().setup.unwrap();
        setup.mode = DomainMode::Project;
        let mut facts = DomainFacts::new(Some(setup));
        facts.apply_verified_fact(&project_catalog(1));
        assert_eq!(
            facts.apply_verified_fact(&project_selection(2, state == "selected")),
            FactProjection::Applied,
            "{state}"
        );
        assert!(facts.snapshot.proposal.is_none());
    }
    for status in ["configured", "derived", "needs_wall_layers"] {
        let mut setup = sequenced_facts().setup.unwrap();
        setup.mode = DomainMode::Project;
        let mut facts = DomainFacts::new(Some(setup));
        facts.apply_verified_fact(&project_catalog(1));
        facts.apply_verified_fact(&project_selection(2, true));
        let mut value = project_location(3);
        value["project_location"]["status"] = json!(status);
        assert_eq!(
            facts.apply_verified_fact(&value),
            FactProjection::Applied,
            "{status}"
        );
        assert_eq!(
            facts.snapshot.project_location.as_ref().unwrap().status,
            status
        );
        assert!(facts.snapshot.proposal.is_none());
    }
    // read_state's internal running/preparing values are normalized to
    // building/unknown before publishing a project_building fact (519-521).
    for state in [
        "queued",
        "building",
        "unknown",
        "cancelled",
        "failed",
        "built",
    ] {
        let mut setup = sequenced_facts().setup.unwrap();
        setup.mode = DomainMode::Project;
        let mut facts = DomainFacts::new(Some(setup));
        facts.apply_verified_fact(&project_catalog(1));
        facts.apply_verified_fact(&project_selection(2, true));
        assert_eq!(
            facts.apply_verified_fact(&project_building(3, "state-fixture", state, None)),
            FactProjection::Applied,
            "{state}"
        );
        assert_eq!(facts.snapshot.buildings[0].state, state);
        assert!(facts.snapshot.proposal.is_none());
    }
    // The helpers retain the evidence/count constraints for connection
    // attention/passed and source reconciliation attention/complete.
    for (value, source, connection) in [
        (project_building_report(3), "attention", "attention"),
        (complete_building_report(3), "complete", "passed"),
    ] {
        let mut setup = sequenced_facts().setup.unwrap();
        setup.mode = DomainMode::Project;
        let mut facts = DomainFacts::new(Some(setup));
        facts.apply_verified_fact(&project_catalog(1));
        facts.apply_verified_fact(&project_selection(2, true));
        assert_eq!(
            facts.apply_verified_fact(&value),
            FactProjection::Applied,
            "{source}/{connection}"
        );
        assert_eq!(facts.snapshot.buildings[0].source_reconciliation, source);
        assert_eq!(facts.snapshot.buildings[0].connection_check, connection);
        assert!(facts.snapshot.proposal.is_none());
    }
    for status in ["not_run", "partial", "recorded"] {
        for verdict in ["error", "doubt", "not_error"] {
            let judgement = json!({"audit_id":"a".repeat(64),"candidate_id":"candidate.1",
                "scene_sha256":"b".repeat(64),"image_sha256":"c".repeat(64),"actor":"platform_agent",
                "human_review":"not_run","scene_application":"not_run",
                "judgement":{"verdict":verdict,"explanation":"局部图纸观察，不是批准"}});
            let mut candidates = vec![
                json!({"id":"candidate.1","detector":"gap","message":"待复核连接",
                "judgement":if status=="not_run" {Value::Null} else {judgement}}),
            ];
            if status == "partial" {
                candidates
                    .push(json!({"id":"candidate.2","detector":"gap","message":"待复核连接"}));
            }
            let value = json!({"contract":CONTRACT,"run_id":"run","context_sha256":"ctx","kind":"drawing_audit","ok":true,
                "drawing_audit":{"audit_id":"a".repeat(64),"scene_sha256":"b".repeat(64),
                    "ai_review":status,"human_review":"not_run","scene_application":"not_run",
                    "candidate_count":candidates.len(),"judged_count":if status=="not_run" {0} else {1},
                    "offset":0,"next_offset":null,"candidates":candidates}});
            let mut facts = sequenced_facts();
            assert_eq!(
                facts.apply_verified_fact(&value),
                FactProjection::Applied,
                "{status}/{verdict}"
            );
            assert!(facts.snapshot.drawing_audit.is_some() && facts.snapshot.proposal.is_none());
        }
    }
}

#[test]
fn missing_first_use_contract_is_unknown_and_user_request_is_explicit_without_fake_cad() {
    let (mut proposal, mut evidence) = first_use_fixture();
    proposal["proposal"]
        .as_object_mut()
        .unwrap()
        .remove("component_first_use_requirements");
    let unknown = review::requirements(&proposal["proposal"]).unwrap();
    assert_eq!(unknown.status, "unknown");
    assert!(!unknown.satisfied(&[]));
    let (mut proposal, _) = first_use_fixture();
    let required = &mut proposal["proposal"]["component_first_use_requirements"]["required"][0];
    required["source_kind"] = json!("user_request");
    required["source_instance_id"] = Value::Null;
    for source in required["source_bindings"].as_array_mut().unwrap() {
        source["source_instance_id"] = Value::Null;
    }
    let requirement = review::requirements(&proposal["proposal"]).unwrap();
    let fields = proposal["proposal"]["component_first_use_requirements"]["required"][0]
        .as_object()
        .unwrap();
    for (key, value) in fields {
        evidence["component_first_use_review"]["manifest"]["items"][0][key] = value.clone();
    }
    evidence["component_first_use_review"]["manifest"]["items"][0]["dimensions"]["cad_xy_m"] =
        Value::Null;
    evidence["component_first_use_review"]["manifest"]["requirements_sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&proposal["proposal"]["component_first_use_requirements"]).unwrap()
        )
    ));
    let (_, images) = review::project(
        &evidence,
        &proposal_fact(&proposal).unwrap().binding,
        &requirement,
    )
    .unwrap();
    assert!(images[0].label.contains("用户请求（无CAD）"));
}

#[test]
fn proposal_categories_count_actual_component_instances_and_keep_all_distinct_assumptions() {
    let (mut proposal, _) = first_use_fixture();
    proposal["proposal"]["draft"]["operations"] = json!([]);
    assert_eq!(
        proposal_category_counts(&proposal["proposal"]),
        [("chair".into(), 2)].into()
    );
    proposal["proposal"]["assumed"] = json!((0..110)
        .map(|n| json!({"message":format!("height {n}")}))
        .collect::<Vec<_>>());
    assert_eq!(proposal_fact(&proposal).unwrap().assumptions.len(), 110);
}

#[test]
fn current_candidate_delivery_alone_cannot_override_a_missing_or_changed_image_file() {
    let mut facts = sequenced_facts();
    let proposal = proposal_fact(&envelope()).unwrap();
    facts.snapshot.proposal = Some(proposal.clone());
    add_reviewed_candidate(&mut facts.snapshot, &proposal.binding);
    assert!(facts.can_authorize(&proposal.binding));
    facts.snapshot.review_images[0].file_state = ImageFileState::Unavailable;
    assert!(!facts.can_authorize(&proposal.binding));
    assert!(facts.snapshot.review_images[0].verified_path().is_none());
    facts.snapshot.review_images.clear();
    assert!(!facts.can_authorize(&proposal.binding));
}

#[tokio::test]
async fn png_verification_rejects_missing_changed_dimensions_and_nonlocal_files_before_opening() {
    let (proposal, mut evidence) = first_use_fixture();
    let dir = std::env::temp_dir().join(format!(
        "tui-review-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    tokio::fs::create_dir(&dir).await.unwrap();
    let path = dir.join("image.png");
    let bytes: Vec<u8> = vec![
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 11, 73, 68, 65, 84, 120, 156, 99, 0, 1, 0, 0, 5, 0, 1,
        13, 10, 45, 180, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    let image = &mut evidence["component_first_use_review"]["manifest"]["items"][0]["images"][0];
    image["image_path"] = json!(path);
    image["dimensions_px"] = json!([1, 1]);
    image["image_sha256"] = json!(format!("{:x}", Sha256::digest(&bytes)));
    let requirements = review::requirements(&proposal["proposal"]).unwrap();
    let binding = proposal_fact(&proposal).unwrap().binding;
    let (_, images) = review::project(&evidence, &binding, &requirements).unwrap();
    let missing = review::open_image(images[0].binding.clone()).await;
    assert!(!missing.file_valid);
    assert!(missing.result.is_err());
    tokio::fs::write(&path, &bytes).await.unwrap();
    assert!(review::verify_image(&images[0].binding).await.is_ok());
    let mut web_image = images[0].binding.clone();
    web_image.web_review = Some(crate::review_web::WebReviewPackage {
        html_path: dir.join("index.html"),
        png_path: path.clone(),
        data_sha256: "tampered".into(),
        manifest_sha256: Some("b".repeat(64)),
        artifact_sha256: Some("c".repeat(64)),
        url_hash: Some("view=perspective".into()),
        first_use_comparison: None,
    });
    assert!(review::verify_image(&web_image).await.is_ok());
    let setup = sequenced_facts().setup.unwrap();
    let error = authorize_with_policy(
        setup,
        binding.clone(),
        web_image.review_set_sha256.clone(),
        vec![web_image],
        crate::config::DomainApprovalPolicy::Manual,
    )
    .await
    .unwrap_err();
    assert!(error.contains("复核"), "{error}");
    tokio::fs::write(&path, [bytes.clone(), vec![0]].concat())
        .await
        .unwrap();
    assert!(review::verify_image(&images[0].binding)
        .await
        .unwrap_err()
        .contains("SHA256"));
    tokio::fs::write(&path, &bytes).await.unwrap();
    evidence["component_first_use_review"]["manifest"]["items"][0]["images"][0]["dimensions_px"] =
        json!([2, 1]);
    let (_, different) = review::project(&evidence, &binding, &requirements).unwrap();
    assert!(review::verify_image(&different[0].binding)
        .await
        .unwrap_err()
        .contains("尺寸"));
    for value in [
        "https://example.com/image.png",
        r"\\server\image.png",
        r"C:\image.exe",
        "relative.png",
    ] {
        evidence["component_first_use_review"]["manifest"]["items"][0]["images"][0]["image_path"] =
            json!(value);
        assert!(review::project(&evidence, &binding, &requirements).is_none());
    }
    tokio::fs::remove_dir_all(&dir).await.unwrap();
}

#[test]
fn failed_external_open_keeps_card_delivery_and_new_review_sets_clear_old_openings() {
    let (proposal, evidence) = first_use_fixture();
    let first_use = review::requirements(&proposal["proposal"]).unwrap();
    let binding = proposal_fact(&proposal).unwrap().binding;
    let (ready, mut images) = review::project(&evidence, &binding, &first_use).unwrap();
    images[0].file_state = ImageFileState::Available;
    images[0].finish_open(review::ImageOpenOutcome {
        file_valid: true,
        result: Err("viewer failed".into()),
    });
    assert!(ready.satisfied(&images));
    assert!(images[0].verified_path().is_some());
    images[0].finish_open(review::ImageOpenOutcome {
        file_valid: true,
        result: Ok(()),
    });
    assert!(ready.satisfied(&images));
    let mut facts = sequenced_facts();
    facts.snapshot.first_use_review = Some(ready);
    facts.snapshot.review_images = images;
    let mut changed = evidence;
    changed["component_first_use_review"]["review_set_sha256"] = json!("b".repeat(64));
    let (new_review, new_images) = review::project(
        &changed,
        &binding,
        facts.snapshot.first_use_review.as_ref().unwrap(),
    )
    .unwrap();
    facts.snapshot.first_use_review = Some(new_review);
    facts.replace_review_images(new_images, true);
    assert!(!facts.snapshot.first_use_satisfied());
    assert!(!facts.snapshot.review_images[0].externally_opened);
    facts.snapshot.review_images[0].finish_open(review::ImageOpenOutcome {
        file_valid: false,
        result: Err("图片不可用".into()),
    });
    assert!(facts.snapshot.review_images[0].verified_path().is_none());
}

#[test]
fn independent_first_use_receipt_and_late_blocked_facts_keep_exact_proposal_ownership() {
    let (mut proposal, evidence) = first_use_fixture();
    proposal["fact_sequence"] = json!(1);
    proposal["ok"] = json!(true);
    let mut facts = sequenced_facts();
    assert_eq!(
        facts.apply_verified_fact(&proposal),
        FactProjection::Applied
    );
    let mut independent = envelope();
    independent.as_object_mut().unwrap().remove("proposal");
    independent["kind"] = json!("component_first_use_review");
    independent["ok"] = json!(true);
    independent["fact_sequence"] = json!(2);
    independent["component_first_use_review"] = evidence["component_first_use_review"].clone();
    for field in [
        "proposal_id",
        "revision",
        "prepare_token",
        "base_sha256",
        "candidate_sha256",
    ] {
        independent["component_first_use_review"][field] = proposal["proposal"][field].clone();
    }
    let setup = facts.setup.as_ref().unwrap();
    let receipt = private_receipt(&independent);
    let raw = serde_json::to_vec(&independent).unwrap();
    assert_eq!(receipt::verify(setup, &receipt, &raw).unwrap(), independent);
    for field in [
        "proposal_id",
        "revision",
        "prepare_token",
        "base_sha256",
        "candidate_sha256",
    ] {
        let mut changed = receipt.clone();
        changed.binding[field] = json!("foreign");
        assert!(receipt::verify(setup, &changed, &raw).is_err(), "{field}");
    }
    assert_eq!(
        facts.apply_verified_fact(&independent),
        FactProjection::Applied
    );
    assert_eq!(facts.snapshot.review_set_sha256(), Some("a".repeat(64)));
    let current_images = facts.snapshot.review_images.clone();
    for field in [
        "proposal_id",
        "revision",
        "prepare_token",
        "base_sha256",
        "candidate_sha256",
    ] {
        let mut late = independent.clone();
        late["fact_sequence"] = json!(3);
        late["component_first_use_review"]["status"] = json!("blocked");
        late["component_first_use_review"][field] = json!("old");
        assert_eq!(
            facts.apply_verified_fact(&late),
            FactProjection::Superseded,
            "{field}"
        );
        assert_eq!(facts.snapshot.review_images, current_images);
        assert_eq!(facts.snapshot.review_set_sha256(), Some("a".repeat(64)));
    }
    independent["component_first_use_review"]["status"] = json!("blocked");
    independent["fact_sequence"] = json!(4);
    assert_eq!(
        facts.apply_verified_fact(&independent),
        FactProjection::Applied
    );
    assert!(facts.snapshot.review_images.is_empty());
    assert!(!facts.snapshot.first_use_satisfied());
}

#[test]
fn review_labels_show_chinese_categories_and_drawing_evidence() {
    assert_eq!(review::category_label("armchair"), "扶手椅");
    assert_eq!(review::category_label("cad_reference"), "图纸参考线");
    assert_eq!(review::category_label("wall_detail"), "墙面细部");
    assert_eq!(review::category_label("未分类"), "其他结构");
    assert_eq!(
        review::category_label("open_custom_category"),
        "open_custom_category"
    );
    let (proposal, evidence) = first_use_fixture();
    let requirement = review::requirements(&proposal["proposal"]).unwrap();
    let binding = proposal_fact(&proposal).unwrap().binding;
    let (ready, images) = review::project(&evidence, &binding, &requirement).unwrap();
    assert!(images[0]
        .label
        .contains("椅子 · 图纸原线稿 · 三维与原图对照"));
    assert!(ready.dimensions[0].contains("参数说明未提供，详见封存清单"));
    assert!(!images[0].label.contains("comparison"));
}

#[test]
fn first_use_details_use_all_chinese_display_parameters_and_never_dump_raw_templates() {
    let (proposal, mut evidence) = first_use_fixture();
    let requirement = review::requirements(&proposal["proposal"]).unwrap();
    let binding = proposal_fact(&proposal).unwrap().binding;
    let dimensions =
        &mut evidence["component_first_use_review"]["manifest"]["items"][0]["dimensions"];
    dimensions["parameters"] =
        json!({"parameter_basis":{"height":"assumed"},"template":"do_not_display_raw_template"});
    dimensions["assumed"] = json!([{"basis":"assumed","message":"do_not_display_raw_assumptions"}]);
    dimensions["category_label"] = json!("示意楼梯");
    let mut parameters = (0..110).map(|index| json!({"name":format!("part.{index}.height"),"label":"高度","value":index,"basis":"assumed","basis_label":"示意","unit_label":"米"})).collect::<Vec<_>>();
    parameters.push(
        json!({"name":"steps","label":"级数","value":16,"basis_label":"图纸","unit_label":"级"}),
    );
    parameters.push(json!({"name":"segments","label":"分段数","value":20,"basis_label":"用户确认","unit_label":"段"}));
    parameters.push(json!({"name":"custom","label":"其他参数","value":7,"basis_label":"示意"}));
    parameters.push(parameters[0].clone());
    dimensions["display_parameters"] = json!(parameters);
    dimensions["interpretation"] = json!([
        "侧面连接采用示意，固定件未模拟",
        "排水与安装五金未核实",
        "侧面连接采用示意，固定件未模拟"
    ]);
    let (ready, images) = review::project(&evidence, &binding, &requirement).unwrap();
    let detail = images[0].detail.as_ref().unwrap();
    assert_eq!(detail, &ready.dimensions[0]);
    for expected in [
        "示意楼梯",
        "图纸尺寸（米）[0.6,0.5]",
        "三维尺寸（米）[0.6,0.5,0.83]",
        "高度：109米（示意）",
        "级数：16级（图纸）",
        "分段数：20段（用户确认）",
        "其他参数：7（示意）",
        "侧面连接采用示意，固定件未模拟",
        "排水与安装五金未核实",
    ] {
        assert!(detail.contains(expected), "{expected} absent from {detail}");
    }
    assert_eq!(detail.matches("高度：").count(), 110);
    assert_eq!(detail.matches("侧面连接采用示意").count(), 1);
    for raw in [
        "parameter_basis",
        "assumed",
        "do_not_display",
        "display_parameters",
        "basis_label",
        "unit_label",
    ] {
        assert!(!detail.contains(raw), "leaked {raw}");
    }
    assert!(!detail.contains("其他参数：7米"));
    assert_eq!(review::category_label("hospital_bed"), "移动病床");
    assert_eq!(review::category_label("stair"), "示意楼梯");
    evidence["component_first_use_review"]["manifest"]["items"][0]["dimensions"]
        .as_object_mut()
        .unwrap()
        .remove("display_parameters");
    let (_, images) = review::project(&evidence, &binding, &requirement).unwrap();
    let fallback = images[0].detail.as_ref().unwrap();
    assert!(fallback.contains("参数说明未提供，详见封存清单"));
    assert!(!fallback.contains("parameter_basis"));
    assert!(!fallback.contains("assumed"));
}

#[test]
fn first_use_form_metadata_is_optional_version_compatible_and_validated_when_present() {
    let (proposal, evidence) = first_use_fixture();
    let requirement = review::requirements(&proposal["proposal"]).unwrap();
    let binding = proposal_fact(&proposal).unwrap().binding;
    assert!(review::project(&evidence, &binding, &requirement).is_some());
    for (version, kind, diagnostic) in [
        (
            "cad-minimal/1",
            "schematic_extrusion",
            "cad_internal_evidence_insufficient",
        ),
        (
            "cad-minimal/2",
            "cad_minimal_form",
            "back_direction_assumed,desktop_closed_outline_unresolved",
        ),
        ("cad-minimal/3", "cad_minimal_form", ""),
    ] {
        let mut rendered = evidence.clone();
        rendered["component_first_use_review"]["manifest"]["items"][0]["dimensions"]
            ["parameters"] = json!({"metadata":{
            "template_kind":kind,"form_version":version,"category_family":"chair",
            "geometry_basis":"CAD local bounds and linework", "diagnostics":diagnostic,
            "reason":"Vertical dimensions and back end remain assumed"
        }});
        let (ready, images) = review::project(&rendered, &binding, &requirement).unwrap();
        let detail = images[0].detail.as_ref().unwrap();
        assert_eq!(detail, &ready.dimensions[0]);
        for expected in [
            version,
            kind,
            "类别族：chair",
            "几何依据：CAD local bounds and linework",
            "Vertical dimensions and back end remain assumed",
        ] {
            assert!(detail.contains(expected), "{expected}");
        }
        if !diagnostic.is_empty() {
            assert!(detail.contains(diagnostic));
        }
        for (field, invalid) in [
            ("form_version", json!(2)),
            ("template_kind", Value::Null),
            ("diagnostics", json!([])),
            ("reason", json!("x".repeat(4001))),
        ] {
            let mut bad = rendered.clone();
            bad["component_first_use_review"]["manifest"]["items"][0]["dimensions"]["parameters"]
                ["metadata"][field] = invalid;
            assert!(
                review::project(&bad, &binding, &requirement).is_none(),
                "{version}/{field}"
            );
        }
        rendered["component_first_use_review"]["manifest"]["items"][0]["dimensions"]
            ["parameters"]["metadata"] = json!({"form_version":version});
        assert!(review::project(&rendered, &binding, &requirement).is_some());
    }
}

#[test]
fn first_use_requirements_digest_survives_python_float_text() {
    // Python writes the shortest round-trip float; re-encoding must reproduce it
    // byte for byte or the sealed requirements digest no longer matches.
    let text = r#"{"cad_x":[0.19999999999999574,2.099999999999998]}"#;
    let value: Value = serde_json::from_str(text).unwrap();
    assert_eq!(serde_json::to_string(&value).unwrap(), text);
}

#[test]
#[ignore = "read-only diagnosis; set DOMAIN_REAL_CONTEXT and DOMAIN_REAL_CWD"]
fn replay_real_context_like_resume() {
    let context = std::env::var("DOMAIN_REAL_CONTEXT").expect("DOMAIN_REAL_CONTEXT");
    let cwd = std::env::var("DOMAIN_REAL_CWD").expect("DOMAIN_REAL_CWD");
    let config = configure(Config {
        cwd: cwd.into(),
        domain_context: Some(context.into()),
        ..Config::default()
    })
    .unwrap();
    let facts = DomainFacts::new(config.domain_setup.clone());
    let s = &facts.snapshot;
    eprintln!(
        "proposal={:?}",
        s.proposal
            .as_ref()
            .map(|p| (p.binding.proposal_id.clone(), p.binding.revision, p.ready))
    );
    eprintln!(
        "first_use={:?}",
        s.first_use_review.as_ref().map(|r| (
            &r.status,
            r.required_count,
            r.blocked_count,
            &r.review_set_sha256
        ))
    );
    eprintln!(
        "images={} required={}",
        s.review_images.len(),
        s.review_images.iter().filter(|i| i.required).count()
    );
    eprintln!("sync={:?}", s.fact_sync_notice);
}

#[test]
#[ignore = "read-only diagnosis"]
fn replay_real_one_fact() {
    let context = std::env::var("DOMAIN_REAL_CONTEXT").unwrap();
    let cwd = std::env::var("DOMAIN_REAL_CWD").unwrap();
    let path = std::env::var("DOMAIN_REAL_FACT").unwrap();
    let config = configure(Config {
        cwd: cwd.into(),
        domain_context: Some(context.into()),
        ..Config::default()
    })
    .unwrap();
    let mut facts = DomainFacts::new(config.domain_setup.clone());
    let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let result = facts.apply_verified_fact(&value);
    eprintln!("result={result:?} failure={:?}", facts.projection_failure);
    eprintln!("reason={}", review::project_failure_reason(&value));
    let manifest = &value["component_first_use_review"]["manifest"];
    eprintln!(
        "manifest contract={} items={} renderer0={}",
        manifest["contract"],
        manifest["items"].as_array().map_or(0, Vec::len),
        manifest["items"][0]["renderer"]
    );
}

#[test]
fn project_activation_clears_sync_errors_from_the_previous_scope() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let mut first = project_scope(3, "f08", &"d".repeat(64));
    first["project_scope"]["activation_revision"] = json!(1);
    assert_eq!(facts.apply_verified_fact(&first), FactProjection::Applied);
    // A one-off fact sealed while the scope was switching failed to project.
    facts.synchronization_failed("opening_type_representatives", 4, "scope changed".into());
    assert!(facts.snapshot.fact_sync_notice.is_some());
    let mut second = project_scope(6, "f09.b", &"e".repeat(64));
    second["project_scope"]["activation_revision"] = json!(2);
    assert_eq!(facts.apply_verified_fact(&second), FactProjection::Applied);
    assert!(facts.synchronization_errors.is_empty());
    // A failure after the activation still blocks until re-verified.
    facts.synchronization_failed("artifact", 9, "corrupt".into());
    assert_eq!(facts.synchronization_errors.get("artifact"), Some(&9));
}

#[test]
fn polled_late_scope_activation_is_applied_and_reprojects_later_facts() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let mut first = project_scope(3, "f08", &"d".repeat(64));
    first["project_scope"]["activation_revision"] = json!(1);
    assert_eq!(
        facts.apply_polled_fact(&first),
        Some(FactProjection::Applied)
    );
    // A background job activates scope 2 (seq 6) but a later tool receipt for that
    // scope (seq 9) is projected first and fails against the old scope.
    let mut second = project_scope(6, "f09.b", &"e".repeat(64));
    second["project_scope"]["activation_revision"] = json!(2);
    let mut later = second.clone();
    later["fact_sequence"] = json!(9);
    facts.highest_sequence = 9;
    facts.synchronization_failed("project_scope", 9, "scope not current".into());
    assert!(facts.poll_floor() <= 3);
    assert_eq!(
        facts.apply_polled_fact(&second),
        Some(FactProjection::Applied)
    );
    assert_eq!(
        facts
            .snapshot
            .project_scope
            .as_ref()
            .unwrap()
            .activation_revision,
        Some(2)
    );
    assert_eq!(facts.synchronization_errors.get("project_scope"), Some(&9));
    assert_eq!(facts.highest_sequence, 9);
    // The rescanned later fact now projects under the right scope.
    assert_eq!(
        facts.apply_polled_fact(&later),
        Some(FactProjection::Applied)
    );
    // Facts that are neither newer nor a late activation are ignored by the poller.
    assert_eq!(facts.apply_polled_fact(&first), None);
}

#[test]
fn sealed_manual_grant_survives_busy_sync_but_not_a_changed_or_applied_version() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let mut scope = project_scope(3, "f08", &"d".repeat(64));
    scope["project_scope"]["activation_revision"] = json!(1);
    assert_eq!(facts.apply_verified_fact(&scope), FactProjection::Applied);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    // The agent keeps tools running: facts are syncing when the grant comes back.
    facts.synchronizing = true;
    assert!(!facts.can_authorize(&binding));
    assert!(facts.shows_binding(&binding));
    let mut other = binding.clone();
    other.revision += 1;
    assert!(!facts.shows_binding(&other));
    facts.snapshot.operation = Some(crate::domain::OperationFact {
        operation_id: "op".into(),
        state: "applied".into(),
        recovery: "not_required".into(),
        current_sha256: String::new(),
    });
    assert!(!facts.shows_binding(&binding));
}

#[tokio::test]
#[ignore = "read-only diagnosis; set DOMAIN_REAL_CONTEXT, DOMAIN_REAL_CWD, DOMAIN_REAL_FACTS"]
async fn replay_real_facts_in_sequence() {
    let context = std::env::var("DOMAIN_REAL_CONTEXT").unwrap();
    let cwd = std::env::var("DOMAIN_REAL_CWD").unwrap();
    let dir = std::env::var("DOMAIN_REAL_FACTS").unwrap();
    let from: u64 = std::env::var("DOMAIN_REAL_FROM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let config = configure(Config {
        cwd: cwd.into(),
        domain_context: Some(context.into()),
        ..Config::default()
    })
    .unwrap();
    // Replay the captured sequence from empty state, without a second baseline scan.
    let mut facts = DomainFacts::new(None);
    facts.setup = config.domain_setup;
    let mut values: Vec<Value> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| std::fs::read(e.ok()?.path()).ok())
        .filter_map(|b| serde_json::from_slice(&b).ok())
        .collect();
    values.sort_by_key(|v| v["fact_sequence"].as_u64().unwrap_or(0));
    let last_sequence = values.last().and_then(|v| v["fact_sequence"].as_u64());
    let mut invalid_sequences = Vec::new();
    for value in values {
        let seq = value["fact_sequence"].as_u64().unwrap_or(0);
        let kind = value["kind"].as_str().unwrap_or("").to_owned();
        let result = facts
            .apply_polled_fact(&value)
            .unwrap_or(FactProjection::Unrelated);
        if seq >= from && kind == "project_scope" {
            eprintln!("ACTIVATION seq={seq} result={result:?}");
        }
        if seq >= from && matches!(kind.as_str(), "proposal" | "fact_page") {
            eprintln!("PROJECTION seq={seq} kind={kind} result={result:?}");
        }
        if seq >= from && result == FactProjection::Invalid {
            invalid_sequences.push(seq);
            let scope = facts
                .snapshot
                .project_scope
                .as_ref()
                .map(|s| (s.scope_id[..12].to_owned(), s.activation_revision));
            eprintln!(
                "INVALID seq={seq} kind={kind} current_scope={scope:?} failure={:?} incoming={:?}",
                facts.projection_failure,
                value["project_scope"]["scope_id"]
                    .as_str()
                    .map(|s| &s[..12])
            );
        }
    }
    for image in &mut facts.snapshot.review_images {
        image.file_state = if review::verify_image(&image.binding).await.is_ok() {
            ImageFileState::Available
        } else {
            ImageFileState::Unavailable
        };
    }
    facts.synchronization_queue_drained();
    eprintln!(
        "FINAL can_authorize={} poll_floor={} sync_notice={:?}",
        facts
            .snapshot
            .proposal
            .as_ref()
            .is_some_and(|p| facts.can_authorize(&p.binding)),
        facts.poll_floor(),
        facts.snapshot.fact_sync_notice
    );
    eprintln!("FINAL readiness={:?} operation={:?} candidate_review={} first_use={} pending_dependencies={}",
        facts.snapshot.proposal.as_ref().map(|p| (p.binding.revision, p.ready, p.state.as_str())),
        facts.snapshot.operation.as_ref().map(|operation| operation.state.as_str()),
        facts.snapshot.proposal.as_ref().is_some_and(|p| facts.snapshot.candidate_review_delivered(&p.binding)),
        facts.snapshot.first_use_satisfied(), facts.deferred_sequences.len());
    for fact in facts
        .snapshot
        .rejected_facts
        .iter()
        .filter(|fact| fact.sequence >= from)
    {
        eprintln!(
            "REJECTED kind={} seq={} blocking={} dependency_rejected={} reason={}",
            fact.kind, fact.sequence, fact.blocking, fact.dependency_rejected, fact.reason
        );
    }
    eprintln!("REPLAY last_sequence={last_sequence:?} invalid_sequences={invalid_sequences:?}");
    eprintln!(
        "FINAL scope={:?} proposal_objects={:?}",
        facts
            .snapshot
            .project_scope
            .as_ref()
            .map(|s| (&s.scope_id, s.activation_revision)),
        facts.snapshot.proposal.as_ref().map(|p| p.objects.len())
    );
}

#[test]
fn immutable_fact_pages_are_unrelated_without_project_scope() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    let mut page = project_catalog(2459);
    page["kind"] = json!("fact_page");
    page.as_object_mut().unwrap().remove("project_scope");
    page["items"] = json!([{"id":"source.1"}]);
    assert_eq!(
        facts.apply_polled_fact(&page),
        Some(FactProjection::Unrelated)
    );
    assert!(facts.snapshot.rejected_facts.is_empty());
    assert!(facts.snapshot.fact_sync_notice.is_none());
    assert_eq!(facts.poll_floor(), 2459);
}

#[test]
fn rejected_unrelated_fact_does_not_stop_current_proposal_or_poll_floor() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_polled_fact(&project_catalog(1));
    facts.apply_polled_fact(&project_selection(2, true));
    let mut scope = project_scope(3, "f08", &"d".repeat(64));
    scope["project_scope"]["activation_revision"] = json!(2);
    assert_eq!(
        facts.apply_polled_fact(&scope),
        Some(FactProjection::Applied)
    );
    let mut rejected = scope.clone();
    rejected["kind"] = json!("proposal");
    rejected["fact_sequence"] = json!(4);
    rejected["project_scope"]["scope_id"] = json!("e".repeat(64));
    rejected["proposal"]["summary"] = json!(null);
    assert_eq!(
        facts.apply_polled_fact(&rejected),
        Some(FactProjection::Invalid)
    );
    facts.synchronization_failed("proposal", 4, "fallback".into());
    assert!(facts.synchronization_errors.is_empty());
    assert_eq!(facts.poll_floor(), 4);
    let mut proposal = scope.clone();
    proposal["kind"] = json!("proposal");
    proposal["fact_sequence"] = json!(5);
    assert_eq!(
        facts.apply_polled_fact(&proposal),
        Some(FactProjection::Applied)
    );
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(facts.can_authorize(&binding));
    assert_eq!(facts.poll_floor(), 5);
    assert!(!facts.snapshot.rejected_facts[0].blocking);
    assert!(facts.snapshot.fact_sync_notice.is_none());
}

#[test]
fn rejected_current_dependencies_block_until_matching_valid_replacement_or_new_activation() {
    for kind in [
        "proposal",
        "artifact",
        "artifact_validation",
        "component_first_use_review",
        "project_scope",
    ] {
        let mut setup = sequenced_facts().setup.unwrap();
        setup.mode = DomainMode::Project;
        let mut facts = DomainFacts::new(Some(setup));
        facts.apply_polled_fact(&project_catalog(1));
        facts.apply_polled_fact(&project_selection(2, true));
        let mut scope = project_scope(3, "f08", &"d".repeat(64));
        scope["project_scope"]["activation_revision"] = json!(1);
        facts.apply_polled_fact(&scope);
        let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
        add_reviewed_candidate(&mut facts.snapshot, &binding);
        assert!(facts.can_authorize(&binding));
        let mut bad = scope.clone();
        bad["kind"] = json!(kind);
        bad["fact_sequence"] = json!(4);
        bad["proposal"]["summary"] = json!(null);
        if kind == "project_scope" {
            bad["proposal"]["proposal_id"] = json!("another-family");
        }
        let owner = json!({"proposal_id":binding.proposal_id,"revision":binding.revision,
            "prepare_token":binding.prepare_token,"base_sha256":binding.base_sha256,
            "candidate_sha256":binding.candidate_sha256,"build_id":"bad-build"});
        bad["artifact"] = owner.clone();
        bad["component_first_use_review"] = owner;
        assert_eq!(
            facts.apply_polled_fact(&bad),
            Some(FactProjection::Invalid),
            "{kind}"
        );
        assert!(!facts.can_authorize(&binding), "{kind}");
        let notice = facts.snapshot.fact_sync_notice.as_deref().unwrap();
        assert!(
            notice.contains(kind)
                && notice.contains("seq 4")
                && notice.contains(&facts.snapshot.rejected_facts[0].reason),
            "{notice}"
        );
        assert_eq!(facts.poll_floor(), 4);
        // Reading the same activation again does not repair an invalid dependency.
        let mut reread = scope.clone();
        reread["fact_sequence"] = json!(5);
        if kind != "project_scope" && kind != "proposal" {
            facts.apply_polled_fact(&reread);
            assert!(!facts.can_authorize(&binding), "{kind}");
        }
        let mut next = project_scope(6, "f09.b", &"e".repeat(64));
        next["project_scope"]["activation_revision"] = json!(2);
        assert_eq!(
            facts.apply_polled_fact(&next),
            Some(FactProjection::Applied)
        );
        let next_binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
        add_reviewed_candidate(&mut facts.snapshot, &next_binding);
        assert!(facts.can_authorize(&next_binding), "{kind}");
        assert!(!facts.snapshot.rejected_facts[0].blocking);
        assert!(facts.snapshot.fact_sync_notice.is_none());
    }
}

#[test]
fn late_activation_dependency_retries_after_independent_facts_advance() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_polled_fact(&project_catalog(1));
    facts.apply_polled_fact(&project_selection(2, true));
    let mut scope = project_scope(3, "f08", &"d".repeat(64));
    scope["project_scope"]["activation_revision"] = json!(1);
    let mut proposal = scope.clone();
    proposal["kind"] = json!("proposal");
    proposal["fact_sequence"] = json!(4);
    assert_eq!(
        facts.apply_polled_fact(&proposal),
        Some(FactProjection::Deferred)
    );
    assert!(facts.snapshot.rejected_facts.is_empty());
    assert_eq!(facts.poll_floor(), 0);
    assert_eq!(
        facts.apply_polled_fact(&scope),
        Some(FactProjection::Applied)
    );
    assert_eq!(
        facts.apply_polled_fact(&proposal),
        Some(FactProjection::Applied)
    );
    assert_eq!(facts.poll_floor(), 4);
}

#[test]
fn malformed_old_activation_is_visible_without_blocking_current_activation() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_polled_fact(&project_catalog(1));
    facts.apply_polled_fact(&project_selection(2, true));
    let mut scope = project_scope(3, "f08", &"d".repeat(64));
    scope["project_scope"]["activation_revision"] = json!(2);
    facts.apply_polled_fact(&scope);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    let mut old = scope.clone();
    old["fact_sequence"] = json!(4);
    old["project_scope"]["activation_revision"] = json!(1);
    old["project_scope"]["component_discovery"] = json!("malformed");
    assert_eq!(facts.apply_polled_fact(&old), Some(FactProjection::Invalid));
    assert!(facts.can_authorize(&binding));
    assert!(!facts.snapshot.rejected_facts[0].blocking);
    assert_eq!(facts.poll_floor(), 4);
}

#[test]
fn rejected_activation_does_not_pin_polling_through_its_dependent_facts() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup));
    facts.apply_polled_fact(&project_catalog(1));
    facts.apply_polled_fact(&project_selection(2, true));
    let mut scope = project_scope(3, "f08", &"d".repeat(64));
    scope["project_scope"]["activation_revision"] = json!(1);
    facts.apply_polled_fact(&scope);
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    let mut rejected = project_scope(4, "f09.b", &"e".repeat(64));
    rejected["project_scope"]["activation_revision"] = json!(2);
    rejected["proposal"]["summary"] = json!(null);
    assert_eq!(
        facts.apply_polled_fact(&rejected),
        Some(FactProjection::Invalid)
    );
    let mut dependent = rejected.clone();
    dependent["kind"] = json!("proposal");
    dependent["fact_sequence"] = json!(5);
    assert_eq!(
        facts.apply_polled_fact(&dependent),
        Some(FactProjection::DependencyRejected)
    );
    assert_eq!(facts.poll_floor(), 5);
    assert!(facts.deferred_sequences.is_empty());
    assert!(facts.can_authorize(&binding));
    let failure = &facts.snapshot.rejected_facts[1];
    assert!(failure.dependency_rejected);
    assert!(failure.reason.contains("project_scope (seq 4)"));
}

#[test]
fn missing_proposal_identity_cannot_prove_a_rejected_dependency_is_historical() {
    let mut facts = sequenced_facts();
    facts.apply_verified_fact(&envelope());
    let binding = facts.snapshot.proposal.as_ref().unwrap().binding.clone();
    add_reviewed_candidate(&mut facts.snapshot, &binding);
    assert!(facts.can_authorize(&binding));
    let mut bad = artifact_envelope("bad", 9, "built", Some(true));
    bad["artifact"]
        .as_object_mut()
        .unwrap()
        .remove("proposal_id");
    bad["artifact"]["revision"] = json!(1);
    assert_eq!(facts.apply_verified_fact(&bad), FactProjection::Invalid);
    assert!(!facts.can_authorize(&binding));
    assert!(facts.snapshot.rejected_facts[0].blocking);
}

#[test]
fn new_read_only_facts_accept_receipts_without_inventing_approval_or_scope() {
    let mut setup = sequenced_facts().setup.unwrap();
    setup.mode = DomainMode::Project;
    let mut facts = DomainFacts::new(Some(setup.clone()));
    facts.apply_verified_fact(&project_catalog(1));
    facts.apply_verified_fact(&project_selection(2, true));
    let scope = project_scope(3, "f08", &"d".repeat(64));
    facts.apply_verified_fact(&scope);
    let previous = facts.snapshot.clone();
    for (index, kind) in [
        "wall_mount_inspection",
        "source_region_inventory",
        "job_index",
        "delivery_status",
        "diagnostic_log",
        "project_floor_preparing",
        "precedent_job",
        "tool_operation",
    ]
    .iter()
    .enumerate()
    {
        let mut value = json!({"contract":CONTRACT,"kind":kind,"run_id":"run","context_sha256":"ctx",
            "fact_sequence":index+4,"ok":true,"state":"running","changed":true,"next_poll_after_s":0,
            "recommended_call":{"tool":"inspect_background_job","arguments":{"wait_s":60}},
            "wait_instruction":"server waits","delivery_status":{"missing":0}});
        if !matches!(
            *kind,
            "diagnostic_log" | "project_floor_preparing" | "tool_operation"
        ) {
            value["project_scope"] = scope["project_scope"].clone();
        }
        let raw = serde_json::to_vec(&value).unwrap();
        let receipt = receipt::FactReceipt {
            contract: "modeling-fact-receipt/v1".into(),
            run_id: "run".into(),
            context_sha256: "ctx".into(),
            kind: (*kind).into(),
            ok: true,
            fact_sequence: (index + 4) as u64,
            fact_bytes: raw.len(),
            fact_sha256: format!("{:x}", Sha256::digest(&raw)),
            binding: json!({}),
        };
        assert!(receipt::validate(&setup, &receipt).is_ok(), "{kind}");
        let verified = receipt::verify(&setup, &receipt, &raw).unwrap();
        assert_eq!(
            facts.apply_verified_fact(&verified),
            FactProjection::Unrelated,
            "{kind}"
        );
        assert!(facts.snapshot == previous, "{kind}");
        value["changed"] = json!(false);
        assert!(receipt::verify(&setup, &receipt, &serde_json::to_vec(&value).unwrap()).is_err());
        value["run_id"] = json!("foreign");
        assert_eq!(
            DomainFacts::new(Some(setup.clone())).apply_verified_fact(&value),
            FactProjection::Invalid,
            "{kind}"
        );
        if !matches!(
            *kind,
            "diagnostic_log" | "project_floor_preparing" | "tool_operation"
        ) {
            value["run_id"] = json!("run");
            value.as_object_mut().unwrap().remove("project_scope");
            assert_eq!(
                DomainFacts::new(Some(setup.clone())).apply_verified_fact(&value),
                FactProjection::Invalid,
                "{kind}"
            );
        }
    }
}

#[test]
fn server_wait_advice_never_overrides_bound_artifact_state_or_integrity() {
    let mut facts = sequenced_facts();
    facts.apply_verified_fact(&envelope());
    let mut value = artifact_envelope("waiting", 4, "building", None);
    value["state"] = json!("built");
    value["changed"] = json!(true);
    value["next_poll_after_s"] = json!(0);
    value["recommended_call"] =
        json!({"tool":"validate_artifact","arguments":{"build_id":"waiting","wait_s":60}});
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    assert_eq!(facts.snapshot.artifacts[0].state, ArtifactState::Building);
    assert!(!facts.snapshot.artifacts[0].reviewed);
    value["artifact"]["state"] = json!("invented");
    value["fact_sequence"] = json!(5);
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Invalid);
}

#[test]
fn frozen_slab_revision_projects_boundary_and_rejects_other_edit_fields() {
    let mut value = envelope();
    value["project_structure"] = json!(true);
    value["proposal"]["scope"] = json!({"floor_id":"floor","structural_additions":true});
    value["proposal"]["slab_revision_source"] = json!({"contract":"cad-slab-revision/1","path":"slab-certificate.json","sha256":"c".repeat(64)});
    value["proposal"]["source_dependencies"] = json!({"slab-certificate.json":"c".repeat(64)});
    value["proposal"]["draft"]["operations"] = json!([{"op":"set","collection":"slabs","target_id":"slab.1","field":"boundary",
        "floor_id":"floor","basis":"assumed","reason":"恢复冻结CAD外轮廓","source_ids":["cad.1"],
        "value_json":json!([{"x":0,"y":0},{"x":3,"y":0},{"x":3,"y":3}]).to_string()}]);
    let mut facts = sequenced_facts();
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    assert!(facts.snapshot.proposal.unwrap().objects[0].contains("楼板轮廓修订"));
    for field in ["holes", "thickness"] {
        value["proposal"]["draft"]["operations"][0]["field"] = json!(field);
        assert_eq!(
            sequenced_facts().apply_verified_fact(&value),
            FactProjection::Invalid
        );
    }
}

#[test]
fn filtered_opening_pages_keep_the_full_inventory_counts() {
    let mut value = opening_inventory_view(linework_source_view(instance_source_view(4), 'a'));
    value["kind"] = json!("capabilities");
    value["opening_inventory"] =
        json!({"items":[],"matched_count":0,"total_count":4,"opening_ids":[],"missing_only":true});
    let mut facts = sequenced_facts();
    assert_eq!(facts.apply_verified_fact(&value), FactProjection::Applied);
    let inventory = facts.snapshot.openings.unwrap();
    assert_eq!(inventory.total_count, 4);
    assert_eq!(inventory.missing_entity_count, 2);
    assert!(!inventory.complete());
}
