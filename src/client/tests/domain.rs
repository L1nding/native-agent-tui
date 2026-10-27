use super::*;

#[tokio::test]
#[ignore = "requires session-e2e1d70b and sealed facts from the validation worktree"]
async fn replay_full_floor_advance_platform_block_stops_auto_continue() {
    use std::collections::HashMap;
    use std::path::PathBuf;
    let records: Vec<Value> =
        std::fs::read_to_string("target/codex-tmp/replay/session-e2e1d70b.jsonl")
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let fixture = PathBuf::from("target/codex-tmp/replay/platform-block-python");
    std::fs::create_dir_all(fixture.join("blender_agent/integrations")).unwrap();
    for file in [
        "blender_agent/__init__.py",
        "blender_agent/integrations/__init__.py",
    ] {
        std::fs::write(fixture.join(file), "").unwrap();
    }
    let context = PathBuf::from("D:/project/bam-workflow-validation-20261022/outputs/domain/project-29052-1792673801123156600/context.json");
    std::fs::write(
        fixture.join("context.json"),
        std::fs::read(context).unwrap(),
    )
    .unwrap();
    std::fs::write(fixture.join("blender_agent/integrations/project_mcp.py"), r#"import sys
from pathlib import Path
sha = sys.argv[sys.argv.index('--read-fact') + 1]
sys.stdout.buffer.write((Path(r'D:/project/bam-workflow-validation-20261022/outputs/domain/project-29052-1792673801123156600/project/facts') / (sha + '.json')).read_bytes())
"#).unwrap();
    let fixture = fixture.canonicalize().unwrap();
    let source = std::path::Path::new("D:/project/bam-workflow-validation-20261022/outputs/domain/project-29052-1792673801123156600/project/facts");
    let baseline = fixture.join("outputs/domain/project-29052-1792673801123156600/project/facts");
    std::fs::create_dir_all(&baseline).unwrap();
    let mut pending = vec![
        "673b1c06a0476ca5f7441c93bc78aaf0bc87f9a7b578bf2763d86487e85f8261".to_owned(),
        "10a57ecaddb783bb11893cbbdb3b346a023bfb9ca9df8790b7787ab00072d197".to_owned(),
        "182cc03cc0e4e5b67cd371bfea7a9f6107b1d60a32c429e1eb3c2c76e8de3016".to_owned(),
        "a4a991420c68ae844cfb19db3429afeb2d884b0221f27861b00beea7fddad9c9".to_owned(),
        "95089184fd96cbbfe8360fc5b3f2d4c87a7a63a9f22e295074c829f1c992299b".to_owned(),
    ];
    let mut index = serde_json::Map::new();
    while let Some(sha) = pending.pop() {
        if index.contains_key(&sha) {
            continue;
        }
        let raw = std::fs::read(source.join(format!("{sha}.json"))).unwrap();
        let value: Value = serde_json::from_slice(&raw).unwrap();
        index.insert(sha.clone(), value["fact_sequence"].clone());
        std::fs::write(baseline.join(format!("{sha}.json")), raw).unwrap();
        let mut fields = vec![value];
        while let Some(field) = fields.pop() {
            match field {
                Value::Object(values) => {
                    if let Some(pages) = values.get("$fact_pages").and_then(Value::as_array) {
                        pending.extend(pages.iter().map(|page| page.as_str().unwrap().to_owned()));
                    }
                    fields.extend(values.into_values());
                }
                Value::Array(values) => fields.extend(values),
                _ => {}
            }
        }
    }
    std::fs::write(
        baseline.parent().unwrap().join("fact-sequences.json"),
        serde_json::to_vec(&json!({"next":2075,"facts":index})).unwrap(),
    )
    .unwrap();
    // Replay both real terminal boundaries, also returning the same sealed result
    // directly from advance_floor to cover the synchronous tool completion path.
    for direct in [false, true] {
        for (start_seq, end_seq) in [(277, 310), (320, 353)] {
            let (mut client, mut server) = harness_with_config(Config {
                auto_continue: 20,
                domain_setup: Some(crate::domain::DomainSetup {
                    mode: crate::domain::DomainMode::Project,
                    context_path: fixture.join("context.json"),
                    repository: fixture.clone(),
                    python: "D:/project/bam-workflow-validation-20261022/.venv/Scripts/python.exe"
                        .into(),
                    run_id: "project-29052-1792673801123156600".into(),
                    context_sha256:
                        "bb49ba58552e29bfc9e993c4061b292c19174f1b325debce6ca9851b8b767186".into(),
                    floor_id: "项目目标".into(),
                }),
                ..Default::default()
            })
            .await;
            running_root(&mut client, &mut server).await;
            assert_eq!(
                client.snapshots.borrow().extensions[0]
                    .modeling()
                    .unwrap()
                    .floor_progress_v2
                    .as_ref()
                    .map(|progress| progress.completion),
                Some(false)
            );
            let mut calls = HashMap::new();
            for record in records.iter().filter(|r| {
                r["seq"]
                    .as_u64()
                    .is_some_and(|seq| seq >= start_seq && seq <= end_seq)
            }) {
                let event = match record["type"].as_str() {
                    Some("tool/call") => {
                        let name = record["data"]["name"].as_str().unwrap();
                        let id = record["data"]["callId"].as_str().unwrap();
                        calls.insert(id.to_owned(), name.to_owned());
                        name.strip_prefix("mcp__modeling_domain__").map(|tool| {
                            let tool = if direct && tool == "inspect_tool_operation" { "advance_floor" } else { tool };
                            json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":id,"type":"mcpToolCall","server":"modeling_domain","tool":tool,"status":"inProgress"}}})
                        })
                    }
                    Some("tool/result") => {
                        let message = &record["data"]["message"];
                        let id = message["toolCallId"].as_str().unwrap();
                        calls.get(id).and_then(|name| name.strip_prefix("mcp__modeling_domain__")).map(|tool| {
                            let tool = if direct && tool == "inspect_tool_operation" { "advance_floor" } else { tool };
                            json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn","item":{"id":id,"type":"mcpToolCall","server":"modeling_domain","tool":tool,"status":"completed","result":{"content":message["content"].clone()}}}})
                        })
                    }
                    Some("turn/end") => Some(
                        json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}}),
                    ),
                    _ => None,
                };
                if let Some(event) = event {
                    observation_event(&mut client, &mut server, event).await;
                }
            }
            tokio::time::timeout(
                Duration::from_secs(20),
                client.snapshots.wait_for(|snapshot| {
                    snapshot.root_start_requests > 1
                        || snapshot
                            .notice
                            .as_deref()
                            .is_some_and(|notice| notice.contains("推进器报告平台阻塞"))
                }),
            )
            .await
            .ok();
            let snapshot = client.snapshots.borrow().clone();
            assert_eq!(
                snapshot.root_start_requests, 1,
                "automatic continuation at real seq {end_seq}, direct={direct}; notice={:?}",
                snapshot.notice
            );
            assert!(snapshot.notice.as_deref().unwrap_or("").contains("推进器报告平台阻塞（invalid_fact）"), "missing platform blocker at real seq {end_seq}, direct={direct}; notice={:?}; domain={:?}", snapshot.notice, snapshot.extensions[0].modeling().unwrap().rejected_facts.iter().map(|fact| &fact.reason).collect::<Vec<_>>());
            let domain = snapshot.extensions[0].modeling().unwrap();
            let verified = domain.verified_fact.as_ref().unwrap();
            assert_eq!(verified.kind, "floor_advance");
            assert_eq!(
                (verified.sequence, verified.bytes),
                if end_seq == 310 {
                    (2164, 412009)
                } else {
                    (2166, 795573)
                }
            );
            let needs = snapshot.extensions[0]
                .modeling()
                .unwrap()
                .floor_advance_needs
                .as_ref()
                .unwrap();
            assert_eq!(needs.split_batch_allowed, Some(false));
            assert_eq!(
                needs.next_action.as_deref(),
                Some("stop_and_report_original_binding_and_diagnostic")
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(150), next(&mut server))
                    .await
                    .is_err()
            );
            client.commands.send(Command::Quit).await.unwrap();
            client.join.await.unwrap();
        }
    }
}

