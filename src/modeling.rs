//! Domain workflow owner. Views consume snapshots; only this module owns the child and stdin.
use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::owned_process;
use crate::state::display_text;

pub mod feedback;
#[cfg(test)]
mod feedback_tests;
use feedback::{ComponentContext, ProposalBinding};

const FRAME_BYTES: usize = 64 * 1024;
const HISTORY: usize = 160;

#[derive(Clone, Debug)]
pub struct Options {
    pub repository: PathBuf,
    pub project: String,
    pub component: Option<ComponentContext>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Running,
    Waiting,
    Completed,
    Attention,
    Failed,
    Cancelled,
    Unknown,
}
impl Phase {
    pub fn terminal(&self) -> bool {
        !matches!(self, Self::Running | Self::Waiting)
    }
    pub fn label(&self) -> &str {
        match self {
            Self::Running => "运行中",
            Self::Waiting => "等待确认",
            Self::Completed => "已完成",
            Self::Attention => "已结束 · 有待处理问题",
            Self::Failed => "失败",
            Self::Cancelled => "已取消",
            Self::Unknown => "结果未确认",
        }
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct Question {
    pub request_id: String,
    pub title: String,
    pub lines: Vec<String>,
    pub proposal: Option<ProposalBinding>,
    pub can_approve: bool,
}
#[derive(Clone, Debug)]
pub struct Artifact {
    pub name: String,
    pub path: PathBuf,
}
#[derive(Clone, Debug)]
pub struct Stage {
    pub name: String,
    pub floor: String,
    pub state: String,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub run_id: Option<String>,
    pub phase: Phase,
    pub target: String,
    pub stages: Vec<Stage>,
    pub question: Option<Question>,
    pub artifacts: Vec<Artifact>,
    pub notes: VecDeque<String>,
    pub sequence: u64,
    pub protocol_version: u32,
    pub outcome: Option<String>,
}
impl Snapshot {
    fn new(target: String) -> Self {
        Self {
            run_id: None,
            phase: Phase::Running,
            target,
            stages: vec![],
            question: None,
            artifacts: vec![],
            notes: VecDeque::new(),
            sequence: 0,
            protocol_version: 1,
            outcome: None,
        }
    }
    fn note(&mut self, text: impl AsRef<str>) {
        if self.notes.len() == HISTORY {
            self.notes.pop_front();
        }
        self.notes.push_back(display_text(text.as_ref()));
    }
    fn apply(&mut self, frame: Frame) -> Result<(), String> {
        if frame.version != self.protocol_version
            || frame.seq != self.sequence + 1
            || self.outcome.is_some()
            || self.run_id.as_ref().is_some_and(|id| id != &frame.run_id)
            || frame.run_id.is_empty()
            || frame.run_id.len() > 128
        {
            return Err("工作流事件版本、顺序或运行标识无效".into());
        }
        self.run_id = Some(frame.run_id);
        self.sequence = frame.seq;
        match frame.event {
            Event::Started { target } => self.note(format!("开始：{target}")),
            Event::Note { message } => self.note(message),
            Event::Stage { name, floor, state } => {
                if !["running", "completed", "failed"].contains(&state.as_str()) {
                    return Err("无效的阶段状态".into());
                }
                if let Some(stage) = self
                    .stages
                    .iter_mut()
                    .find(|s| s.name == name && s.floor == floor)
                {
                    stage.state = state;
                } else {
                    if self.stages.len() >= 512 {
                        return Err("工作流阶段数量超过上限".into());
                    }
                    self.stages.push(Stage { name, floor, state });
                }
            }
            Event::Ai {
                name,
                state,
                message,
            } => self.note(format!("AI {name} · {state} {message}")),
            Event::Request {
                request_id,
                title,
                lines,
            } => {
                if self.question.is_some() {
                    return Err("收到重叠的确认请求".into());
                }
                self.question = Some(Question {
                    request_id,
                    title,
                    lines,
                    proposal: None,
                    can_approve: true,
                });
                self.phase = Phase::Waiting;
            }
            Event::FeedbackRequested {
                request_id,
                title,
                lines,
                binding,
                can_approve,
            } => {
                if self.question.is_some() || self.protocol_version != 2 {
                    return Err("收到重叠或不支持的方案反馈请求".into());
                }
                self.question = Some(Question {
                    request_id,
                    title,
                    lines,
                    proposal: Some(binding),
                    can_approve,
                });
                self.phase = Phase::Waiting;
            }
            Event::FeedbackReceived { request_id, action } => {
                if self.question.as_ref().map(|q| &q.request_id) != Some(&request_id) {
                    return Err("反馈回执不匹配".into());
                }
                self.question = None;
                self.phase = Phase::Running;
                self.note(format!("反馈已接收：{action}；等待修订或应用结果"));
            }
            Event::ProposalPrepared {
                revision,
                summary,
                state,
            } => {
                self.note(format!("方案 v{revision} · {state}：{summary}"));
            }
            Event::ProposalSuperseded {} => self.note("旧方案已被修订替代，旧批准失效"),
            Event::ChangeApplied { scene_sha256 } => {
                self.note(format!("已应用并构建：{scene_sha256}"))
            }
            Event::ChangeRolledBack {} => self.note("构建未完成，检查领域执行记录"),
            Event::Answered {
                request_id,
                accepted,
            } => {
                if self.question.as_ref().map(|q| &q.request_id) != Some(&request_id) {
                    return Err("确认响应不匹配".into());
                }
                self.question = None;
                self.phase = Phase::Running;
                self.note(if accepted { "已采纳" } else { "已拒绝" });
            }
            Event::Artifact { name, path } => {
                if !self.artifacts.iter().any(|a| a.path == Path::new(&path)) {
                    if self.artifacts.len() >= 1024 {
                        return Err("产物数量超过上限".into());
                    }
                    self.artifacts.push(Artifact {
                        name,
                        path: path.into(),
                    });
                }
            }
            Event::FloorResult { floor, state } => {
                if state == "failed" {
                    for stage in self
                        .stages
                        .iter_mut()
                        .filter(|s| s.floor == floor && s.state == "running")
                    {
                        stage.state = "failed".into();
                    }
                }
                self.note(format!("{floor}：{state}"));
            }
            Event::Finished { outcome, message } => {
                if !["completed", "attention", "failed", "cancelled"].contains(&outcome.as_str()) {
                    return Err("无效的完成状态".into());
                }
                self.outcome = Some(outcome);
                self.question = None;
                self.phase = Phase::Running;
                self.note(message);
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct Frame {
    version: u32,
    run_id: String,
    seq: u64,
    #[serde(flatten)]
    event: Event,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Event {
    Started {
        target: String,
    },
    Note {
        message: String,
    },
    Stage {
        name: String,
        floor: String,
        state: String,
    },
    Ai {
        name: String,
        state: String,
        #[serde(default)]
        message: String,
    },
    Request {
        request_id: String,
        title: String,
        lines: Vec<String>,
    },
    FeedbackRequested {
        request_id: String,
        title: String,
        lines: Vec<String>,
        #[serde(flatten)]
        binding: ProposalBinding,
        can_approve: bool,
    },
    FeedbackReceived {
        request_id: String,
        action: String,
    },
    ProposalPrepared {
        revision: u64,
        summary: String,
        state: String,
    },
    ProposalSuperseded {},
    ChangeApplied {
        scene_sha256: String,
    },
    ChangeRolledBack {},
    Answered {
        request_id: String,
        accepted: bool,
    },
    Artifact {
        name: String,
        path: String,
    },
    FloorResult {
        floor: String,
        state: String,
    },
    Finished {
        outcome: String,
        message: String,
    },
}
#[derive(Debug)]
pub enum Command {
    Revise {
        run_id: String,
        request_id: String,
        text: String,
    },
    Answer {
        run_id: String,
        request_id: String,
        accepted: bool,
    },
    Cancel,
}

#[derive(Debug, Clone)]
enum Operation {
    Component { context: ComponentContext },
    Model { verified: bool },
    RetryReview { source_run_id: Option<String> },
}

impl Operation {
    fn frame(&self, project: &str, target: &str) -> serde_json::Value {
        match self {
            Self::Component { context } => json!({"version":2,"command":"start_change",
                "target":target,"scene":context.scene,"scope":context.scope,"max_revisions":3}),
            Self::Model { verified } => json!({"version":1,"command":"start","target":target,
                "project":project,"policy":if *verified {"verified"} else {"manual"},"max_repairs":1}),
            Self::RetryReview { source_run_id } => json!({"version":1,"command":"retry_review",
                "project":project,"source_run_id":source_run_id}),
        }
    }
}

pub struct Handle {
    pub snapshots: watch::Receiver<Arc<Snapshot>>,
    pub commands: mpsc::Sender<Command>,
    pub join: JoinHandle<()>,
}
impl Handle {
    pub fn start(options: Options, target: String, verified: bool) -> Self {
        let operation = if let Some(context) = options.component.clone() {
            Operation::Component { context }
        } else if target.trim() == "复核上次" {
            Operation::RetryReview {
                source_run_id: None,
            }
        } else {
            Operation::Model { verified }
        };
        Self::launch(options, target, operation)
    }

    pub fn retry_review(options: Options, source_run_id: String) -> Self {
        Self::launch(
            options,
            "重试最终复核".into(),
            Operation::RetryReview {
                source_run_id: Some(source_run_id),
            },
        )
    }

    fn launch(options: Options, target: String, operation: Operation) -> Self {
        let snapshot = Snapshot::new(target);
        let (updates, snapshots) = watch::channel(Arc::new(snapshot.clone()));
        let (commands, receiver) = mpsc::channel(16);
        let join = tokio::spawn(async move {
            let mut state = snapshot;
            if let Err(error) = execute(options, &operation, &mut state, &updates, receiver).await {
                state.phase = Phase::Unknown;
                state.question = None;
                state.note(error.to_string());
            }
            updates.send_replace(Arc::new(state));
        });
        Self {
            snapshots,
            commands,
            join,
        }
    }
}

async fn line<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut result = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return if result.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::other("不完整的 JSONL 事件"))
            };
        }
        let end = buffer.iter().position(|b| *b == b'\n');
        let count = end.map_or(buffer.len(), |n| n + 1);
        if result.len() + count > FRAME_BYTES {
            return Err(io::Error::other("JSONL 事件过大"));
        }
        result.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(result));
        }
    }
}

