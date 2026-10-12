//! Core-owned preview child and typed feedback. It cannot grant authorization.
use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::Command;
use crate::domain::{DomainSetup, DomainSnapshot, ProposalBinding};
use crate::protocol::Envelope;
use crate::transport::PipeTransport;

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feedback {
    pub request_id: String,
    pub binding: ProposalBinding,
    pub snapshot_id: String,
    pub selections: Vec<String>,
    pub text: String,
    pub object_ids: Vec<String>,
    pub instances: Vec<String>,
}

impl std::fmt::Debug for Feedback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewFeedback")
            .field("request_id", &self.request_id)
            .field("revision", &self.binding.revision)
            .field("objects", &self.object_ids.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Ready { connection_file: String },
    Feedback(Box<Feedback>),
    Closed,
}

pub fn feedback_text(feedback: &Feedback, snapshot: &DomainSnapshot) -> Result<String, String> {
    let proposal = snapshot.proposal.as_ref().ok_or("尚无当前方案")?;
    if feedback.binding != proposal.binding
        || !matches!(proposal.state.as_str(), "prepared" | "blocked")
        || feedback.request_id.is_empty()
        || feedback.request_id.len() > 80
        || !feedback
            .request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        || feedback.snapshot_id.len() != 64
        || !feedback.snapshot_id.bytes().all(|b| b.is_ascii_hexdigit())
        || feedback.text.trim().is_empty()
        || feedback.text.chars().count() > 4000
        || feedback.object_ids.is_empty()
        || feedback.object_ids.len() > 200
        || feedback.selections.is_empty()
        || feedback.selections.len() > 200
        || feedback.instances.len() > 200
    {
        return Err("Blender 反馈已过期或身份不匹配；请同步当前未应用方案".into());
    }
    let payload = json!({
        "user_action":"revise_preview_selection", "proposal":feedback.binding,
        "snapshot_id":feedback.snapshot_id, "selected_parts":feedback.selections,
        "instances":feedback.instances,
        "feedback":{"feedback_id":feedback.request_id,"action":"revise",
                    "text":feedback.text,"object_ids":feedback.object_ids},
        "instruction":"用户从 Blender 当前候选提交对象意见。只解释意见并用 prepare_change 整体替换未应用候选；保留未选择实例、稳定 ID 与 assumed 标签，parent_id 绑定所看 proposal。先核对来源和能力；不要批准或应用，等待 TUI 具体批准。"
    });
    Ok(format!(
        "Blender 所选对象反馈：{}\n{payload}",
        feedback.text
    ))
}

pub struct Bridge {
    task: JoinHandle<()>,
    receipts: mpsc::Sender<Value>,
}

impl Bridge {
    pub fn start(setup: DomainSetup, epoch: u64, commands: mpsc::WeakSender<Command>) -> Self {
        let (receipts, mut receipt_rx) = mpsc::channel(32);
        let task = tokio::spawn(async move {
            let mut command = crate::owned_process::Command::new(&setup.python);
            command
                .current_dir(&setup.repository)
                .args([
                    "-u",
                    "-m",
                    "blender_agent.integrations.preview_service",
                    "--context",
                ])
                .args([&setup.context_path]);
            let Ok(mut child) =
                crate::owned_process::spawn(command, crate::owned_process::Input::Pipe)
            else {
                send_event(&commands, epoch, Event::Closed).await;
                return;
            };
            let mut pipe = PipeTransport::with_limit(
                child.stdout.take().unwrap(),
                child.stdin.take().unwrap(),
                64 * 1024,
            );
            // Drain without projecting unsanitized diagnostics or tokens.
            let stderr = child.stderr.take().unwrap();
            let drain = tokio::spawn(async move {
                let mut reader = stderr;
                let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
            });
            let mut seen = BTreeSet::new();
            loop {
                tokio::select! {
                    message = pipe.recv() => {
                        let Ok(message) = message else { break; };
                        let value = message.params.unwrap_or(Value::Null);
                        let event = match message.method.as_deref() {
                            Some("preview/ready") if value["run_id"] == setup.run_id && value["context_sha256"] == setup.context_sha256 => {
                                let Some(path) = value["connection_file"].as_str().filter(|s| s.len() <= 4096) else { break; };
                                Event::Ready { connection_file:path.into() }
                            }
                            Some("preview/feedback") => {
                                let Ok(feedback) = serde_json::from_value::<Feedback>(value) else { break; };
                                if seen.len() >= 256 { break; }
                                if !seen.insert(feedback.request_id.clone()) { continue; }
                                Event::Feedback(Box::new(feedback))
                            }
                            _ => break,
                        };
                        if !send_event(&commands, epoch, event).await { break; }
                    }
                    receipt = receipt_rx.recv() => {
                        let Some(receipt) = receipt else { break; };
                        if pipe.send(Envelope::notification("preview/receipt", Some(receipt))).is_err() { break; }
                    }
                }
            }
            pipe.close_writer().await;
            let _ = child.kill().await;
            drain.abort();
            send_event(&commands, epoch, Event::Closed).await;
        });
        Self { task, receipts }
    }