#[tokio::test]
#[ignore = "requires the supplied full session and sealed facts from the validation worktree"]
async fn replay_full_advance_floor_approval_card_is_projected_before_auto_continue() {
    use sha2::Digest;
    use std::collections::HashMap;
    use std::path::PathBuf;

    let replay = PathBuf::from("target/codex-tmp/replay/session-862606cb-full.jsonl");
    assert!(
        replay.is_file(),
        "missing real replay: {}",
        replay.display()
    );
    let fixture = PathBuf::from("target/codex-tmp/replay/advance-floor-python");
    std::fs::create_dir_all(fixture.join("blender_agent/cli")).unwrap();
    std::fs::create_dir_all(fixture.join("blender_agent/integrations")).unwrap();
    std::fs::write(
        fixture.join("blender_agent/__init__.py"),
        "__path__.append(r'D:/project/bam-workflow-validation-20261022/src/blender_agent')\n",
    )
    .unwrap();
    std::fs::write(fixture.join("blender_agent/cli/__init__.py"), "").unwrap();
    std::fs::write(fixture.join("blender_agent/integrations/__init__.py"), "").unwrap();
    let context = PathBuf::from("D:/project/bam-workflow-validation-20261022/outputs/domain/project-29052-1792673801123156600/context.json");
    let context_hash = format!(
        "{:x}",
        sha2::Sha256::digest(std::fs::read(&context).unwrap())
    );
    std::fs::write(
        fixture.join("context.json"),
        std::fs::read(&context).unwrap(),
    )
    .unwrap();
    std::fs::write(
        fixture.join("blender_agent/integrations/project_mcp.py"),
        r#"import sys,json
from pathlib import Path
roots = [
 r'D:/project/bam-workflow-validation-20261022/outputs/domain/project-29052-1792673801123156600/project/facts',
 r'D:/project/bam-workflow-validation-20261022/outputs/domain/project-29052-1792673801123156600-db8dc1473b54/changes/facts',
]
if '--authorize' in sys.argv:
    Path('approval-arguments.json').write_text(json.dumps(sys.argv),encoding='utf-8')
    def arg(name):
        return sys.argv[sys.argv.index(name)+1]
    context=Path(arg('--context')).read_bytes()
    import hashlib
    print(json.dumps({'ok':True,'kind':'authorization','run_id':'project-29052-1792673801123156600',
      'context_sha256':hashlib.sha256(context).hexdigest(),'grant':{'authorization_id':'replay-grant',
      'proposal_id':arg('--proposal'),'revision':int(arg('--revision')),'prepare_token':arg('--prepare-token'),
      'review_set_sha256':arg('--expected-review-set-sha256')}}))
    raise SystemExit(0)
sha = sys.argv[sys.argv.index('--read-fact') + 1]
for root in roots:
    p = Path(root) / (sha + '.json')
    if p.is_file():
        sys.stdout.buffer.write(p.read_bytes())
        break
else:
    raise SystemExit(2)
"#,
    ).unwrap();
    let fixture = fixture.canonicalize().unwrap();
    let source = context.parent().unwrap().join("project/facts");
    let facts = fixture.join("outputs/domain/project-29052-1792673801123156600/project/facts");
    std::fs::create_dir_all(&facts).unwrap();
    let mut pending = vec![
        "673b1c06a0476ca5f7441c93bc78aaf0bc87f9a7b578bf2763d86487e85f8261".to_owned(),
        "10a57ecaddb783bb11893cbbdb3b346a023bfb9ca9df8790b7787ab00072d197".to_owned(),
        "182cc03cc0e4e5b67cd371bfea7a9f6107b1d60a32c429e1eb3c2c76e8de3016".to_owned(),
        "a4a991420c68ae844cfb19db3429afeb2d884b0221f27861b00beea7fddad9c9".to_owned(),
        "95089184fd96cbbfe8360fc5b3f2d4c87a7a63a9f22e295074c829f1c992299b".to_owned(),
    ];
    let mut index = serde_json::Map::new();
    while let Some(sha) = pending.pop() {
        if index.contains_key(&sha) {
            continue;
        }
        let raw = std::fs::read(source.join(format!("{sha}.json"))).unwrap();
        let value: Value = serde_json::from_slice(&raw).unwrap();
        index.insert(sha.clone(), value["fact_sequence"].clone());
        std::fs::write(facts.join(format!("{sha}.json")), raw).unwrap();
        let mut fields = vec![value];
        while let Some(field) = fields.pop() {
            match field {
                Value::Object(values) => {
                    if let Some(pages) = values.get("$fact_pages").and_then(Value::as_array) {
                        pending.extend(pages.iter().map(|page| page.as_str().unwrap().to_owned()));
                    }
                    fields.extend(values.into_values());
                }
                Value::Array(values) => fields.extend(values),
                _ => {}
            }
        }
    }
    std::fs::write(
        facts.parent().unwrap().join("fact-sequences.json"),
        serde_json::to_vec(&json!({"next":2075,"facts":index})).unwrap(),
    )
    .unwrap();

    let (mut client, mut server) = harness_with_config(Config {
        auto_continue: 20,
        domain_setup: Some(crate::domain::DomainSetup {
            mode: crate::domain::DomainMode::Project,
            context_path: fixture.join("context.json"),
            repository: fixture.clone(),
            python: "D:/project/bam-workflow-validation-20261022/.venv/Scripts/python.exe".into(),
            run_id: "project-29052-1792673801123156600".into(),
            context_sha256: context_hash,
            floor_id: "项目目标".into(),
        }),
        ..Default::default()
    })
    .await;
    running_root(&mut client, &mut server).await;

    let records: Vec<Value> = std::fs::read_to_string(replay)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut calls = HashMap::new();
    let mut after = 0;
    let mut confirmation = None;
    for (end_sequence, expected_card) in [
        (321, "c_J-xTIjTC"),
        (400, "c_5zkZHd-4"),
        (553, "c_5zkZHd-4"),
    ] {
        let expected_starts = match end_sequence {
            321 => 1,
            400 => 2,
            _ => 8,
        };
        for value in records.iter().filter(|value| {
            value["seq"]
                .as_u64()
                .is_some_and(|seq| seq > after && seq <= end_sequence)
        }) {
            let turn = value["data"]["turn"].as_u64().unwrap_or(1);
            let turn_id = if turn == 1 {
                "root-turn".to_owned()
            } else {
                format!("replay-turn-{turn}")
            };
            if value["type"] == "turn/start" && turn > 1 {
                // Automatic continuation is checked at each terminal event. Start
                // later recorded turns explicitly to inspect repeated card receipts.
                client
                    .commands
                    .send(Command::SubmitRootInput {
                        text: "继续".into(),
                    })
                    .await
                    .unwrap();
                let start = next(&mut server).await;
                assert_eq!(start["method"], "turn/start");
                send(
                    &mut server,
                    json!({"id":start["id"],"result":{"turn":{"id":turn_id}}}),
                )
                .await;
                phase(&mut client, SessionPhase::Running).await;
            }
            let event = match value["type"].as_str() {
                Some("turn/start") => Some(
                    json!({"method":"turn/started","params":{"threadId":"root","turn":{"id":turn_id}}}),
                ),
                Some("turn/end") => Some(
                    json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":turn_id,"status":"completed"}}}),
                ),
                Some("tool/call") => {
                    let name = value["data"]["name"].as_str().unwrap();
                    let call_id = value["data"]["callId"].as_str().unwrap();
                    calls.insert(call_id.to_owned(), name.to_owned());
                    name.strip_prefix("mcp__modeling_domain__").map(|tool| json!({"method":"item/started","params":{"threadId":"root","turnId":turn_id,"item":{"id":call_id,"type":"mcpToolCall","server":"modeling_domain","tool":tool,"arguments":serde_json::from_str::<Value>(value["data"]["arguments"].as_str().unwrap()).unwrap(),"status":"inProgress"}}}))
                }
                Some("tool/result") => {
                    let message = &value["data"]["message"];
                    let call_id = message["toolCallId"].as_str().unwrap();
                    calls.get(call_id).and_then(|name| name.strip_prefix("mcp__modeling_domain__"))
                        .map(|tool| json!({"method":"item/completed","params":{"threadId":"root","turnId":turn_id,"item":{"id":call_id,"type":"mcpToolCall","server":"modeling_domain","tool":tool,"status":"completed","result":{"content":message["content"].clone()}}}}))
                }
                _ => None,
            };
            if let Some(event) = event {
                observation_event(&mut client, &mut server, event).await;
            }
            if value["type"] == "turn/end" {
                let _ = tokio::time::timeout(
                    Duration::from_secs(20),
                    client.snapshots.wait_for(|snapshot| {
                        let domain = snapshot.extensions[0].modeling().unwrap();
                        snapshot.root_start_requests > turn
                            || (domain.fact_sync_notice.is_none() && domain.approval_card_waiting())
                    }),
                )
                .await;
                assert_eq!(
                    client.snapshots.borrow().root_start_requests,
                    turn,
                    "automatic continuation at real seq {}",
                    value["seq"]
                );
            }
        }
        // End the turn while private fact reads are still queued, as in the real
        // client. Completion must wait for these reads before deciding to continue.
        let _ = tokio::time::timeout(
            Duration::from_secs(20),
            client.snapshots.wait_for(|snapshot| {
                let domain = snapshot.extensions[0].modeling().unwrap();
                snapshot.root_start_requests > expected_starts
                    || (domain.approval_card_waiting()
                        && domain
                            .approval_card_request
                            .as_ref()
                            .is_some_and(|request| request.card_ref == expected_card))
            }),
        )
        .await;
        let snapshot = client.snapshots.borrow().clone();
        let domain = snapshot.extensions[0].modeling().unwrap();
        assert!(domain.approval_card_waiting(), "expected approvable card at real seq {end_sequence}; root_starts={}, sync={:?}, proposal={:?}, rejected={:?}", snapshot.root_start_requests, domain.fact_sync_notice, domain.proposal.as_ref().map(|p| &p.binding), domain.rejected_facts.iter().map(|f| &f.reason).collect::<Vec<_>>());
        let card = domain.selected_approval_card().unwrap();
        let binding = &card.proposal.as_ref().unwrap().binding;
        assert_eq!(binding.proposal_id, "5f0bb072a1d74d578432e010ef6a948d");
        assert_eq!(binding.revision, 1);
        assert_eq!(
            binding.prepare_token,
            "cf6beecadcbff29089847735ef37785c8a9171fc7a7256bba2c67d58834d0bc0"
        );
        assert_eq!(
            domain.approval_card_request.as_ref().unwrap().card_ref,
            expected_card
        );
        let (screen, rendered_confirmation) = crate::ui::replay_modeling_confirmation(&snapshot);
        assert!(screen.contains("建模确认"), "{screen}");
        assert!(screen.contains("待批1张（可确认1）"), "{screen}");
        assert!(screen.contains("Ctrl+Y"), "{screen}");
        assert!(
            rendered_confirmation.is_some(),
            "real Ctrl+Y must confirm the rendered card"
        );
        confirmation = rendered_confirmation;
        assert_eq!(domain.approval_card_snapshots().len(), 1);
        phase(&mut client, SessionPhase::Completed).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(150), next(&mut server))
                .await
                .is_err()
        );
        assert_eq!(
            client.snapshots.borrow().root_start_requests,
            expected_starts
        );
        after = end_sequence;
    }
    client
        .commands
        .send(Command::ConfirmExtension {
            confirmation: confirmation.unwrap(),
        })
        .await
        .unwrap();
    let mut approved_line = String::new();
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        server.read_line(&mut approved_line),
    )
    .await;
    assert!(
        result.is_ok(),
        "Ctrl+Y approval request timed out: {:?}; domain={:?}",
        client.snapshots.borrow().notice,
        client.snapshots.borrow().extensions[0]
            .modeling()
            .unwrap()
            .notice
    );
    result.unwrap().unwrap();
    let approved: Value = serde_json::from_str(&approved_line).unwrap();
    assert_eq!(approved["method"], "turn/start");
    let text = approved["params"]["input"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(
        payload["apply_change_arguments"]["proposal_id"],
        "5f0bb072a1d74d578432e010ef6a948d"
    );
    assert_eq!(payload["apply_change_arguments"]["revision"], 1);
    assert_eq!(
        payload["apply_change_arguments"]["prepare_token"],
        "cf6beecadcbff29089847735ef37785c8a9171fc7a7256bba2c67d58834d0bc0"
    );
    let arguments: Vec<String> =
        serde_json::from_slice(&std::fs::read(fixture.join("approval-arguments.json")).unwrap())
            .unwrap();
    for (flag, expected) in [
        ("--proposal", "5f0bb072a1d74d578432e010ef6a948d"),
        ("--revision", "1"),
        (
            "--prepare-token",
            "cf6beecadcbff29089847735ef37785c8a9171fc7a7256bba2c67d58834d0bc0",
        ),
    ] {
        let index = arguments
            .iter()
            .position(|argument| argument == flag)
            .unwrap();
        assert_eq!(arguments[index + 1], expected);
    }
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn restored_approval_receipt_stops_auto_continue_and_confirms_the_restored_proposal() {
    use sha2::{Digest, Sha256};
    let directory = std::env::temp_dir().join(format!(
        "restored-card-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(directory.join("blender_agent/cli")).unwrap();
    std::fs::write(directory.join("blender_agent/__init__.py"), "").unwrap();
    std::fs::write(directory.join("blender_agent/cli/__init__.py"), "").unwrap();
    std::fs::write(directory.join("context.json"), b"{}").unwrap();
    let context_hash = format!("{:x}", Sha256::digest(b"{}"));
    std::fs::write(directory.join("blender_agent/cli/main.py"), format!(r#"
import sys,json
from pathlib import Path
if '--fact-sha256' in sys.argv:
    sys.stdout.buffer.write(Path('restore.json').read_bytes())
else:
    print(json.dumps({{'ok':True,'kind':'authorization','run_id':'run','context_sha256':'{context_hash}',
    'grant':{{'authorization_id':'restored-grant','proposal_id':'5f0bb072','revision':2,'prepare_token':'token'}}}}))
"#)).unwrap();
    let png = [
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
    ];
    let image_path = directory.join("review.png");
    std::fs::write(&image_path, png).unwrap();
    let image_sha = format!("{:x}", Sha256::digest(png));
    let (mut client, mut server) = harness_with_config(Config {
        auto_continue: 20,
        domain_setup: Some(crate::domain::DomainSetup {
            mode: crate::domain::DomainMode::Floor,
            context_path: directory.join("context.json"),
            repository: directory.clone(),
            python: crate::domain::receipt::tests::python(),
            run_id: "run".into(),
            context_sha256: context_hash.clone(),
            floor_id: "floor".into(),
        }),
        ..Default::default()
    })
    .await;
    running_root(&mut client, &mut server).await;
    let mut proposal = crate::domain::tests::floor_story_height_proposal();
    proposal["context_sha256"] = json!(context_hash);
    proposal["fact_sequence"] = json!(1);
    proposal["source_current"] = json!(true);
    proposal["proposal"]["proposal_id"] = json!("5f0bb072");
    observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
        "item":{"id":"prepared","type":"mcpToolCall","server":"modeling_domain","tool":"inspect_change","status":"completed","result":{"structuredContent":proposal.clone()}}}})).await;
    let artifact = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,
        "kind":"artifact_validation","fact_sequence":2,
        "artifact":{"build_id":"restored-build","proposal_id":"5f0bb072","revision":2,"prepare_token":"token",
            "candidate":true,"authority_sha256":proposal["proposal"]["base_sha256"],
            "input_sha256":proposal["proposal"]["candidate_sha256"],"state":"built","output":"model.glb","valid":true,"current":true,
            "review_image":{"delivery":"attached_png","image_sha256":image_sha,"image_path":image_path,"dimensions_px":[1,1],
                "request":{"build_id":"restored-build","proposal_id":"5f0bb072","revision":2,"prepare_token":"token",
                    "input_sha256":proposal["proposal"]["candidate_sha256"],"blend_sha256":"a".repeat(64),"focus_m":null}}}});
    observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
        "item":{"id":"review","type":"mcpToolCall","server":"modeling_domain","tool":"read_artifact_review_image","status":"completed","result":{"structuredContent":artifact}}}})).await;
    tokio::time::timeout(
        Duration::from_secs(5),
        client.snapshots.wait_for(|snapshot| {
            snapshot.extensions[0]
                .modeling()
                .unwrap()
                .approval_card_waiting()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let mut other = proposal.clone();
    other["fact_sequence"] = json!(3);
    other["proposal"]["proposal_id"] = json!("other-unready");
    other["proposal"]["prepare_token"] = json!("other-token");
    other["proposal"]["state"] = json!("draft");
    observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
        "item":{"id":"other","type":"mcpToolCall","server":"modeling_domain","tool":"prepare_change","status":"completed","result":{"structuredContent":other}}}})).await;

    let mut restored = crate::domain::tests::approval_restore_fact("5f0bb072", "restored-build", 4);
    restored["context_sha256"] = json!(context_hash);
    let raw = serde_json::to_vec(&restored).unwrap();
    std::fs::write(directory.join("restore.json"), &raw).unwrap();
    let receipt = json!({"contract":"modeling-fact-receipt/v1","kind":"approval_card_restore","ok":true,
        "run_id":"run","context_sha256":context_hash,"fact_sequence":4,
        "fact_sha256":format!("{:x}",Sha256::digest(&raw)),"fact_bytes":raw.len(),"binding":{},
        "agent_view":{"value":{"status":"ready","proposal_id":"5f0bb072","build_id":"restored-build",
            "card_ref":"opaque-card-reference","first_use_readers":[],"overview_readers":[],"missing":[],
            "next_call":{"action":"end_turn_for_core_approval"},"replayed_facts":{"count":5}}}});
    observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
        "item":{"id":"restore","type":"mcpToolCall","server":"modeling_domain","tool":"restore_approval_card","status":"completed",
            "result":{"content":[{"type":"text","text":receipt.to_string()}]}}}})).await;
    tokio::time::timeout(
        Duration::from_secs(5),
        client.snapshots.wait_for(|snapshot| {
            let domain = snapshot.extensions[0].modeling().unwrap();
            domain.fact_sync_notice.is_none() && domain.restored_approval_selection().is_some()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let snapshot = client.snapshots.borrow().clone();
    let domain = snapshot.extensions[0].modeling().unwrap();
    assert_eq!(
        domain.proposal.as_ref().unwrap().binding.proposal_id,
        "other-unready"
    );
    let binding = domain
        .selected_approval_card()
        .unwrap()
        .proposal
        .as_ref()
        .unwrap()
        .binding
        .clone();
    assert_eq!(binding.proposal_id, "5f0bb072");
    assert_eq!(binding.revision, 2);
    assert!(domain.approval_card_waiting());
    assert!(domain.approval_restore_waiting());
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"turn/completed","params":{"threadId":"root",
        "turn":{"id":"root-turn","status":"completed"}}}),
    )
    .await;
    phase(&mut client, SessionPhase::Completed).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), next(&mut server))
            .await
            .is_err()
    );
    assert_eq!(client.snapshots.borrow().root_start_requests, 1);
    client
        .commands
        .send(Command::ConfirmExtension {
            confirmation: crate::extensions::ExtensionConfirmation::Modeling(
                binding.clone().into(),
            ),
        })
        .await
        .unwrap();
    let approved = next(&mut server).await;
    assert_eq!(approved["method"], "turn/start");
    let text = approved["params"]["input"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(
        payload["apply_change_arguments"]["proposal_id"],
        binding.proposal_id
    );
    assert_eq!(
        payload["apply_change_arguments"]["revision"],
        binding.revision
    );
    assert_eq!(
        payload["apply_change_arguments"]["prepare_token"],
        binding.prepare_token
    );
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn restore_end_turn_instruction_blocks_auto_continue_before_card_evidence_arrives() {
    let (mut client, mut server) = harness_with_config(Config {
        auto_continue: 20,
        domain_setup: Some(crate::domain::DomainSetup {
            mode: crate::domain::DomainMode::Floor,
            context_path: "context.json".into(),
            repository: ".".into(),
            python: "unused-python".into(),
            run_id: "run".into(),
            context_sha256: "ctx".into(),
            floor_id: "floor".into(),
        }),
        ..Default::default()
    })
    .await;
    running_root(&mut client, &mut server).await;
    let restored = crate::domain::tests::approval_restore_fact("5f0bb072", "restored-build", 4);
    observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
        "item":{"id":"restore","type":"mcpToolCall","server":"modeling_domain","tool":"restore_approval_card","status":"completed",
            "result":{"content":[{"type":"text","text":restored.to_string()}]}}}})).await;
    assert!(!client.snapshots.borrow().extensions[0]
        .modeling()
        .unwrap()
        .approval_card_waiting());
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"turn/completed","params":{"threadId":"root",
        "turn":{"id":"root-turn","status":"completed"}}}),
    )
    .await;
    phase(&mut client, SessionPhase::Completed).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), next(&mut server))
            .await
            .is_err()
    );
    assert_eq!(client.snapshots.borrow().root_start_requests, 1);
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn tool_operation_waits_never_replace_scope_or_authorize_business_facts() {
    let (mut client, mut server) = harness_with_config(Config {
        domain_setup: Some(crate::domain::DomainSetup {
            mode: crate::domain::DomainMode::Project,
            context_path: "context.json".into(),
            repository: ".".into(),
            python: "unused-python".into(),
            run_id: "run".into(),
            context_sha256: "ctx".into(),
            floor_id: "floor".into(),
        }),
        ..Default::default()
    })
    .await;
    ready(&mut server).await;
    phase(&mut client, SessionPhase::Ready).await;
    client
        .commands
        .send(Command::SubmitRootInput {
            text: "检查作业".into(),
        })
        .await
        .unwrap();
    let start = next(&mut server).await;
    send(
        &mut server,
        json!({"id":start["id"],"result":{"turn":{"id":"root-turn"}}}),
    )
    .await;
    phase(&mut client, SessionPhase::Running).await;
    for (index, state) in ["running", "unknown"].into_iter().enumerate() {
        let fact = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":"ctx",
            "kind":"tool_operation","ok":true,"fact_sequence":801+index,
            "op_id":"c".repeat(32),"state":state,"replay_allowed":false,"error":null,
            "recommended_call":{"tool":"inspect_tool_operation","arguments":{"op_id":"c".repeat(32),"wait_s":60}}});
        observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
            "item":{"id":format!("wait-{index}"),"type":"mcpToolCall","server":"modeling_domain","tool":"inspect_tool_operation","status":"completed","result":{"structuredContent":fact}}}})).await;
        let snapshot = client.snapshots.borrow().clone();
        let domain = snapshot.extensions[0].modeling().unwrap();
        assert!(domain.rejected_facts.is_empty());
        assert!(domain.fact_sync_notice.is_none());
        assert!(
            domain.project_scope.is_none()
                && domain.proposal.is_none()
                && domain.operation.is_none()
        );
        assert_eq!(snapshot.root_start_requests, 1);
    }
    let mut unscoped = crate::domain::tests::floor_story_height_proposal();
    unscoped["fact_sequence"] = json!(803);
    observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
        "item":{"id":"business","type":"mcpToolCall","server":"modeling_domain","tool":"inspect_change","status":"completed","result":{"structuredContent":unscoped}}}})).await;
    let snapshot = client.snapshots.borrow().clone();
    let domain = snapshot.extensions[0].modeling().unwrap();
    assert!(domain.proposal.is_none());
    assert!(domain
        .rejected_facts
        .iter()
        .any(|f| f.kind == "proposal" && f.blocking));
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn domain_poll_refreshes_snapshot_without_any_parent_tool_receipt() {
    use sha2::{Digest, Sha256};
    let root = std::env::temp_dir().join(format!(
        "domain-client-poll-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = root.join("outputs/domain/run/changes");
    std::fs::create_dir_all(store.join("facts")).unwrap();
    std::fs::write(root.join("context.json"), b"{}").unwrap();
    let context_sha256 = format!("{:x}", Sha256::digest(b"{}"));
    let (mut client, mut server) = harness_with_config(Config {
        domain_setup: Some(crate::domain::DomainSetup {
            mode: crate::domain::DomainMode::Floor,
            context_path: root.join("context.json"),
            repository: root.clone(),
            python: "unused-python".into(),
            run_id: "run".into(),
            context_sha256: context_sha256.clone(),
            floor_id: "floor".into(),
        }),
        ..Default::default()
    })
    .await;
    ready(&mut server).await;
    phase(&mut client, SessionPhase::Ready).await;
    let value = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_sha256,"ok":true,"kind":"source_groups","fact_sequence":7,"scan_id":"scan","scene_sha256":"scene","total_groups":0,"total_objects":0,"state_counts":{},"groups":[]});
    let raw = serde_json::to_vec(&value).unwrap();
    let sha = format!("{:x}", Sha256::digest(&raw));
    std::fs::write(store.join("facts").join(format!("{sha}.json")), &raw).unwrap();
    std::fs::write(
        store.join("fact-sequences.json"),
        json!({"next":8,"facts":{sha:7}}).to_string(),
    )
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(8),
        client
            .snapshots
            .wait_for(|snapshot| snapshot.extensions[0].modeling().unwrap().groups.is_some()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(client.snapshots.borrow().phase, SessionPhase::Ready);
    assert_eq!(client.snapshots.borrow().root_start_requests, 0);
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn resumed_domain_verifies_old_and_out_of_order_receipts_and_rejects_bad_files() {
    use sha2::{Digest, Sha256};
    let directory = std::env::temp_dir().join(format!(
        "domain-resume-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = directory.join("outputs/domain/run/changes");
    std::fs::create_dir_all(store.join("facts")).unwrap();
    std::fs::create_dir_all(directory.join("blender_agent/cli")).unwrap();
    for file in ["blender_agent/__init__.py", "blender_agent/cli/__init__.py"] {
        std::fs::write(directory.join(file), "").unwrap();
    }
    std::fs::write(directory.join("context.json"), b"{}").unwrap();
    let hash = format!("{:x}", Sha256::digest(b"{}"));
    std::fs::write(directory.join("blender_agent/cli/main.py"),
        "import sys\nfrom pathlib import Path\na=sys.argv\nsha=a[a.index('--fact-sha256')+1]\nsys.stdout.buffer.write(Path('outputs/domain/run/changes/facts',sha+'.json').read_bytes())\n").unwrap();
    let proposal = json!({"kind":"proposal","fact_sequence":150,"current":true,"diagnostics":[],
        "proposal":{"proposal_id":"p","revision":1,"prepare_token":"token","base_sha256":"base",
        "candidate_sha256":"candidate","state":"prepared","summary":"chair","draft":{"operations":[]},
        "issues":[],"pending":[],"assumed":[],"component_first_use_requirements":{"contract":"component-first-use/1","required":[],"reused":[]}}});
    let groups = json!({"kind":"source_groups","fact_sequence":159,"scan_id":"scan","scene_sha256":"scene",
        "total_groups":0,"total_objects":0,"state_counts":{},"groups":[]});
    let artifact = json!({"kind":"artifact","fact_sequence":172,"artifact":{"build_id":"b","proposal_id":"p",
        "revision":1,"prepare_token":"token","candidate":true,"authority_sha256":"base","input_sha256":"candidate",
        "state":"built","output":"b.glb","current":true}});
    let mut validation = artifact.clone();
    validation["kind"] = json!("artifact_validation");
    validation["fact_sequence"] = json!(178);
    validation["artifact"]["valid"] = json!(true);
    let mut values = vec![proposal, groups, artifact, validation];
    let mut receipts = Vec::new();
    let mut sequences = serde_json::Map::new();
    for value in &mut values {
        value["contract"] = json!("modeling-domain/v2");
        value["run_id"] = json!("run");
        value["context_sha256"] = json!(hash);
        value["ok"] = json!(true);
        let raw = serde_json::to_vec(value).unwrap();
        let sha = format!("{:x}", Sha256::digest(&raw));
        std::fs::write(store.join("facts").join(format!("{sha}.json")), &raw).unwrap();
        sequences.insert(sha.clone(), value["fact_sequence"].clone());
        let owner = value.get("proposal").or_else(|| value.get("artifact"));
        let mut binding = serde_json::Map::new();
        for key in [
            "proposal_id",
            "revision",
            "prepare_token",
            "base_sha256",
            "candidate_sha256",
            "build_id",
            "authority_sha256",
        ] {
            if let Some(field) = owner.and_then(|owner| owner.get(key)) {
                binding.insert(key.into(), field.clone());
            }
        }
        receipts.push(
            json!({"contract":"modeling-fact-receipt/v1","run_id":"run","context_sha256":hash,
            "kind":value["kind"],"ok":true,"fact_sequence":value["fact_sequence"],"fact_sha256":sha,
            "fact_bytes":raw.len(),"binding":binding}),
        );
    }
    std::fs::write(
        store.join("fact-sequences.json"),
        json!({"next":183,"facts":sequences}).to_string(),
    )
    .unwrap();
    let config = Config {
        permission: Some("read-only".into()),
        domain_setup: Some(crate::domain::DomainSetup {
            mode: crate::domain::DomainMode::Floor,
            context_path: directory.join("context.json"),
            repository: directory.clone(),
            python: crate::domain::receipt::tests::python(),
            run_id: "run".into(),
            context_sha256: hash,
            floor_id: "floor".into(),
        }),
        ..Default::default()
    };
    let (mut client, mut server) = harness_with_config(config).await;
    initialized(&mut server).await;
    let set = next(&mut server).await;
    send(
        &mut server,
        json!({"id":set["id"],"result":{"current":"read-only","presets":[]}}),
    )
    .await;
    phase(&mut client, SessionPhase::Ready).await;
    skip_skills(&mut server).await;
    client
        .commands
        .send(Command::SubmitRootInput {
            text: "continue".into(),
        })
        .await
        .unwrap();
    let start = next(&mut server).await;
    send(
        &mut server,
        json!({"id":start["id"],"result":{"turn":{"id":"root-turn"}}}),
    )
    .await;
    phase(&mut client, SessionPhase::Running).await;
    assert!(client.snapshots.borrow().extensions[0]
        .modeling()
        .unwrap()
        .groups
        .is_some());
    for (attempt, index) in [3, 2, 1, 0, 2, 3, 1].into_iter().enumerate() {
        observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
            "item":{"id":format!("receipt-{attempt}"),"type":"mcpToolCall","server":"modeling_domain","tool":"inspect_change","status":"completed",
            "result":{"structuredContent":receipts[index]}}}})).await;
    }
    tokio::time::timeout(
        Duration::from_secs(10),
        client.snapshots.wait_for(|snapshot| {
            let domain = snapshot.extensions[0].modeling().unwrap();
            domain.verified_fact.is_some() && domain.fact_sync_notice.is_none()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let snapshot = client.snapshots.borrow().clone();
    let domain = snapshot.extensions[0].modeling().unwrap();
    assert_eq!(domain.artifacts[0].valid, Some(true));
    assert!(domain.groups.is_some());
    assert_eq!(domain.proposal.as_ref().unwrap().binding.revision, 1);
    // Even a fact recovered at startup must be read and hashed again on a new receipt.
    let artifact_path = store.join("facts").join(format!(
        "{}.json",
        receipts[2]["fact_sha256"].as_str().unwrap()
    ));
    for (attempt, corrupt) in [false, true].into_iter().enumerate() {
        if corrupt {
            std::fs::write(&artifact_path, b"{}").unwrap();
        } else {
            std::fs::remove_file(&artifact_path).unwrap();
        }
        observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
            "item":{"id":format!("bad-{attempt}"),"type":"mcpToolCall","server":"modeling_domain","tool":"inspect_artifact","status":"completed",
            "result":{"structuredContent":receipts[2]}}}})).await;
        tokio::time::timeout(
            Duration::from_secs(10),
            client.snapshots.wait_for(|snapshot| {
                snapshot.extensions[0]
                    .modeling()
                    .unwrap()
                    .fact_sync_notice
                    .as_ref()
                    .is_some_and(|n| n.contains("尚未核验 artifact"))
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(client.snapshots.borrow().extensions[0]
            .modeling()
            .unwrap()
            .fact_sync_notice
            .is_some());
    }
    std::fs::write(&artifact_path, serde_json::to_vec(&values[2]).unwrap()).unwrap();
    observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
        "item":{"id":"recovered-old-receipt","type":"mcpToolCall","server":"modeling_domain","tool":"inspect_artifact","status":"completed",
        "result":{"structuredContent":receipts[2]}}}})).await;
    tokio::time::timeout(
        Duration::from_secs(10),
        client.snapshots.wait_for(|snapshot| {
            snapshot.extensions[0]
                .modeling()
                .unwrap()
                .fact_sync_notice
                .is_none()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    // A failed or aborted read produces no fact and must not leave the card unsynchronized.
    observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
        "item":{"id":"aborted-read","type":"mcpToolCall","server":"modeling_domain","tool":"read_component_first_use_image","status":"failed",
        "result":{"isError":true,"content":[{"type":"text","text":"Error: [object Object]"}]}}}})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client.snapshots.borrow().extensions[0]
        .modeling()
        .unwrap()
        .fact_sync_notice
        .is_none());
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn reviewed_template_policy_keeps_parameter_decisions_manual_and_binds_grants_to_version() {
    use crate::config::DomainApprovalPolicy;
    use sha2::{Digest, Sha256};
    for (policy, parameter_only) in [
        (DomainApprovalPolicy::Manual, false),
        (DomainApprovalPolicy::ReviewedTemplate, false),
        (DomainApprovalPolicy::ReviewedTemplate, true),
    ] {
        let directory = std::env::temp_dir().join(format!(
            "tui-policy-{}-{:?}-{}",
            std::process::id(),
            policy,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(directory.join("blender_agent/cli")).unwrap();
        std::fs::write(directory.join("blender_agent/__init__.py"), "").unwrap();
        std::fs::write(directory.join("blender_agent/cli/__init__.py"), "").unwrap();
        std::fs::write(directory.join("context.json"), b"{}").unwrap();
        let context_hash = format!("{:x}", Sha256::digest(b"{}"));
        std::fs::write(directory.join("blender_agent/cli/main.py"), format!(r#"
import sys,json
from pathlib import Path
a=sys.argv
actor=a[a.index('--actor')+1]
Path('actor.txt').write_text(actor)
print(json.dumps({{'ok':True,'kind':'authorization','run_id':'run','context_sha256':'{context_hash}',
'grant':{{'authorization_id':'unit-policy-grant','proposal_id':'p','revision':1,'prepare_token':'token','approval_policy':'reviewed-template'}}}}))
"#)).unwrap();
        let png = [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
        ];
        let path = directory.join("review.png");
        std::fs::write(&path, png).unwrap();
        let image_sha = format!("{:x}", Sha256::digest(png));
        let config = Config {
            permission: Some("read-only".into()),
            domain_approval_policy: policy,
            domain_setup: Some(crate::domain::DomainSetup {
                mode: crate::domain::DomainMode::Floor,
                context_path: directory.join("context.json"),
                repository: directory.clone(),
                python: crate::domain::receipt::tests::python(),
                run_id: "run".into(),
                context_sha256: context_hash.clone(),
                floor_id: "floor".into(),
            }),
            ..Default::default()
        };
        let (mut client, mut server) = harness_with_config(config).await;
        initialized(&mut server).await;
        let set = next(&mut server).await;
        send(
            &mut server,
            json!({"id":set["id"],"result":{"current":"read-only","presets":[]}}),
        )
        .await;
        phase(&mut client, SessionPhase::Ready).await;
        skip_skills(&mut server).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "建模二层".into(),
            })
            .await
            .unwrap();
        let start = next(&mut server).await;
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"root-turn"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        let mut proposal = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,
            "kind":"proposal","fact_sequence":1,"current":true,"source_current":true,"diagnostics":[],
            "auto_approval":{"contract":"reviewed-template-approval/1","eligible":true},
            "proposal":{"proposal_id":"p","revision":1,"prepare_token":"token","base_sha256":"base",
            "candidate_sha256":"candidate","state":"prepared","summary":"reviewed chair","draft":{"operations":[]},
            "assumed":[],"issues":[],"pending":[],"component_first_use_requirements":{
                "contract":"component-first-use/1","required":[],"reused":[{"review_key":"human-reviewed"}]}}});
        if parameter_only {
            proposal = crate::domain::tests::floor_story_height_proposal();
            proposal["context_sha256"] = json!(context_hash);
            proposal["proposal"]["proposal_id"] = json!("p");
            proposal["proposal"]["revision"] = json!(1);
            proposal["auto_approval"] =
                json!({"contract":"reviewed-template-approval/1","eligible":true});
            proposal["source_current"] = json!(true);
        }
        observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
            "item":{"id":"p","type":"mcpToolCall","server":"modeling_domain","tool":"prepare_change","status":"completed","result":{"structuredContent":proposal.clone()}}}})).await;
        let artifact = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,"kind":"artifact_validation","fact_sequence":2,
            "artifact":{"build_id":"b","proposal_id":"p","revision":1,"prepare_token":"token","candidate":true,"authority_sha256":proposal["proposal"]["base_sha256"],"input_sha256":proposal["proposal"]["candidate_sha256"],"state":"built","output":"model.glb","valid":true,"current":true,
            "review_image":{"delivery":"attached_png","image_sha256":image_sha,"image_path":path,"dimensions_px":[1,1],"request":{"build_id":"b","proposal_id":"p","revision":1,"prepare_token":"token","input_sha256":proposal["proposal"]["candidate_sha256"],"blend_sha256":"a".repeat(64),"focus_m":null}}}});
        observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
            "item":{"id":"b","type":"mcpToolCall","server":"modeling_domain","tool":"read_artifact_review_image","status":"completed","result":{"structuredContent":artifact}}}})).await;
        tokio::time::timeout(
            Duration::from_secs(5),
            client.snapshots.wait_for(|s| {
                s.extensions[0]
                    .modeling()
                    .unwrap()
                    .review_images
                    .first()
                    .is_some_and(|i| i.file_state == crate::domain::ImageFileState::Available)
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!directory.join("actor.txt").exists());
        assert_eq!(client.snapshots.borrow().root_start_requests, 1);
        if parameter_only {
            let mut other = proposal.clone();
            other["fact_sequence"] = json!(3);
            other["proposal"]["proposal_id"] = json!("other");
            other["proposal"]["prepare_token"] = json!("other-token");
            other["proposal"]["state"] = json!("draft");
            observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
                "item":{"id":"other-before-confirm","type":"mcpToolCall","server":"modeling_domain","tool":"prepare_change","status":"completed","result":{"structuredContent":other}}}})).await;
            tokio::time::timeout(
                Duration::from_secs(5),
                client.snapshots.wait_for(|snapshot| {
                    snapshot.extensions[0]
                        .modeling()
                        .unwrap()
                        .proposal
                        .as_ref()
                        .is_some_and(|proposal| proposal.binding.proposal_id == "other")
                }),
            )
            .await
            .unwrap()
            .unwrap();
            let snapshot = client.snapshots.borrow().clone();
            let domain = snapshot.extensions[0].modeling().unwrap();
            assert_eq!(domain.approval_card_snapshots().len(), 2);
        }
        observation_event(&mut client,&mut server,json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}})).await;
        if policy == DomainApprovalPolicy::ReviewedTemplate && !parameter_only {
            let approved = next(&mut server).await;
            assert_eq!(approved["method"], "turn/start");
            let text = approved["params"]["input"][0]["text"].as_str().unwrap();
            assert!(text.contains("policy_approve_concrete_change"));
            assert!(text.contains("不代表新的人工确认"));
            assert!(!text.contains("用户已批准此具体版本"));
            assert_eq!(
                std::fs::read_to_string(directory.join("actor.txt")).unwrap(),
                "native-agent-tui:policy:reviewed-template"
            );
        } else {
            phase(&mut client, SessionPhase::Completed).await;
            assert!(!directory.join("actor.txt").exists());
            assert_eq!(client.snapshots.borrow().root_start_requests, 1);
            if parameter_only {
                let binding = client.snapshots.borrow().extensions[0]
                    .modeling()
                    .unwrap()
                    .selected_approval_card()
                    .unwrap()
                    .proposal
                    .as_ref()
                    .unwrap()
                    .binding
                    .clone();
                assert!(client.snapshots.borrow().extensions[0]
                    .modeling()
                    .unwrap()
                    .selected_approval_card()
                    .unwrap()
                    .first_use_satisfied());
                client
                    .commands
                    .send(Command::ConfirmExtension {
                        confirmation: crate::extensions::ExtensionConfirmation::Modeling(
                            binding.clone().into(),
                        ),
                    })
                    .await
                    .unwrap();
                let approved = next(&mut server).await;
                assert_eq!(approved["method"], "turn/start");
                let text = approved["params"]["input"][0]["text"].as_str().unwrap();
                let payload: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
                assert_eq!(payload["user_action"], "approve_concrete_change");
                assert_eq!(
                    payload["apply_change_arguments"]["proposal_id"],
                    binding.proposal_id
                );
                assert_eq!(
                    payload["apply_change_arguments"]["revision"],
                    binding.revision
                );
                assert_eq!(
                    payload["apply_change_arguments"]["expected_sha256"],
                    binding.base_sha256
                );
                assert_eq!(
                    payload["apply_change_arguments"]["prepare_token"],
                    binding.prepare_token
                );
                assert_eq!(
                    std::fs::read_to_string(directory.join("actor.txt")).unwrap(),
                    "native-agent-tui:user"
                );
                send(
                    &mut server,
                    json!({"id":approved["id"],"result":{"turn":{"id":"apply-turn"}}}),
                )
                .await;
                phase(&mut client, SessionPhase::Running).await;
                let applied = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,
                    "kind":"operation","fact_sequence":4,"receipt":{"proposal_id":binding.proposal_id,
                    "revision":binding.revision,"prepare_token":binding.prepare_token,"base_sha256":binding.base_sha256,
                    "candidate_sha256":binding.candidate_sha256,"operation_id":"height-apply","state":"applied",
                    "current_sha256":binding.candidate_sha256}});
                observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"apply-turn",
                    "item":{"id":"apply","type":"mcpToolCall","server":"modeling_domain","tool":"apply_change","status":"completed","result":{"structuredContent":applied}}}})).await;
                let snapshot = client.snapshots.borrow().clone();
                let domain = snapshot.extensions[0].modeling().unwrap();
                assert_eq!(
                    domain
                        .approval_card(&binding)
                        .unwrap()
                        .operation
                        .as_ref()
                        .unwrap()
                        .state,
                    "applied"
                );
                assert!(!domain.proposal.as_ref().unwrap().ready);
                assert_eq!(domain.approval_card_snapshots().len(), 1);
                assert!(domain.rejected_facts.is_empty());
            }
        }
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[tokio::test]
async fn manual_grant_steered_into_an_ending_turn_is_resubmitted_when_still_unapplied() {
    use crate::config::DomainApprovalPolicy;
    use sha2::{Digest, Sha256};
    for completion in [
        "approved",
        "other_unready",
        "failed",
        "expired",
        "newer_notice",
        "preview_required",
        "restore_candidate_review",
        "unknown",
        "pending_after_steer",
        "restore_wait_after_steer",
        "ready_card_after_steer",
    ] {
        let policy = DomainApprovalPolicy::Manual;
        let directory = std::env::temp_dir().join(format!(
            "tui-policy-{}-{:?}-{}",
            std::process::id(),
            policy,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(directory.join("blender_agent/cli")).unwrap();
        std::fs::write(directory.join("blender_agent/__init__.py"), "").unwrap();
        std::fs::write(directory.join("blender_agent/cli/__init__.py"), "").unwrap();
        std::fs::write(directory.join("context.json"), b"{}").unwrap();
        let context_hash = format!("{:x}", Sha256::digest(b"{}"));
        std::fs::write(directory.join("blender_agent/cli/main.py"), format!(r#"
import sys,json,time
from pathlib import Path
a=sys.argv
actor=a[a.index('--actor')+1]
Path('actor.txt').write_text(actor)
deadline=time.monotonic()+10
while not Path('release-authorization').exists() and time.monotonic()<deadline:
 time.sleep(0.01)
print(json.dumps({{'ok':True,'kind':'authorization','run_id':'run','context_sha256':'{context_hash}',
'grant':{{'authorization_id':'unit-policy-grant','proposal_id':'p','revision':1,'prepare_token':'token','approval_policy':'reviewed-template'}}}}))
"#)).unwrap();
        let png = [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
        ];
        let path = directory.join("review.png");
        std::fs::write(&path, png).unwrap();
        let image_sha = format!("{:x}", Sha256::digest(png));
        let config = Config {
            permission: Some("read-only".into()),
            domain_approval_policy: policy,
            auto_continue: if matches!(
                completion,
                "restore_wait_after_steer" | "ready_card_after_steer"
            ) {
                20
            } else {
                0
            },
            domain_setup: Some(crate::domain::DomainSetup {
                mode: crate::domain::DomainMode::Floor,
                context_path: directory.join("context.json"),
                repository: directory.clone(),
                python: crate::domain::receipt::tests::python(),
                run_id: "run".into(),
                context_sha256: context_hash.clone(),
                floor_id: "floor".into(),
            }),
            ..Default::default()
        };
        let (mut client, mut server) = harness_with_config(config).await;
        initialized(&mut server).await;
        let set = next(&mut server).await;
        send(
            &mut server,
            json!({"id":set["id"],"result":{"current":"read-only","presets":[]}}),
        )
        .await;
        phase(&mut client, SessionPhase::Ready).await;
        skip_skills(&mut server).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "建模二层".into(),
            })
            .await
            .unwrap();
        let start = next(&mut server).await;
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"root-turn"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        let proposal = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,
            "kind":"proposal","fact_sequence":1,"current":true,"source_current":true,"diagnostics":[],
            "auto_approval":{"contract":"reviewed-template-approval/1","eligible":true},
            "proposal":{"proposal_id":"p","revision":1,"prepare_token":"token","base_sha256":"base",
            "candidate_sha256":"candidate","state":"prepared","summary":"reviewed chair","draft":{"operations":[]},
            "assumed":[],"issues":[],"pending":[],"component_first_use_requirements":{
                "contract":"component-first-use/1","required":[],"reused":[{"review_key":"human-reviewed"}]}}});
        observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
            "item":{"id":"p","type":"mcpToolCall","server":"modeling_domain","tool":"prepare_change","status":"completed","result":{"structuredContent":proposal.clone()}}}})).await;
        let artifact = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,"kind":"artifact_validation","fact_sequence":2,
            "artifact":{"build_id":"b","proposal_id":"p","revision":1,"prepare_token":"token","candidate":true,"authority_sha256":"base","input_sha256":"candidate","state":"built","output":"model.glb","valid":true,"current":true,
            "review_image":{"delivery":"attached_png","image_sha256":image_sha,"image_path":path,"dimensions_px":[1,1],"request":{"build_id":"b","proposal_id":"p","revision":1,"prepare_token":"token","input_sha256":"candidate","blend_sha256":"a".repeat(64),"focus_m":null}}}});
        observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
            "item":{"id":"b","type":"mcpToolCall","server":"modeling_domain","tool":"read_artifact_review_image","status":"completed","result":{"structuredContent":artifact.clone()}}}})).await;
        tokio::time::timeout(
            Duration::from_secs(5),
            client.snapshots.wait_for(|s| {
                s.extensions[0]
                    .modeling()
                    .unwrap()
                    .review_images
                    .first()
                    .is_some_and(|i| i.file_state == crate::domain::ImageFileState::Available)
            }),
        )
        .await
        .unwrap()
        .unwrap();
        let binding = client.snapshots.borrow().extensions[0]
            .modeling()
            .unwrap()
            .proposal
            .as_ref()
            .unwrap()
            .binding
            .clone();
        client
            .commands
            .send(Command::ConfirmExtension {
                confirmation: crate::extensions::ExtensionConfirmation::Modeling(
                    binding.clone().into(),
                ),
            })
            .await
            .unwrap();
        client
            .snapshots
            .wait_for(|s| s.extensions[0].modeling().unwrap().authorizing)
            .await
            .unwrap();
        let image = client.snapshots.borrow().extensions[0]
            .modeling()
            .unwrap()
            .review_images[0]
            .clone();
        client
            .commands
            .send(Command::OpenDomainImage {
                binding: image.binding.clone(),
            })
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(2),
            client.snapshots.wait_for(|s| {
                s.notice
                    .as_deref()
                    .is_some_and(|text| text.contains("确认正在保存"))
            }),
        )
        .await
        .unwrap()
        .unwrap();
        let during = client.snapshots.borrow().clone();
        let domain = during.extensions[0].modeling().unwrap();
        assert!(domain.authorizing);
        assert_eq!(domain.review_images[0], image);
        assert!(!directory.join("release-authorization").exists());
        if completion == "other_unready" {
            let other = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,
                "kind":"proposal","fact_sequence":3,"current":true,"source_current":true,"diagnostics":[],
                "proposal":{"proposal_id":"other","revision":1,"prepare_token":"other-token","base_sha256":"base",
                    "candidate_sha256":"other-candidate","state":"draft","summary":"其他未就绪方案","draft":{"operations":[]},
                    "assumed":[],"issues":[],"pending":[],"component_first_use_requirements":{
                        "contract":"component-first-use/1","required":[],"reused":[]}}});
            observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
                "item":{"id":"other","type":"mcpToolCall","server":"modeling_domain","tool":"prepare_change","status":"completed","result":{"structuredContent":other}}}})).await;
            tokio::time::timeout(
                Duration::from_secs(5),
                client.snapshots.wait_for(|snapshot| {
                    snapshot.extensions[0]
                        .modeling()
                        .unwrap()
                        .proposal
                        .as_ref()
                        .is_some_and(|proposal| proposal.binding.proposal_id == "other")
                }),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                client.snapshots.borrow().extensions[0]
                    .modeling()
                    .unwrap()
                    .proposal
                    .as_ref()
                    .unwrap()
                    .binding
                    .proposal_id,
                "other"
            );
        }
        if completion == "approved" {
            let mut unbound_web = image.binding.clone();
            unbound_web.web_review = Some(crate::review_web::WebReviewPackage {
                html_path: directory.join("unbound.html"),
                png_path: path.clone(),
                data_sha256: "a".repeat(64),
                manifest_sha256: None,
                artifact_sha256: None,
                url_hash: None,
                first_use_comparison: None,
            });
            client
                .commands
                .send(Command::OpenDomainImage {
                    binding: unbound_web,
                })
                .await
                .unwrap();
            tokio::time::timeout(
                Duration::from_secs(2),
                client.snapshots.wait_for(|s| {
                    s.extensions[0].modeling().unwrap().notice.as_deref()
                        == Some("图片不可用或已属于旧版本；请核对当前图片列表")
                }),
            )
            .await
            .unwrap()
            .unwrap();
            let current = client.snapshots.borrow();
            assert!(current.extensions[0].modeling().unwrap().authorizing);
            assert_eq!(
                current.extensions[0].modeling().unwrap().review_images[0],
                image
            );
            assert_eq!(current.root_start_requests, 1);
        }
        if matches!(
            completion,
            "preview_required" | "restore_candidate_review" | "unknown"
        ) {
            let pending = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,
                "kind":"component_first_use_review","fact_sequence":3,
                "component_first_use_review":{"proposal_id":"p","revision":1,"prepare_token":"token",
                    "base_sha256":"base","candidate_sha256":"candidate","current":true,"status":completion}});
            observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
                "item":{"id":"pending-review","type":"mcpToolCall","server":"modeling_domain","tool":"inspect_change","status":"completed","result":{"structuredContent":pending}}}})).await;
            assert!(!client.snapshots.borrow().extensions[0]
                .modeling()
                .unwrap()
                .first_use_satisfied());
            std::fs::write(directory.join("release-authorization"), b"ready").unwrap();
            tokio::time::timeout(
                Duration::from_secs(10),
                client
                    .snapshots
                    .wait_for(|s| !s.extensions[0].modeling().unwrap().authorizing),
            )
            .await
            .unwrap_or_else(|_| panic!("authorization did not finish for {completion}"))
            .unwrap();
            let finished = client.snapshots.borrow().clone();
            assert_eq!(finished.root_start_requests, 1);
            assert_eq!(
                finished.extensions[0].modeling().unwrap().notice.as_deref(),
                Some("批准结果已过期；请核对当前方案")
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                    .await
                    .is_err()
            );
            client.commands.send(Command::Quit).await.unwrap();
            client.join.await.unwrap();
            std::fs::remove_dir_all(directory).unwrap();
            continue;
        }
        if !matches!(
            completion,
            "approved"
                | "pending_after_steer"
                | "other_unready"
                | "restore_wait_after_steer"
                | "ready_card_after_steer"
        ) {
            if completion == "newer_notice" {
                client
                    .commands
                    .send(Command::SaveSetting {
                        setting: crate::preferences::Setting::Language,
                        value: "unsupported".into(),
                    })
                    .await
                    .unwrap();
            }
            let mut returned_binding = binding.clone();
            if completion == "expired" {
                returned_binding.revision += 1;
            }
            client
                .commands
                .send(Command::DomainAuthorizationFinished {
                    attempt_id: 1,
                    generation: 1,
                    root_turn_count: 1,
                    binding: returned_binding,
                    review_set_sha256: None,
                    policy,
                    outcome: Err("测试批准失败".into()),
                })
                .await
                .unwrap();
            tokio::time::timeout(
                Duration::from_secs(2),
                client
                    .snapshots
                    .wait_for(|s| !s.extensions[0].modeling().unwrap().authorizing),
            )
            .await
            .unwrap()
            .unwrap();
            let finished = client.snapshots.borrow().clone();
            assert_eq!(
                finished.notice.as_deref(),
                if completion == "newer_notice" {
                    Some("Unsupported setting value.")
                } else {
                    None
                }
            );
            assert_eq!(
                finished.extensions[0].modeling().unwrap().notice.as_deref(),
                Some(if completion == "expired" {
                    "批准结果已过期；请核对当前方案"
                } else {
                    "测试批准失败"
                })
            );
            std::fs::write(directory.join("release-authorization"), b"ready").unwrap();
            client.commands.send(Command::Quit).await.unwrap();
            client.join.await.unwrap();
            std::fs::remove_dir_all(directory).unwrap();
            continue;
        }
        std::fs::write(directory.join("release-authorization"), b"ready").unwrap();
        // Approval while the turn runs is steered in; the turn then ends without
        // applying it, so the grant must come back as the next input.
        let steer = next(&mut server).await;
        assert_eq!(steer["method"], "turn/steer");
        if completion == "other_unready" {
            let text = steer["params"]["text"].as_str().unwrap();
            let payload: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
            assert_eq!(payload["apply_change_arguments"]["proposal_id"], "p");
            assert_eq!(payload["apply_change_arguments"]["prepare_token"], "token");
            assert_eq!(
                client.snapshots.borrow().extensions[0]
                    .modeling()
                    .unwrap()
                    .approval_card_snapshots()
                    .len(),
                1
            );
        }
        if completion == "pending_after_steer" {
            let pending = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,
                "kind":"component_first_use_review","fact_sequence":3,
                "component_first_use_review":{"proposal_id":"p","revision":1,"prepare_token":"token",
                    "base_sha256":"base","candidate_sha256":"candidate","current":true,"status":"preview_required"}});
            observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
                "item":{"id":"pending-after-steer","type":"mcpToolCall","server":"modeling_domain","tool":"inspect_change","status":"completed","result":{"structuredContent":pending}}}})).await;
        }
        if completion == "restore_wait_after_steer" {
            let mut restored =
                crate::domain::tests::approval_restore_fact("p", "revision-2-build", 3);
            restored["context_sha256"] = json!(context_hash);
            observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
                "item":{"id":"restore-after-steer","type":"mcpToolCall","server":"modeling_domain","tool":"restore_approval_card","status":"completed","result":{"structuredContent":restored}}}})).await;
            assert!(client.snapshots.borrow().extensions[0]
                .modeling()
                .unwrap()
                .approval_restore_waiting());
        }
        if completion == "ready_card_after_steer" {
            let mut other = proposal.clone();
            other["fact_sequence"] = json!(3);
            other["proposal"]["proposal_id"] = json!("other");
            other["proposal"]["prepare_token"] = json!("other-token");
            observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
                "item":{"id":"other-after-steer","type":"mcpToolCall","server":"modeling_domain","tool":"prepare_change","status":"completed","result":{"structuredContent":other}}}})).await;
            let mut other_artifact = artifact.clone();
            other_artifact["fact_sequence"] = json!(4);
            other_artifact["artifact"]["build_id"] = json!("other-build");
            other_artifact["artifact"]["proposal_id"] = json!("other");
            other_artifact["artifact"]["prepare_token"] = json!("other-token");
            other_artifact["artifact"]["review_image"]["request"]["build_id"] =
                json!("other-build");
            other_artifact["artifact"]["review_image"]["request"]["proposal_id"] = json!("other");
            other_artifact["artifact"]["review_image"]["request"]["prepare_token"] =
                json!("other-token");
            observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
                "item":{"id":"other-review-after-steer","type":"mcpToolCall","server":"modeling_domain","tool":"read_artifact_review_image","status":"completed","result":{"structuredContent":other_artifact}}}})).await;
            tokio::time::timeout(
                Duration::from_secs(5),
                client.snapshots.wait_for(|snapshot| {
                    snapshot.extensions[0]
                        .modeling()
                        .unwrap()
                        .approval_card_waiting()
                }),
            )
            .await
            .unwrap()
            .unwrap();
        }
        observation_event(&mut client,&mut server,json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}})).await;
        if matches!(
            completion,
            "pending_after_steer" | "restore_wait_after_steer" | "ready_card_after_steer"
        ) {
            assert!(
                tokio::time::timeout(Duration::from_millis(100), next(&mut server))
                    .await
                    .is_err()
            );
            assert_eq!(client.snapshots.borrow().root_start_requests, 1);
            client.commands.send(Command::Quit).await.unwrap();
            client.join.await.unwrap();
            std::fs::remove_dir_all(directory).unwrap();
            continue;
        }
        let resent = next(&mut server).await;
        assert_eq!(resent["method"], "turn/start");
        assert_eq!(
            resent["params"]["input"][0]["text"],
            steer["params"]["text"]
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}

