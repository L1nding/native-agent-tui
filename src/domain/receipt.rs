//! The model carries only references. Core verifies sealed facts on a private channel.

use super::*;
use tokio::io::AsyncReadExt;

pub const FACT_LIMIT: usize = 8 * 1024 * 1024;
const RECEIPT_LIMIT: usize = 16 * 1024;
const RECEIPT_CONTRACT: &str = "modeling-fact-receipt/v1";
const BINDING_KEYS: &[&str] = &[
    "proposal_id",
    "revision",
    "prepare_token",
    "base_sha256",
    "candidate_sha256",
    "operation_id",
    "build_id",
    "authority_sha256",
    "authorization_id",
];

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactReceipt {
    pub contract: String,
    pub run_id: String,
    pub context_sha256: String,
    pub kind: String,
    pub ok: bool,
    pub fact_sha256: String,
    pub fact_bytes: usize,
    pub fact_sequence: u64,
    pub binding: Value,
}

impl std::fmt::Debug for FactReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactReceipt")
            .field("kind", &self.kind)
            .field("fact_sequence", &self.fact_sequence)
            .finish_non_exhaustive()
    }
}

fn unwrap_receipt(value: &Value, depth: usize) -> Option<Value> {
    if depth > 4 {
        return None;
    }
    if value
        .get("isError")
        .is_some_and(|error| !error.is_null() && error.as_bool() != Some(false))
    {
        return None;
    }
    if value["contract"] == RECEIPT_CONTRACT {
        return Some(value.clone());
    }
    if let Some(raw) = value.as_str().filter(|raw| raw.len() <= RECEIPT_LIMIT) {
        return unwrap_receipt(&serde_json::from_str::<Value>(raw).ok()?, depth + 1);
    }
    for key in ["structuredContent", "result"] {
        if let Some(result) = value.get(key).and_then(|v| unwrap_receipt(v, depth + 1)) {
            return Some(result);
        }
    }
    for block in value["content"].as_array().into_iter().flatten().take(8) {
        if let Some(result) = unwrap_receipt(&block["text"], depth + 1) {
            return Some(result);
        }
    }
    None
}

pub fn from_tool(
    setup: &DomainSetup,
    notice: &ObservedTool,
    item: &Value,
) -> Result<Option<FactReceipt>, String> {
    if notice.category != ToolCategory::Mcp
        || notice.outcome.is_none()
        || item["type"] != "mcpToolCall"
        || item["server"] != SERVER
    {
        return Ok(None);
    }
    if notice.outcome != Some(ObservedToolOutcome::Completed) || item["result"]["isError"] == true {
        return Err("领域工具未成功返回事实回执；请检查当前执行结果".into());
    }
    if super::unwrap_envelope(item["result"].clone(), 0).is_some() {
        return Ok(None);
    }
    if serde_json::to_vec(&item["result"]).map_or(true, |v| v.len() > RECEIPT_LIMIT) {
        return Err("领域事实回执超过大小上限；完整事实尚未同步".into());
    }
    let value =
        unwrap_receipt(&item["result"], 0).ok_or("领域事实回执缺失或损坏；完整事实尚未同步")?;
    let receipt: FactReceipt =
        serde_json::from_value(value).map_err(|_| "领域事实回执字段无效；完整事实尚未同步")?;
    validate(setup, &receipt)?;
    Ok(Some(receipt))
}

