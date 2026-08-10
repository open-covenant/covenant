use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::Utc;
use covenant_inference_protocol::{
    node_id, ModelIdentity, NodeEnrollment, NodeTelemetry, SequenceGuard, UnsignedNodeEnrollment,
    UnsignedTelemetry,
};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

/// On-disk node identity: the ed25519 secret plus the high-water mark of the
/// telemetry sequence, persisted so a restart can never rewind and reuse a number
/// the control plane has already seen.
#[derive(Serialize, Deserialize)]
struct DeviceIdentity {
    signing_key_hex: String,
    #[serde(default)]
    telemetry_sequence: u64,
}

/// Generates a fresh identity, writes it 0600, and returns the node id. Refuses
/// to clobber an existing file so an operator can't silently rotate the key out
/// from under an enrolled node.
pub fn create_identity(path: &Path) -> anyhow::Result<String> {
    if path.exists() {
        anyhow::bail!("refusing to overwrite an existing device identity");
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut seed = [0_u8; 32];
    getrandom::getrandom(&mut seed)
        .map_err(|error| anyhow::anyhow!("draw identity seed: {error}"))?;
    let key = SigningKey::from_bytes(&seed);
    let identity = DeviceIdentity {
        signing_key_hex: hex::encode(key.to_bytes()),
        telemetry_sequence: 0,
    };
    write_private_new(path, serde_json::to_string(&identity)?.as_bytes())?;
    Ok(node_id(&key.verifying_key()))
}

pub fn load_signing_key(path: &Path) -> anyhow::Result<SigningKey> {
    signing_key(&load_identity(path)?)
}

pub struct EnrollArgs {
    pub identity: PathBuf,
    pub control_plane: String,
    pub operator_wallet: String,
    pub payout_wallet: String,
    pub model: ModelIdentity,
    pub max_tokens_per_second: u32,
    pub rate_per_second: u64,
}

pub async fn enroll(args: EnrollArgs) -> anyhow::Result<()> {
    let key = signing_key(&load_identity(&args.identity)?)?;
    let enrollment = NodeEnrollment::sign(
        UnsignedNodeEnrollment {
            node_id: node_id(&key.verifying_key()),
            device_public_key: URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
            operator_wallet: args.operator_wallet,
            payout_wallet: args.payout_wallet,
            served_models: vec![args.model],
            max_tokens_per_second: args.max_tokens_per_second,
            rate_per_second: args.rate_per_second,
            issued_at: Utc::now(),
        },
        &key,
    )?;
    let endpoint = control_plane_endpoint(&args.control_plane, "v1/nodes/enroll")?;
    let response = http_client()?
        .post(endpoint)
        .json(&enrollment)
        .send()
        .await?;
    require_success(response).await
}

pub struct HeartbeatArgs {
    pub identity: PathBuf,
    pub control_plane: String,
    pub active_sessions: u32,
    pub tokens_per_second: u32,
    pub tunnel_connected: bool,
    pub served_model_digest: Option<String>,
    pub interval: Duration,
}

/// Signs and POSTs telemetry on a fixed cadence, forever. Delivery failures are
/// logged and retried on the next tick rather than aborting the loop — a node
/// that can't reach the control plane for a beat should keep trying, not die.
pub async fn heartbeat(args: HeartbeatArgs) -> anyhow::Result<()> {
    if args.interval < Duration::from_secs(1) || args.interval > Duration::from_secs(300) {
        anyhow::bail!("heartbeat interval must be between one second and five minutes");
    }
    let mut identity = load_identity(&args.identity)?;
    let key = signing_key(&identity)?;
    let node = node_id(&key.verifying_key());
    let client = http_client()?;
    let endpoint =
        control_plane_endpoint(&args.control_plane, &format!("v1/nodes/{node}/heartbeat"))?;
    // Strictly-increasing invariant on what we emit. The persisted high-water mark
    // is what actually stops a rewind across restarts; the guard turns any local
    // bug that regressed the sequence into an immediate, loud failure.
    let mut guard = SequenceGuard::default();

    loop {
        identity.telemetry_sequence = identity
            .telemetry_sequence
            .checked_add(1)
            .context("telemetry sequence exhausted")?;
        // Persist before sending: a crash can then only skip a sequence, never reuse one.
        save_identity(&args.identity, &identity)?;
        let telemetry = NodeTelemetry::sign(
            UnsignedTelemetry {
                node_id: node.clone(),
                sequence: identity.telemetry_sequence,
                observed_at: Utc::now(),
                active_sessions: args.active_sessions,
                tokens_per_second: args.tokens_per_second,
                tunnel_connected: args.tunnel_connected,
                served_model_digest: args.served_model_digest.clone(),
                // Transport is trust-model-agnostic; posture is the verification
                // layer's to assert, not ours.
                posture: None,
            },
            &key,
        )?;
        if !guard.admit(&telemetry) {
            anyhow::bail!("telemetry sequence regressed");
        }
        match client.post(endpoint.clone()).json(&telemetry).send().await {
            Ok(response) => {
                if let Err(error) = require_success(response).await {
                    tracing::warn!(%error, "heartbeat rejected by control plane");
                }
            }
            Err(error) => tracing::warn!(%error, "heartbeat delivery failed"),
        }
        tokio::time::sleep(args.interval).await;
    }
}

fn load_identity(path: &Path) -> anyhow::Result<DeviceIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
            anyhow::bail!("device identity permissions must not grant group or other access");
        }
    }
    serde_json::from_slice(&fs::read(path)?).context("read device identity")
}