fn selection(sequence: u64) -> crate::preview::Selection {
    serde_json::from_value(json!({"client_id":"gui", "sequence":sequence,
        "binding":{"run_id":"run", "context_sha256":"context", "proposal_id":"proposal", "revision":1,
            "prepare_token":"token", "base_sha256":"base", "candidate_sha256":"candidate"},
        "snapshot_id":"a".repeat(64), "active_part_id":"bed.1.mattress", "active_layer":"candidate",
        "objects":[{"part_id":"bed.1.mattress", "entity_id":"bed.1.mattress", "instance_id":"bed.1",
            "category":"bed", "floor_id":"floor.ground", "source_ids":[], "feedback_targets":["bed.1.base","bed.1.mattress"],
            "layer":"candidate", "diff":"added", "center":["1.5","1.7","0.5"], "dimensions":["0.9","2","0.2"], "diagnostics":[]}]})).unwrap()
}

#[tokio::test]
async fn observing_and_clearing_selection_never_starts_an_agent_or_grants_approval() {
    let (mut client, mut server) = harness().await;
    initialized(&mut server).await;
    phase(&mut client, SessionPhase::Ready).await;
    client
        .commands
        .send(Command::PreviewEvent {
            epoch: 1,
            event: crate::preview::Event::Selection(Box::new(selection(2))),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            client.snapshots.changed().await.unwrap();
            if client.snapshots.borrow().extensions[0]
                .modeling()
                .unwrap()
                .preview_selection
                .is_some()
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    for (epoch, event) in [
        (1, crate::preview::Event::Selection(Box::new(selection(1)))),
        (0, crate::preview::Event::Selection(Box::new(selection(3)))),
        (
            1,
            crate::preview::Event::SelectionClosed {
                client_id: "other".into(),
            },
        ),
    ] {
        client
            .commands
            .send(Command::PreviewEvent { epoch, event })
            .await
            .unwrap();
    }
    tokio::task::yield_now().await;
    assert_eq!(
        client.snapshots.borrow().extensions[0]
            .modeling()
            .unwrap()
            .preview_selection
            .as_ref()
            .unwrap()
            .sequence,
        2
    );
    assert_eq!(client.snapshots.borrow().root_start_requests, 0);
    assert!(
        !client.snapshots.borrow().extensions[0]
            .modeling()
            .unwrap()
            .authorizing
    );
    client
        .commands
        .send(Command::PreviewEvent {
            epoch: 1,
            event: crate::preview::Event::SelectionClosed {
                client_id: "gui".into(),
            },
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            client.snapshots.changed().await.unwrap();
            if client.snapshots.borrow().extensions[0]
                .modeling()
                .unwrap()
                .preview_selection
                .is_none()
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(client.snapshots.borrow().root_start_requests, 0);
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn preview_events_from_previous_session_epoch_do_not_project_or_submit() {
    let (mut client, mut server) = harness().await;
    initialized(&mut server).await;
    phase(&mut client, SessionPhase::Ready).await;
    client
        .commands
        .send(Command::PreviewEvent {
            epoch: 0,
            event: crate::preview::Event::Ready {
                connection_file: "old-token-file".into(),
            },
        })
        .await
        .unwrap();
    client
        .commands
        .send(Command::PreviewEvent {
            epoch: 1,
            event: crate::preview::Event::Ready {
                connection_file: "current-file".into(),
            },
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            client.snapshots.changed().await.unwrap();
            if client.snapshots.borrow().extensions[0]
                .modeling()
                .unwrap()
                .preview_connection
                .as_deref()
                == Some("current-file")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(client.snapshots.borrow().root_start_requests, 0);
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn domain_sessions_reconfirm_read_only_after_resume_reload_and_new_conversation() {
    for transition in ["resume", "reload", "new"] {
        for granted in [true, false] {
            let config = Config {
                permission: Some("read-only".into()),
                domain_setup: Some(crate::domain::DomainSetup {
                    mode: crate::domain::DomainMode::Floor,
                    context_path: "context.json".into(),
                    repository: ".".into(),
                    python: "python".into(),
                    run_id: "run".into(),
                    context_sha256: "hash".into(),
                    floor_id: "floor".into(),
                }),
                ..Default::default()
            };
            let (mut client, mut server) = harness_with_config(config).await;
            initialized(&mut server).await;
            let set = next(&mut server).await;
            send(
                &mut server,
                json!({"id":set["id"],"result":{"current":"read-only","presets":[]}}),
            )
            .await;
            phase(&mut client, SessionPhase::Ready).await;
            skip_skills(&mut server).await;
            let command = match transition {
                "resume" => Command::ResumeConversation { id: "other".into() },
                "reload" => Command::ReloadBackend,
                _ => Command::NewConversation,
            };
            client.commands.send(command).await.unwrap();
            let request = next(&mut server).await;
            send(
                &mut server,
                json!({"id":request["id"],"result":{"thread":{"id":"other"},"model":"test-model"}}),
            )
            .await;
            if transition == "resume" {
                let history = next(&mut server).await;
                assert_eq!(history["method"], "thread/history");
                send(
                    &mut server,
                    json!({"id":history["id"],"result":{"messages":[],"omitted":0}}),
                )
                .await;
            }
            let set = next(&mut server).await;
            assert_eq!(set["method"], "permission/set", "{transition}");
            assert_eq!(set["params"]["preset"], "read-only");
            assert_eq!(set["params"]["remember"], false);
            client
                .commands
                .send(Command::SubmitRootInput {
                    text: "continue modeling".into(),
                })
                .await
                .unwrap();
            tokio::task::yield_now().await;
            assert_ne!(client.snapshots.borrow().phase, SessionPhase::Ready);
            assert_eq!(client.snapshots.borrow().root_start_requests, 0);
            send(&mut server, json!({"id":set["id"],"result":{"current":if granted {"read-only"} else {"workspace-write"},"presets":[]}})).await;
            if granted {
                let mut turn = None;
                for _ in 0..5 {
                    let request = next(&mut server).await;
                    if request["method"] == "turn/start" {
                        turn = Some(request);
                        break;
                    }
                    send(
                        &mut server,
                        json!({"id":request["id"],"error":{"code":-32601,"message":"unsupported"}}),
                    )
                    .await;
                }
                assert_eq!(turn.unwrap()["params"]["threadId"], "other");
                assert_eq!(client.snapshots.borrow().permission.current, "read-only");
            } else {
                phase(&mut client, SessionPhase::Failed).await;
                assert_eq!(client.snapshots.borrow().root_start_requests, 0);
            }
            client.commands.send(Command::Quit).await.unwrap();
            client.join.await.unwrap();
        }
    }
}

#[tokio::test]
async fn domain_proposals_survive_display_clipping_but_reject_foreign_or_incomplete_events() {
    let config = Config {
        permission: Some("read-only".into()),
        domain_setup: Some(crate::domain::DomainSetup {
            mode: crate::domain::DomainMode::Floor,
            context_path: "context.json".into(),
            repository: ".".into(),
            python: "python".into(),
            run_id: "run".into(),
            context_sha256: "hash".into(),
            floor_id: "floor".into(),
        }),
        ..Default::default()
    };
    let (mut client, mut server) = harness_with_config(config).await;
    initialized(&mut server).await;
    let set = next(&mut server).await;
    send(
        &mut server,
        json!({"id":set["id"],"result":{"current":"read-only","presets":[]}}),
    )
    .await;
    phase(&mut client, SessionPhase::Ready).await;
    skip_skills(&mut server).await;
    client
        .commands
        .send(Command::SubmitRootInput {
            text: "prepare source-backed chair".into(),
        })
        .await
        .unwrap();
    let start = next(&mut server).await;
    send(
        &mut server,
        json!({"id":start["id"],"result":{"turn":{"id":"root-turn"}}}),
    )
    .await;
    phase(&mut client, SessionPhase::Running).await;
    let result = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":"hash",
        "kind":"proposal","fact_sequence":1,"current":true,"diagnostics":[],"proposal":{
        "proposal_id":"chair-proposal","revision":1,"prepare_token":"token","base_sha256":"base",
        "candidate_sha256":"candidate","state":"prepared","summary":"source-backed chair",
        "draft":{"operations":[{"collection":"solids","basis":"assumed","value_json":
            json!({"id":"chair.base","origin":[12,7,0],"source_ids":["source.chair"],
                "metadata":{"category":"chair","reason":"source evidence ".repeat(800)}}).to_string()}]},
        "assumed":[],"issues":[],"pending":[],"component_plan":{}}});
    assert!(result.to_string().len() > 8 * 1024);
    assert!(result.to_string().len() < 64 * 1024);
    let valid = json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
        "item":{"id":"chair","type":"mcpToolCall","server":"modeling_domain","tool":"prepare_change",
        "status":"completed","result":{"structuredContent":result.clone()}}}});
    for (index, invalid) in [
        ("/params/turnId", json!("stale-turn")),
        ("/params/threadId", json!("foreign-thread")),
        ("/params/item/server", json!("other_server")),
        ("/params/item/status", json!("failed")),
        (
            "/params/item/result/structuredContent/context_sha256",
            json!("foreign-context"),
        ),
        (
            "/params/item/result/structuredContent/proposal/summary",
            json!("x".repeat(64 * 1024)),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut event = valid.clone();
        event["params"]["item"]["id"] = json!(format!("invalid-{index}"));
        *event.pointer_mut(invalid.0).unwrap() = invalid.1;
        observation_event(&mut client, &mut server, event).await;
        assert!(
            client.snapshots.borrow().extensions[0]
                .modeling()
                .unwrap()
                .proposal
                .is_none(),
            "{}",
            invalid.0
        );
    }
    observation_event(&mut client, &mut server, valid).await;
    let snapshot = client.snapshots.borrow().clone();
    assert!(snapshot
        .tool_details
        .entries
        .iter()
        .any(|entry| entry.clipped));
    let proposal = snapshot.extensions[0]
        .modeling()
        .unwrap()
        .proposal
        .as_ref()
        .expect("complete domain result was lost to display clipping");
    assert_eq!(proposal.binding.proposal_id, "chair-proposal");
    assert!(proposal.ready);
    assert!(proposal.objects[0].contains("chair.base"));
    let stored = serde_json::to_string(&StoredSnapshot::capture(&snapshot)).unwrap();
    assert!(!stored.contains("source evidence"));
    client
        .commands
        .send(Command::ConfirmExtension {
            confirmation: crate::extensions::ExtensionConfirmation::Modeling(
                proposal.binding.clone().into(),
            ),
        })
        .await
        .unwrap();
    client
        .snapshots
        .wait_for(|snapshot| {
            snapshot.extensions[0].modeling().unwrap().notice.as_deref()
                == Some("当前方案或执行状态不允许批准；请等待当前轮完成并核对版本")
        })
        .await
        .unwrap();
    assert!(
        !client.snapshots.borrow().extensions[0]
            .modeling()
            .unwrap()
            .authorizing
    );
    assert_eq!(client.snapshots.borrow().root_start_requests, 1);
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn domain_proposals_use_private_facts_and_reject_foreign_or_incomplete_receipts() {
    use sha2::{Digest, Sha256};
    let directory = std::env::temp_dir().join(format!(
        "domain-core-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(directory.join("blender_agent/cli")).unwrap();
    std::fs::write(directory.join("blender_agent/__init__.py"), "").unwrap();
    std::fs::write(directory.join("blender_agent/cli/__init__.py"), "").unwrap();
    let context_path = directory.join("context.json");
    std::fs::write(&context_path, b"{}").unwrap();
    let context_hash = format!("{:x}", Sha256::digest(b"{}"));
    std::fs::write(directory.join("blender_agent/cli/main.py"),
        "import sys\nfrom pathlib import Path\nsys.stdout.buffer.write(Path(__file__).with_name('fact.bin').read_bytes())\n").unwrap();
    let config = Config {
        permission: Some("read-only".into()),
        domain_setup: Some(crate::domain::DomainSetup {
            mode: crate::domain::DomainMode::Floor,
            context_path,
            repository: directory.clone(),
            python: crate::domain::receipt::tests::python(),
            run_id: "run".into(),
            context_sha256: context_hash.clone(),
            floor_id: "floor".into(),
        }),
        ..Default::default()
    };
    let (mut client, mut server) = harness_with_config(config).await;
    initialized(&mut server).await;
    let set = next(&mut server).await;
    send(
        &mut server,
        json!({"id":set["id"],"result":{"current":"read-only","presets":[]}}),
    )
    .await;
    phase(&mut client, SessionPhase::Ready).await;
    skip_skills(&mut server).await;
    client
        .commands
        .send(Command::SubmitRootInput {
            text: "prepare source-backed chair".into(),
        })
        .await
        .unwrap();
    let start = next(&mut server).await;
    send(
        &mut server,
        json!({"id":start["id"],"result":{"turn":{"id":"root-turn"}}}),
    )
    .await;
    phase(&mut client, SessionPhase::Running).await;
    let result = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,"ok":true,
        "kind":"proposal","fact_sequence":1,"current":true,"diagnostics":[],"proposal":{
        "proposal_id":"chair-proposal","revision":1,"prepare_token":"token","base_sha256":"base",
        "candidate_sha256":"candidate","state":"prepared","summary":"source-backed chair",
        "draft":{"operations":[{"collection":"solids","basis":"assumed","value_json":
            json!({"id":"chair.base","origin":[12,7,0],"source_ids":["source.chair"],
                "metadata":{"category":"chair","reason":"source evidence ".repeat(5000)}}).to_string()}]},
        "assumed":[],"issues":[],"pending":[],"component_plan":{}}});
    let raw = serde_json::to_vec(&result).unwrap();
    assert!(raw.len() > 64 * 1024);
    std::fs::write(directory.join("blender_agent/cli/fact.bin"), &raw).unwrap();
    let receipt = json!({"contract":"modeling-fact-receipt/v1", "run_id":"run",
        "context_sha256":context_hash, "kind":"proposal", "ok":true,
        "fact_sequence":1, "fact_sha256":format!("{:x}", Sha256::digest(&raw)), "fact_bytes":raw.len(),
        "binding":{"proposal_id":"chair-proposal", "revision":1, "prepare_token":"token",
        "base_sha256":"base", "candidate_sha256":"candidate"}});
    assert!(receipt.to_string().len() < 1000);
    let valid = json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
        "item":{"id":"chair","type":"mcpToolCall","server":"modeling_domain","tool":"prepare_change",
        "status":"completed","result":{"structuredContent":receipt.clone()}}}});
    for (index, invalid) in [
        ("/params/turnId", json!("stale-turn")),
        ("/params/threadId", json!("foreign-thread")),
        ("/params/item/server", json!("other_server")),
        ("/params/item/status", json!("failed")),
        (
            "/params/item/result/structuredContent/context_sha256",
            json!("foreign-context"),
        ),
        (
            "/params/item/result/structuredContent/fact_sha256",
            json!("x".repeat(64 * 1024)),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut event = valid.clone();
        event["params"]["item"]["id"] = json!(format!("invalid-{index}"));
        *event.pointer_mut(invalid.0).unwrap() = invalid.1;
        observation_event(&mut client, &mut server, event).await;
        assert!(
            client.snapshots.borrow().extensions[0]
                .modeling()
                .unwrap()
                .proposal
                .is_none(),
            "{}",
            invalid.0
        );
    }
    // The real ACP representation has a single JSON text content block.
    let mut valid = valid;
    valid["params"]["item"]["result"] =
        json!({"content":[{"type":"text","text":receipt.to_string()}]});
    observation_event(&mut client, &mut server, valid).await;
    tokio::time::timeout(
        Duration::from_secs(5),
        client.snapshots.wait_for(|snapshot| {
            let domain = snapshot.extensions[0].modeling().unwrap();
            domain.proposal.is_some() && domain.fact_sync_notice.is_none()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let snapshot = client.snapshots.borrow().clone();
    let domain = snapshot.extensions[0].modeling().unwrap();
    let proposal = domain.proposal.as_ref().unwrap();
    assert_eq!(proposal.binding.proposal_id, "chair-proposal");
    assert!(proposal.ready);
    assert!(proposal.objects[0].contains("chair.base"));
    let stored = serde_json::to_string(&StoredSnapshot::capture(&snapshot)).unwrap();
    assert!(!stored.contains("source evidence"));
    assert_eq!(domain.verified_fact.as_ref().unwrap().bytes, raw.len());
    // The next read fails visibly and cannot grant the retained old proposal.
    std::fs::remove_file(directory.join("blender_agent/cli/fact.bin")).unwrap();
    let event = json!({"method":"item/completed", "params":{"threadId":"root", "turnId":"root-turn",
        "item":{"id":"missing","type":"mcpToolCall","server":"modeling_domain","tool":"inspect_change",
        "status":"completed","result":{"content":[{"type":"text","text":receipt.to_string()}]}}}});
    observation_event(&mut client, &mut server, event.clone()).await;
    tokio::time::timeout(
        Duration::from_secs(5),
        client.snapshots.wait_for(|snapshot| {
            snapshot.extensions[0]
                .modeling()
                .unwrap()
                .fact_sync_notice
                .as_ref()
                .is_some_and(|notice| notice.contains("同步失败"))
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(client.snapshots.borrow().extensions[0]
        .modeling()
        .unwrap()
        .proposal
        .is_some());
    std::fs::write(directory.join("blender_agent/cli/fact.bin"), &raw).unwrap();
    let mut recovered = event;
    recovered["params"]["item"]["id"] = json!("recovered");
    observation_event(&mut client, &mut server, recovered).await;
    tokio::time::timeout(
        Duration::from_secs(5),
        client.snapshots.wait_for(|snapshot| {
            snapshot.extensions[0]
                .modeling()
                .unwrap()
                .fact_sync_notice
                .is_none()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn project_discovery_facts_flow_through_the_normal_client_and_cannot_be_approved() {
    let config = Config {
        permission: Some("read-only".into()),
        domain_setup: Some(crate::domain::DomainSetup {
            mode: crate::domain::DomainMode::Project,
            context_path: "project-context.json".into(),
            repository: ".".into(),
            python: "python".into(),
            run_id: "run".into(),
            context_sha256: "ctx".into(),
            floor_id: "项目目标".into(),
        }),
        ..Default::default()
    };
    let (mut client, mut server) = harness_with_config(config).await;
    initialized(&mut server).await;
    let set = next(&mut server).await;
    send(
        &mut server,
        json!({"id":set["id"],"result":{"current":"read-only","presets":[]}}),
    )
    .await;
    phase(&mut client, SessionPhase::Ready).await;
    skip_skills(&mut server).await;
    client
        .commands
        .send(Command::SubmitRootInput {
            text: "建模整栋".into(),
        })
        .await
        .unwrap();
    let start = next(&mut server).await;
    send(
        &mut server,
        json!({"id":start["id"],"result":{"turn":{"id":"root-turn"}}}),
    )
    .await;
    phase(&mut client, SessionPhase::Running).await;
    let catalog = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":"ctx",
        "kind":"project_catalog","ok":true,"fact_sequence":1,"project":{
            "catalog_id":"a".repeat(64),"source_sha256":"b".repeat(64),"project_path":"projects/test",
            "floors":[{"key":"f08","title":"八层"}]}});
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/completed","params":{
        "threadId":"root","turnId":"root-turn","item":{"id":"catalog","type":"mcpToolCall",
        "server":"modeling_domain","tool":"inspect_project","status":"completed",
        "result":{"structuredContent":catalog}}}}),
    )
    .await;
    assert_eq!(
        client.snapshots.borrow().extensions[0]
            .modeling()
            .unwrap()
            .project
            .as_ref()
            .unwrap()
            .total_floors,
        1
    );
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"turn/completed","params":{
        "threadId":"root","turn":{"id":"root-turn","status":"completed"}}}),
    )
    .await;
    client
        .commands
        .send(Command::ConfirmExtension {
            confirmation: crate::extensions::ExtensionConfirmation::Modeling(
                crate::domain::ProposalBinding {
                    run_id: "run".into(),
                    context_sha256: "ctx".into(),
                    proposal_id: "target-is-not-a-proposal".into(),
                    revision: 1,
                    prepare_token: "none".into(),
                    base_sha256: "none".into(),
                    candidate_sha256: "none".into(),
                }
                .into(),
            ),
        })
        .await
        .unwrap();
    client
        .snapshots
        .wait_for(|snapshot| snapshot.extensions[0].modeling().unwrap().notice.is_some())
        .await
        .unwrap();
    assert!(
        !client.snapshots.borrow().extensions[0]
            .modeling()
            .unwrap()
            .authorizing
    );
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn old_image_callbacks_do_not_release_current_check_and_missing_file_open_is_not_credited() {
    use sha2::{Digest, Sha256};
    for completion in [
        "missing",
        "replaced",
        "newer_notice",
        "stale_attempt",
        "stale_generation",
    ] {
        let path = std::env::temp_dir().join(format!(
            "tui-owned-image-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut bytes = vec![0u8; 16 * 1024 * 1024];
        bytes[..24].copy_from_slice(&[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
        ]);
        std::fs::write(&path, &bytes).unwrap();
        let sha = format!("{:x}", Sha256::digest(&bytes));
        let config = Config {
            permission: Some("read-only".into()),
            domain_setup: Some(crate::domain::DomainSetup {
                mode: crate::domain::DomainMode::Floor,
                context_path: "context.json".into(),
                repository: ".".into(),
                python: "python".into(),
                run_id: "run".into(),
                context_sha256: "hash".into(),
                floor_id: "floor".into(),
            }),
            ..Default::default()
        };
        let (mut client, mut server) = harness_with_config(config).await;
        initialized(&mut server).await;
        let set = next(&mut server).await;
        send(
            &mut server,
            json!({"id":set["id"],"result":{"current":"read-only","presets":[]}}),
        )
        .await;
        phase(&mut client, SessionPhase::Ready).await;
        skip_skills(&mut server).await;
        client
            .commands
            .send(Command::SubmitRootInput {
                text: "prepare chair".into(),
            })
            .await
            .unwrap();
        let start = next(&mut server).await;
        send(
            &mut server,
            json!({"id":start["id"],"result":{"turn":{"id":"root-turn"}}}),
        )
        .await;
        phase(&mut client, SessionPhase::Running).await;
        let proposal = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":"hash","kind":"proposal","fact_sequence":1,"current":true,"diagnostics":[],"proposal":{
        "proposal_id":"p","revision":1,"prepare_token":"token","base_sha256":"base","candidate_sha256":"candidate","state":"prepared","summary":"chair","draft":{"operations":[]},"assumed":[],"issues":[],"pending":[],
        "component_first_use_requirements":{"contract":"component-first-use/1","required":[],"reused":[]}}});
        observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn","item":{"id":"p","type":"mcpToolCall","server":"modeling_domain","tool":"prepare_change","status":"completed","result":{"structuredContent":proposal}}}})).await;
        let artifact = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":"hash","kind":"artifact_validation","fact_sequence":2,
        "artifact":{"build_id":"b","proposal_id":"p","revision":1,"prepare_token":"token","candidate":true,"authority_sha256":"base","input_sha256":"candidate","state":"built","output":"model.glb","valid":true,"current":true,
        "review_image":{"delivery":"attached_png","image_sha256":sha,"image_path":path,"dimensions_px":[1,1],"request":{"build_id":"b","proposal_id":"p","revision":1,"prepare_token":"token","input_sha256":"candidate","blend_sha256":"a".repeat(64),"focus_m":null}}}});
        observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn","item":{"id":"b","type":"mcpToolCall","server":"modeling_domain","tool":"read_artifact_review_image","status":"completed","result":{"structuredContent":artifact}}}})).await;
        let image = client.snapshots.borrow().extensions[0]
            .modeling()
            .unwrap()
            .review_images[0]
            .binding
            .clone();
        client
            .commands
            .send(Command::DomainImagesChecked {
                attempt_id: 0,
                generation: 1,
                results: vec![(image.clone(), Err("stale check must not apply".into()))],
            })
            .await
            .unwrap();
        client
            .commands
            .send(Command::DomainImageOpened {
                attempt_id: 0,
                generation: 1,
                binding: image.clone(),
                outcome: crate::domain::review::ImageOpenOutcome {
                    file_valid: true,
                    result: Ok(()),
                },
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let current = client.snapshots.borrow().extensions[0]
                    .modeling()
                    .unwrap()
                    .review_images[0]
                    .clone();
                assert!(!current.externally_opened);
                assert!(!current
                    .notice
                    .as_deref()
                    .is_some_and(|text| text.contains("stale check")));
                if current.file_state == crate::domain::ImageFileState::Available {
                    break;
                }
                client.snapshots.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        std::fs::remove_file(&path).unwrap();
        client
            .commands
            .try_send(Command::OpenDomainImage {
                binding: image.clone(),
            })
            .unwrap();
        if completion != "missing" {
            if completion == "newer_notice" {
                client
                    .commands
                    .try_send(Command::SaveSetting {
                        setting: crate::preferences::Setting::Language,
                        value: "unsupported".into(),
                    })
                    .unwrap();
            }
            let mut removed = image.clone();
            removed.proposal.revision += 1;
            client
                .commands
                .try_send(Command::DomainImageOpened {
                    // 本地文件验真占用 attempt 1，首次打开属于 attempt 2。
                    attempt_id: 2,
                    generation: u64::from(completion != "stale_generation"),
                    binding: removed,
                    outcome: crate::domain::review::ImageOpenOutcome {
                        file_valid: true,
                        result: Ok(()),
                    },
                })
                .unwrap();
            if completion == "stale_attempt" {
                client
                    .commands
                    .try_send(Command::OpenDomainImage {
                        binding: image.clone(),
                    })
                    .unwrap();
                client
                    .commands
                    .try_send(Command::SaveSetting {
                        setting: crate::preferences::Setting::Language,
                        value: "unsupported".into(),
                    })
                    .unwrap();
                client
                    .commands
                    .try_send(Command::DomainImageOpened {
                        attempt_id: 2,
                        generation: 1,
                        binding: image.clone(),
                        outcome: crate::domain::review::ImageOpenOutcome {
                            file_valid: true,
                            result: Err("旧任务不能结束新打开任务".into()),
                        },
                    })
                    .unwrap();
            }
            // A command after the callback gives the snapshot a deterministic barrier.
            client
                .commands
                .try_send(Command::CompletePath {
                    query: "SUP028-barrier".into(),
                })
                .unwrap();
            tokio::time::timeout(
                Duration::from_secs(2),
                client
                    .snapshots
                    .wait_for(|s| s.path_hints.query == "SUP028-barrier"),
            )
            .await
            .unwrap()
            .unwrap();
            let finished = client.snapshots.borrow().clone();
            assert_eq!(
                finished.notice.as_deref(),
                if matches!(completion, "newer_notice" | "stale_attempt") {
                    Some("Unsupported setting value.")
                } else {
                    None
                }
            );
            assert!(!finished.extensions[0].modeling().unwrap().review_images[0].externally_opened);
            if completion == "stale_attempt" {
                tokio::time::timeout(
                    Duration::from_secs(2),
                    client.snapshots.wait_for(|s| {
                        s.extensions[0].modeling().unwrap().review_images[0].file_state
                            == crate::domain::ImageFileState::Unavailable
                    }),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(
                    client.snapshots.borrow().notice.as_deref(),
                    Some("Unsupported setting value.")
                );
            }
            client.commands.send(Command::Quit).await.unwrap();
            client.join.await.unwrap();
            continue;
        }
        client
            .commands
            .try_send(Command::OpenDomainImage { binding: image })
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(2),
            client.snapshots.wait_for(|s| {
                s.notice
                    .as_deref()
                    .is_some_and(|text| text.contains("复核图片或网页正在打开"))
            }),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            client.snapshots.wait_for(|snapshot| {
                snapshot.extensions[0].modeling().unwrap().review_images[0].file_state
                    == crate::domain::ImageFileState::Unavailable
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            !client.snapshots.borrow().extensions[0]
                .modeling()
                .unwrap()
                .review_images[0]
                .externally_opened
        );
        client.commands.send(Command::Quit).await.unwrap();
        client.join.await.unwrap();
    }
}
