//! Covenant Verified: independent, Covenant-signed attestations over
//! zauth's public directory.
//!
//! zauth lists and health-monitors x402 endpoints. For each one we probe
//! it ourselves (free, unpaid 402), record liveness + a valid x402
//! challenge + the on-chain terms it advertises, and emit an Ed25519
//! attestation anyone can verify with our published issuer key. zauth's
//! directory says "listed + health-monitored"; this adds an independent
//! "Covenant Verified" signal alongside it.
//!
//! The signing/canonicalization here is a deliberate, self-contained
//! mirror of `covenant_zauth::attest`. The indexer is a standalone crate
//! and cannot path-depend on the agent-os workspace without dragging the
//! whole workspace into its Docker build, so the small attestation
//! surface is duplicated. The canonical JSON shape is pinned by
//! `canonical_shape_is_frozen` so it can never silently drift from that
//! crate's `verify_attestation`.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{extract::State, Json};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::api::AppState;

const DIRECTORY_URL: &str = "https://api.zauth.inc/api/directory";
const ATTESTATION_TYPE: &str = "covenant_endpoint_attestation_v1";
const SOURCE: &str = "zauth-directory";
const REFRESH_INTERVAL: Duration = Duration::from_secs(600);
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_CONCURRENCY: usize = 8;
const DEFAULT_LIMIT: u32 = 50;

#[derive(Clone)]
pub struct VerifiedState {
    signer: Arc<SigningKey>,
    issuer_hex: String,
    http: reqwest::Client,
    limit: u32,
    cache: Arc<RwLock<VerifiedSnapshot>>,
}

#[derive(Clone, Default, Serialize)]
pub struct VerifiedSnapshot {
    issuer: String,
    source: String,
    #[serde(rename = "generatedAt")]
    generated_at: Option<String>,
    ready: bool,
    count: usize,
    attestations: Vec<EndpointAttestation>,
}

impl VerifiedState {
    /// Enabled only when `COVENANT_VERIFIED_ISSUER_SEED` (hex, 32 bytes)
    /// is set. `COVENANT_VERIFIED_LIMIT` caps how many directory entries
    /// are attested per refresh.
    pub fn from_env() -> Option<Self> {
        let seed_hex = std::env::var("COVENANT_VERIFIED_ISSUER_SEED").ok()?;
        let seed = hex::decode(seed_hex.trim()).ok()?;
        let seed: [u8; 32] = seed.as_slice().try_into().ok()?;
        let signer = SigningKey::from_bytes(&seed);
        let issuer_hex = hex::encode(signer.verifying_key().to_bytes());
        let limit = std::env::var("COVENANT_VERIFIED_LIMIT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_LIMIT);
        let http = reqwest::Client::builder()
            .user_agent("Mozilla/5.0")
            .timeout(PROBE_TIMEOUT)
            .build()
            .ok()?;
        Some(Self {
            signer: Arc::new(signer),
            issuer_hex,
            http,
            limit,
            cache: Arc::new(RwLock::new(VerifiedSnapshot::default())),
        })
    }

    pub fn issuer(&self) -> &str {
        &self.issuer_hex
    }

    /// Spawn a background task that refreshes the attestation set
    /// immediately and then every `REFRESH_INTERVAL`. Failures are
    /// logged and retried on the next tick; the cached snapshot is left
    /// intact so a transient zauth outage never empties the feed.
    pub fn spawn_refresher(&self) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                match me.refresh().await {
                    Ok(n) => info!(count = n, "covenant-verified: attested directory"),
                    Err(e) => warn!(error = %e, "covenant-verified: refresh failed"),
                }
                tokio::time::sleep(REFRESH_INTERVAL).await;
            }
        });
    }

    async fn refresh(&self) -> anyhow::Result<usize> {
        let endpoints = self.list_directory().await?;
        let now = now_unix();
        let mut probed: Vec<(String, ProbeOutcome)> = Vec::with_capacity(endpoints.len());
        for chunk in endpoints.chunks(PROBE_CONCURRENCY) {
            let mut handles = Vec::with_capacity(chunk.len());
            for ep in chunk {
                let http = self.http.clone();
                let ep = ep.clone();
                handles.push(tokio::spawn(async move {
                    let probe = probe_endpoint(&http, &ep.url, &ep.method).await;
                    (ep.url, probe)
                }));
            }
            for h in handles {
                if let Ok(pair) = h.await {
                    probed.push(pair);
                }
            }
        }

        let signer = self.signer.as_ref();
        let mut attestations = Vec::with_capacity(probed.len());
        for (url, probe) in probed {
            attestations.push(attest(&url, &probe, signer, now)?);
        }
        let count = attestations.len();
        *self.cache.write().await = VerifiedSnapshot {
            issuer: self.issuer_hex.clone(),
            source: SOURCE.to_string(),
            generated_at: Some(rfc3339(now)?),
            ready: true,
            count,
            attestations,
        };
        Ok(count)
    }

    async fn list_directory(&self) -> anyhow::Result<Vec<DirEndpoint>> {
        let limit = self.limit.to_string();
        let resp = self
            .http
            .get(DIRECTORY_URL)
            .query(&[("verified", "true"), ("limit", limit.as_str())])
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("directory status {}", status.as_u16());
        }
        let page: DirPage = resp.json().await?;
        Ok(page.endpoints)
    }
}

