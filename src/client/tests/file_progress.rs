use super::*;
use crate::observation::{ActivityScope, EvidenceKind};
use crate::scheduler::{TaskAttempt, TaskKind};

fn file_snapshot(
    client: &ClientHandle,
    scope: ActivityScope,
) -> crate::observation::ActivitySnapshot {
    client
        .snapshots
        .borrow()
        .observation
        .activities
        .iter()
        .find(|activity| {
            activity.scope == scope
                && (scope != ActivityScope::Tool || activity.item_id.as_deref() == Some("file-1"))
        })
        .unwrap()
        .clone()
}

fn change(path: &str, diff: &str) -> Value {
    json!({"path":path,"kind":{"type":"update","move_path":null},"diff":diff})
}

fn observed(
    client: &ClientHandle,
    agent: &str,
    scope: ActivityScope,
    item: Option<&str>,
) -> crate::observation::ActivitySnapshot {
    client
        .snapshots
        .borrow()
        .observation
        .activities
        .iter()
        .find(|activity| {
            activity.identity.agent_id == agent
                && activity.scope == scope
                && activity.item_id.as_deref() == item
        })
        .unwrap()
        .clone()
}

async fn drain_thread_reads(server: &mut BufReader<tokio::io::DuplexStream>, parent: &str) {
    loop {
        let Ok(request) = tokio::time::timeout(Duration::from_millis(50), next(server)).await
        else {
            break;
        };
        assert_eq!(request["method"], "thread/read");
        let id = request["id"].clone();
        let thread = request["params"]["threadId"].as_str().unwrap();
        assert_ne!(thread, "root");
        send(
            server,
            json!({"id":id,"result":{"thread":{"id":thread,"parentThreadId":parent}}}),
        )
        .await;
    }
}

async fn assert_no_outgoing(server: &mut BufReader<tokio::io::DuplexStream>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(50), next(server))
            .await
            .is_err(),
        "file observations must not emit a root turn, control RPC, or wait response"
    );
}

