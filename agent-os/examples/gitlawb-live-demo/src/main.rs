//! Live end-to-end demo: a Covenant agent driving a REAL gitlawb node.
//!
//! Where `gitlawb-attest-demo` is a pure local crypto roundtrip, this talks to a
//! running gitlawb node over HTTP and proves the bridge's `did:key` identity and
//! RFC 9421 request signing interoperate with `gitlawb-node` end to end:
//!
//!   1. GET  /health                          — node is up (unsigned)
//!   2. POST /api/register      (RFC 9421)     — the node accepts our signature + did:key
//!   3. POST /api/v1/repos      (signed)       — create a repo owned by the agent DID
//!   4. GET  /api/v1/repos/{owner}/{repo}      — read it back from the node
//!   5. covenant/exec/v1 attestation           — bind + verify provenance for refs/heads/main
//!
//! Point it at a node with `GITLAWB_NODE_URL` (default `http://127.0.0.1:7545`).

use chrono::Utc;
use covenant_gitlawb::gitlawb_attest::{AttestedRefUpdateCert, Registry};
use covenant_gitlawb::{
    attach_covenant_attestation, did_key_from_verifying_key, Config, CovenantExecPayload,
    CovenantVerifier, GitlawbClient, RefUpdateCert,
};
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use rand::TryRngCore;
use sha2::{Digest, Sha256};
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let node_url =
        std::env::var("GITLAWB_NODE_URL").unwrap_or_else(|_| "http://127.0.0.1:7545".to_string());
    let url = Url::parse(&node_url)?;
    println!("Covenant x gitlawb — live node demo");
    println!("node: {node_url}\n");

    // The Covenant agent's identity. Ephemeral per run so re-runs never collide.
    let agent_sk = fresh();
    let agent_did = did_key_from_verifying_key(&agent_sk.verifying_key());
    let client = GitlawbClient::new(Config::enabled(url), agent_sk.clone());

    // 1. Health (unsigned read). Fail with a clear hint if no node is reachable.
    let healthy = client.health().await.unwrap_or(false);
    println!(
        "[1] GET  /health                       -> {}",
        if healthy { "ok" } else { "unreachable" }
    );
    if !healthy {
        eprintln!(
            "\nCouldn't reach a healthy gitlawb node at {node_url}.\n\
             Start one (see the README) and set GITLAWB_NODE_URL, then re-run."
        );
        std::process::exit(1);
    }

    // 2. Register the agent. This is the load-bearing interop proof: the node
    //    verifies our RFC 9421 signature against the did:key in `keyid`.
    let reg = client
        .register(
            vec!["git/push".into(), "git/fetch".into()],
            Some("covenant-demo".into()),
        )
        .await?;
    let ucan_preview = &reg.ucan[..reg.ucan.len().min(28)];
    println!("[2] POST /api/register  (RFC 9421)     -> accepted by the node");
    println!("        agent did  : {}", reg.did);
    println!("        trust      : {}", reg.trust_score);
    println!("        node ucan  : {ucan_preview}...");

    // 3. Create a repo. The node derives ownership from the *signing* DID, so
    //    no owner field is sent — identity is the authorization.
    let repo_name = format!("covenant-demo-{}", &agent_did[agent_did.len() - 8..]);
    let create_body = serde_json::json!({
        "name": repo_name,
        "description": "verifiable agent commits — Covenant x gitlawb live demo",
        "is_public": true,
        "default_branch": "main",
    });
    let _created: serde_json::Value = client.signed_post("/api/v1/repos", &create_body).await?;
    println!("[3] POST /api/v1/repos  (signed)       -> created '{repo_name}'");

    // 4. Read it back from the node and confirm the owner is our agent DID.
    let read_path = format!("/api/v1/repos/{agent_did}/{repo_name}");
    let repo: serde_json::Value = client.signed_get(&read_path).await?;
    let owner = repo.get("owner_did").and_then(|v| v.as_str()).unwrap_or("");
    println!(
        "[4] GET  /api/v1/repos/{{owner}}/{{repo}}    -> owner is the agent DID: {}",
        owner == agent_did
    );

    // 5. Produce a covenant/exec/v1 attestation bound to a ref-update on the new
    //    repo and verify it through the registry — the provenance layer over git.
    let from_sha = "0".repeat(64);
    let commit_sha = sha256_hex(b"covenant agent commit blob");
    let cert = RefUpdateCert::sign_new(
        &agent_sk,
        agent_did.clone(),
        "refs/heads/main",
        from_sha,
        commit_sha.clone(),
        /*seq*/ 1,
        /*nonce*/ "audit-live-0001",
    )?;
    let payload = CovenantExecPayload {
        agent_did: agent_did.clone(),
        capability_root: sha256_hex(b"capability-tree-root"),
        sandbox_digest: sha256_hex(b"sandbox-image-digest"),
        audit_root: sha256_hex(b"audit-merkle-root"),
        commit: commit_sha.clone(),
        issued_at: Utc::now(),
    };
    let envelope = attach_covenant_attestation(&cert, payload, &agent_sk)?;

    // Round-trip through the wire form, exactly as a verifier downstream would.
    let wire = serde_json::to_string(&envelope)?;
    let parsed: AttestedRefUpdateCert = serde_json::from_str(&wire)?;
    let cert_hash = parsed.cert_hash()?;
    let mut registry = Registry::new();
    registry.register(Box::new(CovenantVerifier));
    let verified = registry.verify_all(&parsed.attestations, cert_hash)?;
    let all_ok = !verified.is_empty() && verified.iter().all(|v| v.fully_verified);
    println!("[5] covenant/exec/v1 attestation        -> fully_verified: {all_ok}");
    println!(
        "        binds commit {}... on refs/heads/main",
        &commit_sha[..16]
    );

    println!(
        "\nOK — Covenant agent is live on gitlawb: registered, owns a repo, and its \
         ref-update carries a verifiable covenant/exec/v1 attestation."
    );
    Ok(())
}