pub fn validate(setup: &DomainSetup, receipt: &FactReceipt) -> Result<(), String> {
    if receipt.contract != RECEIPT_CONTRACT
        || receipt.run_id != setup.run_id
        || receipt.context_sha256 != setup.context_sha256
        || receipt.kind.is_empty()
        || receipt.kind.len() > 64
        || !matches!(
            receipt.kind.as_str(),
            "capabilities"
                | "project_catalog"
                | "project_selection"
                | "project_location"
                | "project_scope"
                | "project_building"
                | "project_floor_discarded"
                | "drawing_audit"
                | "source_ledger"
                | "source_groups"
                | "source_linework"
                | "source_linework_selection"
                | "source_interpretation"
                | "source_disposition"
                | "proposal"
                | "operation"
                | "application"
                | "artifact"
                | "artifact_validation"
                | "component_first_use_review"
                | "authorization"
                | "diagnostic"
        )
        || receipt.fact_sequence == 0
        || receipt.fact_bytes == 0
        || receipt.fact_bytes > FACT_LIMIT
        || receipt.fact_sha256.len() != 64
        || !receipt
            .fact_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !receipt.binding.is_object()
    {
        return Err("领域事实回执身份或大小无效；完整事实尚未同步".into());
    }
    Ok(())
}

pub fn verify(setup: &DomainSetup, receipt: &FactReceipt, raw: &[u8]) -> Result<Value, String> {
    if raw.len() > FACT_LIMIT
        || raw.len() != receipt.fact_bytes
        || format!("{:x}", Sha256::digest(raw)) != receipt.fact_sha256
    {
        return Err("完整领域事实的大小或 SHA256 与回执不一致".into());
    }
    let value: Value = serde_json::from_slice(raw).map_err(|_| "完整领域事实不是有效 JSON")?;
    let owner = [
        "proposal",
        "receipt",
        "artifact",
        "grant",
        "component_first_use_review",
    ]
    .into_iter()
    .find_map(|key| value[key].as_object());
    let binding: serde_json::Map<String, Value> = BINDING_KEYS
        .iter()
        .filter_map(|key| {
            owner
                .and_then(|map| map.get(*key))
                .map(|v| ((*key).into(), v.clone()))
        })
        .collect();
    if value["contract"] != CONTRACT
        || receipt.contract != RECEIPT_CONTRACT
        || receipt.run_id != setup.run_id
        || receipt.context_sha256 != setup.context_sha256
        || value["run_id"] != receipt.run_id
        || value["context_sha256"] != receipt.context_sha256
        || value["kind"] != receipt.kind
        || value["ok"] != receipt.ok
        || value["fact_sequence"] != receipt.fact_sequence
        || Value::Object(binding) != receipt.binding
    {
        return Err("完整领域事实的上下文、序号或提案绑定与回执不一致".into());
    }
    Ok(value)
}

/// Replay sealed facts belonging to this context, in semantic sequence order.
/// The sequence index names each fact file and bounds membership; it is never proof
/// that a fact is valid, so every file is re-hashed and checked before `apply` sees it.
/// Facts are read one at a time, so memory and time do not depend on keeping the
/// whole history resident.
pub(super) fn baseline(setup: &DomainSetup, mut apply: impl FnMut(Value)) {
    use std::io::Read;
    let mut load = || -> Option<()> {
        let context = std::fs::read(&setup.context_path).ok()?;
        if context.len() > 16 * 1024
            || format!("{:x}", Sha256::digest(&context)) != setup.context_sha256
        {
            return None;
        }
        let root = setup.repository.canonicalize().ok()?;
        let directory = root
            .join("outputs/domain")
            .join(&setup.run_id)
            .join(match setup.mode {
                DomainMode::Floor => "changes",
                DomainMode::Project => "project",
            })
            .canonicalize()
            .ok()?;
        if !directory.starts_with(&root) {
            return None;
        }
        let read = |path: &Path, limit: usize| -> Option<Vec<u8>> {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .ok()?
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() <= limit).then_some(bytes)
        };
        let index: Value =
            serde_json::from_slice(&read(&directory.join("fact-sequences.json"), 256 * 1024)?)
                .ok()?;
        let next = index["next"].as_u64()?;
        let mut ordered: Vec<(u64, &str)> = index["facts"]
            .as_object()?
            .iter()
            .filter_map(|(sha, sequence)| Some((sequence.as_u64()?, sha.as_str())))
            .filter(|(sequence, sha)| {
                *sequence > 0
                    && *sequence < next
                    && sha.len() == 64
                    && sha
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .collect();
        ordered.sort_unstable();
        let facts = directory.join("facts").canonicalize().ok()?;
        if !facts.starts_with(&directory) {
            return None;
        }
        for (sequence, sha) in ordered {
            let path = facts.join(format!("{sha}.json"));
            if !path
                .canonicalize()
                .is_ok_and(|resolved| resolved.starts_with(&facts))
            {
                continue;
            }
            let Some(raw) = read(&path, FACT_LIMIT) else {
                continue;
            };
            if format!("{:x}", Sha256::digest(&raw)) != sha {
                continue;
            }
            let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
                continue;
            };
            drop(raw);
            if value["contract"] == CONTRACT
                && value["run_id"] == setup.run_id
                && value["context_sha256"] == setup.context_sha256
                && value["fact_sequence"].as_u64() == Some(sequence)
            {
                apply(value);
            }
        }
        Some(())
    };
    let _ = load();
}