#[tokio::test]
async fn changed_file_snapshots_advance_progress_once_without_counting_bytes() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;

    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"file-1","type":"fileChange","status":"inProgress","changes":[change("a.rs","old")]}}}),
    )
    .await;
    let started_tool = file_snapshot(&client, ActivityScope::Tool);
    let started_root = file_snapshot(&client, ActivityScope::Turn);

    let changed = json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1","changes":[change("a.rs","old"),change("b.rs","added")]}});
    observation_event(&mut client, &mut server, changed.clone()).await;
    let after_change = file_snapshot(&client, ActivityScope::Tool);
    let after_change_root = file_snapshot(&client, ActivityScope::Turn);
    assert_eq!(after_change.progress_seq, started_tool.progress_seq + 1);
    assert_eq!(
        after_change_root.progress_seq,
        started_root.progress_seq + 1
    );
    assert_eq!(after_change.output_bytes, started_tool.output_bytes);
    assert_eq!(
        after_change.last_evidence.as_ref().unwrap().kind,
        EvidenceKind::Output
    );
    assert_eq!(after_change.last_evidence.as_ref().unwrap().output_bytes, 0);

    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"file-1","type":"fileChange","status":"inProgress","changes":[change("a.rs","old")]}}}),
    )
    .await;
    observation_event(&mut client, &mut server, changed.clone()).await;
    assert_eq!(
        file_snapshot(&client, ActivityScope::Tool).progress_seq,
        after_change.progress_seq,
        "a repeated item/started must not roll the baseline back"
    );

    observation_event(&mut client, &mut server, changed.clone()).await;
    assert_eq!(
        file_snapshot(&client, ActivityScope::Tool).progress_seq,
        after_change.progress_seq,
        "an identical full snapshot is not new progress"
    );

    let removed = json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1","changes":[change("a.rs","old")]}});
    observation_event(&mut client, &mut server, removed).await;
    assert_eq!(
        file_snapshot(&client, ActivityScope::Tool).progress_seq,
        after_change.progress_seq + 1,
        "removing a previously present change updates the complete snapshot"
    );
    let after_removal = file_snapshot(&client, ActivityScope::Tool).progress_seq;
    let empty = json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1","changes":[]}});
    observation_event(&mut client, &mut server, empty.clone()).await;
    assert_eq!(
        file_snapshot(&client, ActivityScope::Tool).progress_seq,
        after_removal + 1,
        "an empty snapshot differs from a snapshot containing changes"
    );
    observation_event(&mut client, &mut server, empty).await;
    assert_eq!(
        file_snapshot(&client, ActivityScope::Tool).progress_seq,
        after_removal + 1,
        "repeating an empty snapshot is not new progress"
    );
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn missing_start_snapshot_does_not_block_first_nonempty_patch_progress() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"file-1","type":"fileChange","status":"inProgress"}}}),
    )
    .await;
    let before = file_snapshot(&client, ActivityScope::Tool);
    let first_patch = json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1","changes":[change("a.rs","added")]}});
    observation_event(&mut client, &mut server, first_patch).await;
    assert_eq!(
        file_snapshot(&client, ActivityScope::Tool).progress_seq,
        before.progress_seq + 1
    );
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn delayed_start_cannot_rewind_a_patch_snapshot_seeded_by_file_output() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/fileChange/outputDelta","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1","delta":"PRIVATE_DELTA"}}),
    )
    .await;
    let current = json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1","changes":[change("PRIVATE_PATH.rs","PRIVATE_CURRENT_PATCH")]}});
    observation_event(&mut client, &mut server, current.clone()).await;
    observation_event(
        &mut client,
        &mut server,
        json!({"id":"file-approval","method":"item/fileChange/requestApproval","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1"}}),
    )
    .await;
    let latest_preview = client.snapshots.borrow().requests[0]
        .details
        .file_preview
        .clone()
        .unwrap();
    let after_patch = file_snapshot(&client, ActivityScope::Tool).progress_seq;

    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"file-1","type":"fileChange","status":"inProgress","changes":[change("PRIVATE_PATH.rs","PRIVATE_OLD_PATCH")]}}}),
    )
    .await;
    let after_delayed_start = file_snapshot(&client, ActivityScope::Tool).progress_seq;
    assert!(
        after_delayed_start > after_patch,
        "the delayed start itself remains valid tool evidence"
    );
    assert_eq!(
        client.snapshots.borrow().requests[0].details.file_preview,
        Some(latest_preview.clone()),
        "a delayed start cannot overwrite a newer approval preview"
    );
    observation_event(&mut client, &mut server, current).await;
    assert_eq!(
        file_snapshot(&client, ActivityScope::Tool).progress_seq,
        after_delayed_start,
        "the delayed old start did not rewind the accepted patch fingerprint"
    );
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn late_terminal_and_stale_file_patches_do_not_advance_progress() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"file-1","type":"fileChange","status":"inProgress","changes":[]}}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn","item":{"id":"file-1","type":"fileChange","status":"completed","changes":[]}}}),
    )
    .await;
    let terminal = file_snapshot(&client, ActivityScope::Tool);
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"root","turnId":"root-turn","itemId":"file-1","changes":[change("late.rs","late")]}}),
    )
    .await;
    assert_eq!(
        file_snapshot(&client, ActivityScope::Tool).progress_seq,
        terminal.progress_seq
    );

    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"root","turnId":"old-turn","itemId":"file-1","changes":[change("stale.rs","stale")]}}),
    )
    .await;
    assert_eq!(
        file_snapshot(&client, ActivityScope::Tool).progress_seq,
        terminal.progress_seq
    );
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn gated_child_file_progress_is_scoped_bounded_and_does_not_resume_the_root() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;
    child(&mut server, "a", "a-1").await;
    activity(&mut server, "item/started", "spawn-a", "started").await;
    activity(&mut server, "item/completed", "spawn-a", "started").await;
    wait_call(&mut server, "wait-1", vec!["a"]).await;
    phase(&mut client, SessionPhase::GatePending).await;
    drain_thread_reads(&mut server, "root").await;

    let before = client.snapshots.borrow().clone();
    let gate = before.gate.as_ref().unwrap().clone();
    assert!(gate.pending);
    assert_eq!(gate.targets.len(), 1);
    assert_eq!(gate.targets[0].id, "a");
    assert_eq!(gate.targets[0].turn_id.as_deref(), Some("a-1"));
    assert_eq!(gate.targets[0].generation, 1);
    let child_task = before
        .scheduler
        .tasks
        .iter()
        .find(|task| {
            task.kind == TaskKind::NativeChild
                && task.external.as_ref().is_some_and(|turn| {
                    turn.thread_id == "a" && turn.turn_id == "a-1" && turn.generation == 1
                })
        })
        .unwrap();
    assert_eq!(child_task.attempt, 1);
    let root_task = before
        .scheduler
        .tasks
        .iter()
        .find(|task| task.kind == TaskKind::RootTurn)
        .unwrap();
    assert!(root_task.wait_targets.contains(&TaskAttempt {
        task: child_task.id,
        attempt: child_task.attempt,
    }));
    let child_turn = observed(&client, "a", ActivityScope::Turn, None);
    assert_eq!(child_turn.identity.task_id, Some(child_task.id));
    assert_eq!(child_turn.identity.attempt_id, Some(1));
    assert_eq!(child_turn.identity.generation, Some(1));
    let root_turn = observed(&client, "root", ActivityScope::Turn, None);
    let root_requests = before
        .requests
        .iter()
        .filter(|request| request.thread_id == "root")
        .count();

    let prefix = "PRIVATE_PREFIX".repeat(3_000);
    let old_diff = format!("{prefix}PRIVATE_OLD_SUFFIX");
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"a","turnId":"a-1","item":{"id":"child-file","type":"fileChange","status":"inProgress","changes":[change("PRIVATE_PATH.rs",&old_diff)]}}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"id":"approval-before-cache-eviction","method":"item/fileChange/requestApproval","params":{"threadId":"a","turnId":"a-1","itemId":"child-file"}}),
    )
    .await;
    let before_patch_tool = observed(&client, "a", ActivityScope::Tool, Some("child-file"));
    let before_patch_turn = observed(&client, "a", ActivityScope::Turn, None);
    let visible_preview = client.snapshots.borrow().requests[0]
        .details
        .file_preview
        .clone()
        .unwrap();
    assert!(visible_preview.truncated);

    let new_diff = format!("{prefix}PRIVATE_NEW_SUFFIX");
    let changed = json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"a","turnId":"a-1","itemId":"child-file","changes":[change("PRIVATE_PATH.rs",&new_diff)]}});
    observation_event(&mut client, &mut server, changed.clone()).await;
    let after_patch_tool = observed(&client, "a", ActivityScope::Tool, Some("child-file"));
    let after_patch_turn = observed(&client, "a", ActivityScope::Turn, None);
    let latest_preview = client.snapshots.borrow().requests[0]
        .details
        .file_preview
        .clone()
        .unwrap();
    assert_eq!(
        after_patch_tool.progress_seq,
        before_patch_tool.progress_seq + 1
    );
    assert_eq!(
        after_patch_turn.progress_seq,
        before_patch_turn.progress_seq + 1
    );
    assert_eq!(
        after_patch_tool.output_bytes,
        before_patch_tool.output_bytes
    );
    assert_eq!(latest_preview.text, visible_preview.text);
    assert!(latest_preview.truncated);
    let after_patch = client.snapshots.borrow().clone();
    assert_eq!(after_patch.phase, SessionPhase::GatePending);
    assert_eq!(after_patch.gate.as_ref(), Some(&gate));
    assert_eq!(after_patch.root_start_requests, before.root_start_requests);
    assert_eq!(
        after_patch
            .requests
            .iter()
            .filter(|request| request.thread_id == "root")
            .count(),
        root_requests
    );
    assert_eq!(
        observed(&client, "root", ActivityScope::Turn, None).progress_seq,
        root_turn.progress_seq
    );
    let private_storage = format!(
        "{}{:?}{}",
        serde_json::to_string(&after_patch.observation).unwrap(),
        after_patch.timeline,
        serde_json::to_string(&StoredSnapshot::capture(&client.snapshots.borrow())).unwrap(),
    );
    for private in [
        "PRIVATE_DIFF",
        "PRIVATE_PATH",
        "PRIVATE_PREFIX",
        "PRIVATE_OLD_SUFFIX",
        "PRIVATE_NEW_SUFFIX",
    ] {
        assert!(!private_storage.contains(private));
    }
    assert_no_outgoing(&mut server).await;

    // Fill the bounded preview cache past its 64-item cap while Core retains
    // the old activity fingerprint for duplicate suppression.
    for index in 1..=64 {
        let item = format!("cache-file-{index}");
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"item/started","params":{"threadId":"a","turnId":"a-1","item":{"id":item,"type":"fileChange","status":"inProgress","changes":[]}}}),
        )
        .await;
    }
    assert_no_outgoing(&mut server).await;
    let before_replay = observed(&client, "a", ActivityScope::Turn, None).progress_seq;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"a","turnId":"a-1","item":{"id":"child-file","type":"fileChange","status":"inProgress","changes":[change("PRIVATE_PATH.rs",&old_diff)]}}}),
    )
    .await;
    assert_eq!(
        observed(&client, "a", ActivityScope::Turn, None).progress_seq,
        before_replay,
        "an evicted item's repeated old start is not accepted again"
    );
    assert_eq!(
        client.snapshots.borrow().requests[0].details.file_preview,
        Some(latest_preview.clone()),
        "the pending approval keeps its newer preview after an evicted duplicate start"
    );
    observation_event(
        &mut client,
        &mut server,
        json!({"id":"approval-after-eviction","method":"item/fileChange/requestApproval","params":{"threadId":"a","turnId":"a-1","itemId":"child-file"}}),
    )
    .await;
    assert!(client.snapshots.borrow().requests[1]
        .details
        .file_preview
        .is_none());
    observation_event(&mut client, &mut server, changed.clone()).await;
    assert_eq!(
        observed(&client, "a", ActivityScope::Tool, Some("child-file")).progress_seq,
        after_patch_tool.progress_seq,
        "a repeated patch after preview eviction restores context without adding progress"
    );
    assert_eq!(
        client.snapshots.borrow().requests[1]
            .details
            .file_preview
            .as_ref()
            .unwrap()
            .text,
        latest_preview.text
    );

    // Unknown item, non-file tool ID, malformed patch, and foreign thread
    // inputs cannot advance the active child file tool.
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"a","turnId":"a-1","item":{"id":"unknown-file","type":"futureFileTool"}}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"a","turnId":"a-1","item":{"id":"dynamic-file","type":"dynamicToolCall","status":"inProgress","arguments":{}}}}),
    )
    .await;
    let before_invalid = observed(&client, "a", ActivityScope::Turn, None).progress_seq;
    for event in [
        json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"a","turnId":"a-1","itemId":"child-file","changes":[{"path":"bad.rs","kind":{"type":"update","move_path":42},"diff":"bad"}]}}),
        json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"a","turnId":"a-1","itemId":"unknown-file","changes":[change("future.rs","unknown")]}}),
        json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"a","turnId":"a-1","itemId":"dynamic-file","changes":[change("dynamic.rs","wrong category")]}}),
        json!({"method":"item/fileChange/patchUpdated","params":{"threadId":"foreign","turnId":"a-1","itemId":"child-file","changes":[change("foreign.rs","foreign")]}}),
    ] {
        observation_event(&mut client, &mut server, event).await;
    }
    assert_eq!(
        observed(&client, "a", ActivityScope::Turn, None).progress_seq,
        before_invalid
    );
    assert_eq!(
        observed(&client, "root", ActivityScope::Turn, None).progress_seq,
        root_turn.progress_seq
    );
    assert_eq!(client.snapshots.borrow().phase, SessionPhase::GatePending);
    assert_no_outgoing(&mut server).await;

    send(&mut server, json!({"method":"turn/completed","params":{"threadId":"a","turn":{"id":"a-1","status":"completed"}}})).await;
    assert_eq!(next(&mut server).await["id"], "wait-1");
    phase(&mut client, SessionPhase::Running).await;
    send(
        &mut server,
        json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"a-2"}}}),
    )
    .await;
    drain_thread_reads(&mut server, "root").await;
    let second_wait_generation = client
        .snapshots
        .borrow()
        .agents
        .iter()
        .find(|agent| agent.info.id == "a")
        .unwrap()
        .generation;
    assert_eq!(second_wait_generation, 2);
    wait_call(&mut server, "wait-2", vec!["a"]).await;
    phase(&mut client, SessionPhase::GatePending).await;
    assert_eq!(
        client.snapshots.borrow().gate.as_ref().unwrap().targets[0].generation,
        2
    );
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"a","turnId":"a-2","item":{"id":"new-file","type":"fileChange","status":"inProgress","changes":[]}}}),
    )
    .await;
    let new_file_before_old_patch = observed(&client, "a", ActivityScope::Tool, Some("new-file"));
    observation_event(&mut client, &mut server, changed).await;
    assert_eq!(
        observed(&client, "a", ActivityScope::Tool, Some("new-file")).progress_seq,
        new_file_before_old_patch.progress_seq,
        "a prior-turn patch cannot advance the new child generation"
    );
    assert_eq!(client.snapshots.borrow().phase, SessionPhase::GatePending);
    assert_no_outgoing(&mut server).await;

    server.get_mut().shutdown().await.unwrap();
    phase(&mut client, SessionPhase::Disconnected).await;
    let disconnected = client.snapshots.borrow().observation.clone();
    let report = client.join.await.unwrap();
    assert_eq!(report.final_phase, SessionPhase::Disconnected);
    assert_eq!(client.snapshots.borrow().observation, disconnected);
}
