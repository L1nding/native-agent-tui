//! Read-only, integrity-bound review packages served for the Core owner's lifetime.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;

use crate::domain::DomainSetup;

const MAX_PACKAGE_BYTES: usize = 256 * 1024 * 1024;
const MAX_REGISTERED_PACKAGES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebReviewPackage {
    pub html_path: PathBuf,
    pub png_path: PathBuf,
    pub data_sha256: String,
    pub manifest_sha256: Option<String>,
    pub artifact_sha256: Option<String>,
    pub url_hash: Option<String>,
    pub first_use_comparison: Option<FirstUseComparison>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FirstUseComparison {
    pub proposal_sha256: String,
    pub cad_required: bool,
    pub part_ids: Vec<String>,
    pub dimensions: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSummary {
    pub review_type: String,
    pub candidate_count: usize,
}

#[derive(PartialEq, Eq)]
struct Resource {
    bytes: Vec<u8>,
    content_type: &'static str,
}
struct VerifiedPackage {
    files: BTreeMap<String, Resource>,
    summary: ReviewSummary,
}
type Packages = Arc<RwLock<BTreeMap<String, Arc<VerifiedPackage>>>>;

pub struct ReviewWebServer {
    setup: DomainSetup,
    packages: Packages,
    address: Option<SocketAddr>,
    task: Option<JoinHandle<()>>,
    connections: Arc<Mutex<BTreeMap<u64, TcpStream>>>,
    closed: Arc<AtomicBool>,
}

impl ReviewWebServer {
    pub fn new(setup: DomainSetup) -> Self {
        Self {
            setup,
            packages: Arc::default(),
            address: None,
            task: None,
            connections: Arc::default(),
            closed: Arc::default(),
        }
    }

    /// Validate before opening a listener or publishing a capability URL.
    pub async fn register(&mut self, package: WebReviewPackage) -> Result<String, String> {
        let fragment = package.url_hash.clone().unwrap_or_default();
        if fragment.bytes().any(|b| b.is_ascii_control()) {
            return Err("复核视角包含非法字符".into());
        }
        let verified = validate(&self.setup, package).await?;
        let token = random_token()?;
        let mut packages = self.packages.write().map_err(|_| "复核服务已关闭")?;
        let existing = packages
            .iter()
            .find(|(_, registered)| registered.files == verified.files)
            .map(|(token, _)| token.clone());
        if let Some(token) = existing {
            return Ok(format!(
                "http://{}/{token}/index.html{}{}",
                self.address.ok_or("复核服务未启动")?,
                if fragment.is_empty() { "" } else { "#" },
                fragment.trim_start_matches('#')
            ));
        }
        if packages.len() >= MAX_REGISTERED_PACKAGES {
            return Err("已打开的复核包达到上限，请重启会话后重试".into());
        }
        let total: usize = packages
            .values()
            .flat_map(|p| p.files.values())
            .map(|r| r.bytes.len())
            .sum();
        let additional: usize = verified.files.values().map(|r| r.bytes.len()).sum();
        if total.saturating_add(additional) > MAX_PACKAGE_BYTES {
            return Err("复核服务总字节数超过256MiB".into());
        }
        if self.address.is_none() {
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .map_err(|e| format!("无法启动复核服务：{e}"))?;
            listener.set_nonblocking(true).map_err(|e| e.to_string())?;
            self.address = Some(listener.local_addr().map_err(|e| e.to_string())?);
            let registered = self.packages.clone();
            let active_connections = self.connections.clone();
            let closed = self.closed.clone();
            self.task = Some(tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                let mut connection_id = 0u64;
                loop {
                    tokio::select! {
                        biased;
                        Some(_) = connections.join_next(), if !connections.is_empty() => {},
                        _ = tokio::time::sleep(Duration::from_millis(20)) => {
                            while connections.len() < 16 {
                                match listener.accept() {
                                    Ok((stream, _)) => {
                                        let packages = registered.clone();
                                        let Ok(clone) = stream.try_clone() else { continue };
                                        if let Ok(mut active) = active_connections.lock() {
                                            if closed.load(Ordering::Acquire) { break; }
                                            active.insert(connection_id, clone);
                                        }
                                        let active = active_connections.clone();
                                        let id = connection_id;
                                        connection_id = connection_id.wrapping_add(1);
                                        connections.spawn_blocking(move || {
                                            let _ = serve(stream, &packages);
                                            if let Ok(mut sockets) = active.lock() { sockets.remove(&id); }
                                        });
                                    }
                                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                                    Err(_) => return,
                                }
                            }
                        }
                    }
                }
            }));
        }
        packages.insert(token.clone(), Arc::new(verified));
        Ok(format!(
            "http://{}/{token}/index.html{}{}",
            self.address.unwrap(),
            if fragment.is_empty() { "" } else { "#" },
            fragment.trim_start_matches('#')
        ))
    }
}

