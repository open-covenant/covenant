//! Verify a MagicBlock Private ER's Intel TDX enclave off chain (DCAP via the
//! Phala quote-verification library) and relay the result into a signed Covenant
//! attestation. Optionally binds an agent and its on-chain provenance root into
//! the quote challenge, so the attestation proves a specific agent's verifiable
//! record came from a genuine, attested enclave.
//!
//! Trust model: the attestation carries the DCAP result, not the raw quote. The
//! attester's ed25519 signature is the anchor, so a verifier MUST pin which keys
//! it trusts; [`verify_attestation`] fails closed without a trusted set.

use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BIND_DOMAIN: &[u8] = b"covenant.tee.bind.v1";
const PCCS: &str = "https://pccs.phala.network";
const ACCEPTABLE: &[&str] = &["UpToDate"];

#[derive(Serialize, Deserialize, Clone)]
pub struct Er {
    pub rpc_url: String,
    pub validator: Option<String>,
    pub tee: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Enclave {
    pub status: String,
    pub advisory_ids: Vec<String>,
    pub mr_td: String,
    pub rt_mr0: String,
    pub rt_mr1: String,
    pub rt_mr2: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Subject {
    pub agent: String,
    pub provenance_root: String,
}

/// The signed half of an attestation. Serialized in declaration order, which is
/// the canonical form the signature covers.
#[derive(Serialize, Deserialize, Clone)]
pub struct AttestationBody {
    pub kind: String,
    pub er: Er,
    pub enclave: Enclave,
    pub subject: Option<Subject>,
    pub challenge: String,
    pub nonce: String,
    pub verified_at: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Attestation {
    pub body: AttestationBody,
    pub attester: String,
    pub sig_alg: String,
    pub signature: String,
}

/// Load a Solana keypair file (the 64-byte JSON array) as an ed25519 signer,
/// returning the signing key and its base58 public key.
pub fn signer_from_keypair_file(path: &str) -> Result<(SigningKey, String)> {
    let bytes: Vec<u8> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    if bytes.len() < 32 {
        bail!("keypair file holds {} bytes, need at least 32", bytes.len());
    }
    let seed: [u8; 32] = bytes[..32].try_into().unwrap();
    let sk = SigningKey::from_bytes(&seed);
    let pubkey = bs58::encode(sk.verifying_key().to_bytes()).into_string();
    Ok((sk, pubkey))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 64-byte quote challenge. With an agent and provenance root, the first 32 bytes
/// commit to sha256(domain || agent || provenance_root) and the last 32 are a
/// fresh nonce. Unbound: 64 random bytes.
fn build_challenge(agent: Option<&str>, prov: Option<&str>) -> Result<([u8; 64], [u8; 32])> {
    let mut nonce = [0u8; 32];
    getrandom::getrandom(&mut nonce)?;
    let mut challenge = [0u8; 64];
    challenge[32..].copy_from_slice(&nonce);
    match (agent, prov) {
        (Some(a), Some(p)) => {
            let mut h = Sha256::new();
            h.update(BIND_DOMAIN);
            h.update(bs58::decode(a).into_vec()?);
            h.update(hex::decode(p)?);
            challenge[..32].copy_from_slice(&h.finalize());
        }
        _ => getrandom::getrandom(&mut challenge[..32])?,
    }
    Ok((challenge, nonce))
}

async fn fetch_quote(client: &reqwest::Client, tee_url: &str, challenge: &[u8; 64]) -> Result<Vec<u8>> {
    let url = format!(
        "{}/quote?challenge={}",
        tee_url.trim_end_matches('/'),
        urlencoding::encode(&B64.encode(challenge))
    );
    let v: serde_json::Value = client.get(&url).send().await?.error_for_status()?.json().await?;
    let q = v["quote"].as_str().ok_or_else(|| anyhow!("TEE /quote returned no quote field"))?;
    Ok(B64.decode(q)?)
}

async fn fetch_identity(client: &reqwest::Client, tee_url: &str) -> Option<String> {
    let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "getIdentity"});
    let v: serde_json::Value = client
        .post(tee_url.trim_end_matches('/'))
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v["result"]["identity"].as_str().map(String::from)
}

/// Fetch a TDX quote from `tee_url`, DCAP-verify it against the Phala PCCS, and
/// return a signed Covenant attestation. `agent`/`provenance_root` (base58/hex)
/// bind that record into the quote challenge.
pub async fn attest(
    tee_url: &str,
    sk: &SigningKey,
    pubkey: &str,
    agent: Option<&str>,
    provenance_root: Option<&str>,
    now: Option<u64>,
) -> Result<Attestation> {
    let client = reqwest::Client::new();
    let (challenge, nonce) = build_challenge(agent, provenance_root)?;
    let raw_quote = fetch_quote(&client, tee_url, &challenge).await?;
    let identity = fetch_identity(&client, tee_url).await;

    let collateral = dcap_qvl::collateral::CollateralClient::with_default_http(PCCS)?
        .fetch(&raw_quote)
        .await?;
    let verified_at = now.unwrap_or_else(now_secs);
    let verified = dcap_qvl::verify::verify(&raw_quote, &collateral, verified_at)
        .map_err(|e| anyhow!("dcap verify failed: {e:?}"))?;

    let (report_data, mr_td, rt_mr0, rt_mr1, rt_mr2) = match &verified.report {
        dcap_qvl::quote::Report::TD10(t) => (t.report_data, t.mr_td, t.rt_mr0, t.rt_mr1, t.rt_mr2),
        dcap_qvl::quote::Report::TD15(t) => (
            t.base.report_data,
            t.base.mr_td,
            t.base.rt_mr0,
            t.base.rt_mr1,
            t.base.rt_mr2,
        ),
        _ => bail!("expected a TDX quote, got an SGX enclave quote"),
    };
    if report_data != challenge {
        bail!("report_data != challenge (stale or replayed quote)");
    }
    if !ACCEPTABLE.contains(&verified.status.as_str()) {
        bail!("unacceptable TCB status: {}", verified.status);
    }

    let body = AttestationBody {
        kind: "covenant.tee.attestation.v1".into(),
        er: Er { rpc_url: tee_url.to_string(), validator: identity, tee: "intel-tdx".into() },
        enclave: Enclave {
            status: verified.status.clone(),
            advisory_ids: verified.advisory_ids.clone(),
            mr_td: hex::encode(mr_td),
            rt_mr0: hex::encode(rt_mr0),
            rt_mr1: hex::encode(rt_mr1),
            rt_mr2: hex::encode(rt_mr2),
        },
        subject: match (agent, provenance_root) {
            (Some(a), Some(p)) => Some(Subject { agent: a.into(), provenance_root: p.into() }),
            _ => None,
        },
        challenge: B64.encode(challenge),
        nonce: B64.encode(nonce),
        verified_at,
    };
    let signature = sk.sign(&serde_json::to_vec(&body)?);
    Ok(Attestation {
        body,
        attester: pubkey.to_string(),
        sig_alg: "ed25519".into(),
        signature: B64.encode(signature.to_bytes()),
    })
}

/// Verify a Covenant TEE attestation against a pinned set of trusted attesters.
/// Checks the attester is trusted, its ed25519 signature over the canonical body,
/// the TCB status, the agent/provenance binding committed in the quote challenge,
/// and the nonce. Fails closed if `trusted` is empty.
pub fn verify_attestation(att: &Attestation, trusted: &[String]) -> Result<()> {
    if trusted.is_empty() {
        bail!("no trusted attesters configured");
    }
    if !trusted.iter().any(|t| t == &att.attester) {
        bail!("attester {} is not trusted", att.attester);
    }
    if att.sig_alg != "ed25519" {
        bail!("unsupported sig_alg {}", att.sig_alg);
    }
    let pk: [u8; 32] = bs58::decode(&att.attester)
        .into_vec()?
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("attester is not a 32-byte key"))?;
    let vk = VerifyingKey::from_bytes(&pk)?;
    let sig = Signature::from_slice(&B64.decode(&att.signature)?)?;
    vk.verify_strict(&serde_json::to_vec(&att.body)?, &sig)
        .map_err(|_| anyhow!("bad attester signature"))?;

    if !ACCEPTABLE.contains(&att.body.enclave.status.as_str()) {
        bail!("TCB status {}", att.body.enclave.status);
    }
    let challenge = B64.decode(&att.body.challenge)?;
    if challenge.len() != 64 {
        bail!("challenge is not 64 bytes");
    }
    if let Some(sub) = &att.body.subject {
        let mut h = Sha256::new();
        h.update(BIND_DOMAIN);
        h.update(bs58::decode(&sub.agent).into_vec()?);
        h.update(hex::decode(&sub.provenance_root)?);
        if h.finalize()[..] != challenge[..32] {
            bail!("agent/provenance not committed in the quote");
        }
    }
    if B64.decode(&att.body.nonce)? != challenge[32..64] {
        bail!("nonce mismatch");
    }
    Ok(())
}