async fn execute(
    options: Options,
    operation: &Operation,
    state: &mut Snapshot,
    updates: &watch::Sender<Arc<Snapshot>>,
    commands: mpsc::Receiver<Command>,
) -> io::Result<()> {
    let root = options.repository.canonicalize()?;
    let python = root.join(if cfg!(windows) {
        ".venv/Scripts/python.exe"
    } else {
        ".venv/bin/python"
    });
    let mut command = owned_process::Command::new(python);
    command.current_dir(&root).args([
        "-X",
        "utf8",
        "-u",
        "-m",
        "blender_agent.cli.main",
        if matches!(operation, Operation::Component { .. }) {
            "change-stdio"
        } else {
            "workflow-stdio"
        },
    ]);
    let child = owned_process::spawn(command, owned_process::Input::Pipe)?;
    run_child(child, &options.project, operation, state, updates, commands).await
}

async fn run_child(
    mut child: owned_process::Child,
    project: &str,
    operation: &Operation,
    state: &mut Snapshot,
    updates: &watch::Sender<Arc<Snapshot>>,
    mut commands: mpsc::Receiver<Command>,
) -> io::Result<()> {
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing stdin"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing stdout"))?;
    let mut errors = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing stderr"))?;
    let drain =
        tokio::spawn(async move { tokio::io::copy(&mut errors, &mut tokio::io::sink()).await });
    let (events_tx, mut events) = mpsc::channel(32);
    let reader = tokio::spawn(async move {
        let mut output = BufReader::new(output);
        loop {
            let frame = line(&mut output).await;
            let done = !matches!(frame, Ok(Some(_)));
            if events_tx.send(frame).await.is_err() || done {
                break;
            }
        }
    });
    let result = async {
        let start = operation.frame(project, &state.target);
        state.protocol_version = start["version"].as_u64().unwrap() as u32;
        input.write_all(format!("{start}\n").as_bytes()).await?;
        input.flush().await?;
        let mut answered = false;
        loop {
            tokio::select! {
                event = events.recv() => {
                    match event {
                        Some(Ok(Some(bytes))) => {
                            let frame: Frame = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                            state.apply(frame).map_err(io::Error::other)?;
                            if state.question.is_none() { answered = false; }
                            updates.send_replace(Arc::new(state.clone()));
                        }
                        Some(Err(error)) => return Err(error),
                        Some(Ok(None)) | None => break,
                    }
                }
                command = commands.recv() => match command {
                    Some(Command::Answer { run_id, request_id, accepted }) => {
                        if answered || state.run_id.as_ref() != Some(&run_id)
                            || state.question.as_ref().map(|q| &q.request_id) != Some(&request_id) {
                            state.note("忽略已失效或重复的回答");
                            updates.send_replace(Arc::new(state.clone()));
                            continue;
                        }
                        let question = state.question.as_ref().unwrap();
                        if accepted && !question.can_approve {
                            state.note("方案有阻塞项，请填写修改意见或拒绝");
                            updates.send_replace(Arc::new(state.clone()));
                            continue;
                        }
                        let answer = if let Some(binding) = &question.proposal {
                            binding.frame(&run_id, &request_id, state.sequence,
                                if accepted { "approve" } else { "reject" }, "")
                        } else { json!({"command":"answer","run_id":run_id,"request_id":request_id,"accepted":accepted}) };
                        tokio::time::timeout(Duration::from_secs(3), input.write_all(format!("{answer}\n").as_bytes())).await??;
                        input.flush().await?;
                        answered = true;
                    }
                    Some(Command::Revise { run_id, request_id, text }) => {
                        if answered || state.run_id.as_ref() != Some(&run_id)
                            || state.question.as_ref().map(|q| &q.request_id) != Some(&request_id)
                            || text.trim().is_empty() || text.chars().count() > 4000 {
                            state.note("忽略失效、重复或过长的修改意见");
                            updates.send_replace(Arc::new(state.clone()));
                            continue;
                        }
                        let Some(binding) = state.question.as_ref().and_then(|q| q.proposal.as_ref()) else {
                            state.note("此确认不支持文字修订");
                            updates.send_replace(Arc::new(state.clone()));
                            continue;
                        };
                        let answer = binding.frame(&run_id, &request_id, state.sequence, "revise", &text);
                        tokio::time::timeout(Duration::from_secs(3), input.write_all(format!("{answer}\n").as_bytes())).await??;
                        input.flush().await?;
                        answered = true;
                    }
                    Some(Command::Cancel) | None => {
                        child.kill().await?;
                        state.phase = Phase::Cancelled;
                        state.question = None;
                        state.note("任务已停止；已写入的检查点保留，重新运行前可查阅报告。");
                        return Ok(());
                    }
                }
            }
        }
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await??;
        state.phase = match (state.outcome.as_deref(), status.success()) {
            (Some("completed"), true) => Phase::Completed,
            (Some("attention"), true) => Phase::Attention,
            (Some("failed"), _) => Phase::Failed,
            (Some("cancelled"), _) => Phase::Cancelled,
            _ => Phase::Unknown,
        };
        if state.phase == Phase::Unknown { state.note("进程退出但未确认完整工作流结果"); }
        Ok(())
    }.await;
    // A process-tree failure remains explicit even if the protocol reported success.
    #[cfg(windows)]
    let cleanup = child.shutdown_tree().await;
    #[cfg(not(windows))]
    let cleanup = if result.is_err() {
        child.kill().await
    } else {
        Ok(())
    };
    reader.abort();
    drain.abort();
    let _ = reader.await;
    let _ = drain.await;
    result.and(cleanup)
}

