use super::*;

const PRIVATE_MARKERS: [&str; 5] = [
    "PRIVATE_COMMAND",
    "PRIVATE_CWD",
    "PRIVATE_DELTA_ONE",
    "PRIVATE_DELTA_TWO",
    "PRIVATE_AGGREGATE",
];

#[tokio::test]
async fn root_tool_details_follow_exact_identity_stay_volatile_and_retire_with_turn() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"shell-1","type":"commandExecution","status":"inProgress","command":"PRIVATE_COMMAND","cwd":"PRIVATE_CWD"}}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/commandExecution/outputDelta","params":{"threadId":"root","turnId":"root-turn","itemId":"shell-1","delta":"PRIVATE_DELTA_ONE"}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/commandExecution/outputDelta","params":{"threadId":"root","turnId":"root-turn","itemId":"shell-1","delta":"PRIVATE_DELTA_TWO"}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn","item":{"id":"shell-1","type":"commandExecution","status":"completed","exitCode":0,"aggregatedOutput":"PRIVATE_AGGREGATE"}}}),
    )
    .await;

    let snapshot = client.snapshots.borrow().clone();
    let detail = snapshot.tool_details.entries.front().unwrap().as_ref();
    assert_eq!(detail.command.as_deref(), Some("PRIVATE_COMMAND"));
    assert_eq!(detail.cwd.as_deref(), Some("PRIVATE_CWD"));
    assert_eq!(detail.lifecycle, ToolLifecycle::Completed);
    assert_eq!(detail.output, "PRIVATE_AGGREGATE");
    assert!(!detail.output.contains("PRIVATE_DELTA_ONE"));
    let timeline_item = snapshot
        .timeline
        .entries
        .iter()
        .find(|entry| entry.item_id.as_deref() == Some("shell-1"))
        .unwrap();
    assert_eq!(detail.locator.session_id, snapshot.timeline.session_id);
    assert_eq!(detail.locator.identity, timeline_item.identity);
    assert_eq!(
        detail.locator.item_id,
        timeline_item.item_id.as_deref().unwrap()
    );

    for (thread, turn) in [("foreign-thread", "root-turn"), ("root", "stale-turn")] {
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"item/started","params":{"threadId":thread,"turnId":turn,"item":{"id":"spoofed","type":"commandExecution","status":"inProgress","command":"PRIVATE_COMMAND"}}}),
        )
        .await;
    }
    let current = client.snapshots.borrow().clone();
    assert_eq!(current.tool_details.entries.len(), 1);
    assert_eq!(current.tool_details.entries[0].locator, detail.locator);

    let stored = StoredSnapshot::capture(&current);
    let serialized = serde_json::to_string(&stored).unwrap();
    let observation = serde_json::to_string(&current.observation).unwrap();
    let timeline = format!("{:?}", current.timeline);
    let diagnostics = format!("{:?}", current.diagnostics);
    let debug = format!("{current:?}");
    for marker in PRIVATE_MARKERS {
        assert!(!serialized.contains(marker));
        assert!(!observation.contains(marker));
        assert!(!timeline.contains(marker));
        assert!(!diagnostics.contains(marker));
        assert!(!debug.contains(marker));
    }

    observation_event(
        &mut client,
        &mut server,
        json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}}),
    )
    .await;
    assert!(client.snapshots.borrow().tool_details.entries.is_empty());
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn child_tool_details_use_the_confirmed_child_turn_identity() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"thread/started","params":{"thread":{"id":"a","parentThreadId":"root","source":{"subAgent":{}}}}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"spawn-a","type":"subAgentActivity","agentThreadId":"a","agentPath":"/root/a","kind":"started"}}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"a-turn"}}}),
    )
    .await;
    let read = next(&mut server).await;
    assert_eq!(read["method"], "thread/read");
    send(
        &mut server,
        json!({"id":read["id"],"result":{"thread":{"id":"a","parentThreadId":"root"}}}),
    )
    .await;
    phase(&mut client, SessionPhase::Running).await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"a","turnId":"a-turn","item":{"id":"child-tool","type":"dynamicToolCall","status":"inProgress","arguments":{"input":"PRIVATE_CHILD_INPUT"}}}}),
    )
    .await;

    let snapshot = client.snapshots.borrow();
    let detail = snapshot
        .tool_details
        .entries
        .iter()
        .find(|detail| detail.locator.item_id == "child-tool")
        .unwrap();
    assert_eq!(detail.locator.identity.agent_id, "a");
    assert_eq!(detail.locator.identity.thread_id.as_deref(), Some("a"));
    assert_eq!(detail.locator.identity.turn_id.as_deref(), Some("a-turn"));
    assert_eq!(
        detail.parameters.as_deref(),
        Some(r#"{"input":"PRIVATE_CHILD_INPUT"}"#)
    );
    let timeline_item = snapshot
        .timeline
        .entries
        .iter()
        .find(|entry| entry.item_id.as_deref() == Some("child-tool"))
        .unwrap();
    assert_eq!(detail.locator.identity, timeline_item.identity);
    drop(snapshot);
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn child_turn_replacement_retires_tool_details_and_file_approval_preview() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"thread/started","params":{"thread":{"id":"a","parentThreadId":"root","source":{"subAgent":{}}}}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"spawn-a","type":"subAgentActivity","agentThreadId":"a","agentPath":"/root/a","kind":"started"}}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"a-turn"}}}),
    )
    .await;
    let read = next(&mut server).await;
    assert_eq!(read["method"], "thread/read");
    send(
        &mut server,
        json!({"id":read["id"],"result":{"thread":{"id":"a","parentThreadId":"root"}}}),
    )
    .await;
    phase(&mut client, SessionPhase::Running).await;

    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"a","turnId":"a-turn","item":{"id":"tool-a","type":"dynamicToolCall","status":"inProgress","arguments":{"input":"PRIVATE_OLD_INPUT"}}}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"a","turnId":"a-turn","item":{"id":"file-a","type":"fileChange","status":"inProgress","changes":[{"path":"PRIVATE_OLD_FILE","kind":{"type":"update","move_path":null},"diff":"PRIVATE_OLD_DIFF"}]}}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"id":"old-file-approval","method":"item/fileChange/requestApproval","params":{"threadId":"a","turnId":"a-turn","itemId":"file-a"}}),
    )
    .await;
    {
        let snapshot = client.snapshots.borrow();
        assert!(snapshot
            .tool_details
            .entries
            .iter()
            .any(|detail| detail.locator.item_id == "tool-a"));
        assert!(snapshot.requests[0]
            .details
            .file_preview
            .as_ref()
            .is_some_and(|preview| preview.text.contains("PRIVATE_OLD_DIFF")));
    }

    observation_event(
        &mut client,
        &mut server,
        json!({"method":"turn/started","params":{"threadId":"a","turn":{"id":"a-turn-2"}}}),
    )
    .await;
    {
        let snapshot = client.snapshots.borrow();
        assert!(!snapshot
            .tool_details
            .entries
            .iter()
            .any(|detail| detail.locator.item_id == "tool-a"));
        let old_tool_started = snapshot
            .timeline
            .entries
            .iter()
            .find(|entry| {
                entry.scope == crate::observation::ActivityScope::Tool
                    && entry.identity.turn_id.as_deref() == Some("a-turn")
                    && entry.evidence.kind == crate::observation::EvidenceKind::ToolStarted
                    && entry.evidence.item_id.as_deref() == Some("tool-a")
            })
            .unwrap();
        let old_tool_retired = snapshot
            .timeline
            .entries
            .iter()
            .find(|entry| {
                entry.activity_id == old_tool_started.activity_id
                    && entry.evidence.kind == crate::observation::EvidenceKind::ExecutionUnknown
            })
            .unwrap();
        assert_eq!(
            old_tool_retired.execution_state,
            crate::observation::ExecutionState::Unknown
        );
        assert!(!snapshot
            .requests
            .iter()
            .any(|request| request.id == RpcId::String("old-file-approval".into())));
    }

    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"a","turnId":"a-turn-2","item":{"id":"tool-current","type":"dynamicToolCall","status":"inProgress","arguments":{"input":"PRIVATE_CURRENT_INPUT"}}}}),
    )
    .await;
    {
        let snapshot = client.snapshots.borrow();
        let detail = snapshot
            .tool_details
            .entries
            .iter()
            .find(|detail| detail.locator.item_id == "tool-current")
            .unwrap();
        assert_eq!(detail.locator.identity.turn_id.as_deref(), Some("a-turn-2"));
        assert_eq!(detail.locator.identity.generation, Some(2));
        assert_eq!(detail.locator.identity.attempt_id, Some(2));
        assert!(detail.locator.identity.task_id.is_some());
    }

    observation_event(
        &mut client,
        &mut server,
        json!({"id":"new-file-approval","method":"item/fileChange/requestApproval","params":{"threadId":"a","turnId":"a-turn-2","itemId":"file-a"}}),
    )
    .await;
    let snapshot = client.snapshots.borrow();
    let request = snapshot
        .requests
        .iter()
        .find(|request| request.id == RpcId::String("new-file-approval".into()))
        .unwrap();
    assert!(request.details.file_preview.is_none());
    drop(snapshot);

    // 换轮后 Core 当前 turn 所有权必须拒绝旧轮通知。
    let accepted_before_stale = client
        .snapshots
        .borrow()
        .observation
        .accepted_evidence_count;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"a","turnId":"a-turn","item":{"id":"boundary-late","type":"dynamicToolCall","status":"inProgress","arguments":{"input":"PRIVATE_STALE_INPUT"}}}}),
    )
    .await;
    let snapshot = client.snapshots.borrow();
    assert_eq!(
        snapshot.observation.accepted_evidence_count,
        accepted_before_stale
    );
    assert!(!snapshot
        .tool_details
        .entries
        .iter()
        .any(|detail| detail.locator.item_id == "boundary-late"));
    drop(snapshot);
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn terminal_observer_items_cannot_reappear_after_detail_cache_eviction() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;
    let item = |id: &str, method: &str, status: &str, output: Option<&str>| {
        let mut item = json!({"id":id,"type":"commandExecution","status":status});
        if method == "started" {
            item["command"] = json!("echo fixture");
        }
        if let Some(output) = output {
            item["exitCode"] = json!(0);
            item["aggregatedOutput"] = json!(output);
        }
        item
    };
    let first = "terminal-first";
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":item(first,"started","inProgress",None)}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn","item":item(first,"completed","completed",Some("first result"))}}),
    )
    .await;

    for index in 0..crate::tool_details::ENTRY_LIMIT {
        let id = format!("filler-{index}");
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":item(&id,"started","inProgress",None)}}),
        )
        .await;
        observation_event(
            &mut client,
            &mut server,
            json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn","item":item(&id,"completed","completed",Some("filler result"))}}),
        )
        .await;
    }
    let before_repeat = client.snapshots.borrow().clone();
    assert_eq!(
        before_repeat.tool_details.entries.len(),
        crate::tool_details::ENTRY_LIMIT
    );
    assert!(before_repeat
        .tool_details
        .entries
        .iter()
        .all(|detail| detail.locator.item_id != first));
    let evidence_before_repeat = before_repeat.observation.accepted_evidence_count;

    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":item(first,"started","inProgress",None)}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn","item":item(first,"completed","completed",Some("replacement result"))}}),
    )
    .await;
    let after_repeat = client.snapshots.borrow();
    assert_eq!(
        after_repeat.tool_details.entries.len(),
        crate::tool_details::ENTRY_LIMIT
    );
    assert!(after_repeat
        .tool_details
        .entries
        .iter()
        .all(|detail| detail.locator.item_id != first));
    assert_eq!(
        after_repeat.observation.accepted_evidence_count,
        evidence_before_repeat
    );
    drop(after_repeat);
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn disconnect_marks_running_details_unknown_and_keeps_partial_output() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"shell-disconnect","type":"commandExecution","status":"inProgress","command":"PRIVATE_COMMAND"}}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/commandExecution/outputDelta","params":{"threadId":"root","turnId":"root-turn","itemId":"shell-disconnect","delta":"PRIVATE_PARTIAL_OUTPUT"}}),
    )
    .await;
    drop(server);
    client
        .snapshots
        .wait_for(|snapshot| snapshot.phase == SessionPhase::Disconnected)
        .await
        .unwrap();
    let snapshot = client.snapshots.borrow();
    let detail = snapshot
        .tool_details
        .entries
        .iter()
        .find(|detail| detail.locator.item_id == "shell-disconnect")
        .unwrap();
    assert_eq!(detail.lifecycle, ToolLifecycle::Unknown);
    assert_eq!(detail.output, "PRIVATE_PARTIAL_OUTPUT");
    assert!(!detail.output.is_empty());
    drop(snapshot);
    client.join.await.unwrap();
}

