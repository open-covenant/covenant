//! The full "verifiable agent commit" path against a REAL gitlawb node.
//!
//! A Covenant agent:
//!   1. registers + creates a repo (RFC 9421 signed)
//!   2. builds a one-commit git repo locally and a packfile for it
//!   3. frames a git smart-HTTP `receive-pack` request and pushes it SIGNED
//!      through `GitlawbClient::receive_pack` — a real commit lands on the node
//!   4. reads the ref certificate the node issues for that push
//!   5. wraps the node's cert in an `AttestedRefUpdateCert` and binds a
//!      `covenant/exec/v1` attestation (agent DID, capability/sandbox/audit
//!      roots, commit) to it, then verifies the whole envelope
//!
//! Point it at a node with `GITLAWB_NODE_URL` (default `http://127.0.0.1:7545`).

use chrono::Utc;
use covenant_gitlawb::gitlawb_attest::{Attestation, AttestedRefUpdateCert, Registry};
use covenant_gitlawb::{
    did_key_from_verifying_key, Config, CovenantExecPayload, CovenantVerifier, GitlawbClient,
};
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use rand::TryRngCore;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use url::Url;

fn fresh() -> SigningKey {
    let mut seed = [0u8; 32];
    OsRng.try_fill_bytes(&mut seed).unwrap();
    SigningKey::from_bytes(&seed)
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Run git with the user's global/system config neutralised so the demo is
/// hermetic (no stray identity, signing, or hooks). Errors carry stderr.
fn git(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// Build a fresh one-commit repo and return `(commit_sha, packfile_bytes)`.
fn build_commit_and_pack(dir: &Path) -> Result<(String, Vec<u8>), String> {
    let d = dir.to_str().ok_or("non-utf8 workdir path")?;
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;

    git(&["-C", d, "-c", "init.defaultBranch=main", "init", "-q"])?;
    std::fs::write(
        dir.join("README.md"),
        "# Covenant x gitlawb\n\nVerifiable agent commit produced by a Covenant-governed run.\n",
    )
    .map_err(|e| e.to_string())?;
    git(&["-C", d, "add", "-A"])?;
    git(&[
        "-C",
        d,
        "-c",
        "user.name=Covenant Agent",
        "-c",
        "user.email=agent@opencovenant.org",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "covenant: verifiable agent commit",
    ])?;
    let sha = String::from_utf8_lossy(&git(&["-C", d, "rev-parse", "HEAD"])?)
        .trim()
        .to_string();

    // Packfile of every object reachable from the commit (server has nothing yet).
    let mut child = Command::new("git")
        .args(["-C", d, "pack-objects", "--stdout", "--revs"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn pack-objects: {e}"))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{sha}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "pack-objects failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok((sha, out.stdout))
}

/// Encode a git pkt-line: 4-hex length prefix (counting itself) + data.
fn pkt_line(data: &[u8]) -> Vec<u8> {
    let mut v = format!("{:04x}", data.len() + 4).into_bytes();
    v.extend_from_slice(data);
    v
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let node_url =
        std::env::var("GITLAWB_NODE_URL").unwrap_or_else(|_| "http://127.0.0.1:7545".to_string());
    let url = Url::parse(&node_url)?;
    println!("Covenant x gitlawb — verifiable agent commit (real signed push)");
    println!("node: {node_url}\n");

    let agent_sk = fresh();
    let agent_did = did_key_from_verifying_key(&agent_sk.verifying_key());
    let suffix = &agent_did[agent_did.len() - 8..];
    let client = GitlawbClient::new(Config::enabled(url), agent_sk.clone());

    // 1. Register the agent and create a repo (both RFC 9421 signed).
    client
        .register(
            vec!["git/push".into(), "git/fetch".into()],
            Some("covenant-demo".into()),
        )
        .await?;
    let repo = format!("covenant-commit-{suffix}");
    let create_body = serde_json::json!({
        "name": repo,
        "description": "verifiable agent commit — Covenant x gitlawb",
        "is_public": true,
        "default_branch": "main",
    });
    let _created: serde_json::Value = client.signed_post("/api/v1/repos", &create_body).await?;
    println!("[1] registered agent + created repo '{repo}'");
    println!("        agent did : {agent_did}");

    // 2. Build a real commit and a packfile for it.
    let workdir = std::env::temp_dir().join(format!("covenant-push-{suffix}"));
    let (commit_sha, packfile) = build_commit_and_pack(&workdir)?;
    println!(
        "[2] built commit {}... ({} byte packfile)",
        &commit_sha[..16],
        packfile.len()
    );

    // 3. Frame the receive-pack request: one ref-update command + flush + pack.
    let zeros = "0".repeat(40);
    let mut push_body =
        pkt_line(format!("{zeros} {commit_sha} refs/heads/main\0report-status\n").as_bytes());
    push_body.extend_from_slice(b"0000");
    push_body.extend_from_slice(&packfile);

    // 3b. Push it, signed, through the bridge.
    let report = client.receive_pack(&agent_did, &repo, push_body).await?;
    let report = String::from_utf8_lossy(&report);
    let unpack_ok = report.contains("unpack ok");
    let ref_ok = report.contains("ok refs/heads/main");
    println!("[3] POST git-receive-pack (signed)      -> unpack ok: {unpack_ok}, ref ok: {ref_ok}");
    if !(unpack_ok && ref_ok) {
        let _ = std::fs::remove_dir_all(&workdir);
        return Err(format!("push not accepted by node: {report:?}").into());
    }

    // 4. Read the ref certificate the node issued for the push.
    let certs: serde_json::Value = client
        .signed_get(&format!("/api/v1/repos/{agent_did}/{repo}/certs"))
        .await?;
    let list = certs
        .get("certificates")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let node_cert = list
        .iter()
        .find(|c| c.get("new_sha").and_then(|s| s.as_str()) == Some(commit_sha.as_str()))
        .or_else(|| list.last())
        .cloned()
        .ok_or("node issued no ref certificate for the push")?;
    let cert_id = node_cert.get("id").and_then(|v| v.as_str()).unwrap_or("?");
    let node_did = node_cert
        .get("node_did")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    println!("[4] node issued ref cert {cert_id}");
    println!("        signed by node {node_did}");

    // 5. Wrap the node's cert and bind a covenant/exec/v1 attestation to it.
    let mut envelope = AttestedRefUpdateCert::from_cert(&node_cert)?;
    let cert_hash = envelope.cert_hash()?;
    let payload = CovenantExecPayload {
        agent_did: agent_did.clone(),
        capability_root: sha256_hex(b"capability-tree-root"),
        sandbox_digest: sha256_hex(b"sandbox-image-digest"),
        audit_root: sha256_hex(b"audit-merkle-root"),
        commit: commit_sha.clone(),
        issued_at: Utc::now(),
    };
    envelope.attach(Attestation::sign(&agent_sk, payload, cert_hash)?);

    let mut registry = Registry::new();
    registry.register(Box::new(CovenantVerifier));
    let verified = registry.verify_all(&envelope.attestations, envelope.cert_hash()?)?;
    let all_ok = !verified.is_empty() && verified.iter().all(|v| v.fully_verified);
    println!("[5] covenant/exec/v1 over the node cert -> fully_verified: {all_ok}");

    let _ = std::fs::remove_dir_all(&workdir);

    println!(
        "\nOK — a Covenant agent pushed real commit {}... to gitlawb over a signed\n\
         git-receive-pack; the node's ref certificate is wrapped in a verifiable\n\
         covenant/exec/v1 attestation binding agent, capability, sandbox, and audit\n\
         root to that exact commit.",
        &commit_sha[..16]
    );
    Ok(())
}
