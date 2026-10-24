use super::*;

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
async fn reviewed_template_policy_waits_for_turn_end_and_uses_the_private_authorization_chain() {
    use crate::config::DomainApprovalPolicy;
    use sha2::{Digest, Sha256};
    for policy in [
        DomainApprovalPolicy::Manual,
        DomainApprovalPolicy::ReviewedTemplate,
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
        let proposal = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,
            "kind":"proposal","fact_sequence":1,"current":true,"source_current":true,"diagnostics":[],
            "auto_approval":{"contract":"reviewed-template-approval/1","eligible":true},
            "proposal":{"proposal_id":"p","revision":1,"prepare_token":"token","base_sha256":"base",
            "candidate_sha256":"candidate","state":"prepared","summary":"reviewed chair","draft":{"operations":[]},
            "assumed":[],"issues":[],"pending":[],"component_first_use_requirements":{
                "contract":"component-first-use/1","required":[],"reused":[{"review_key":"human-reviewed"}]}}});
        observation_event(&mut client,&mut server,json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn",
            "item":{"id":"p","type":"mcpToolCall","server":"modeling_domain","tool":"prepare_change","status":"completed","result":{"structuredContent":proposal}}}})).await;
        let artifact = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,"kind":"artifact_validation","fact_sequence":2,
            "artifact":{"build_id":"b","proposal_id":"p","revision":1,"prepare_token":"token","candidate":true,"authority_sha256":"base","input_sha256":"candidate","state":"built","output":"model.glb","valid":true,"current":true,
            "review_image":{"delivery":"attached_png","image_sha256":image_sha,"image_path":path,"dimensions_px":[1,1],"request":{"build_id":"b","proposal_id":"p","revision":1,"prepare_token":"token","input_sha256":"candidate","blend_sha256":"a".repeat(64),"focus_m":null}}}});
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
        observation_event(&mut client,&mut server,json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}})).await;
        if policy == DomainApprovalPolicy::ReviewedTemplate {
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
    for completion in ["approved", "failed", "expired", "newer_notice"] {
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
            "item":{"id":"p","type":"mcpToolCall","server":"modeling_domain","tool":"prepare_change","status":"completed","result":{"structuredContent":proposal}}}})).await;
        let artifact = json!({"contract":"modeling-domain/v2","run_id":"run","context_sha256":context_hash,"kind":"artifact_validation","fact_sequence":2,
            "artifact":{"build_id":"b","proposal_id":"p","revision":1,"prepare_token":"token","candidate":true,"authority_sha256":"base","input_sha256":"candidate","state":"built","output":"model.glb","valid":true,"current":true,
            "review_image":{"delivery":"attached_png","image_sha256":image_sha,"image_path":path,"dimensions_px":[1,1],"request":{"build_id":"b","proposal_id":"p","revision":1,"prepare_token":"token","input_sha256":"candidate","blend_sha256":"a".repeat(64),"focus_m":null}}}});
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
        if completion == "approved" {
            let mut unbound_web = image.binding.clone();
            unbound_web.web_review = Some(crate::review_web::WebReviewPackage {
                html_path: directory.join("unbound.html"),
                png_path: path.clone(),
                data_sha256: "a".repeat(64),
                manifest_sha256: None,
                artifact_sha256: None,
                url_hash: None,
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
        if completion != "approved" {
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
        observation_event(&mut client,&mut server,json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}})).await;
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
