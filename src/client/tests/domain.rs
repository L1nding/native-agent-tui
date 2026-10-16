use super::*;

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
        .send(Command::OpenDomainImage { binding: image })
        .await
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
