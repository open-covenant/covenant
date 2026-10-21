//! Bounded acceptance helper for the explicitly admitted ephemeral Phala profile.
//! This re-verifies provider bytes; it never creates a lease, signs, pays, or
//! promotes production trust. Expected challenge and Cloud identity pins must
//! come from the independently retained local acceptance plan.
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use covenant_compute_attestation::binding::{
    GuestEvidence, HardwareChallenge, HardwareEvidence, HardwareProfile, MAX_EVIDENCE_BYTES,
    reference_digest_for_profile, verify_evidence_for_profile,
};
use covenant_compute_attestation::vendor_types::TdxEventEntry;
use dcap_qvl::{
    QuoteCollateralV3,
    collateral::{CollateralClient, INTEL_PCS_URL},
    configs::DefaultConfig,
    http::{HttpClient, HttpResponse},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_INPUT: usize = 1024 * 1024;
const MAX_COLLATERAL: usize = 128 * 1024;
const MAX_HTTP_BODY: usize = 256 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema_version: u8,
    verification_status: String,
    challenge: HardwareChallenge,
    identity: Value,
    dstack_quote: GuestQuote,
}

#[derive(Deserialize)]
struct GuestQuote {
    quote: String,
    event_log: String,
    report_data: String,
    vm_config: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloudPins {
    schema_version: u8,
    app_id: String,
    #[serde(default)]
    instance_id: Option<String>,
}
impl CloudPins {
    fn validate(&self) -> Result<()> {
        ensure!(self.schema_version == 1, "unknown Cloud pin schema");
        for value in std::iter::once(&self.app_id).chain(self.instance_id.iter()) {
            ensure!(
                value.len() == 40
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "Cloud identity pin must be 20 canonical hex bytes"
            );
        }
        Ok(())
    }
}

// Call only after the full public verifier has authenticated this entire log.
fn match_verified_boot_pins(
    events: &[TdxEventEntry],
    pins: &CloudPins,
) -> Result<(String, String)> {
    pins.validate()?;
    let identity = |name: &str| -> Result<String> {
        let mut found = events.iter().filter(|event| {
            event.imr == 3 && event.event_type == 0x0800_0001 && event.event == name
        });
        let first = found.next().context("missing verified boot identity")?;
        ensure!(found.next().is_none(), "duplicate verified boot identity");
        let bytes = hex::decode(&first.event_payload)?;
        ensure!(bytes.len() == 20, "invalid verified boot identity length");
        Ok(hex::encode(bytes))
    };
    let app = identity("app-id")?;
    let instance = identity("instance-id")?;
    ensure!(
        app == pins.app_id,
        "verified app differs from independently pinned Cloud app"
    );
    if let Some(expected) = &pins.instance_id {
        ensure!(
            &instance == expected,
            "verified instance differs from independently pinned Cloud instance"
        );
    }
    Ok((app, instance))
}

#[derive(Serialize)]
struct FetchLog {
    url: String,
    status: u16,
    bytes: usize,
    body_sha256: String,
    completed_at_ms: u64,
}

#[derive(Clone)]
struct BoundedHttp {
    client: reqwest::Client,
    log: Arc<Mutex<Vec<FetchLog>>>,
    requests: Arc<AtomicUsize>,
}

fn allowed_url(raw: &str) -> Result<()> {
    ensure!(raw.len() <= 8192, "collateral URL exceeds limit");
    let url = reqwest::Url::parse(raw)?;
    ensure!(
        url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "collateral URL is not plain HTTPS"
    );
    let allowed = match url.host_str() {
        Some("api.trustedservices.intel.com") => {
            url.path().starts_with("/sgx/certification/v4/")
                || url.path().starts_with("/tdx/certification/v4/")
        }
        Some("certificates.trustedservices.intel.com") => {
            url.path() == "/IntelSGXRootCA.der" && url.query().is_none()
        }
        _ => false,
    };
    ensure!(
        allowed,
        "collateral URL is outside the fixed Intel allowlist"
    );
    Ok(())
}

impl HttpClient for BoundedHttp {
    async fn get(&self, url: &str) -> Result<HttpResponse> {
        allowed_url(url)?;
        ensure!(
            self.requests.fetch_add(1, Ordering::Relaxed) < 8,
            "too many collateral requests"
        );
        let mut response = self.client.get(url).send().await?;
        let status = response.status().as_u16();
        ensure!(
            response.headers().len() <= 64,
            "too many collateral headers"
        );
        let mut header_bytes = 0;
        let mut headers = BTreeMap::new();
        for (name, value) in response.headers() {
            header_bytes += name.as_str().len() + value.as_bytes().len();
            ensure!(header_bytes <= 64 * 1024, "collateral headers exceed limit");
            headers.insert(name.as_str().into(), value.to_str()?.into());
        }
        if let Some(length) = response.content_length() {
            ensure!(
                length <= MAX_HTTP_BODY as u64,
                "collateral body exceeds limit"
            );
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                body.len() + chunk.len() <= MAX_HTTP_BODY,
                "collateral body exceeds limit"
            );
            body.extend_from_slice(&chunk);
        }
        self.log.lock().unwrap().push(FetchLog {
            url: url.into(),
            status,
            bytes: body.len(),
            body_sha256: hex::encode(Sha256::digest(&body)),
            completed_at_ms: now_ms()?,
        });
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

fn now_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    ensure!(
        file.metadata()?.len() <= limit as u64,
        "input exceeds byte limit"
    );
    let mut data = Vec::new();
    file.take((limit + 1) as u64).read_to_end(&mut data)?;
    ensure!(data.len() <= limit, "input exceeds byte limit");
    Ok(data)
}

fn save_new(path: &Path, value: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(value)?;
    file.sync_all()?;
    Ok(())
}

fn output_directory(path: &Path) -> Result<PathBuf> {
    ensure!(
        !path.exists(),
        "run directory already exists; preserve prior evidence"
    );
    let parent = path
        .parent()
        .context("missing output parent")?
        .canonicalize()?;
    ensure!(
        parent.metadata()?.permissions().mode() & 0o077 == 0,
        "output parent must be a private directory (mode 0700)"
    );
    fs::create_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(path.canonicalize()?)
}

fn prepare(
    envelope_path: &Path,
    challenge_path: &Path,
) -> Result<(Envelope, Vec<u8>, Vec<TdxEventEntry>)> {
    let envelope: Envelope = serde_json::from_slice(&read_bounded(envelope_path, MAX_INPUT)?)?;
    let expected: HardwareChallenge =
        serde_json::from_slice(&read_bounded(challenge_path, 16 * 1024)?)?;
    ensure!(
        envelope.schema_version == 1 && envelope.verification_status == "unverified",
        "unexpected envelope"
    );
    ensure!(
        envelope.challenge == expected,
        "collector challenge differs from independently supplied challenge"
    );
    // Never substitute a historic clock or rewrite the consumed challenge.
    expected.validate(now_ms()?)?;
    ensure!(
        expected.scope.profile == HardwareProfile::PhalaEphemeralDstack061,
        "expected challenge must explicitly select the ephemeral profile"
    );
    let q = &envelope.dstack_quote;
    ensure!(
        q.quote.len() <= 48 * 1024,
        "raw quote exceeds Covenant base64 bound"
    );
    let quote = hex::decode(&q.quote).context("quote hex")?;
    ensure!(
        STANDARD.encode(&quote).len() <= 32 * 1024,
        "quote exceeds Covenant bound"
    );
    let expected_report_data = expected.guest_report_data()?;
    ensure!(
        hex::decode(&q.report_data)? == expected_report_data,
        "guest report_data differs from full challenge"
    );
    ensure!(
        q.event_log.len() <= MAX_EVIDENCE_BYTES,
        "event log exceeds bound"
    );
    let events: Vec<TdxEventEntry> = serde_json::from_str(&q.event_log)?;
    ensure!(events.len() <= 256, "too many events");
    covenant_compute_attestation::binding::parse_events(&events)?;
    let scope = &expected.scope;
    for (name, wanted) in [
        ("host", json!(scope.host)),
        ("port", json!(scope.port)),
        ("username", json!(scope.username)),
        ("transport", json!(scope.transport)),
        ("guest_channel_key", json!(scope.guest_channel_key)),
    ] {
        ensure!(
            envelope.identity.get(name) == Some(&wanted),
            "identity {name} mismatch"
        );
    }
    let info = &envelope.identity["guest_info"];
    ensure!(
        info["compose_hash"].as_str() == Some(&scope.workload_sha256),
        "diagnostic compose differs from scope"
    );
    ensure!(
        info["vm_config"].as_str() == Some(&q.vm_config),
        "VM config changed during collection"
    );
    Ok((envelope, quote, events))
}

fn verify(
    envelope: &Envelope,
    quote: &[u8],
    events: Vec<TdxEventEntry>,
    collateral: &QuoteCollateralV3,
    pins: &CloudPins,
    output: &Path,
) -> Result<bool> {
    let time = now_ms()?;
    let collateral_json = serde_json::to_string(collateral)?;
    ensure!(
        collateral_json.len() <= MAX_COLLATERAL,
        "collateral exceeds Covenant128KiB limit"
    );
    let evidence = HardwareEvidence {
        challenge: envelope.challenge.clone(),
        guest: GuestEvidence::Tdx {
            quote_base64: STANDARD.encode(quote),
            collateral_json,
            events: events.clone(),
        },
        gpu_cc: None,
    };
    let bytes = serde_json::to_vec(&evidence)?;
    ensure!(
        bytes.len() <= MAX_EVIDENCE_BYTES,
        "evidence exceeds Covenant512KiB limit"
    );
    save_new(&output.join("hardware-evidence.json"), &bytes)?;
    let mut diagnostics = json!({
        "schema_version": 1, "verification_mode": "actual_current_clock",
        "verified_at_ms": time, "quote_sha256": hex::encode(Sha256::digest(quote)),
        "evidence_sha256": hex::encode(Sha256::digest(&bytes)),
        "reference_sha256": reference_digest_for_profile(HardwareProfile::PhalaEphemeralDstack061),
        "profile": "phala_ephemeral_dstack061", "production_qualification": false,
        "challenge_id": envelope.challenge.challenge_id,
        "intel_signature_and_collateral_valid": false,
        "covenant_policy_accepted": false,
        "served_trust_ceiling": "open",
    });
    // Diagnostic extraction uses the same production verifier, root, clock,
    // backend and debug setting. TCB status alone is never success: only the
    // full Covenant verifier below can accept.
    match dcap_qvl::verify::verify(quote, collateral, time / 1000) {
        Ok(vendor) => {
            diagnostics["intel_signature_and_collateral_valid"] = json!(true);
            diagnostics["intel_status"] = json!(vendor.status);
            diagnostics["intel_advisory_ids"] = json!(vendor.advisory_ids);
            diagnostics["intel_platform_status"] = json!(vendor.platform_status);
            diagnostics["intel_qe_status"] = json!(vendor.qe_status);
            diagnostics["intel_tcb_policy_accepted"] =
                json!(vendor.status == "UpToDate" && vendor.advisory_ids.is_empty());
        }
        Err(error) => diagnostics["intel_error"] = json!(format!("{error:#}")),
    }
    let accepted = match verify_evidence_for_profile(
        &evidence,
        time,
        HardwareProfile::PhalaEphemeralDstack061,
    ) {
        Ok(verdict) => {
            let (app, instance) = match_verified_boot_pins(&events, pins)?;
            ensure!(
                verdict.guest_identity == format!("tdx/{instance}"),
                "verified event identity differs from hardware verdict"
            );
            ensure!(
                envelope.identity["guest_info"]["instance_id"].as_str() == Some(instance.as_str())
                    && envelope.identity["guest_info"]["app_id"].as_str() == Some(app.as_str()),
                "collected Info identity differs from the verified boot"
            );
            diagnostics["verified_app_id"] = json!(app);
            diagnostics["verified_instance_id"] = json!(instance);
            save_new(
                &output.join("verified-event-log.json"),
                &serde_json::to_vec(&events)?,
            )?;
            diagnostics["covenant_policy_accepted"] = json!(true);
            diagnostics["verdict"] = serde_json::to_value(verdict)?;
            true
        }
        Err(error) => {
            diagnostics["covenant_error"] = json!(error.to_string());
            false
        }
    };
    save_new(
        &output.join("verification.json"),
        &serde_json::to_vec_pretty(&diagnostics)?,
    )?;
    println!("{}", serde_json::to_string(&diagnostics)?);
    Ok(accepted)
}

async fn run() -> Result<bool> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        (args.len() == 6 && args[1] == "fetch-verify")
            || (args.len() == 7 && args[1] == "offline-verify"),
        "usage: fetch-verify ENVELOPE EXPECTED_CHALLENGE CLOUD_PINS NEW_RUN_DIRECTORY\n       offline-verify ENVELOPE EXPECTED_CHALLENGE CLOUD_PINS COLLATERAL NEW_RUN_DIRECTORY"
    );
    let (envelope, quote, events) = prepare(Path::new(&args[2]), Path::new(&args[3]))?;
    let pins_bytes = read_bounded(Path::new(&args[4]), 16 * 1024)?;
    let pins: CloudPins = serde_json::from_slice(&pins_bytes)?;
    pins.validate()?;
    let output = output_directory(Path::new(args.last().unwrap()))?;
    save_new(
        &output.join("expected-challenge.json"),
        &serde_json::to_vec_pretty(&envelope.challenge)?,
    )?;
    save_new(&output.join("cloud-pins.json"), &pins_bytes)?;
    let collateral = if args[1] == "fetch-verify" {
        let log = Arc::new(Mutex::new(Vec::new()));
        let transport = BoundedHttp {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(20))
                .user_agent("Covenant-Ephemeral-Acceptance-Verifier/1")
                .build()?,
            log: log.clone(),
            requests: Arc::new(AtomicUsize::new(0)),
        };
        let client = CollateralClient::<DefaultConfig, _>::new(transport, INTEL_PCS_URL);
        let result = tokio::time::timeout(Duration::from_secs(90), client.fetch(&quote)).await;
        save_new(
            &output.join("intel-fetches.json"),
            &serde_json::to_vec_pretty(&*log.lock().unwrap())?,
        )?;
        result.context("overall Intel collateral fetch timeout")??
    } else {
        serde_json::from_slice(&read_bounded(Path::new(&args[5]), MAX_COLLATERAL)?)?
    };
    let collateral_bytes = serde_json::to_vec(&collateral)?;
    ensure!(
        collateral_bytes.len() <= MAX_COLLATERAL,
        "collateral exceeds Covenant128KiB limit"
    );
    save_new(&output.join("collateral.json"), &collateral_bytes)?;
    verify(&envelope, &quote, events, &collateral, &pins, &output)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    match run().await {
        Ok(true) => {}
        Ok(false) => std::process::exit(2),
        Err(error) => {
            eprintln!("verification refused: {error:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_event(name: &str, value: u8) -> TdxEventEntry {
        TdxEventEntry {
            imr: 3,
            event_type: 0x0800_0001,
            event: name.into(),
            digest: String::new(),
            event_payload: hex::encode([value; 20]),
            version: 1,
            preimage: None,
        }
    }
    #[test]
    fn cloud_pins_must_match_authenticated_events_and_optional_instance_never_replaces_them() {
        let events = vec![
            identity_event("app-id", 1),
            identity_event("instance-id", 2),
        ];
        let mut pins = CloudPins {
            schema_version: 1,
            app_id: hex::encode([1; 20]),
            instance_id: None,
        };
        assert_eq!(
            match_verified_boot_pins(&events, &pins).unwrap().1,
            hex::encode([2; 20])
        );
        pins.instance_id = Some(hex::encode([3; 20]));
        assert!(match_verified_boot_pins(&events, &pins).is_err());
        pins.instance_id = Some(hex::encode([2; 20]));
        pins.app_id = hex::encode([3; 20]);
        assert!(match_verified_boot_pins(&events, &pins).is_err());
        pins.app_id = hex::encode([1; 20]);
        let mut duplicate = events;
        duplicate.push(identity_event("instance-id", 2));
        assert!(match_verified_boot_pins(&duplicate, &pins).is_err());
    }

    #[test]
    fn collateral_transport_accepts_only_exact_intel_https_origins_and_routes() {
        for url in [
            "https://api.trustedservices.intel.com/tdx/certification/v4/tcb?fmspc=AABBCCDDEEFF",
            "https://certificates.trustedservices.intel.com/IntelSGXRootCA.der",
        ] {
            assert!(allowed_url(url).is_ok());
        }
        for url in [
            "http://api.trustedservices.intel.com/tdx/certification/v4/tcb",
            "https://api.trustedservices.intel.com.evil.test/tdx/certification/v4/tcb",
            "https://user@api.trustedservices.intel.com/tdx/certification/v4/tcb",
            "https://api.trustedservices.intel.com:444/tdx/certification/v4/tcb",
            "https://127.0.0.1/tdx/certification/v4/tcb",
            "https://certificates.trustedservices.intel.com/other",
            "https://api.trustedservices.intel.com/anything",
        ] {
            assert!(allowed_url(url).is_err(), "{url}");
        }
    }
}