/// Open only known data artifacts inside this modeler's output directory, never a shell command.
pub fn open_artifact(options: &Options, path: &Path) -> io::Result<()> {
    let path = path.canonicalize()?;
    let outputs = options.repository.join("outputs").canonicalize()?;
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !path.is_file()
        || !path.starts_with(outputs)
        || !["glb", "blend", "md", "json", "png", "jpg"].contains(&extension.as_str())
    {
        return Err(io::Error::other(
            "只能打开本项目 outputs 下的模型、图像或报告",
        ));
    }
    #[cfg(windows)]
    return open_windows_artifact(&path, &extension);
    #[cfg(not(windows))]
    {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener).arg(path).spawn()?;
        Ok(())
    }
}

#[cfg(windows)]
fn shell_path(path: &Path) -> std::ffi::OsString {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let value: Vec<u16> = path.as_os_str().encode_wide().collect();
    let prefix: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
    let disk: Vec<u16> = r"\\?\".encode_utf16().collect();
    if value.starts_with(&prefix) {
        let mut normal = vec![92, 92];
        normal.extend_from_slice(&value[prefix.len()..]);
        std::ffi::OsString::from_wide(&normal)
    } else if value.starts_with(&disk) {
        std::ffi::OsString::from_wide(&value[disk.len()..])
    } else {
        path.as_os_str().to_owned()
    }
}

