use super::*;

#[tokio::test]
async fn domain_sessions_reconfirm_read_only_after_resume_reload_and_new_conversation() {
    for transition in ["resume", "reload", "new"] {
        for granted in [true, false] {
            let config = Config {
                permission: Some("read-only".into()),
                domain_setup: Some(crate::domain::DomainSetup {
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
            context_path,
            repository: directory.clone(),
            python: std::env::var("MODELING_TEST_PYTHON")
                .unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.into())
                .into(),
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
            client.snapshots.borrow().domain.proposal.is_none(),
            "{}",
            invalid.0
        );
    }
    // The real ACP representation has a single JSON text content block.
    let mut valid = valid;
    valid["params"]["item"]["result"] =
        json!({"content":[{"type":"text","text":receipt.to_string()}]});
    observation_event(&mut client, &mut server, valid).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.snapshots.borrow().domain.proposal.is_none() {
            client.snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let snapshot = client.snapshots.borrow().clone();
    assert!(!snapshot
        .tool_details
        .entries
        .iter()
        .any(|entry| entry.locator.item_id == "chair" && entry.clipped));
    let proposal = snapshot
        .domain
        .proposal
        .as_ref()
        .expect("complete domain result was lost to display clipping");
    assert_eq!(proposal.binding.proposal_id, "chair-proposal");
    assert!(proposal.ready);
    assert!(proposal.objects[0].contains("chair.base"));
    let stored = serde_json::to_string(&StoredSnapshot::capture(&snapshot)).unwrap();
    assert!(!stored.contains("source evidence"));
    assert_eq!(
        snapshot.domain.verified_fact.as_ref().unwrap().bytes,
        raw.len()
    );
    // The next read fails visibly and cannot grant the retained old proposal.
    std::fs::remove_file(directory.join("blender_agent/cli/fact.bin")).unwrap();
    let event = json!({"method":"item/completed", "params":{"threadId":"root", "turnId":"root-turn",
        "item":{"id":"missing","type":"mcpToolCall","server":"modeling_domain","tool":"inspect_change",
        "status":"completed","result":{"content":[{"type":"text","text":receipt.to_string()}]}}}});
    observation_event(&mut client, &mut server, event.clone()).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !client
            .snapshots
            .borrow()
            .domain
            .fact_sync_notice
            .as_ref()
            .is_some_and(|n| n.contains("同步失败"))
        {
            client.snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(client.snapshots.borrow().domain.proposal.is_some());
    std::fs::write(directory.join("blender_agent/cli/fact.bin"), &raw).unwrap();
    let mut recovered = event;
    recovered["params"]["item"]["id"] = json!("recovered");
    observation_event(&mut client, &mut server, recovered).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.snapshots.borrow().domain.fact_sync_notice.is_some() {
            client.snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