pub async fn serve_verified(State(state): State<AppState>) -> Json<VerifiedSnapshot> {
    match &state.verified {
        Some(v) => Json(v.cache.read().await.clone()),
        None => Json(VerifiedSnapshot::default()),
    }
}

#[derive(Deserialize)]
struct DirPage {
    #[serde(default)]
    endpoints: Vec<DirEndpoint>,
}

#[derive(Deserialize, Clone)]
struct DirEndpoint {
    url: String,
    #[serde(default = "default_method")]
    method: String,
}

fn default_method() -> String {
    "GET".to_string()
}

#[derive(Debug, Clone, PartialEq)]
struct ProbeOutcome {
    live: bool,
    valid_x402: bool,
    network: Option<String>,
    asset: Option<String>,
    pay_to: Option<String>,
}

impl ProbeOutcome {
    fn dead() -> Self {
        Self {
            live: false,
            valid_x402: false,
            network: None,
            asset: None,
            pay_to: None,
        }
    }
    fn live_only(live: bool) -> Self {
        Self {
            live,
            valid_x402: false,
            network: None,
            asset: None,
            pay_to: None,
        }
    }
}

#[derive(Deserialize)]
struct ChallengeLite {
    #[serde(default)]
    accepts: Vec<AcceptLite>,
}

#[derive(Deserialize)]
struct AcceptLite {
    #[serde(default)]
    network: Option<String>,
    #[serde(default)]
    asset: Option<String>,
    #[serde(rename = "payTo", default)]
    pay_to: Option<String>,
}

