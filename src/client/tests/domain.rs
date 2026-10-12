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
