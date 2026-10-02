use super::*;
use crate::journal::{tests::Fixture, Journal, StoredSnapshot};
use crate::observation::Observer;
use crate::state::{CoreSnapshot, SessionPhase};
use std::sync::Mutex;

fn snapshot(session: &str, version: u64) -> StoredSnapshot {
    let now = tokio::time::Instant::now();
    let observer = Observer::new_at(session.into(), Default::default(), now, Some(1000));
    StoredSnapshot::capture(&CoreSnapshot {
        phase: SessionPhase::Running,
        observation: observer.snapshot_at(version, now),
        ..Default::default()
    })
}

#[derive(Clone)]
struct Buffer(Arc<Mutex<Vec<u8>>>);
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn live_jsonl_tails_committed_records_and_flushes_a_final_snapshot_after_close() {
    let fixture = Fixture::new();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut journal = Journal::open(&fixture.settings(), cwd, snapshot("live", 0)).unwrap();
    let bytes = Buffer(Arc::new(Mutex::new(Vec::new())));
    let mut output = LiveOutput::start(
        &fixture.settings(),
        cwd,
        "live",
        bytes.clone(),
        Duration::from_secs(5),
    )
    .unwrap();
    output.ready().await.unwrap();
    journal.append(snapshot("live", 2)).unwrap();
    let mut terminal = snapshot("live", 3);
    terminal.close(SessionPhase::Completed, true);
    journal.finish(terminal).await.unwrap();
    output.finish().await.unwrap();
    let text = String::from_utf8(bytes.0.lock().unwrap().clone()).unwrap();
    let records: Vec<crate::journal::Record> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records.iter().map(|r| r.event_seq).collect::<Vec<_>>(),
        [0, 1, 2, 2]
    );
    assert_eq!(
        records.iter().map(|r| r.kind).collect::<Vec<_>>(),
        [
            RecordKind::Snapshot,
            RecordKind::State,
            RecordKind::State,
            RecordKind::Snapshot
        ]
    );
    assert!(records.iter().all(|r| r.historical.is_none()));
    assert_eq!(
        records.last().unwrap().state().unwrap().execution_result,
        Some(SessionPhase::Completed)
    );
    assert_eq!(
        records.last().unwrap().state().unwrap().cleanup_confirmed,
        Some(true)
    );
}

#[tokio::test]
async fn live_jsonl_failure_is_visible_without_blocking_the_journal_owner() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let journal = Journal::open(&fixture.settings(), cwd, snapshot("broken", 0)).unwrap();
    let mut output = LiveOutput::start(
        &fixture.settings(),
        cwd,
        "broken",
        Broken,
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(output.ready().await, Err(OutputError::Write));
    assert_eq!(output.finish().await, Err(OutputError::Write));
    let mut terminal = snapshot("broken", 1);
    terminal.close(SessionPhase::Unknown, true);
    journal.finish(terminal).await.unwrap();
}

#[tokio::test]
async fn live_jsonl_missing_terminal_is_explicit_after_the_execution_owner_exits() {
    let fixture = Fixture::new();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let journal = Journal::open(&fixture.settings(), cwd, snapshot("incomplete", 0)).unwrap();
    let bytes = Buffer(Arc::new(Mutex::new(Vec::new())));
    let mut output = LiveOutput::start(
        &fixture.settings(),
        cwd,
        "incomplete",
        bytes,
        Duration::from_secs(5),
    )
    .unwrap();
    output.ready().await.unwrap();
    drop(journal);
    assert_eq!(output.finish().await, Err(OutputError::Incomplete));
}

#[tokio::test]
async fn live_jsonl_slow_reader_recaptures_the_final_prefix_after_core_has_closed() {
    struct HeldBuffer {
        buffer: Buffer,
        entered: Arc<AtomicBool>,
        gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }
    impl Write for HeldBuffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.entered.store(true, Ordering::Release);
            let (lock, condition) = &*self.gate;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = condition.wait(released).unwrap();
            }
            self.buffer.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut journal = Journal::open(&fixture.settings(), cwd, snapshot("slow", 0)).unwrap();
    let bytes = Buffer(Arc::new(Mutex::new(Vec::new())));
    let entered = Arc::new(AtomicBool::new(false));
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let mut output = LiveOutput::start(
        &fixture.settings(),
        cwd,
        "slow",
        HeldBuffer {
            buffer: bytes.clone(),
            entered: entered.clone(),
            gate: gate.clone(),
        },
        Duration::from_secs(5),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !entered.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    journal.append(snapshot("slow", 1)).unwrap();
    let mut final_state = snapshot("slow", 2);
    final_state.close(crate::state::SessionPhase::Completed, true);
    journal.finish(final_state).await.unwrap();
    let producer_closed = output.producer_closed.clone();
    let finishing = tokio::spawn(async move { output.finish().await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !producer_closed.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    finishing.await.unwrap().unwrap();
    let text = String::from_utf8(bytes.0.lock().unwrap().clone()).unwrap();
    let records: Vec<crate::journal::Record> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records
            .iter()
            .map(|record| record.event_seq)
            .collect::<Vec<_>>(),
        [0, 1, 2, 2]
    );
    assert_eq!(
        records.last().unwrap().state().unwrap().execution_result,
        Some(SessionPhase::Completed)
    );
}