pub async fn read(setup: DomainSetup, receipt: FactReceipt) -> Result<Vec<u8>, String> {
    read_with_deadline(setup, receipt, std::time::Duration::from_secs(10)).await
}

async fn read_with_deadline(
    setup: DomainSetup,
    receipt: FactReceipt,
    deadline: std::time::Duration,
) -> Result<Vec<u8>, String> {
    let raw = tokio::fs::read(&setup.context_path)
        .await
        .map_err(|_| "无法读取建模上下文")?;
    if raw.len() > 16 * 1024 || format!("{:x}", Sha256::digest(raw)) != setup.context_sha256 {
        return Err("建模上下文已变化；完整事实尚未同步".into());
    }
    let mut command = crate::owned_process::Command::new(&setup.python);
    command
        .current_dir(&setup.repository)
        .args(match setup.mode {
            DomainMode::Floor => vec![
                "-X",
                "utf8",
                "-m",
                "blender_agent.cli.main",
                "domain-read-fact",
                "--context",
            ],
            DomainMode::Project => vec![
                "-X",
                "utf8",
                "-m",
                "blender_agent.integrations.project_mcp",
                "--context",
            ],
        })
        .args([setup.context_path.as_os_str()])
        .args([
            if setup.mode == DomainMode::Floor {
                "--fact-sha256"
            } else {
                "--read-fact"
            },
            &receipt.fact_sha256,
        ]);
    let mut child = crate::owned_process::spawn(command, crate::owned_process::Input::Null)
        .map_err(|_| "无法启动领域事实读取进程")?;
    let mut stdout = child.stdout.take().ok_or("缺少领域事实读取通道")?;
    let mut stderr = child.stderr.take().ok_or("缺少领域事实错误通道")?;
    async fn bounded<R: tokio::io::AsyncRead + Unpin>(
        stream: &mut R,
        limit: usize,
    ) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        stream
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "领域事实读取通道失败")?;
        if bytes.len() > limit {
            return Err("领域事实读取超过资源上限".into());
        }
        Ok(bytes)
    }
    let work = async {
        let (bytes, _) =
            tokio::try_join!(bounded(&mut stdout, FACT_LIMIT), bounded(&mut stderr, 4096))?;
        let status = child.wait().await.map_err(|_| "领域事实读取进程异常退出")?;
        if !status.success() {
            return Err("封存领域事实不存在或校验失败".to_owned());
        }
        Ok(bytes)
    };
    let result = tokio::time::timeout(deadline, work)
        .await
        .map_err(|_| "领域事实读取超时；请重新检查提案以同步事实".to_owned())
        .and_then(|result| result);
    if result.is_err() {
        child
            .kill()
            .await
            .map_err(|_| "领域事实读取进程退出未确认".to_owned())?;
    }
    result
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn python() -> PathBuf {
        if let Ok(path) = std::env::var("MODELING_TEST_PYTHON") {
            return path.into();
        }
        if cfg!(windows) {
            for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
                for name in ["python.exe", "python.cmd", "python.bat"] {
                    let path = directory.join(name);
                    if path.is_file() {
                        return path;
                    }
                }
            }
        }
        if cfg!(windows) { "python" } else { "python3" }.into()
    }

    fn fixture(script: &str) -> (DomainSetup, FactReceipt, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "domain-reader-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("blender_agent/cli")).unwrap();
        for file in ["blender_agent/__init__.py", "blender_agent/cli/__init__.py"] {
            std::fs::write(root.join(file), "").unwrap();
        }
        std::fs::write(root.join("blender_agent/cli/main.py"), script).unwrap();
        std::fs::write(root.join("context.json"), b"{}").unwrap();
        let setup = DomainSetup {
            mode: crate::domain::DomainMode::Floor,
            context_path: root.join("context.json"),
            repository: root.clone(),
            python: python(),
            run_id: "run".into(),
            context_sha256: format!("{:x}", Sha256::digest(b"{}")),
            floor_id: "floor".into(),
        };
        let receipt = FactReceipt {
            contract: RECEIPT_CONTRACT.into(),
            run_id: "run".into(),
            context_sha256: setup.context_sha256.clone(),
            kind: "proposal".into(),
            ok: true,
            fact_sequence: 1,
            fact_sha256: "0".repeat(64),
            fact_bytes: 2,
            binding: json!({}),
        };
        (setup, receipt, root)
    }

    /// A just-terminated process can hold its working directory for a moment on Windows.
    async fn remove(root: &Path) {
        for _ in 0..150 {
            if std::fs::remove_dir_all(root).is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn exited(root: &Path) {
        let pid: u32 = std::fs::read_to_string(root.join("reader.pid"))
            .unwrap()
            .parse()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while crate::owned_process::process_alive(pid) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        #[cfg(windows)]
        {
            let child: u32 = std::fs::read_to_string(root.join("descendant.pid"))
                .unwrap()
                .parse()
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while crate::owned_process::process_alive(child) {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
        }
    }

    // Pid files appear atomically, so the test never reads a half-written one.
    const SLEEP: &str = "import os,time,subprocess,sys\nfrom pathlib import Path\ndef record(name,pid):\n    Path(name+'.tmp').write_text(str(pid))\n    os.replace(name+'.tmp',name)\nchild=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'])\nrecord('descendant.pid',child.pid)\nrecord('reader.pid',os.getpid())\ntime.sleep(60)\n";

    #[tokio::test]
    async fn deadline_and_cancellation_terminate_private_readers() {
        let (setup, receipt, root) = fixture(SLEEP);
        let result = read_with_deadline(setup, receipt, std::time::Duration::from_secs(3)).await;
        assert!(
            result.as_ref().is_err_and(|error| error.contains("超时")),
            "{result:?}"
        );
        exited(&root).await;
        remove(&root).await;
        let (setup, receipt, root) = fixture(SLEEP);
        let task = tokio::spawn(read(setup, receipt));
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !root.join("reader.pid").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        let _ = task.await;
        exited(&root).await;
        remove(&root).await;
    }

    #[tokio::test]
    async fn independent_reader_rejects_oversized_stdout_and_stderr() {
        for script in [
            "import sys\nsys.stdout.buffer.write(b'x'*(8*1024*1024+1))\n",
            "import sys\nsys.stderr.buffer.write(b'x'*4097)\n",
        ] {
            let (setup, receipt, root) = fixture(script);
            let result = read(setup, receipt).await;
            assert!(
                result
                    .as_ref()
                    .is_err_and(|error| error.contains("资源上限")),
                "{}",
                result.as_ref().err().unwrap()
            );
            remove(&root).await;
        }
    }
}