fn signing_key(identity: &DeviceIdentity) -> anyhow::Result<SigningKey> {
    let bytes: [u8; 32] = hex::decode(&identity.signing_key_hex)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("device identity key is malformed"))?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn save_identity(path: &Path, identity: &DeviceIdentity) -> anyhow::Result<()> {
    let mut suffix = [0_u8; 16];
    getrandom::getrandom(&mut suffix)
        .map_err(|error| anyhow::anyhow!("draw temp-file suffix: {error}"))?;
    let temporary = path.with_extension(format!("tmp-{}", hex::encode(suffix)));
    let outcome = write_private_new(&temporary, &serde_json::to_vec(identity)?)
        .and_then(|()| fs::rename(&temporary, path).context("persist device identity"));
    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome
}

fn write_private_new(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

/// Enforces HTTPS on the control-plane base URL, allowing plain HTTP only for a
/// loopback host so a local dev coordinator works without a certificate.
fn control_plane_endpoint(base: &str, path: &str) -> anyhow::Result<url::Url> {
    let mut base = url::Url::parse(base).context("control-plane URL is invalid")?;
    if base.username() != ""
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        anyhow::bail!("control-plane URL must not carry credentials, a query or a fragment");
    }
    let local_http = base.scheme() == "http"
        && base.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
    if base.scheme() != "https" && !local_http {
        anyhow::bail!("control-plane URL must use HTTPS outside localhost");
    }
    if !base.path().ends_with('/') {
        let with_slash = format!("{}/", base.path());
        base.set_path(&with_slash);
    }
    base.join(path).context("build control-plane endpoint")
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("covenant-inferd/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build control-plane client")
}

async fn require_success(response: reqwest::Response) -> anyhow::Result<()> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    let body: String = body.chars().take(512).collect();
    anyhow::bail!("control plane returned {status}: {body}")
}

#[cfg(test)]
mod tests {
    use super::control_plane_endpoint;

    #[test]
    fn https_is_required_off_localhost() {
        assert!(control_plane_endpoint("http://coordinator.example", "v1/x").is_err());
        assert!(control_plane_endpoint("https://coordinator.example", "v1/x").is_ok());
    }

    #[test]
    fn loopback_may_use_plain_http() {
        assert!(control_plane_endpoint("http://localhost:8080", "v1/x").is_ok());
        assert!(control_plane_endpoint("http://127.0.0.1:8080", "v1/x").is_ok());
    }

    #[test]
    fn endpoint_joins_onto_the_base_path() {
        let endpoint = control_plane_endpoint("https://c.example/api", "v1/nodes/enroll").unwrap();
        assert_eq!(endpoint.as_str(), "https://c.example/api/v1/nodes/enroll");
    }

    #[test]
    fn credentialed_urls_are_rejected() {
        assert!(control_plane_endpoint("https://user:pass@c.example", "v1/x").is_err());
    }
}