#[cfg(windows)]
fn open_windows_artifact(path: &Path, extension: &str) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let document = shell_path(path);
    let mut parameters = None;
    let program = if ["md", "json"].contains(&extension) {
        let mut argument = vec![34];
        argument.extend(document.encode_wide());
        argument.extend([34, 0]);
        parameters = Some(argument);
        let system = std::env::var_os("SystemRoot")
            .ok_or_else(|| io::Error::other("未找到 Windows 系统目录"))?;
        PathBuf::from(system)
            .join("System32/notepad.exe")
            .into_os_string()
    } else {
        document
    };
    let file: Vec<u16> = program.encode_wide().chain(Some(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    // SAFETY: all UTF-16 strings remain alive and NUL-terminated through this call.
    // Only a validated data artifact or the fixed Windows text viewer is launched.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            parameters.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result <= 32 {
        return Err(io::Error::other(format!(
            "无法打开产物（Windows Shell 错误 {result}）；请选择已关联应用的模型格式"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_retry_start_frame_has_no_modeling_or_repair_authorization() {
        let frame = Operation::RetryReview {
            source_run_id: Some("previous".into()),
        }
        .frame("project", "八层");
        assert_eq!(frame["command"], "retry_review");
        assert_eq!(frame["source_run_id"], "previous");
        assert!(frame.get("policy").is_none());
        assert!(frame.get("max_repairs").is_none());
    }

    fn event(seq: u64, body: serde_json::Value) -> Frame {
        let mut value = body;
        value["version"] = json!(1);
        value["run_id"] = json!("run");
        value["seq"] = json!(seq);
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn pending_consent_is_resolved_only_by_matching_acknowledgement() {
        let mut state = Snapshot::new("建模八层".into());
        state
            .apply(event(
                1,
                json!({"kind":"request","request_id":"q1","title":"层高？","lines":["CAD 3.9 m"]}),
            ))
            .unwrap();
        assert_eq!(state.phase, Phase::Waiting);
        assert!(state
            .apply(event(
                2,
                json!({"kind":"answered","request_id":"old","accepted":true})
            ))
            .is_err());
        assert!(state.question.is_some());
    }
    #[test]
    fn protocol_completion_does_not_claim_process_exit() {
        let mut state = Snapshot::new("八层".into());
        state
            .apply(event(
                1,
                json!({"kind":"finished","outcome":"completed","message":"done"}),
            ))
            .unwrap();
        assert!(!state.phase.terminal());
        assert!(state
            .apply(event(2, json!({"kind":"note","message":"late"})))
            .is_err());
    }
    #[test]
    fn stale_run_or_sequence_is_rejected() {
        let mut state = Snapshot::new("八层".into());
        state
            .apply(event(1, json!({"kind":"started","target":"八层"})))
            .unwrap();
        assert!(state
            .apply(event(1, json!({"kind":"note","message":"duplicate"})))
            .is_err());
        let mut other = event(2, json!({"kind":"note","message":"other"}));
        other.run_id = "other".into();
        assert!(state.apply(other).is_err());
    }
    #[tokio::test]
    async fn frame_reader_bounds_unterminated_data() {
        let data = vec![b'x'; FRAME_BYTES + 1];
        assert!(line(&mut BufReader::new(data.as_slice())).await.is_err());
        assert!(line(&mut BufReader::new(b"{}".as_slice())).await.is_err());
        assert_eq!(
            line(&mut BufReader::new(b"{}\n".as_slice())).await.unwrap(),
            Some(b"{}\n".to_vec())
        );
    }
    fn fixture() -> &'static Path {
        static EXECUTABLE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        EXECUTABLE
            .get_or_init(|| {
                let root = Path::new(env!("CARGO_MANIFEST_DIR"));
                let output = root.join("target").join(format!(
                    "modeling-fixture-{}{}",
                    std::process::id(),
                    std::env::consts::EXE_SUFFIX
                ));
                let status = std::process::Command::new("rustc")
                    .arg(root.join("tests/fixtures/modeling_child.rs"))
                    .arg("--edition=2021")
                    .arg("-o")
                    .arg(&output)
                    .status()
                    .unwrap();
                assert!(status.success());
                output
            })
            .as_path()
    }

    pub(super) async fn wait_for_question(updates: &mut watch::Receiver<Arc<Snapshot>>) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while updates.borrow().question.is_none() {
                updates.changed().await.unwrap();
            }
        })
        .await
        .expect("native child must deliver its question");
    }

    #[tokio::test]
    async fn native_child_answer_is_acknowledged_and_exit_is_confirmed() {
        let child = owned_process::spawn(
            owned_process::Command::new(fixture()),
            owned_process::Input::Pipe,
        )
        .unwrap();
        let state = Snapshot::new("八层".into());
        let (updates, mut snapshots) = watch::channel(Arc::new(state.clone()));
        let (commands, receiver) = mpsc::channel(4);
        let task = tokio::spawn(async move {
            let mut state = state;
            run_child(
                child,
                "project",
                &Operation::Model { verified: false },
                &mut state,
                &updates,
                receiver,
            )
            .await
            .unwrap();
            state
        });
        wait_for_question(&mut snapshots).await;
        assert_eq!(snapshots.borrow().phase, Phase::Waiting);
        commands
            .send(Command::Answer {
                run_id: "fixture".into(),
                request_id: "q1".into(),
                accepted: true,
            })
            .await
            .unwrap();
        let state = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.phase, Phase::Completed);
        assert_eq!(state.sequence, 4);
        assert!(state.question.is_none());
        assert!(state.notes.iter().any(|n| n == "已采纳"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cancelling_modeling_terminates_the_native_descendant() {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };
        let pid_file =
            std::env::temp_dir().join(format!("modeling-child-{}.pid", std::process::id()));
        let mut command = owned_process::Command::new(fixture());
        command.args([pid_file.as_os_str()]);
        let child = owned_process::spawn(command, owned_process::Input::Pipe).unwrap();
        let state = Snapshot::new("八层".into());
        let (updates, mut snapshots) = watch::channel(Arc::new(state.clone()));
        let (commands, receiver) = mpsc::channel(4);
        let task = tokio::spawn(async move {
            let mut state = state;
            run_child(
                child,
                "project",
                &Operation::Model { verified: false },
                &mut state,
                &updates,
                receiver,
            )
            .await
            .unwrap();
            state
        });
        wait_for_question(&mut snapshots).await;
        let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        // SAFETY: request only synchronization rights for the observed fixture child.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!process.is_null());
        commands.send(Command::Cancel).await.unwrap();
        let state = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.phase, Phase::Cancelled);
        // SAFETY: the handle stays open until the process termination check completes.
        let exited = unsafe { WaitForSingleObject(process, 3000) };
        // SAFETY: close the handle opened above exactly once.
        unsafe {
            CloseHandle(process);
        }
        std::fs::remove_file(pid_file).unwrap();
        assert_eq!(
            exited, 0,
            "owned descendant must exit before cancellation completes"
        );
    }
    #[cfg(windows)]
    #[test]
    fn artifact_viewers_receive_normal_windows_paths_including_unicode() {
        assert_eq!(
            shell_path(Path::new(r"\\?\D:\项目\report.md")),
            std::ffi::OsString::from(r"D:\项目\report.md")
        );
        assert_eq!(
            shell_path(Path::new(r"\\?\UNC\server\project\floor.blend")),
            std::ffi::OsString::from(r"\\server\project\floor.blend")
        );
    }
}
