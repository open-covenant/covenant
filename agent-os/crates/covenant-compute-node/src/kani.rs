//! Typed Kani checks. The node selects a pinned tool image and resource profile;
//! repository code never supplies the host command or the evidence envelope.
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::Engine as _;
use covenant_compute_protocol::{
    kani_checker_model, kani_command_label, sha256_hex, validate_kani_evidence, CommandOutcome,
    KaniCheckSpec,
};
use covenant_mcp::Content;
use flate2::{write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::executor::{configure_process_group, ExecutorError};

pub const RAW_LIMIT: usize = 16 * 1024 * 1024;
pub const COMPRESSED_LIMIT: usize = 4 * 1024 * 1024;
const JSON_LIMIT: usize = 24 * 1024 * 1024;
const HOST_HEADROOM_MIB: u64 = 2048;
const MIB: u64 = 1024 * 1024;
const CAPTURE: &str = include_str!("kani_capture.py");

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KaniProfile {
    pub image: String,
    pub memory_mib: u32,
    pub cpus: u32,
    pub scratch_mib: u32,
}

impl KaniProfile {
    pub fn label(&self) -> String {
        kani_checker_model(&self.image, self.memory_mib, self.cpus, self.scratch_mib)
    }

    pub fn matches(&self, image: &str, spec: &KaniCheckSpec) -> bool {
        self.image == image
            && self.memory_mib == spec.memory_mib
            && self.cpus == spec.cpus
            && self.scratch_mib == spec.scratch_mib
    }

    fn validate(&self) -> Result<(), String> {
        if !pinned_image(&self.image) {
            return Err(
                "Kani images require sha256:<64 lowercase hex> or repository@sha256:<digest>"
                    .into(),
            );
        }
        if self.memory_mib < 1024
            || self.memory_mib > 262_144
            || self.cpus == 0
            || self.cpus > 256
            || self.scratch_mib < 256
            || self.scratch_mib >= self.memory_mib
            || self.scratch_mib > 65_536
        {
            return Err("Kani profile requires 1024..262144 MiB RAM, 1..256 CPUs and 256..65536 MiB scratch below RAM".into());
        }
        Ok(())
    }
}

pub fn pinned_image(image: &str) -> bool {
    let digest = if let Some(d) = image.strip_prefix("sha256:") {
        Some(d)
    } else {
        image.rsplit_once("@sha256:").and_then(|(repo, d)| {
            (!repo.is_empty() && !repo.chars().any(char::is_whitespace)).then_some(d)
        })
    };
    digest.is_some_and(|d| {
        d.len() == 64
            && d.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

pub fn parse_profiles(raw: &str) -> Result<Vec<KaniProfile>, String> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    if raw.len() > 32 * 1024 {
        return Err("Kani profiles exceed 32 KiB".into());
    }
    let profiles: Vec<KaniProfile> =
        serde_json::from_str(raw).map_err(|e| format!("Kani profiles: {e}"))?;
    if profiles.len() > 16 {
        return Err("at most 16 Kani profiles may be advertised".into());
    }
    let mut seen = std::collections::HashSet::new();
    for profile in &profiles {
        profile.validate()?;
        if !seen.insert(profile.label()) {
            return Err("duplicate Kani resource profile".into());
        }
    }
    Ok(profiles)
}

#[derive(Debug)]
struct Capacity {
    available_bytes: u64,
    cpus: usize,
}

fn validate_capacity(profile: &KaniProfile, capacity: &Capacity) -> Result<(), String> {
    let needed = (u64::from(profile.memory_mib) + HOST_HEADROOM_MIB) * MIB;
    if capacity.available_bytes < needed {
        return Err(format!("Kani profile needs {} MiB plus {HOST_HEADROOM_MIB} MiB host headroom; {} MiB available", profile.memory_mib, capacity.available_bytes / MIB));
    }
    if u64::from(profile.cpus) > capacity.cpus as u64 {
        return Err(format!(
            "Kani profile needs {} CPUs; {} available",
            profile.cpus, capacity.cpus
        ));
    }
    Ok(())
}

fn memory_available(meminfo: &str) -> Result<u64, String> {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse::<u64>().ok())
        .and_then(|v| v.checked_mul(1024))
        .ok_or_else(|| "cannot establish Linux MemAvailable".into())
}

fn host_capacity() -> Result<Capacity, String> {
    if !cfg!(target_os = "linux") {
        return Err("Kani profiles currently require a Linux cgroup-v2 host".into());
    }
    let mut available = memory_available(
        &std::fs::read_to_string("/proc/meminfo")
            .map_err(|e| format!("read memory capacity: {e}"))?,
    )?;
    // Account for every enclosing cgroup: a process can have plenty of host
    // RAM visible while its service's parent has a much smaller allowance.
    let membership =
        std::fs::read_to_string("/proc/self/cgroup").map_err(|e| format!("read cgroup: {e}"))?;
    let relative = membership
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .ok_or("Kani requires cgroup v2")?;
    let mut group = PathBuf::from("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
    let root = Path::new("/sys/fs/cgroup");
    loop {
        if group == root {
            break;
        } // The root cgroup has no memory.max controller file.
        let max = std::fs::read_to_string(group.join("memory.max"))
            .map_err(|e| format!("read memory.max: {e}"))?;
        if max.trim() != "max" {
            let maximum = max
                .trim()
                .parse::<u64>()
                .map_err(|_| "invalid memory.max")?;
            let used = std::fs::read_to_string(group.join("memory.current"))
                .map_err(|e| format!("read memory.current: {e}"))?
                .trim()
                .parse::<u64>()
                .map_err(|_| "invalid memory.current")?;
            available = available.min(maximum.saturating_sub(used));
        }
        if group == root {
            break;
        }
        if !group.pop() || !group.starts_with(root) {
            return Err("invalid cgroup membership path".into());
        }
    }
    let cpus = std::thread::available_parallelism()
        .map_err(|e| format!("read CPU allowance: {e}"))?
        .get();
    Ok(Capacity {
        available_bytes: available,
        cpus,
    })
}

async fn docker_json(runtime: &str, args: &[&str]) -> Result<Value, String> {
    let out = tokio::time::timeout(
        Duration::from_secs(15),
        Command::new(runtime)
            .args(args)
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| "container inspect timed out")?
    .map_err(|e| format!("container inspect: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "container inspection failed: {}",
            String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(512)
                .collect::<String>()
        ));
    }
    if out.stdout.len() > 1024 * 1024 {
        return Err("container inspection exceeded 1 MiB".into());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("container inspect JSON: {e}"))
}

async fn inspect_image(runtime: &str, image: &str) -> Result<String, String> {
    if !pinned_image(image) {
        return Err("Kani requires an immutable image reference".into());
    }
    let value = docker_json(runtime, &["image", "inspect", image]).await?;
    let entry = value
        .as_array()
        .filter(|v| v.len() == 1)
        .and_then(|v| v.first())
        .ok_or("image inspect returned no unique image")?;
    let id = entry
        .get("Id")
        .and_then(Value::as_str)
        .filter(|s| s.starts_with("sha256:") && pinned_image(s))
        .ok_or("image has no immutable ID")?;
    // Docker accepts both configuration IDs and registry manifest digests.
    // Inspecting the exact digest reference is the binding; never re-run by tag.
    if image.starts_with("sha256:") && id != image {
        return Err("Docker resolved a different image ID".into());
    }
    Ok(id.to_string())
}

pub async fn validate_startup(runtime: &str, profiles: &[KaniProfile]) -> Result<(), String> {
    if profiles.is_empty() {
        return Ok(());
    }
    let host = host_capacity()?;
    let info = docker_json(runtime, &["info", "--format", "{{json .}}"]).await?;
    if info.get("OSType").and_then(Value::as_str) != Some("linux")
        || info.get("CgroupVersion").and_then(Value::as_str) != Some("2")
    {
        return Err("Kani requires a Linux Docker engine with cgroup v2".into());
    }
    let capacity = Capacity {
        available_bytes: host.available_bytes.min(
            info.get("MemTotal")
                .and_then(Value::as_u64)
                .ok_or("Docker did not report memory capacity")?,
        ),
        cpus: host.cpus.min(
            info.get("NCPU")
                .and_then(Value::as_u64)
                .ok_or("Docker did not report CPUs")? as usize,
        ),
    };
    for profile in profiles {
        profile.validate()?;
        validate_capacity(profile, &capacity)?;
        inspect_image(runtime, &profile.image).await?;
    }
    Ok(())
}

pub struct KaniRequest<'a> {
    pub runtime: &'a str,
    pub image: &'a str,
    pub profiles: &'a [KaniProfile],
    pub spec: &'a KaniCheckSpec,
    pub tree: &'a Path,
    pub work_dir: &'a Path,
    pub deps_volume: Option<&'a str>,
    pub name: &'a str,
    pub job_id: uuid::Uuid,
    pub task_job_id: uuid::Uuid,
    pub repo_commit: &'a str,
    pub tree_hash: &'a str,
    pub patch_sha256: &'a str,
    pub hidden_sha256: Option<&'a str>,
    pub until: Instant,
}

pub struct KaniRun {
    pub outcome: CommandOutcome,
    pub evidence: Content,
}

fn container_args(req: &KaniRequest<'_>, request_dir: &Path, owner: &str) -> Vec<String> {
    let spec = req.spec;
    let mut args: Vec<String> = [
        "run",
        "--pull",
        "never",
        "--name",
        req.name,
        "--network",
        "none",
        "--read-only",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--memory",
        &format!("{}m", spec.memory_mib),
        "--memory-swap",
        &format!("{}m", spec.memory_mib),
        "--cpus",
        &spec.cpus.to_string(),
        "--pids-limit",
        "512",
        "--user",
        owner,
        "--tmpfs",
        &format!(
            "/scratch:rw,nosuid,nodev,size={}m,uid={},gid={},mode=0700",
            spec.scratch_mib,
            owner.split(':').next().unwrap_or("0"),
            owner.split(':').nth(1).unwrap_or("0")
        ),
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,size=16m",
        "-v",
        &format!("{}:/work:ro", req.tree.display()),
        "-v",
        &format!("{}:/request:ro", request_dir.display()),
        "--tmpfs",
        &format!(
            "/evidence:rw,nosuid,nodev,size=20m,uid={},gid={},mode=0700",
            owner.split(':').next().unwrap_or("0"),
            owner.split(':').nth(1).unwrap_or("0")
        ),
        "-w",
        "/work",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    for env in [
        "PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin",
        "HOME=/tmp",
        "KANI_HOME=/opt/kani",
        "RUSTUP_HOME=/usr/local/rustup",
        "CARGO_HOME=/usr/local/cargo",
        "CARGO_TARGET_DIR=/scratch/target",
        "TMPDIR=/scratch",
        "CARGO_NET_OFFLINE=true",
        "CARGO_TERM_COLOR=never",
        "PYTHONDONTWRITEBYTECODE=1",
        "PYTHONPATH=",
        "PYTHONSAFEPATH=1",
        "RUSTFLAGS=",
        "CARGO_ENCODED_RUSTFLAGS=",
        "RUSTDOCFLAGS=",
        "RUSTC_WRAPPER=",
        "RUSTC_WORKSPACE_WRAPPER=",
    ] {
        args.extend(["-e".into(), env.into()]);
    }
    if let Some(volume) = req.deps_volume {
        args.extend([
            "-v".into(),
            format!("{volume}:/deps:ro"),
            "-e".into(),
            "CARGO_HOME=/deps/cargo".into(),
        ]);
    }
    args.extend([
        "--entrypoint".into(),
        "/usr/bin/python3".into(),
        req.image.into(),
        "-I".into(),
        "-c".into(),
        CAPTURE.into(),
    ]);
    args
}

async fn read_bounded(
    mut stream: impl tokio::io::AsyncRead + Unpin,
    cap: usize,
) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let mut overflow = false;
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(bytes.len());
                bytes.extend_from_slice(&buf[..n.min(room)]);
                overflow |= n > room;
            }
        }
    }
    (bytes, overflow)
}

