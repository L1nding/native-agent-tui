//! Native protocol-v2 peer for feedback and stale-answer transport tests.
use std::io::{self, BufRead, Write};
fn emit(value: &str) { println!("{value}"); io::stdout().flush().unwrap(); }
fn main() {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    assert!(lines.next().unwrap().unwrap().contains("start_change"));
    emit(r#"{"version":2,"run_id":"fixture","seq":1,"kind":"started","target":"新增两张床"}"#);
    emit(r#"{"version":2,"run_id":"fixture","seq":2,"kind":"proposal_prepared","revision":1,"summary":"两张床","state":"prepared"}"#);
    emit(r#"{"version":2,"run_id":"fixture","seq":3,"kind":"feedback_requested","request_id":"q1","title":"方案 v1","lines":["墙体受保护"],"proposal_id":"p1","proposal_revision":1,"base_scene_hash":"base","prepare_token":"t1","can_approve":true}"#);
    let revision = lines.next().unwrap().unwrap();
    assert!(revision.contains("revise") && revision.contains("移到右侧并保留墙体"));
    assert!(revision.contains("p1") && revision.contains("t1") && revision.contains("base"));
    emit(r#"{"version":2,"run_id":"fixture","seq":4,"kind":"feedback_received","request_id":"q1","action":"revise"}"#);
    emit(r#"{"version":2,"run_id":"fixture","seq":5,"kind":"proposal_superseded","proposal_id":"p1"}"#);
    emit(r#"{"version":2,"run_id":"fixture","seq":6,"kind":"proposal_prepared","revision":2,"summary":"两张床在右侧，墙体未变","state":"prepared"}"#);
    emit(r#"{"version":2,"run_id":"fixture","seq":7,"kind":"feedback_requested","request_id":"q2","title":"方案 v2","lines":["新版本需确认"],"proposal_id":"p2","proposal_revision":2,"base_scene_hash":"base","prepare_token":"t2","can_approve":true}"#);
    let approval = lines.next().unwrap().unwrap();
    assert!(approval.contains("approve") && approval.contains("p2") && approval.contains("t2"));
    emit(r#"{"version":2,"run_id":"fixture","seq":8,"kind":"feedback_received","request_id":"q2","action":"approve"}"#);
    emit(r#"{"version":2,"run_id":"fixture","seq":9,"kind":"change_applied","scene_sha256":"new"}"#);
    emit(r#"{"version":2,"run_id":"fixture","seq":10,"kind":"finished","outcome":"completed","message":"已构建"}"#);
}
