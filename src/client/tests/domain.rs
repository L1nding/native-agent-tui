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
async fn domain_proposals_survive_display_clipping_but_reject_foreign_or_incomplete_events() {
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
            client.snapshots.borrow().domain.proposal.is_none(),
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
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}