impl Drop for ReviewWebServer {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        if let Ok(mut connections) = self.connections.lock() {
            for connection in connections.values() {
                let _ = connection.shutdown(std::net::Shutdown::Both);
            }
            connections.clear();
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub async fn inspect(
    setup: &DomainSetup,
    package: WebReviewPackage,
) -> Result<ReviewSummary, String> {
    Ok(validate(setup, package).await?.summary)
}

pub async fn verify(setup: &DomainSetup, package: WebReviewPackage) -> Result<(), String> {
    validate(setup, package).await.map(|_| ())
}

// Reuse the owning modeler's schema, canonical Python number encoding, gzip
// transport and screenshot checks. This imports no MCP or Blender entry point.
const VALIDATOR: &str = r#"
import json,sys,gzip,io
from pathlib import Path
from PIL import Image
from blender_agent.review_web.integrity import validate_review_package
from blender_agent.review_web.contract import SCHEMA_PATH
try:
 p=json.load(sys.stdin)
 root=Path(p['html_path']).resolve().parent
 # First-use screenshots live beside the sealed manifest, outside w/<key>/.
 image_paths={Path(p['png_path']).resolve(),Path(p['png_path']+'.review.json').resolve()}
 # All repeated integrity and Pillow reads must describe the same byte snapshot.
 cached={}
 def bounded_bytes(path):
  resolved=path.resolve(strict=True)
  if not resolved.is_relative_to(root) and resolved not in image_paths and resolved != SCHEMA_PATH.resolve():
   raise ValueError('review_resource_outside_package')
  if resolved in cached:
   return cached[resolved]
  if not resolved.is_file() or resolved.stat().st_size > 268435456:
   raise ValueError('review_resource_size_limit')
  with resolved.open('rb') as stream:
   data=stream.read(268435457)
  if len(data) > 268435456:
   raise ValueError('review_resource_size_limit')
  if sum(map(len,cached.values())) + len(data) > 268435456:
   raise ValueError('review_snapshot_size_limit')
  cached[resolved]=data
  return data
 def bounded_text(path,encoding=None,errors=None):
  return bounded_bytes(path).decode(encoding or 'utf-8',errors or 'strict')
 def bounded_gzip(data):
  with gzip.GzipFile(fileobj=io.BytesIO(data)) as stream:
   decoded=stream.read(67108865)
  if len(decoded) > 67108864:
   raise ValueError('review_decompressed_size_limit')
  return decoded
 Path.read_bytes=bounded_bytes
 Path.read_text=bounded_text
 gzip.decompress=bounded_gzip
 image_open=Image.open
 def snapshot_image(path,*args,**kwargs):
  if isinstance(path,(str,Path)):
   path=io.BytesIO(bounded_bytes(Path(path)))
  return image_open(path,*args,**kwargs)
 Image.open=snapshot_image
 v=validate_review_package(p['html_path'],image_path=p['png_path'],expected_data_sha256=p['data_sha256'],expected_manifest_sha256=p['manifest_sha256'],expected_artifact_sha256=p['artifact_sha256'],expected_url_hash=p['url_hash'])
 d=v['data']
 if expected := p.get('first_use_comparison'):
  import hashlib
  from blender_agent.review_web.contract import comparison_rows
  def compact_dimensions(dimensions):
   if 'assumed' not in dimensions:
    return dimensions
   assumptions=dimensions['assumed']
   identity=hashlib.sha256(json.dumps(assumptions,ensure_ascii=False,sort_keys=True,separators=(',',':')).encode()).hexdigest()
   return {**{k:v for k,v in dimensions.items() if k != 'assumed'},'assumed_ref':{'sha256':identity,'count':len(assumptions)},'assumed_rows':comparison_rows({'assumed':assumptions})['assumptions']}
  comparison=d.get('comparison')
  if (not comparison or comparison.get('contract') != 'component-first-use-comparison/1'
      or comparison.get('cad_required') != expected['cad_required']
      or d.get('review_type') != 'component_first_use_3d'
      or d.get('binding',{}).get('proposal_sha256') != expected['proposal_sha256']
      or sorted(part for item in d['items'] for part in item.get('object_ids',[])) != sorted(expected['part_ids'])
      or compact_dimensions(comparison['dimensions']) != compact_dimensions(expected['dimensions'])):
   raise ValueError('component_first_use_comparison_binding')
 print(json.dumps({'manifest':v['manifest'],'manifest_sha256':v['manifest_sha256'],'review_type':d['review_type'],'candidate_count':len(d['items']),'model_path':d.get('model',{}).get('path')}))
except Exception as e:
 print(json.dumps({'error':str(e)[:300]}))
 sys.exit(1)
"#;

async fn validate(
    setup: &DomainSetup,
    package: WebReviewPackage,
) -> Result<VerifiedPackage, String> {
    if !package.html_path.is_absolute()
        || package
            .html_path
            .file_name()
            .is_none_or(|n| n != "index.html")
        || !package.png_path.is_absolute()
        || !valid_hash(&package.data_sha256)
    {
        return Err("复核包必须有绝对本地路径与有效数据哈希".into());
    }
    for hash in [&package.manifest_sha256, &package.artifact_sha256]
        .into_iter()
        .flatten()
    {
        if !valid_hash(hash) {
            return Err("复核事实哈希无效".into());
        }
    }
    for path in [&package.html_path, &package.png_path] {
        let name = path.to_string_lossy();
        if name.starts_with(r"\\") || name.contains("://") || name.contains('\0') {
            return Err("复核包仅支持本地文件".into());
        }
    }
    let root = tokio::fs::canonicalize(package.html_path.parent().ok_or("复核目录缺失")?)
        .await
        .map_err(|_| "复核目录不可读")?;
    if root.to_string_lossy().starts_with(r"\\?\UNC\") {
        return Err("复核包不能位于网络共享".into());
    }
    let mut directories = vec![root.clone()];
    let mut package_bytes = 0u64;
    while let Some(directory) = directories.pop() {
        let mut entries = tokio::fs::read_dir(directory)
            .await
            .map_err(|_| "复核目录不可读")?;
        while let Some(entry) = entries.next_entry().await.map_err(|_| "复核目录不可读")? {
            let entry_type = entry.file_type().await.map_err(|_| "复核文件信息不可读")?;
            if entry_type.is_symlink() {
                return Err("复核包不允许符号链接".into());
            }
            if entry_type.is_dir() {
                directories.push(entry.path());
            } else if entry_type.is_file() {
                package_bytes = package_bytes
                    .checked_add(
                        entry
                            .metadata()
                            .await
                            .map_err(|_| "复核文件信息不可读")?
                            .len(),
                    )
                    .ok_or("复核包大小溢出")?;
                if package_bytes > MAX_PACKAGE_BYTES as u64 {
                    return Err("复核包超过256MiB".into());
                }
            }
        }
    }
    // Bound every caller-controlled file before the Python validator performs
    // its own reads; this also limits compressed transport input size.
    let data_path = root.join("review.json");
    let envelope_metadata = tokio::fs::metadata(&data_path)
        .await
        .map_err(|_| "复核JSON不可读")?;
    if !envelope_metadata.is_file() || envelope_metadata.len() > 1024 * 1024 {
        return Err("复核JSON超过1MiB".into());
    }
    let envelope: Value = serde_json::from_slice(
        &tokio::fs::read(&data_path)
            .await
            .map_err(|_| "复核JSON不可读")?,
    )
    .map_err(|_| "复核JSON无效")?;
    let mut inputs = vec![
        package.html_path.clone(),
        package.png_path.clone(),
        package.png_path.with_extension("png.review.json"),
        data_path,
    ];
    if envelope["encoding"] == "gzip-script" {
        let relative = envelope["path"].as_str().ok_or("复核数据脚本路径缺失")?;
        if !safe_relative(relative) {
            return Err("复核数据脚本路径无效".into());
        }
        inputs.push(root.join(relative));
    }
    let mut input_total = 0u64;
    for path in inputs {
        let canonical = tokio::fs::canonicalize(&path)
            .await
            .map_err(|_| "复核文件不可读")?;
        if !canonical.starts_with(&root)
            && path != package.png_path
            && path != package.png_path.with_extension("png.review.json")
        {
            return Err("拒绝复核包外文件".into());
        }
        let metadata = tokio::fs::metadata(canonical)
            .await
            .map_err(|_| "复核文件信息不可读")?;
        if !metadata.is_file() {
            return Err("复核路径不是普通文件".into());
        }
        input_total = input_total
            .checked_add(metadata.len())
            .ok_or("复核包大小溢出")?;
        if input_total > MAX_PACKAGE_BYTES as u64 {
            return Err("复核包超过256MiB".into());
        }
    }
    let request = serde_json::json!({"html_path":package.html_path,"png_path":package.png_path,"data_sha256":package.data_sha256,"manifest_sha256":package.manifest_sha256,"artifact_sha256":package.artifact_sha256,"url_hash":package.url_hash,"first_use_comparison":package.first_use_comparison});
    let mut command = tokio::process::Command::new(&setup.python);
    command
        .current_dir(&setup.repository)
        .env("PYTHONUTF8", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .kill_on_drop(true)
        .args(["-B", "-X", "utf8", "-c", VALIDATOR])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let output = tokio::time::timeout(Duration::from_secs(60), async {
        use tokio::io::AsyncWriteExt;
        let mut child = command.spawn()?;
        let mut stdin = child.stdin.take().expect("piped review input");
        stdin.write_all(request.to_string().as_bytes()).await?;
        drop(stdin);
        child.wait_with_output().await
    })
    .await
    .map_err(|_| "复核完整性校验超时")?
    .map_err(|e| format!("无法运行复核校验器：{e}"))?;
    if output.stdout.len() > 16 * 1024 {
        return Err("复核校验结果过大".into());
    }
    let result: Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "复核校验器未返回有效结果")?;
    if !output.status.success() {
        return Err(format!(
            "复核校验失败：{}",
            result["error"].as_str().unwrap_or("校验器不可用")
        ));
    }
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    let manifest = result["manifest"].as_object().ok_or("复核manifest缺失")?;
    for (path, key) in [
        (package.html_path.clone(), "html_sha256"),
        (root.join("review.json"), "review_json_sha256"),
        (package.png_path.clone(), "image_sha256"),
        (
            package.png_path.with_extension("png.review.json"),
            "sidecar_sha256",
        ),
    ] {
        let canonical = tokio::fs::canonicalize(&path)
            .await
            .map_err(|_| "复核文件不可读")?;
        if !canonical.starts_with(&root)
            && (path == package.png_path
                || path == package.png_path.with_extension("png.review.json"))
        {
            // Verify only the two explicitly bound external files; do not serve
            // their parent directory or make it an allowed package resource root.
            add_resource(
                canonical.parent().ok_or("复核图片目录缺失")?,
                &canonical,
                manifest
                    .get(key)
                    .and_then(Value::as_str)
                    .ok_or("复核manifest哈希缺失")?,
                &mut BTreeMap::new(),
                &mut total,
            )
            .await?;
            continue;
        }
        add_resource(
            &root,
            &path,
            manifest
                .get(key)
                .and_then(Value::as_str)
                .ok_or("复核manifest哈希缺失")?,
            &mut files,
            &mut total,
        )
        .await?;
    }
    if let Some(model) = result["model_path"].as_str() {
        if !safe_relative(model) {
            return Err("复核GLB路径无效".into());
        }
        add_resource(
            &root,
            &root.join(model),
            manifest
                .get("model_sha256")
                .and_then(Value::as_str)
                .ok_or("复核模型哈希缺失")?,
            &mut files,
            &mut total,
        )
        .await?;
    }
    if !files.contains_key("index.html") {
        return Err("复核首页路径不匹配".into());
    }
    let envelope: Value =
        serde_json::from_slice(&files.get("review.json").ok_or("复核JSON路径不匹配")?.bytes)
            .map_err(|_| "复核JSON无效")?;
    if envelope["data_sha256"] != package.data_sha256 {
        return Err("复核数据哈希与事实不符".into());
    }
    if envelope["encoding"] == "gzip-script" {
        let path = envelope["path"].as_str().ok_or("复核数据脚本路径缺失")?;
        if !safe_relative(path) {
            return Err("复核数据脚本路径无效".into());
        }
        add_resource(
            &root,
            &root.join(path),
            envelope["sha256"].as_str().ok_or("复核脚本哈希缺失")?,
            &mut files,
            &mut total,
        )
        .await?;
    }
    // Recompute the seal in Rust as well, over precisely the bytes now served.
    if let Some(expected) = &package.manifest_sha256 {
        let bytes = serde_json::to_vec(manifest).map_err(|e| e.to_string())?;
        if digest(&bytes) != *expected {
            return Err("复核manifest与事实不符".into());
        }
    }
    Ok(VerifiedPackage {
        files,
        summary: ReviewSummary {
            review_type: result["review_type"].as_str().ok_or("复核类型缺失")?.into(),
            candidate_count: result["candidate_count"].as_u64().ok_or("复核候选数缺失")? as usize,
        },
    })
}

async fn add_resource(
    root: &Path,
    path: &Path,
    expected: &str,
    files: &mut BTreeMap<String, Resource>,
    total: &mut usize,
) -> Result<(), String> {
    let path = tokio::fs::canonicalize(path)
        .await
        .map_err(|_| "复核文件不可读")?;
    // Windows canonical paths carry a verbatim prefix; normalize before containment checks.
    let requested_text = path
        .strip_prefix(root)
        .map_err(|_| "拒绝复核包外文件")?
        .to_str()
        .ok_or("复核路径编码无效")?
        .replace('\\', "/");
    if !safe_relative(&requested_text) {
        return Err("复核文件路径无效".into());
    }
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| "复核文件不可读")?;
    let metadata = file.metadata().await.map_err(|_| "复核文件信息不可读")?;
    if !metadata.is_file() || metadata.len() > MAX_PACKAGE_BYTES.saturating_sub(*total) as u64 {
        return Err("复核包超过256MiB".into());
    }
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    file.take((MAX_PACKAGE_BYTES - *total + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "复核文件读取失败")?;
    if bytes.len() > MAX_PACKAGE_BYTES - *total || digest(&bytes) != expected {
        return Err(format!("复核文件SHA256不符：{requested_text}"));
    }
    *total += bytes.len();
    files.insert(
        requested_text.clone(),
        Resource {
            bytes,
            content_type: content_type(&requested_text),
        },
    );
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':', '%', '?', '#', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
fn content_type(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "glb" => "model/gltf-binary",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn resource(packages: &Packages, target: &str) -> Option<(Arc<VerifiedPackage>, String)> {
    let path = target.split('?').next()?.strip_prefix('/')?;
    let (token, relative) = path.split_once('/')?;
    if !safe_relative(token) || !safe_relative(relative) {
        return None;
    }
    let package = packages.read().ok()?.get(token)?.clone();
    package
        .files
        .contains_key(relative)
        .then(|| (package, relative.into()))
}

fn serve(mut stream: TcpStream, packages: &Packages) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while request.len() < 8192 && !request.windows(4).any(|w| w == b"\r\n\r\n") {
        if std::time::Instant::now() >= deadline {
            return Ok(());
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        request.extend_from_slice(&chunk[..n]);
    }
    let request = std::str::from_utf8(&request).unwrap_or("");
    let mut first = request.lines().next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("");
    let target = first.next().unwrap_or("");
    let found = resource(packages, target);
    let (status, content, body) = if !matches!(method, "GET" | "HEAD") {
        ("405 Method Not Allowed", "text/plain", &b""[..])
    } else if let Some((ref package, ref path)) = found {
        let r = &package.files[path];
        ("200 OK", r.content_type, r.bytes.as_slice())
    } else {
        ("404 Not Found", "text/plain", &b""[..])
    };
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {content}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n", body.len())?;
    if method != "HEAD" {
        stream.write_all(body)?;
    }
    Ok(())
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        unsafe extern "system" {
            fn BCryptGenRandom(
                algorithm: *mut std::ffi::c_void,
                buffer: *mut u8,
                length: u32,
                flags: u32,
            ) -> i32;
        }
        // BCRYPT_USE_SYSTEM_PREFERRED_RNG supplies a process-independent CSPRNG.
        if unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                2,
            )
        } < 0
        {
            return Err("系统随机数不可用".into());
        }
    }
    #[cfg(not(windows))]
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| "系统随机数不可用")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub async fn open_browser(url: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || launch_browser(&url))
        .await
        .map_err(|_| "浏览器打开任务中断".into())
        .and_then(|r| r)
}

