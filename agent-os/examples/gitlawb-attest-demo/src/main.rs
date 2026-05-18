//! End-to-end roundtrip for External Attestation v1.
//!
//! Steps:
//!   1. Generate two ed25519 keypairs, one for the repo, one for the agent.
//!   2. Build a `gitlawb/ref-update/v1` cert pushing a commit on the repo.
//!   3. Attach a `covenant/exec/v1` attestation signed by the agent.
//!   4. Serialize the envelope and print it (this is what a node would see).
//!   5. Reparse the envelope, look up the cert hash, and run every
//!      attestation through a registry that only knows about
//!      `covenant/exec/v1`.
//!
//! Output is plain text, pipe to `jq` to inspect the envelope.

use chrono::Utc;
use covenant_gitlawb::{
    attach_covenant_attestation, did_key_from_verifying_key, CovenantExecPayload, CovenantVerifier,
    RefUpdateCert,
};
use ed25519_dalek::SigningKey;
use gitlawb_attest::{AttestedRefUpdateCert, Registry};
use rand::rngs::OsRng;
use rand::TryRngCore;
use sha2::{Digest, Sha256};

fn fresh() -> SigningKey {
    let mut seed = [0u8; 32];
    OsRng.try_fill_bytes(&mut seed).unwrap();
    SigningKey::from_bytes(&seed)
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    for b in out.iter() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo_sk = fresh();
    let agent_sk = fresh();
    let repo_did = did_key_from_verifying_key(&repo_sk.verifying_key());
    let agent_did = did_key_from_verifying_key(&agent_sk.verifying_key());

    println!("== keys ==");
    println!("repo   did: {repo_did}");
    println!("agent  did: {agent_did}");
    println!();

    // 1. Build the ref-update cert.
    let from_sha = "0".repeat(64);
    let commit_sha = sha256_hex(b"a commit blob produced by the agent");
    let cert = RefUpdateCert::sign_new(
        &repo_sk,
        repo_did.clone(),
        "refs/heads/main",
        from_sha.clone(),
        commit_sha.clone(),
        /*seq*/ 1,
        /*nonce*/ "audit-0001",
    )?;

    // 2. Build and attach the Covenant attestation.
    let payload = CovenantExecPayload {
        agent_did: agent_did.clone(),
        capability_root: sha256_hex(b"capability-tree-root-bytes"),
        sandbox_digest: sha256_hex(b"sandbox-image-digest-bytes"),
        audit_root: sha256_hex(b"audit-merkle-root-bytes"),
        commit: commit_sha.clone(),
        issued_at: Utc::now(),
    };
    let envelope = attach_covenant_attestation(&cert, payload, &agent_sk)?;

    // 3. Serialize and print.
    let wire = serde_json::to_string_pretty(&envelope)?;
    println!("== envelope on the wire ==");
    println!("{wire}");
    println!();

    // 4. Reparse and verify end-to-end.
    let parsed: AttestedRefUpdateCert = serde_json::from_str(&wire)?;
    let cert_hash = parsed.cert_hash()?;

    let mut registry = Registry::new();
    registry.register(Box::new(CovenantVerifier));

    let verified = registry.verify_all(&parsed.attestations, cert_hash)?;

    println!("== verification ==");
    for v in &verified {
        println!(
            "  type={} signer={} fully_verified={}",
            v.type_, v.signer, v.fully_verified
        );
    }
    println!();

    // 5. Cross-check the payload <-> cert binding the registry does not enforce.
    for att in &parsed.attestations {
        if att.type_ == "covenant/exec/v1" {
            let p: CovenantExecPayload = att.payload_as()?;
            assert_eq!(
                p.commit, commit_sha,
                "exec-attestation commit must match cert.to"
            );
        }
    }

    println!("== OK, envelope verifies and binds to commit {commit_sha} ==");
    Ok(())
}
