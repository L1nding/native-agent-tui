//! Core-owned preview child and typed feedback. It cannot grant authorization.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::Command;
use crate::domain::{DomainSetup, DomainSnapshot, ProposalBinding};
use crate::protocol::Envelope;
use crate::transport::PipeTransport;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedObject {
    pub part_id: String,
    pub entity_id: String,
    pub instance_id: String,
    pub category: String,
    pub floor_id: String,
    pub source_ids: Vec<String>,
    pub feedback_targets: Vec<String>,
    pub layer: String,
    pub diff: String,
    pub center: [String; 3],
    pub dimensions: [String; 3],
    pub diagnostics: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub client_id: String,
    pub sequence: u64,
    pub binding: ProposalBinding,
    pub snapshot_id: String,
    pub active_part_id: Option<String>,
    pub active_layer: Option<String>,
    pub objects: Vec<SelectedObject>,
}

impl std::fmt::Debug for Selection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewSelection")
            .field("revision", &self.binding.revision)
            .field("sequence", &self.sequence)
            .field("objects", &self.objects.len())
            .finish()
    }
}

impl Selection {
    pub fn active(&self) -> Option<&SelectedObject> {
        self.objects
            .iter()
            .find(|o| {
                Some(&o.part_id) == self.active_part_id.as_ref()
                    && Some(&o.layer) == self.active_layer.as_ref()
            })
            .or(self.objects.first())
    }

    pub fn valid(&self, snapshot: &DomainSnapshot) -> bool {
        self.sequence > 0
            && self.objects.len() <= 200
            && self.snapshot_id.len() == 64
            && self.snapshot_id.bytes().all(|b| b.is_ascii_hexdigit())
            && snapshot.proposal.as_ref().is_none_or(|p| {
                self.binding.revision > p.binding.revision || self.binding == p.binding
            })
            && self.objects.iter().all(|o| {
                o.part_id.len() <= 200
                    && o.source_ids.len() <= 200
                    && o.diagnostics.len() <= 4
                    && o.feedback_targets.len() <= 200
                    && matches!(o.layer.as_str(), "baseline" | "candidate")
                    && o.center
                        .iter()
                        .chain(o.dimensions.iter())
                        .all(|s| s.parse::<f64>().is_ok_and(f64::is_finite))
            })
    }

    pub fn brief(&self) -> String {
        self.active()
            .map(|o| {
                format!(
                    "Blender 选中 {} · {} · {} · r{}",
                    o.instance_id, o.part_id, o.category, self.binding.revision
                )
            })
            .unwrap_or_else(|| "Blender 当前未选中构件".into())
    }

    pub fn details(&self) -> Vec<String> {
        let mut lines = vec![self.brief()];
        if let Some(o) = self.active() {
            lines.push(format!(
                "楼层 {} · {} · {} 个部件",
                o.floor_id,
                o.layer,
                self.objects.len()
            ));
            lines.push(format!(
                "中心 {} m · 尺寸 {} m",
                o.center.join(" / "),
                o.dimensions.join(" / ")
            ));
            lines.push(if o.source_ids.is_empty() {
                "来源：未关联实测来源，检查方案假设".into()
            } else {
                format!("来源：{}", o.source_ids.join(", "))
            });
            lines.extend(o.diagnostics.iter().cloned());
        }
        lines
    }
}

#[derive(Clone)]
pub struct WorkTarget {
    pub binding: ProposalBinding,
    pub snapshot_id: Option<String>,
    pub parts: Vec<String>,
    pub instances: Vec<String>,
}