async fn remove(runtime: &str, name: &str) {
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(runtime)
            .args(["rm", "--force", name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await;
}

fn capture_artifacts(packet: &Value) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let encoded = packet
        .get("artifacts")
        .and_then(Value::as_object)
        .ok_or("capture has no artifact map")?;
    let mut artifacts = BTreeMap::new();
    let mut total = 0usize;
    for (name, value) in encoded {
        if ![
            "result.json",
            "stdout.log",
            "stderr.log",
            "kani-export.json",
        ]
        .contains(&name.as_str())
        {
            return Err("unexpected artifact name".into());
        }
        let value = value.as_str().ok_or("artifact is not base64 text")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(|_| "invalid captured artifact base64")?;
        total = total
            .checked_add(bytes.len())
            .ok_or("artifact lengths overflow")?;
        if total > RAW_LIMIT {
            return Err("Kani raw evidence exceeds 16 MiB".into());
        }
        artifacts.insert(name.clone(), bytes);
    }
    Ok(artifacts)
}

fn pack_artifacts(artifacts: &BTreeMap<String, Vec<u8>>) -> Result<(String, String, u64), String> {
    let raw_bytes: usize = artifacts.values().map(Vec::len).sum();
    if raw_bytes > RAW_LIMIT {
        return Err("Kani raw evidence exceeds 16 MiB".into());
    }
    let packed: BTreeMap<_, _> = artifacts
        .iter()
        .map(|(k, v)| (k, base64::engine::general_purpose::STANDARD.encode(v)))
        .collect();
    let json = serde_json::to_vec(&packed).map_err(|e| e.to_string())?;
    if json.len() > JSON_LIMIT {
        return Err("Kani artifact JSON exceeds 24 MiB".into());
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&json).map_err(|e| e.to_string())?;
    let compressed = encoder.finish().map_err(|e| e.to_string())?;
    if compressed.len() > COMPRESSED_LIMIT {
        return Err("Kani compressed evidence exceeds 4 MiB".into());
    }
    Ok((
        base64::engine::general_purpose::STANDARD.encode(&compressed),
        sha256_hex(&compressed),
        raw_bytes as u64,
    ))
}

fn validate_report_artifacts(
    report: &Value,
    artifacts: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    let entries = report
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or("runner report has no artifact manifest")?;
    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .ok_or("artifact has no name")?;
        if name == "result.json" || !seen.insert(name) {
            return Err("invalid or duplicate reported artifact".into());
        }
        let bytes = artifacts
            .get(name)
            .ok_or("reported evidence file is missing")?;
        if entry.get("bytes").and_then(Value::as_u64) != Some(bytes.len() as u64)
            || entry.get("sha256").and_then(Value::as_str) != Some(sha256_hex(bytes).as_str())
        {
            return Err("reported evidence bytes or digest mismatch".into());
        }
    }
    if artifacts
        .keys()
        .any(|k| k != "result.json" && !seen.contains(k.as_str()))
    {
        return Err("unreported evidence file".into());
    }
    if !seen.contains("stdout.log") || !seen.contains("stderr.log") {
        return Err("runner report omitted full logs".into());
    }
    Ok(())
}