#[tokio::test]
async fn tool_calls_leave_one_summary_line_per_item_after_the_turn_retires() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"shell-1","type":"commandExecution","status":"inProgress","command":"cargo test\nsecond line","cwd":"PRIVATE_CWD"}}}),
    )
    .await;
    let running = client.snapshots.borrow().clone();
    let tools: Vec<_> = running
        .messages
        .iter()
        .filter(|message| message.role == "Tool")
        .collect();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].text, "▸ running · cargo test");

    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/completed","params":{"threadId":"root","turnId":"root-turn","item":{"id":"shell-1","type":"commandExecution","status":"completed","exitCode":3,"durationMs":1500,"aggregatedOutput":"PRIVATE_AGGREGATE"}}}),
    )
    .await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}}),
    )
    .await;
    let done = client.snapshots.borrow().clone();
    assert!(done.tool_details.entries.is_empty());
    let tools: Vec<_> = done
        .messages
        .iter()
        .filter(|message| message.role == "Tool")
        .collect();
    assert_eq!(tools.len(), 1);
    assert!(
        tools[0].text == "▸ failed · exit 3 · 1.5s · cargo test",
        "{}",
        tools[0].text
    );
    assert!(!tools[0].text.contains("PRIVATE_"));
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}

#[tokio::test]
async fn disconnect_marks_running_tool_lines_as_unknown() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"shell-1","type":"commandExecution","status":"inProgress","command":"Start-Sleep 60"}}}),
    )
    .await;
    drop(server);
    phase(&mut client, SessionPhase::Disconnected).await;
    let snapshot = client.snapshots.borrow().clone();
    let tools = snapshot
        .messages
        .iter()
        .find(|message| message.role == "Tool")
        .unwrap();
    assert_eq!(tools.text, "▸ ended, outcome unknown · Start-Sleep 60");
    client.join.await.unwrap();
}

#[tokio::test]
async fn dynamic_tool_summary_shows_the_tool_name_not_the_category() {
    let (mut client, mut server) = harness().await;
    running_root(&mut client, &mut server).await;
    observation_event(
        &mut client,
        &mut server,
        json!({"method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"wait-1","type":"dynamicToolCall","status":"inProgress","tool":"wait_for_subagent_completion","arguments":{"targets":[]}}}}),
    )
    .await;
    let snapshot = client.snapshots.borrow().clone();
    let tools = snapshot
        .messages
        .iter()
        .find(|message| message.role == "Tool")
        .unwrap();
    assert_eq!(tools.text, "▸ running · wait_for_subagent_completion");
    client.commands.send(Command::Quit).await.unwrap();
    client.join.await.unwrap();
}
