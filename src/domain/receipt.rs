//! The model carries only references. Core verifies sealed facts on a private channel.

use super::*;
use std::process::Stdio;
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
    if serde_json::to_vec(&item["result"]).map_or(true, |v| v.len() > RECEIPT_LIMIT) {
        return Err("领域事实回执超过大小上限；完整事实尚未同步".into());
    }
    let value =
        unwrap_receipt(&item["result"], 0).ok_or("领域事实回执缺失或损坏；完整事实尚未同步")?;
    let receipt: FactReceipt =
        serde_json::from_value(value).map_err(|_| "领域事实回执字段无效；完整事实尚未同步")?;
    if receipt.run_id != setup.run_id
        || receipt.context_sha256 != setup.context_sha256
        || receipt.kind.is_empty()
        || receipt.kind.len() > 64
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
    Ok(Some(receipt))
}

pub fn verify(setup: &DomainSetup, receipt: &FactReceipt, raw: &[u8]) -> Result<Value, String> {
    if raw.len() > FACT_LIMIT
        || raw.len() != receipt.fact_bytes
        || format!("{:x}", Sha256::digest(raw)) != receipt.fact_sha256
    {
        return Err("完整领域事实的大小或 SHA256 与回执不一致".into());
    }
    let value: Value = serde_json::from_slice(raw).map_err(|_| "完整领域事实不是有效 JSON")?;
    let owner = ["proposal", "receipt", "artifact", "grant"]
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
    let mut command = tokio::process::Command::new(&setup.python);
    command
        .current_dir(&setup.repository)
        .env("PYTHONUTF8", "1")
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args([
            "-m",
            "blender_agent.cli.main",
            "domain-read-fact",
            "--context",
        ])
        .arg(&setup.context_path)
        .args(["--fact-sha256", &receipt.fact_sha256]);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command.spawn().map_err(|_| "无法启动领域事实读取进程")?;
    let mut stdout = child.stdout.take().ok_or("缺少领域事实读取通道")?;
    let mut stderr = child.stderr.take().ok_or("缺少领域事实错误通道")?;
    let work = async {
        let mut bytes = Vec::new();
        let mut errors = Vec::new();
        let mut bounded_stdout = (&mut stdout).take(FACT_LIMIT as u64 + 1);
        let mut bounded_stderr = (&mut stderr).take(4097);
        let (out, err) = tokio::join!(
            bounded_stdout.read_to_end(&mut bytes),
            bounded_stderr.read_to_end(&mut errors)
        );
        out.map_err(|_| "领域事实读取失败")?;
        err.map_err(|_| "领域事实错误通道读取失败")?;
        if bytes.len() > FACT_LIMIT || errors.len() > 4096 {
            return Err("领域事实读取超过资源上限".to_owned());
        }
        let status = child.wait().await.map_err(|_| "领域事实读取进程异常退出")?;
        if !status.success() {
            return Err("封存领域事实不存在或校验失败".to_owned());
        }
        Ok(bytes)
    };
    tokio::time::timeout(deadline, work)
        .await
        .map_err(|_| "领域事实读取超时；请重新检查提案以同步事实".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;

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
            context_path: root.join("context.json"),
            repository: root.clone(),
            python: std::env::var("MODELING_TEST_PYTHON")
                .unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.into())
                .into(),
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
    }

    const SLEEP: &str = "import os,time\nfrom pathlib import Path\nPath('reader.pid').write_text(str(os.getpid()))\ntime.sleep(60)\n";

    #[tokio::test]
    async fn deadline_and_cancellation_terminate_private_readers() {
        let (setup, receipt, root) = fixture(SLEEP);
        let result =
            read_with_deadline(setup, receipt, std::time::Duration::from_millis(500)).await;
        assert!(result.unwrap_err().contains("超时"));
        exited(&root).await;
        std::fs::remove_dir_all(root).unwrap();
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
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn independent_reader_rejects_oversized_stdout_and_stderr() {
        for script in [
            "import sys\nsys.stdout.buffer.write(b'x'*(8*1024*1024+1))\n",
            "import sys\nsys.stderr.buffer.write(b'x'*4097)\n",
        ] {
            let (setup, receipt, root) = fixture(script);
            assert!(read(setup, receipt).await.unwrap_err().contains("资源上限"));
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
