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
            if client.snapshots.borrow().domain.preview_selection.is_some() {
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
        client
            .snapshots
            .borrow()
            .domain
            .preview_selection
            .as_ref()
            .unwrap()
            .sequence,
        2
    );
    assert_eq!(client.snapshots.borrow().root_start_requests, 0);
    assert!(!client.snapshots.borrow().domain.authorizing);
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
            if client.snapshots.borrow().domain.preview_selection.is_none() {
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
            if client
                .snapshots
                .borrow()
                .domain
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
