//! Serialize deterministic in-memory snapshots. This starts no execution owner.
use std::time::Duration;

use native_agent_tui::agents::{AgentInfo, AgentSnapshot};
use native_agent_tui::gate::WaitTarget;
use native_agent_tui::interactions::RequestView;
use native_agent_tui::journal::{Journal, JournalSettings, Replay, StoredSnapshot};
use native_agent_tui::observation::{ChildFact, ObservationFacts, Observer};
use native_agent_tui::protocol::{ObservedTool, RpcId, ToolCategory};
use native_agent_tui::scheduler::{ExternalTurn, RootTaskSpec, Scheduler, TaskState};
use native_agent_tui::state::{CoreSnapshot, GateSnapshot, SessionPhase};
use tokio::time::Instant;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let journal_root = args.next().map(|flag| {
        assert_eq!(flag, "--journal-dir");
        std::path::PathBuf::from(args.next().expect("--journal-dir PATH"))
    });
    assert!(args.next().is_none());
    let now = Instant::now();
    let mut scheduler = Scheduler::default();
    scheduler
        .enqueue(vec![RootTaskSpec::input("PRIVATE_PROMPT".into())])
        .unwrap();
    let attempt = scheduler.dispatch().unwrap().attempt;
    scheduler
        .started_root(
            attempt,
            ExternalTurn {
                thread_id: "parent".into(),
                turn_id: "turn-1".into(),
                generation: 1,
            },
        )
        .unwrap();
    let mut root = scheduler.task(attempt.task).unwrap().clone();
    root.state = TaskState::WaitingChildren;
    let children: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|id| AgentSnapshot {
            info: AgentInfo {
                id: id.into(),
                parent_id: "parent".into(),
                path: None,
                nickname: None,
                role: None,
                model: None,
                confirmed: true,
            },
            generation: 1,
            turn_id: Some(format!("{id}-1")),
            outcome: None,
            awaiting_turn: false,
            usage: Default::default(),
        })
        .collect();
    let gate = GateSnapshot {
        pending: true,
        targets: children
            .iter()
            .map(|agent| WaitTarget {
                id: agent.info.id.clone(),
                turn_id: agent.turn_id.clone(),
                generation: 1,
                outcome: None,
            })
            .collect(),
        root_starts_at_enter: 1,
        root_starts_at_release: None,
    };
    let requests = [RpcId::Number(7), RpcId::String("approval".into())].map(|id| {
        RequestView::decode(
            id,
            "item/commandExecution/requestApproval",
            &serde_json::json!({"threadId":"b","turnId":"b-1","command":"PRIVATE_COMMAND"}),
        )
        .unwrap()
    });
    let session = if journal_root.is_some() {
        format!(
            "fixture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    } else {
        "observation-fixture".into()
    };
    let mut observer = Observer::new_at(session.clone(), Default::default(), now, Some(1_000_000));
    observer
        .reconcile(
            ObservationFacts {
                phase: SessionPhase::GatePending,
                root_thread: Some("parent"),
                root_generation: 1,
                root_task: Some(&root),
                children: children
                    .iter()
                    .map(|agent| ChildFact { agent, task: None })
                    .collect(),
                requests: &requests,
                gate: Some(&gate),
            },
            now,
        )
        .unwrap();
    for id in ["tool-one", "tool-two"] {
        observer
            .tool(
                &ObservedTool {
                    thread_id: "a".into(),
                    turn_id: "a-1".into(),
                    item_id: id.into(),
                    category: ToolCategory::Shell,
                    outcome: None,
                },
                now,
            )
            .unwrap();
    }
    let initial = observer.snapshot_at(1, now);
    let silence = observer.snapshot_at(2, now + Duration::from_secs(130));
    observer
        .output(
            "a",
            "a-1",
            "message",
            8,
            false,
            now + Duration::from_secs(131),
        )
        .unwrap();
    observer
        .tool_output(
            "a",
            "a-1",
            "tool-one",
            8,
            ToolCategory::Shell,
            now + Duration::from_secs(131),
        )
        .unwrap();
    let resumed = observer.snapshot_at(3, now + Duration::from_secs(131));
    if let Some(root) = journal_root {
        let settings = JournalSettings {
            root: Some(root),
            ..Default::default()
        };
        let cwd = std::env::current_dir().unwrap();
        let mut core = CoreSnapshot {
            phase: SessionPhase::GatePending,
            root_start_requests: 1,
            scheduler: scheduler.snapshot(),
            observation: initial,
            ..Default::default()
        };
        let mut journal = Journal::open(&settings, &cwd, StoredSnapshot::capture(&core)).unwrap();
        core.observation = silence;
        journal.append(StoredSnapshot::capture(&core)).unwrap();
        core.observation = resumed;
        journal.append(StoredSnapshot::capture(&core)).unwrap();
        core.observation.snapshot_version = 4;
        core.phase = SessionPhase::Unknown;
        let mut final_state = StoredSnapshot::capture(&core);
        final_state.close(SessionPhase::Unknown, true);
        journal.finish(final_state).await.unwrap();
        eprintln!("session: {session}");
        Replay::open(&settings, &cwd, &session, 0)
            .unwrap()
            .write_jsonl(&mut std::io::stdout().lock())
            .unwrap();
        return;
    }
    println!(
        "{}",
        serde_json::json!({"fixture_version":1,"snapshots":[initial,silence,resumed]})
    );
}