pub async fn run(req: KaniRequest<'_>) -> Result<KaniRun, ExecutorError> {
    let fail = |message: String| ExecutorError::Failed(format!("Kani: {message}"));
    let profile = req
        .profiles
        .iter()
        .find(|p| p.matches(req.image, req.spec))
        .ok_or_else(|| fail("no admitted resource profile matches this proof".into()))?;
    validate_capacity(profile, &host_capacity().map_err(fail)?).map_err(fail)?;
    let image_id = inspect_image(req.runtime, req.image).await.map_err(fail)?;
    let request = tempfile::Builder::new()
        .prefix("kani-request-")
        .tempdir_in(req.work_dir)
        .map_err(|e| fail(e.to_string()))?;
    std::fs::write(
        request.path().join("plan.json"),
        serde_json::to_vec(&req.spec.plan).map_err(|e| fail(e.to_string()))?,
    )
    .map_err(|e| fail(e.to_string()))?;
    let owner = std::fs::metadata(req.tree)
        .map(|m| format!("{}:{}", m.uid(), m.gid()))
        .map_err(|e| fail(e.to_string()))?;
    let args = container_args(&req, request.path(), &owner);
    let started = Instant::now();
    let mut command = Command::new(req.runtime);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| fail(format!("start container: {e}")))?;
    let stdout = tokio::spawn(read_bounded(
        child.stdout.take().expect("stdout piped"),
        JSON_LIMIT,
    ));
    let stderr = tokio::spawn(read_bounded(
        child.stderr.take().expect("stderr piped"),
        64 * 1024,
    ));
    let remaining = req.until.saturating_duration_since(Instant::now());
    let (exit_code, timed_out) = match tokio::time::timeout(remaining, child.wait()).await {
        Ok(Ok(status)) => (status.code().unwrap_or(-1), false),
        Ok(Err(e)) => {
            remove(req.runtime, req.name).await;
            return Err(fail(e.to_string()));
        }
        Err(_) => {
            let _ = tokio::time::timeout(
                Duration::from_secs(10),
                Command::new(req.runtime)
                    .args(["kill", req.name])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status(),
            )
            .await;
            let _ = child.kill().await;
            (-1, true)
        }
    };
    let state = docker_json(
        req.runtime,
        &["inspect", "--format", "{{json .State}}", req.name],
    )
    .await;
    remove(req.runtime, req.name).await;
    let (stdout, stdout_overflow) = stdout.await.map_err(|e| fail(e.to_string()))?;
    let (stderr, stderr_overflow) = stderr.await.map_err(|e| fail(e.to_string()))?;
    let state = state.map_err(fail)?;
    let oom = state
        .get("OOMKilled")
        .and_then(Value::as_bool)
        .ok_or_else(|| fail("Docker omitted OOM status".into()))?;
    if stdout_overflow || stderr_overflow {
        return Err(fail(
            "Kani capture output exceeded its bounded frame".into(),
        ));
    }
    if exit_code == 125 {
        return Err(fail(format!(
            "container engine failure: {}",
            String::from_utf8_lossy(&stderr)
        )));
    }
    let packet: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
    let mut artifacts = if packet.get("artifacts").is_some() {
        capture_artifacts(&packet).map_err(fail)?
    } else {
        BTreeMap::new()
    };
    let control_stdout = packet
        .get("control_stdout_b64")
        .and_then(Value::as_str)
        .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
        .unwrap_or_default();
    let control_stderr = packet
        .get("control_stderr_b64")
        .and_then(Value::as_str)
        .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
        .unwrap_or_default();
    if control_stdout.len() > 64 * 1024 || control_stderr.len() > 64 * 1024 {
        return Err(fail("runner control output exceeded 64 KiB".into()));
    }
    let mut report: Value = artifacts.get("result.json").and_then(|b| serde_json::from_slice(b).ok()).filter(Value::is_object).unwrap_or_else(|| json!({"schema":"covenant.kani.result.v1","harness":req.spec.plan.harness,"outcome":"tool_error","accepted":false,"reason":"runner produced no valid result report","artifacts":[]}));
    if timed_out || oom || !artifacts.contains_key("result.json") {
        report["outcome"] = json!(if oom {
            "out_of_memory"
        } else if timed_out {
            "timeout"
        } else {
            "tool_error"
        });
        report["accepted"] = json!(false);
        report["reason"] = json!("outer executor stopped the proof or received no complete report");
        report["evidence_complete"] = json!(false);
        // Preserve partial original logs; control streams are the full evidence
        // available when the runner never started and are not proof output.
        artifacts
            .entry("stdout.log".into())
            .or_insert(if control_stdout.is_empty() {
                stdout
            } else {
                control_stdout.clone()
            });
        artifacts
            .entry("stderr.log".into())
            .or_insert(if control_stderr.is_empty() {
                stderr
            } else {
                control_stderr
            });
        report["artifacts"] = json!(artifacts
            .iter()
            .filter(|(name, _)| name.as_str() != "result.json")
            .map(
                |(name, bytes)| json!({"name":name,"bytes":bytes.len(),"sha256":sha256_hex(bytes)})
            )
            .collect::<Vec<_>>());
        report["exit_code"] = json!(exit_code);
        report["timed_out"] = json!(timed_out);
        report["oom_killed"] = json!(oom);
        artifacts.insert(
            "result.json".into(),
            serde_json::to_vec(&report).map_err(|e| fail(e.to_string()))?,
        );
    } else {
        validate_report_artifacts(&report, &artifacts).map_err(fail)?;
        let control: Value = serde_json::from_slice(&control_stdout)
            .map_err(|e| fail(format!("runner stdout is not a JSON report: {e}")))?;
        if control != report {
            return Err(fail("runner stdout and result.json disagree".into()));
        }
        if report.get("accepted").and_then(Value::as_bool) == Some(true) && exit_code != 0 {
            return Err(fail(
                "runner claims acceptance after nonzero container exit".into(),
            ));
        }
    }
    let (artifacts_gzip_b64, artifacts_sha256, raw_bytes) =
        pack_artifacts(&artifacts).map_err(fail)?;
    let evidence = json!({
        "schema":"covenant.kani.execution.v1", "plan":req.spec.plan, "image":req.image,
        "memory_mib":req.spec.memory_mib, "cpus":req.spec.cpus, "scratch_mib":req.spec.scratch_mib,
        "job_id":req.job_id,"task_job_id":req.task_job_id,"repo_commit":req.repo_commit,
        "tree_hash":req.tree_hash,"patch_sha256":req.patch_sha256,"hidden_sha256":req.hidden_sha256,"image_id":image_id,
        "tool_pin":{"kani":"0.68.0","cbmc":"6.11.0","rust_toolchain":"nightly-2026-08-21-x86_64-unknown-linux-gnu"},
        "report":report,"artifacts_gzip_b64":artifacts_gzip_b64,"artifacts_sha256":artifacts_sha256,"raw_bytes":raw_bytes,
    });
    let accepted = validate_kani_evidence(req.spec, req.image, &evidence)
        .map_err(|e| fail(format!("evidence validation: {e}")))?;
    Ok(KaniRun {
        outcome: CommandOutcome {
            command: kani_command_label(&req.spec.plan),
            exit_code: if accepted { 0 } else { 1 },
            duration_ms: started.elapsed().as_millis() as u64,
            timed_out,
            output_tail: serde_json::to_string(
                &json!({"outcome":report["outcome"],"accepted":accepted,"reason":report["reason"]}),
            )
            .unwrap_or_default()
            .chars()
            .take(1024)
            .collect(),
        },
        evidence: Content::json(json!({"kani_execution":evidence})),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> KaniProfile {
        KaniProfile {
            image: format!("sha256:{}", "a".repeat(64)),
            memory_mib: 8192,
            cpus: 4,
            scratch_mib: 1024,
        }
    }
    #[test]
    fn image_and_profile_admission_are_fail_closed() {
        assert!(pinned_image(&profile().image));
        assert!(pinned_image(&format!(
            "registry/kani@sha256:{}",
            "a".repeat(64)
        )));
        for image in ["kani:0.68.0", "sha256:bad", "sha256:", "@sha256:aaaa"] {
            assert!(!pinned_image(image));
        }
        let p = serde_json::to_string(&vec![profile()]).unwrap();
        assert_eq!(parse_profiles(&p).unwrap().len(), 1);
        let duplicate = serde_json::to_string(&vec![profile(), profile()]).unwrap();
        assert!(parse_profiles(&duplicate).is_err());
        assert!(parse_profiles(
            "[{\"image\":\"kani:latest\",\"memory_mib\":8192,\"cpus\":4,\"scratch_mib\":1024}]"
        )
        .is_err());
    }
    #[test]
    fn admission_preserves_host_headroom_and_cpu_allowance() {
        assert!(validate_capacity(
            &profile(),
            &Capacity {
                available_bytes: 10 * 1024 * MIB,
                cpus: 4
            }
        )
        .is_ok());
        assert!(validate_capacity(
            &profile(),
            &Capacity {
                available_bytes: 10 * 1024 * MIB - 1,
                cpus: 4
            }
        )
        .is_err());
        assert!(validate_capacity(
            &profile(),
            &Capacity {
                available_bytes: 32 * 1024 * MIB,
                cpus: 3
            }
        )
        .is_err());
        assert_eq!(
            memory_available("MemTotal: 10 kB\nMemAvailable: 3 kB\n").unwrap(),
            3072
        );
        assert!(memory_available("MemFree: 3 kB").is_err());
    }
    #[test]
    fn evidence_rejects_links_and_manifest_lies() {
        assert!(capture_artifacts(&json!({"artifacts":{"../outside":""}})).is_err());
        assert!(capture_artifacts(&json!({"artifacts":{"stdout.log":"notbase64!"}})).is_err());
        let artifacts = BTreeMap::from([
            ("stdout.log".into(), b"complete log".to_vec()),
            ("stderr.log".into(), Vec::new()),
        ]);
        let report = json!({"artifacts":[{"name":"stdout.log","bytes":12,"sha256":"wrong"}]});
        assert!(validate_report_artifacts(&report, &artifacts).is_err());
        let (_, digest, n) = pack_artifacts(&artifacts).unwrap();
        assert_eq!(n, 12);
        assert_eq!(digest.len(), 64);
    }
    #[test]
    fn proof_container_has_no_host_write_mount_or_network() {
        let spec = KaniCheckSpec {
            plan: covenant_compute_protocol::KaniPlan {
                schema: "covenant.kani.plan.v1".into(),
                harness: "proofs::route".into(),
                manifest_path: "Cargo.toml".into(),
                features: vec!["fuzz".into()],
                tests: false,
                timeout_secs: 60,
                unwind: None,
                expected_outcome: "proven".into(),
                expected_assertion: None,
                required_covers: vec!["cover condition: route reachable".into()],
            },
            memory_mib: 8192,
            cpus: 4,
            scratch_mib: 1024,
            check_price_micro_usdc: 50_000,
        };
        let image = profile().image;
        let req = KaniRequest {
            runtime: "docker",
            image: &image,
            profiles: &[],
            spec: &spec,
            tree: Path::new("/host/source"),
            work_dir: Path::new("/host/work"),
            deps_volume: Some("job-deps"),
            name: "proof",
            job_id: uuid::Uuid::nil(),
            task_job_id: uuid::Uuid::nil(),
            repo_commit: "commit",
            tree_hash: "tree",
            patch_sha256: "patch",
            hidden_sha256: None,
            until: Instant::now(),
        };
        let args = container_args(&req, Path::new("/host/request"), "1000:1000");
        let value = |flag: &str| args[args.iter().position(|s| s == flag).unwrap() + 1].as_str();
        assert_eq!(value("--network"), "none");
        assert_eq!(value("--memory"), "8192m");
        assert_eq!(value("--memory-swap"), "8192m");
        assert_eq!(value("--cpus"), "4");
        assert!(args.contains(&"--read-only".into()));
        assert!(args.contains(&"CARGO_NET_OFFLINE=true".into()));
        for pair in args.windows(2).filter(|p| p[0] == "-v") {
            assert!(pair[1].ends_with(":ro"));
        }
        assert!(args
            .iter()
            .any(|a| a.starts_with("/evidence:rw,") && a.contains("size=20m")));
        assert!(args
            .iter()
            .any(|a| a.starts_with("/scratch:rw,") && a.contains("size=1024m")));
        assert_eq!(value("--entrypoint"), "/usr/bin/python3");
    }

    #[test]
    fn evidence_cap_is_on_original_bytes() {
        let artifacts = BTreeMap::from([("stdout.log".into(), vec![0; RAW_LIMIT + 1])]);
        assert!(pack_artifacts(&artifacts).is_err());
    }
}
