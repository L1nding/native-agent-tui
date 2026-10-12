//! Protocol-v2 owner tests exercise the same transport and commands as the UI.
use super::*;

#[tokio::test]
async fn native_feedback_revises_and_requires_the_new_bound_approval() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = root.join("target/component-child-fixture.exe");
    assert!(std::process::Command::new("rustc")
        .args(["--edition=2021", "-o"])
        .arg(&output)
        .arg(root.join("tests/fixtures/component_child.rs"))
        .status()
        .unwrap()
        .success());
    let child = owned_process::spawn(
        owned_process::Command::new(output),
        owned_process::Input::Pipe,
    )
    .unwrap();
    let context = serde_json::from_value(json!({"scene":"scene.json","scope":{
        "floor_id":"floor.ground","region":[0.5,0.5,7.5,5.5]}}))
    .unwrap();
    let (updates, mut snapshots) = watch::channel(Arc::new(Snapshot::new("新增两张床".into())));
    let (commands, receiver) = mpsc::channel(16);
    let owner = tokio::spawn(async move {
        let mut state = Snapshot::new("新增两张床".into());
        run_child(
            child,
            "project",
            &Operation::Component { context },
            &mut state,
            &updates,
            receiver,
        )
        .await
        .unwrap();
        updates.send_replace(Arc::new(state));
    });
    super::tests::wait_for_question(&mut snapshots).await;
    commands
        .send(Command::Revise {
            run_id: "fixture".into(),
            request_id: "q1".into(),
            text: "移到右侧并保留墙体".into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while snapshots
            .borrow()
            .question
            .as_ref()
            .map(|q| q.request_id.as_str())
            != Some("q2")
        {
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(
        snapshots
            .borrow()
            .question
            .as_ref()
            .unwrap()
            .proposal
            .as_ref()
            .unwrap()
            .proposal_revision,
        2
    );
    // This answer belongs to the replaced proposal; the fixture must never receive it.
    commands
        .send(Command::Answer {
            run_id: "fixture".into(),
            request_id: "q1".into(),
            accepted: true,
        })
        .await
        .unwrap();
    commands
        .send(Command::Answer {
            run_id: "fixture".into(),
            request_id: "q2".into(),
            accepted: true,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), owner)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshots.borrow().phase, Phase::Completed);
    assert!(snapshots.borrow().notes.iter().any(|n| n.contains("重复")));
}

#[test]
fn old_clients_do_not_silently_accept_text_feedback_events() {
    let mut state = Snapshot::new("床".into());
    let frame: Frame = serde_json::from_value(json!({"version":2,"run_id":"run","seq":1,
        "kind":"feedback_requested","request_id":"q","title":"床","lines":[],
        "proposal_id":"p","proposal_revision":1,"base_scene_hash":"base",
        "prepare_token":"token","can_approve":true}))
    .unwrap();
    assert!(state.apply(frame).is_err());
    assert!(state.question.is_none());
}