/// Probe an endpoint with its advertised method. A paid x402 endpoint
/// answers an unpaid request with 402 carrying the challenge in the
/// `payment-required` header; decode it and read the first accept's
/// terms. Never errors — failure is just `live:false`.
async fn probe_endpoint(http: &reqwest::Client, url: &str, method: &str) -> ProbeOutcome {
    let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut req = http.request(m.clone(), url);
    if m == reqwest::Method::POST {
        req = req.json(&serde_json::json!({}));
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(_) => return ProbeOutcome::dead(),
    };
    let status = resp.status();
    let live = status.as_u16() < 500;
    if status.as_u16() != 402 {
        return ProbeOutcome::live_only(live);
    }
    let raw = resp
        .headers()
        .get("payment-required")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let Some(raw) = raw else {
        return ProbeOutcome::live_only(true);
    };
    let Ok(bytes) = B64.decode(raw.trim()) else {
        return ProbeOutcome::live_only(true);
    };
    let Ok(ch) = serde_json::from_slice::<ChallengeLite>(&bytes) else {
        return ProbeOutcome::live_only(true);
    };
    match ch.accepts.into_iter().next() {
        Some(a) => ProbeOutcome {
            live: true,
            valid_x402: true,
            network: a.network,
            asset: a.asset,
            pay_to: a.pay_to,
        },
        None => ProbeOutcome::live_only(true),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct EndpointChecks {
    live: bool,
    valid_x402_challenge: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EndpointAttestation {
    #[serde(rename = "type")]
    kind: String,
    url: String,
    network: Option<String>,
    asset: Option<String>,
    pay_to: Option<String>,
    checks: EndpointChecks,
    source: String,
    verified_at: String,
    issuer: String,
    signature: String,
}

/// Signing view: every field except `signature`, in declaration order,
/// so canonical JSON is reproducible without `serde_json` preserve_order.
/// Field order and rename attrs MUST match `covenant_zauth::attest` (see
/// `canonical_shape_is_frozen`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AttestationUnsigned<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    url: &'a str,
    network: &'a Option<String>,
    asset: &'a Option<String>,
    pay_to: &'a Option<String>,
    checks: &'a EndpointChecks,
    source: &'a str,
    verified_at: &'a str,
    issuer: &'a str,
}

impl EndpointAttestation {
    fn unsigned(&self) -> AttestationUnsigned<'_> {
        AttestationUnsigned {
            kind: &self.kind,
            url: &self.url,
            network: &self.network,
            asset: &self.asset,
            pay_to: &self.pay_to,
            checks: &self.checks,
            source: &self.source,
            verified_at: &self.verified_at,
            issuer: &self.issuer,
        }
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn rfc3339(unix_secs: i64) -> anyhow::Result<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(unix_secs, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .ok_or_else(|| anyhow::anyhow!("invalid timestamp {unix_secs}"))
}

fn attest(
    url: &str,
    probe: &ProbeOutcome,
    signer: &SigningKey,
    issued_at_unix: i64,
) -> anyhow::Result<EndpointAttestation> {
    let mut att = EndpointAttestation {
        kind: ATTESTATION_TYPE.to_string(),
        url: url.to_string(),
        network: probe.network.clone(),
        asset: probe.asset.clone(),
        pay_to: probe.pay_to.clone(),
        checks: EndpointChecks {
            live: probe.live,
            valid_x402_challenge: probe.valid_x402,
        },
        source: SOURCE.to_string(),
        verified_at: rfc3339(issued_at_unix)?,
        issuer: "covenant".to_string(),
        signature: String::new(),
    };
    let canon = serde_json::to_string(&att.unsigned())?;
    let sig = signer.sign(canon.as_bytes());
    att.signature = hex::encode(sig.to_bytes());
    Ok(att)
}

/// Verify a Covenant Verified attestation's Ed25519 signature against the
/// issuer pubkey (hex, 32-byte). Strict (rejects malleable signatures).
pub fn verify_attestation(json: &str, issuer_pubkey_hex: &str) -> bool {
    let Ok(att) = serde_json::from_str::<EndpointAttestation>(json) else {
        return false;
    };
    let Ok(canon) = serde_json::to_string(&att.unsigned()) else {
        return false;
    };
    let Ok(pk) = hex::decode(issuer_pubkey_hex.trim_start_matches("0x")) else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(att.signature.trim_start_matches("0x")) else {
        return false;
    };
    let (Ok(pk_arr), Ok(sig_arr)): (
        std::result::Result<[u8; 32], _>,
        std::result::Result<[u8; 64], _>,
    ) = (pk.as_slice().try_into(), sig_bytes.as_slice().try_into()) else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk_arr) else {
        return false;
    };
    vk.verify_strict(canon.as_bytes(), &Signature::from_bytes(&sig_arr))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> EndpointAttestation {
        let signer = SigningKey::from_bytes(&[7u8; 32]);
        let probe = ProbeOutcome {
            live: true,
            valid_x402: true,
            network: Some("solana".to_string()),
            asset: Some("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".to_string()),
            pay_to: Some("ZAU64eKWAgiGNux8bzvgRn8RvWqFhdMVrpJytF7V1qm".to_string()),
        };
        attest("https://example.com/x402/foo", &probe, &signer, 0).unwrap()
    }

    // Pins the canonical (signature-stripped) JSON byte-for-byte. This is
    // exactly what `covenant_zauth::attest::AttestationUnsigned` emits for
    // the same input; if this string ever changes, the indexer's
    // attestations stop verifying against that crate's verifier.
    #[test]
    fn canonical_shape_is_frozen() {
        let att = fixture();
        let canon = serde_json::to_string(&att.unsigned()).unwrap();
        assert_eq!(
            canon,
            r#"{"type":"covenant_endpoint_attestation_v1","url":"https://example.com/x402/foo","network":"solana","asset":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v","payTo":"ZAU64eKWAgiGNux8bzvgRn8RvWqFhdMVrpJytF7V1qm","checks":{"live":true,"validX402Challenge":true},"source":"zauth-directory","verifiedAt":"1970-01-01T00:00:00.000Z","issuer":"covenant"}"#
        );
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let signer = SigningKey::from_bytes(&[7u8; 32]);
        let issuer = hex::encode(signer.verifying_key().to_bytes());
        let att = fixture();
        let json = serde_json::to_string(&att).unwrap();
        assert!(verify_attestation(&json, &issuer));
    }

    #[test]
    fn tamper_breaks_verification() {
        let signer = SigningKey::from_bytes(&[7u8; 32]);
        let issuer = hex::encode(signer.verifying_key().to_bytes());
        let mut att = fixture();
        att.url = "https://evil.example/x402/foo".to_string();
        let json = serde_json::to_string(&att).unwrap();
        assert!(!verify_attestation(&json, &issuer));
    }

    #[test]
    fn wrong_issuer_rejected() {
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let wrong_issuer = hex::encode(other.verifying_key().to_bytes());
        let json = serde_json::to_string(&fixture()).unwrap();
        assert!(!verify_attestation(&json, &wrong_issuer));
    }
}