#[cfg(windows)]
fn launch_browser(url: &str) -> Result<(), String> {
    let operation: Vec<u16> = "open\0".encode_utf16().collect();
    let target: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    } as isize;
    if result <= 32 {
        Err(format!("浏览器打开失败：{result}"))
    } else {
        Ok(())
    }
}
#[cfg(not(windows))]
fn launch_browser(url: &str) -> Result<(), String> {
    let status = std::process::Command::new(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    })
    .arg(url)
    .status()
    .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("浏览器打开失败".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered() -> (Packages, String) {
        let token = random_token().unwrap();
        let package = VerifiedPackage {
            files: [(
                "index.html".into(),
                Resource {
                    bytes: b"sealed page".to_vec(),
                    content_type: content_type("index.html"),
                },
            )]
            .into(),
            summary: ReviewSummary {
                review_type: "question_preview".into(),
                candidate_count: 2,
            },
        };
        (
            Arc::new(RwLock::new([(token.clone(), Arc::new(package))].into())),
            token,
        )
    }

    #[test]
    fn token_paths_only_resolve_registered_resources() {
        let (packages, token) = registered();
        assert_eq!(token.len(), 64);
        assert_ne!(token, random_token().unwrap());
        assert!(resource(&packages, &format!("/{token}/index.html")).is_some());
        for target in [
            "/index.html".into(),
            "/wrong/index.html".into(),
            format!("/{token}/secret.txt"),
            format!("/{token}/../index.html"),
            format!("/{token}/%2e%2e/index.html"),
            format!("/{token}/%252e%252e/index.html"),
            format!("/{token}/C:/secret"),
            format!("/{token}/\\secret"),
            format!("/{token}//index.html"),
        ] {
            assert!(resource(&packages, &target).is_none(), "{target}");
        }
    }

    #[test]
    fn content_types_cover_review_assets() {
        for (name, expected) in [
            ("index.html", "text/html; charset=utf-8"),
            ("a.js", "text/javascript; charset=utf-8"),
            ("a.css", "text/css; charset=utf-8"),
            ("review.json", "application/json"),
            ("review.png", "image/png"),
            ("model.glb", "model/gltf-binary"),
            ("a.svg", "image/svg+xml"),
        ] {
            assert_eq!(content_type(name), expected);
        }
    }

    #[test]
    fn http_is_read_only_and_does_not_serve_unregistered_paths() {
        let (packages, token) = registered();
        for (method, path, status) in [
            ("GET", format!("/{token}/index.html"), "200 OK"),
            ("HEAD", format!("/{token}/index.html"), "200 OK"),
            ("GET", format!("/{token}/../secret"), "404 Not Found"),
            (
                "POST",
                format!("/{token}/index.html"),
                "405 Method Not Allowed",
            ),
        ] {
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let address = listener.local_addr().unwrap();
            let registered = packages.clone();
            let worker = std::thread::spawn(move || {
                serve(listener.accept().unwrap().0, &registered).unwrap()
            });
            let mut client = TcpStream::connect(address).unwrap();
            write!(
                client,
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\n\r\n"
            )
            .unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            worker.join().unwrap();
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status}")),
                "{response}"
            );
            if method == "HEAD" {
                assert!(!response.contains("sealed page"));
            }
            if status == "200 OK" {
                assert!(response.contains("Content-Type: text/html; charset=utf-8"));
            }
        }
    }

    #[tokio::test]
    async fn snapshot_rejects_hash_mismatch_and_files_outside_package() {
        let root = std::env::temp_dir().join(format!("nat-web-{}", random_token().unwrap()));
        tokio::fs::create_dir_all(root.join("package"))
            .await
            .unwrap();
        tokio::fs::write(root.join("package/index.html"), b"changed")
            .await
            .unwrap();
        tokio::fs::write(root.join("secret"), b"secret")
            .await
            .unwrap();
        let directory = tokio::fs::canonicalize(root.join("package")).await.unwrap();
        let mut files = BTreeMap::new();
        let mut total = 0;
        add_resource(
            &directory,
            &root.join("package/index.html"),
            &digest(b"changed"),
            &mut files,
            &mut total,
        )
        .await
        .unwrap();
        assert_eq!(files["index.html"].bytes, b"changed");
        files.clear();
        total = 0;
        assert!(add_resource(
            &directory,
            &directory.join("index.html"),
            &digest(b"original"),
            &mut files,
            &mut total
        )
        .await
        .unwrap_err()
        .contains("SHA256"));
        assert!(add_resource(
            &directory,
            &root.join("secret"),
            &digest(b"secret"),
            &mut files,
            &mut total
        )
        .await
        .unwrap_err()
        .contains("包外"));
        assert!(files.is_empty());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "read-only real package validation; set REVIEW_MODELER_REPO and REVIEW_PACKAGE_DIR"]
    async fn real_review_package_registers_and_serves_sealed_bytes() {
        let repository = PathBuf::from(std::env::var("REVIEW_MODELER_REPO").unwrap());
        let root = PathBuf::from(std::env::var("REVIEW_PACKAGE_DIR").unwrap());
        let envelope: Value =
            serde_json::from_slice(&std::fs::read(root.join("review.json")).unwrap()).unwrap();
        let setup = DomainSetup {
            mode: crate::domain::DomainMode::Floor,
            python: repository.join(if cfg!(windows) {
                ".venv/Scripts/python.exe"
            } else {
                ".venv/bin/python"
            }),
            repository,
            context_path: PathBuf::new(),
            run_id: String::new(),
            context_sha256: String::new(),
            floor_id: String::new(),
        };
        let package = WebReviewPackage {
            html_path: root.join("index.html"),
            png_path: root.join("review.png"),
            data_sha256: envelope["data_sha256"].as_str().unwrap().into(),
            manifest_sha256: None,
            artifact_sha256: None,
            url_hash: None,
            first_use_comparison: None,
        };
        let summary = inspect(&setup, package.clone()).await.unwrap();
        assert!(!summary.review_type.is_empty());
        let mut server = ReviewWebServer::new(setup);
        for field in ["data", "manifest", "artifact"] {
            let mut invalid = package.clone();
            match field {
                "data" => invalid.data_sha256 = "0".repeat(64),
                "manifest" => invalid.manifest_sha256 = Some("0".repeat(64)),
                _ => invalid.artifact_sha256 = Some("0".repeat(64)),
            }
            assert!(server.register(invalid).await.is_err(), "{field}");
        }
        assert!(server.address.is_none());
        assert!(server.task.is_none());
        let url = server.register(package.clone()).await.unwrap();
        assert_eq!(server.register(package.clone()).await.unwrap(), url);
        eprintln!(
            "validated {} ({} candidates), registered {url}",
            summary.review_type, summary.candidate_count
        );
        // Opt-in manual acceptance without starting dsh, MCP or Blender.
        if std::env::var("REVIEW_MANUAL_OPEN").as_deref() == Ok("1") {
            open_browser(url.clone()).await.unwrap();
            tokio::time::sleep(Duration::from_secs(45)).await;
        }
        let address = server.address.unwrap();
        let path = url
            .strip_prefix(&format!("http://{address}"))
            .unwrap()
            .to_owned();
        let resources: Vec<_> = server
            .packages
            .read()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .files
            .iter()
            .map(|(name, resource)| (name.clone(), resource.bytes.clone(), resource.content_type))
            .collect();
        for (name, expected, mime) in resources {
            let target = format!("{}{name}", path.strip_suffix("index.html").unwrap());
            let response = tokio::task::spawn_blocking(move || {
                let mut client = TcpStream::connect(address).unwrap();
                write!(client, "GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
                let mut response = Vec::new();
                client.read_to_end(&mut response).unwrap();
                response
            })
            .await
            .unwrap();
            let end = response
                .windows(4)
                .position(|bytes| bytes == b"\r\n\r\n")
                .unwrap();
            let headers = std::str::from_utf8(&response[..end]).unwrap();
            assert!(headers.starts_with("HTTP/1.1 200 OK"), "{name}");
            assert!(headers.contains(&format!("Content-Type: {mime}")), "{name}");
            assert_eq!(&response[end + 4..], expected, "{name}");
        }
        let mut slow = TcpStream::connect(address).unwrap();
        slow.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        slow.write_all(b"GET /").unwrap();
        let mut tampered = package;
        tampered.data_sha256 = "0".repeat(64);
        assert!(server.register(tampered).await.is_err());
        assert_eq!(server.packages.read().unwrap().len(), 1);
        drop(server);
        let mut closed = [0u8; 1];
        assert_eq!(slow.read(&mut closed).unwrap(), 0);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(TcpStream::connect(address).is_err());
    }

    #[tokio::test]
    #[ignore = "read-only first-use validation; set REVIEW_MODELER_REPO and REVIEW_FIRST_USE_MANIFEST"]
    async fn real_first_use_comparison_rejects_changed_frozen_bindings() {
        let repository = PathBuf::from(std::env::var("REVIEW_MODELER_REPO").unwrap());
        let value: Value = serde_json::from_slice(
            &std::fs::read(std::env::var("REVIEW_FIRST_USE_MANIFEST").unwrap()).unwrap(),
        )
        .unwrap();
        let manifest = &value["manifest"];
        let item = &manifest["items"][0];
        let web = &item["review_web"];
        let setup = DomainSetup {
            mode: crate::domain::DomainMode::Floor,
            python: repository.join(if cfg!(windows) {
                ".venv/Scripts/python.exe"
            } else {
                ".venv/bin/python"
            }),
            repository,
            context_path: PathBuf::new(),
            run_id: String::new(),
            context_sha256: String::new(),
            floor_id: String::new(),
        };
        let package = WebReviewPackage {
            html_path: web["html_path"].as_str().unwrap().into(),
            png_path: item["images"][0]["image_path"].as_str().unwrap().into(),
            data_sha256: web["data_sha256"].as_str().unwrap().into(),
            manifest_sha256: Some(web["manifest_sha256"].as_str().unwrap().into()),
            artifact_sha256: Some(web["artifact_sha256"].as_str().unwrap().into()),
            url_hash: Some("view=perspective".into()),
            first_use_comparison: Some(FirstUseComparison {
                proposal_sha256: manifest["candidate_sha256"].as_str().unwrap().into(),
                cad_required: item["source_kind"] == "cad",
                part_ids: serde_json::from_value(item["part_ids"].clone()).unwrap(),
                dimensions: item["dimensions"].clone(),
            }),
        };
        verify(&setup, package.clone()).await.unwrap();
        // Exercise full assumptions against a frozen reference without writing
        // either the real package or its sealed assumptions file.
        let output=tokio::process::Command::new(&setup.python)
            .current_dir(&setup.repository)
            .env("PYTHONDONTWRITEBYTECODE","1")
            .args(["-B","-X","utf8","-c",r#"
import json,sys
from pathlib import Path
from blender_agent.orchestrator.component_first_use import comparison_dimensions,digest
from blender_agent.review_web.contract import comparison_rows
path=Path(sys.argv[1])
d=comparison_dimensions(path.parent,json.loads(path.read_bytes())['manifest']['items'][0]['dimensions'])
assumptions=d.pop('assumed')
d.update(assumed_ref={'sha256':digest(assumptions),'count':len(assumptions)},assumed_rows=comparison_rows({'assumed':assumptions})['assumptions'])
print(json.dumps(d))
"#])
            .arg(std::env::var("REVIEW_FIRST_USE_MANIFEST").unwrap())
            .output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut referenced = package.clone();
        referenced.first_use_comparison.as_mut().unwrap().dimensions =
            serde_json::from_slice(&output.stdout).unwrap();
        verify(&setup, referenced.clone()).await.unwrap();
        for field in ["sha256", "count", "rows"] {
            let mut changed = referenced.clone();
            let dimensions = &mut changed.first_use_comparison.as_mut().unwrap().dimensions;
            match field {
                "sha256" => dimensions["assumed_ref"]["sha256"] = serde_json::json!("0".repeat(64)),
                "count" => dimensions["assumed_ref"]["count"] = serde_json::json!(99999),
                _ => dimensions["assumed_rows"] = serde_json::json!(["changed"]),
            }
            assert!(verify(&setup, changed).await.is_err(), "{field}");
        }
        for field in ["candidate", "cad", "parts", "dimensions"] {
            let mut changed = package.clone();
            let comparison = changed.first_use_comparison.as_mut().unwrap();
            match field {
                "candidate" => comparison.proposal_sha256 = "0".repeat(64),
                "cad" => comparison.cad_required = !comparison.cad_required,
                "parts" => comparison.part_ids.push("foreign".into()),
                _ => comparison.dimensions["model_xyz_m"] = serde_json::json!([999, 999, 999]),
            }
            assert!(
                verify(&setup, changed)
                    .await
                    .unwrap_err()
                    .contains("component_first_use_comparison_binding"),
                "{field}"
            );
        }
    }
}