    pub fn receipt(&self, request_id: &str, status: &str, detail: &str) {
        let _ = self
            .receipts
            .try_send(json!({"request_id":request_id,"status":status,"detail":detail}));
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        // Dropping the owned child kills its Windows Job tree, including sockets.
        self.task.abort();
    }
}

async fn send_event(commands: &mpsc::WeakSender<Command>, epoch: u64, event: Event) -> bool {
    let Some(commands) = commands.upgrade() else {
        return false;
    };
    commands
        .send(Command::PreviewEvent { epoch, event })
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ProposalFact;

    fn fixture() -> (Feedback, DomainSnapshot) {
        let binding = ProposalBinding {
            run_id: "run".into(),
            context_sha256: "ctx".into(),
            proposal_id: "proposal".into(),
            revision: 2,
            prepare_token: "token".into(),
            base_sha256: "base".into(),
            candidate_sha256: "candidate".into(),
        };
        let feedback = Feedback {
            request_id: "blender-selection".into(),
            binding: binding.clone(),
            snapshot_id: "a".repeat(64),
            selections: vec!["bed.1.mattress".into()],
            text: "移到右侧".into(),
            object_ids: vec!["bed.1.base".into(), "bed.1.mattress".into()],
            instances: vec!["bed.1".into()],
        };
        let snapshot = DomainSnapshot {
            enabled: true,
            proposal: Some(ProposalFact {
                binding,
                state: "prepared".into(),
                summary: "beds".into(),
                objects: vec![],
                assumptions: vec![],
                diagnostics: vec![],
                ready: true,
            }),
            ..Default::default()
        };
        (feedback, snapshot)
    }

    #[test]
    fn feedback_preserves_part_instance_and_version_without_grant() {
        let (feedback, snapshot) = fixture();
        let text = feedback_text(&feedback, &snapshot).unwrap();
        let payload: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(payload["proposal"]["revision"], 2);
        assert_eq!(payload["feedback"]["object_ids"][0], "bed.1.base");
        assert_eq!(payload["instances"][0], "bed.1");
        assert!(payload.get("authorization_id").is_none());
        assert!(!format!("{feedback:?}").contains("移到右侧"));
    }

    #[test]
    fn late_cross_context_applied_and_oversized_feedback_is_rejected() {
        let (mut feedback, mut snapshot) = fixture();
        feedback.binding.revision = 1;
        assert!(feedback_text(&feedback, &snapshot).is_err());
        feedback.binding.revision = 2;
        feedback.binding.run_id = "other".into();
        assert!(feedback_text(&feedback, &snapshot).is_err());
        feedback.binding.run_id = "run".into();
        snapshot.proposal.as_mut().unwrap().state = "applied".into();
        assert!(feedback_text(&feedback, &snapshot).is_err());
        snapshot.proposal.as_mut().unwrap().state = "prepared".into();
        feedback.text = "a".repeat(4001);
        assert!(feedback_text(&feedback, &snapshot).is_err());
    }
}