impl From<&Selection> for WorkTarget {
    fn from(selection: &Selection) -> Self {
        Self {
            binding: selection.binding.clone(),
            snapshot_id: Some(selection.snapshot_id.clone()),
            parts: selection
                .objects
                .iter()
                .flat_map(|o| o.feedback_targets.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            instances: selection
                .objects
                .iter()
                .map(|o| o.instance_id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }
}

pub fn prepare_target(
    snapshot: &DomainSnapshot,
    details: &crate::tool_details::ToolDetailsSnapshot,
    thread: &str,
    turn: &str,
) -> Option<WorkTarget> {
    let proposal = snapshot.proposal.as_ref()?;
    let running = details.entries.iter().find(|d| {
        d.authoritative
            && !d.clipped
            && d.lifecycle == crate::tool_details::ToolLifecycle::Running
            && d.category == crate::protocol::ToolCategory::Mcp
            && d.name.as_deref() == Some("modeling_domain/prepare_change")
            && d.locator.identity.thread_id.as_deref() == Some(thread)
            && d.locator.identity.turn_id.as_deref() == Some(turn)
    })?;
    let value: Value = serde_json::from_str(running.parameters.as_deref()?).ok()?;
    let components = value["plan"]["components"].as_array()?;
    if components.len() > 200 {
        return None;
    }
    let instances = components
        .iter()
        .map(|c| c["component_id"].as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()?;
    Some(WorkTarget {
        binding: proposal.binding.clone(),
        snapshot_id: None,
        parts: vec![],
        instances,
    })
}

pub fn selected_input(
    text: &str,
    selection: &Selection,
    snapshot: &DomainSnapshot,
) -> Option<String> {
    let proposal = snapshot.proposal.as_ref()?;
    if !selection.valid(snapshot)
        || proposal.binding != selection.binding
        || !matches!(proposal.state.as_str(), "prepared" | "blocked")
        || snapshot.authorizing
        || selection.objects.is_empty()
    {
        return None;
    }
    let target = WorkTarget::from(selection);
    if target.parts.len() > 200 {
        return None;
    }
    let feedback_id = format!(
        "tui-{:x}",
        Sha256::digest(format!(
            "{}/{}/{}",
            selection.binding.proposal_id, selection.sequence, text
        ))
    );
    let payload = json!({"user_action":"selected_preview_context", "proposal":selection.binding,
        "snapshot_id":selection.snapshot_id, "selected_parts":selection.objects.iter().map(|o| &o.part_id).collect::<Vec<_>>(),
        "instances":target.instances,
        "feedback":{"feedback_id":feedback_id,"action":"revise","text":text,"object_ids":target.parts},
        "instruction":"这是用户在 Blender 选中的当前对象上下文，仅按用户自然语言需求执行。需要修订时读取当前方案、以该 proposal_id 为 parent_id 整体替换未应用候选，并保持未选择实例和稳定 ID；新增部件可定义选中实例专用模板。不要批准或应用，等待 TUI 具体批准。"});
    Some(format!("{text}\n{payload}"))
}

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
    Selection(Box<Selection>),
    SelectionClosed { client_id: String },
    Fact(Box<crate::domain::receipt::FactReceipt>),
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
    activity: tokio::sync::watch::Sender<Value>,
}

impl Bridge {
    pub fn start(setup: DomainSetup, epoch: u64, commands: mpsc::WeakSender<Command>) -> Self {
        let (receipts, mut receipt_rx) = mpsc::channel::<Value>(32);
        let (activity, mut activity_rx) = tokio::sync::watch::channel(Value::Null);
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
            let mut activity_sequence = 0u64;
            let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(1));
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
                            Some("preview/selection") => {
                                let Ok(selection) = serde_json::from_value::<Selection>(value) else { break; };
                                if selection.binding.run_id != setup.run_id || selection.binding.context_sha256 != setup.context_sha256 { break; }
                                Event::Selection(Box::new(selection))
                            }
                            Some("preview/selection_closed") => {
                                let Some(client_id) = value["client_id"].as_str() else { break; };
                                Event::SelectionClosed { client_id:client_id.into() }
                            }
                            Some("preview/fact") => {
                                let Ok(receipt) = serde_json::from_value::<crate::domain::receipt::FactReceipt>(value) else { break; };
                                if crate::domain::receipt::validate(&setup, &receipt).is_err() { break; }
                                Event::Fact(Box::new(receipt))
                            }
                            _ => break,
                        };
                        if !send_event(&commands, epoch, event).await { break; }
                    }
                    receipt = receipt_rx.recv() => {
                        let Some(receipt) = receipt else { break; };
                        if pipe.send(Envelope::notification(receipt["method"].as_str().unwrap_or("preview/receipt"), Some(receipt["params"].clone()))).is_err() { break; }
                    }
                    _ = heartbeat.tick() => {
                        let mut value = activity_rx.borrow_and_update().clone();
                        if !value.is_null() {
                            activity_sequence += 1;
                            value["sequence"] = json!(activity_sequence);
                            if pipe.send(Envelope::notification("preview/activity", Some(value))).is_err() { break; }
                        }
                    }
                    changed = activity_rx.changed() => {
                        if changed.is_err() { break; }
                        let mut value = activity_rx.borrow_and_update().clone();
                        if !value.is_null() {
                            activity_sequence += 1;
                            value["sequence"] = json!(activity_sequence);
                            if pipe.send(Envelope::notification("preview/activity", Some(value))).is_err() { break; }
                        }
                    }
                }
            }
            pipe.close_writer().await;
            let _ = child.kill().await;
            drain.abort();
            send_event(&commands, epoch, Event::Closed).await;
        });
        Self {
            task,
            receipts,
            activity,
        }
    }

    pub fn receipt(&self, request_id: &str, status: &str, detail: &str) {
        let _ = self
            .receipts
            .try_send(json!({"method":"preview/receipt", "params":{"request_id":request_id,"status":status,"detail":detail}}));
    }

    pub fn activity(
        &self,
        setup: &DomainSetup,
        phase: crate::state::SessionPhase,
        target: Option<&WorkTarget>,
        waiting: bool,
    ) {
        let target = target.filter(|t| t.parts.len() <= 200 && t.instances.len() <= 200);
        use crate::state::SessionPhase;
        let status = match phase {
            SessionPhase::StartingTurn | SessionPhase::Running | SessionPhase::GatePending => {
                if waiting {
                    "waiting"
                } else {
                    "processing"
                }
            }
            p if p.can_submit() => "idle",
            _ => "unavailable",
        };
        // The label is display text for the Blender panel; `status` stays the protocol code.
        let label = match status {
            "processing" => format!(
                "Agent 正在准备候选{}",
                target
                    .map(|t| format!("：{}", t.instances.join("、")))
                    .unwrap_or_default()
            ),
            "waiting" => "Agent：等待交互".into(),
            "idle" => "Agent：空闲".into(),
            _ => "Agent：不可用".into(),
        };
        let value = json!({"run_id":setup.run_id, "context_sha256":setup.context_sha256,
            "status":status, "binding":target.map(|t| &t.binding),
            "snapshot_id":target.and_then(|t| t.snapshot_id.as_ref()),
            "parts":target.map(|t| t.parts.clone()).unwrap_or_default(),
            "instances":target.map(|t| t.instances.clone()).unwrap_or_default(), "label":label.chars().take(160).collect::<String>()});
        if *self.activity.borrow() != value {
            self.activity.send_replace(value);
        }
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

    pub(super) fn selected_fixture() -> (Selection, DomainSnapshot) {
        let (_, snapshot) = fixture();
        let selection = Selection {
            client_id: "gui-client".into(),
            sequence: 1,
            binding: snapshot.proposal.as_ref().unwrap().binding.clone(),
            snapshot_id: "a".repeat(64),
            active_part_id: Some("bed.1.mattress".into()),
            active_layer: Some("candidate".into()),
            objects: vec![SelectedObject {
                part_id: "bed.1.mattress".into(),
                entity_id: "bed.1.mattress".into(),
                instance_id: "bed.1".into(),
                category: "bed".into(),
                floor_id: "floor.ground".into(),
                source_ids: vec![],
                feedback_targets: vec!["bed.1.base".into(), "bed.1.mattress".into()],
                layer: "candidate".into(),
                diff: "added".into(),
                center: ["1.5".into(), "1.7".into(), "0.5".into()],
                dimensions: ["0.9".into(), "2".into(), "0.2".into()],
                diagnostics: vec![],
            }],
        };
        (selection, snapshot)
    }

    #[test]
    fn selected_context_attaches_identity_to_simple_input_without_authorizing() {
        let (selection, snapshot) = selected_fixture();
        assert!(selection.valid(&snapshot));
        assert!(selection
            .details()
            .join("\n")
            .contains("尺寸 0.9 / 2 / 0.2 m"));
        let text = selected_input("给这张床加床头板", &selection, &snapshot).unwrap();
        let payload: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(payload["user_action"], "selected_preview_context");
        assert_eq!(payload["instances"][0], "bed.1");
        assert_eq!(payload["feedback"]["text"], "给这张床加床头板");
        assert_eq!(
            payload["feedback"]["object_ids"].as_array().unwrap().len(),
            2
        );
        assert!(payload.get("authorization_id").is_none());
        assert!(!format!("{selection:?}").contains("bed.1"));
        let repeated = selected_input("给这张床加床头板", &selection, &snapshot).unwrap();
        assert_eq!(repeated, text);
        assert_ne!(
            selected_input("加四条腿", &selection, &snapshot).unwrap(),
            text
        );
    }

    #[test]
    fn stale_or_invalid_selection_cannot_be_attached_to_new_input() {
        let (mut selection, snapshot) = selected_fixture();
        selection.binding.revision = 1;
        assert!(!selection.valid(&snapshot));
        assert!(selected_input("加床头板", &selection, &snapshot).is_none());
        selection.binding.revision = 2;
        selection.objects[0].dimensions[0] = "NaN".into();
        assert!(!selection.valid(&snapshot));
        selection.objects.clear();
        assert!(selected_input("加床头板", &selection, &snapshot).is_none());
    }
}
