//! Intel TDX quote verification, built on Intel's DCAP scheme.
//!
//! A TDX quote is signed by the platform's quoting enclave and chains to
//! Intel's root through collateral the caller fetches out of band: TCB info,
//! QE identity and the certificate revocation lists. The collateral is signed
//! by Intel and carries its own validity windows, so passing it in keeps this
//! crate a pure function over bytes: the caller owns the fetch, this module
//! owns every judgement about what the bytes mean.
//!
//! What a verified quote proves: a genuine Intel processor at the collateral's
//! patch level launched a TD from the measured image (`MRTD`), extended the
//! boot-time registers (`RTMR0..2`) the way the reference set expects, and the
//! guest bound our challenge into `REPORT_DATA`. What it does not prove: who
//! is running the host, what has been extended into `RTMR3` at runtime, or
//! anything about a GPU. Runtime-register replay below is a separate check;
//! NVIDIA CC evidence requires its own reference material. TDX evidence alone
//! stops at Attested exactly like a SEV-SNP report does.

use dcap_qvl::QuoteCollateralV3;
use dcap_qvl::verify::verify as dcap_verify;
use p384::elliptic_curve::subtle::ConstantTimeEq;
use serde::Deserialize;
use sha2::{Digest, Sha384};

use crate::VerificationError;

/// The one TCB status this verifier accepts. Everything else (outdated,
/// configuration needed, software hardening needed) is a platform Intel has
/// published reservations about, and a reservation is a refusal here. Widening
/// this to named statuses with advisory review is a policy change, not a bug.
const ACCEPTED_TCB_STATUS: &str = "UpToDate";

/// The measured state of one TD as this verifier read it out of a quote whose
/// signature chain and collateral already checked out.
pub(crate) struct TdReport {
    pub(crate) mr_td: [u8; 48],
    pub(crate) rtmr0: [u8; 48],
    pub(crate) rtmr1: [u8; 48],
    pub(crate) rtmr2: [u8; 48],
    pub(crate) rtmr3: [u8; 48],
    pub(crate) report_data: [u8; 64],
}

/// One launch identity a TD may present: the image measurement and the three
/// boot-time runtime registers, taken together. A quote matches an entry or it
/// matches nothing; there is no per-register mixing across entries, because a
/// kernel from one image and an initrd from another is not a state anyone
/// published.
#[derive(Debug, Clone)]
pub struct TdxLaunchIdentity {
    pub mr_td: [u8; 48],
    pub rtmr0: [u8; 48],
    pub rtmr1: [u8; 48],
    pub rtmr2: [u8; 48],
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TdxMeasurementSet {
    identities: Vec<(TdxLaunchIdentity, TdxRuntimeProfile)>,
}

/// Runtime semantics belong to the independently pinned launch identity.
/// Event version alone does not identify an OS: dstack 0.6.1 supports V1 and V2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TdxRuntimeProfile {
    LegacyMrKms,
    Dstack061 { os_image_hash: [u8; 32] },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DstackKeyProvider {
    ManagedKms,
    EphemeralNone,
}

impl TdxMeasurementSet {
    #[cfg(any(test, feature = "test-vectors"))]
    pub(crate) fn new(identities: Vec<TdxLaunchIdentity>) -> Self {
        Self {
            identities: identities
                .into_iter()
                .map(|identity| (identity, TdxRuntimeProfile::LegacyMrKms))
                .collect(),
        }
    }

    pub(crate) fn with_profiles(identities: Vec<(TdxLaunchIdentity, TdxRuntimeProfile)>) -> Self {
        Self { identities }
    }

    /// Whether the report names a launch identity in the set. Every entry is
    /// compared in full so the answer does not leak which one matched, or how
    /// close a mismatch came, through timing.
    pub(crate) fn runtime_profile(&self, report: &TdReport) -> Option<TdxRuntimeProfile> {
        let mut selected = None;
        let mut conflict = false;
        for (identity, profile) in &self.identities {
            let hit = identity.mr_td.ct_eq(&report.mr_td)
                & identity.rtmr0.ct_eq(&report.rtmr0)
                & identity.rtmr1.ct_eq(&report.rtmr1)
                & identity.rtmr2.ct_eq(&report.rtmr2);
            if bool::from(hit) {
                conflict |= selected.is_some_and(|previous| previous != *profile);
                selected = Some(*profile);
            }
        }
        if conflict { None } else { selected }
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.identities.is_empty()
    }
}

#[cfg(test)]
#[path = "tdx_tests.rs"]
mod tests;

/// One entry of a dstack event log, as the guest agent reports it.
///
/// Two kinds of entry share this shape. A firmware event carries the digest
/// the TD extended for it and nothing else worth reading. A dstack runtime
/// event carries a name and payload. Its digest is independently derived here;
/// V1 may omit the advertised digest, while V2 must supply a digest and its
/// canonical JSON preimage. The fold across the effective digests has to land
/// on the register the quote signed.
#[derive(Debug, Clone)]
pub struct TdxEvent {
    pub imr: u32,
    pub event_type: u32,
    pub name: String,
    /// The digest as it arrived, which is empty for a runtime event whose
    /// producer derives it. [`effective_digest`] resolves what actually folds
    /// into the register.
    pub digest: Vec<u8>,
    pub payload: Vec<u8>,
    pub version: u32,
    /// Decoded bytes of the hex preimage supplied by dstack's V2 quote API.
    pub preimage: Option<Vec<u8>>,
}

/// The event type dstack writes for every runtime event it extends into the
/// application register. Its digest is independently derived from the name
/// and payload; other event types carry a firmware digest on the wire.
const DSTACK_RUNTIME_EVENT_TYPE: u32 = 0x0800_0001;

/// The digest that actually folds into a register for this event.
///
/// For a runtime event this is `sha384(event_type_le || ":" || name || ":" ||
/// payload)`, the V1 dstack rule, computed here rather than trusted: a changed
/// payload changes this digest, which changes the fold, which no longer meets
/// the quoted register. A producer that also sent the digest (the cloud shape)
/// must have sent the one this computes, or the event is rejected before the
/// fold. V2 hashes the JCS object {name,payload(hex),type} and requires both the
/// advertised digest and preimage to match that independently reconstructed
/// object. For firmware, the wire digest must be a full SHA-384.
fn effective_digest(event: &TdxEvent) -> Result<[u8; 48], VerificationError> {
    if !matches!(event.version, 1 | 2) {
        return Err(VerificationError::TdxEventDigestMismatch);
    }
    if event.event_type == DSTACK_RUNTIME_EVENT_TYPE {
        let preimage = if event.version == 1 {
            let mut bytes = event.event_type.to_le_bytes().to_vec();
            bytes.push(b':');
            bytes.extend_from_slice(event.name.as_bytes());
            bytes.push(b':');
            bytes.extend_from_slice(&event.payload);
            bytes
        } else {
            let canonical = serde_jcs::to_vec(&serde_json::json!({
                "name": event.name,
                "payload": hex::encode(&event.payload),
                "type": event.event_type,
            }))
            .map_err(|_| VerificationError::TdxEventDigestMismatch)?;
            if event.preimage.as_deref() != Some(canonical.as_slice()) {
                return Err(VerificationError::TdxEventDigestMismatch);
            }
            canonical
        };
        let computed: [u8; 48] = Sha384::digest(preimage).into();
        if event.version == 2 || !event.digest.is_empty() {
            let carried: [u8; 48] = event
                .digest
                .as_slice()
                .try_into()
                .map_err(|_| VerificationError::TdxEventDigestMismatch)?;
            if !bool::from(carried.ct_eq(&computed)) {
                return Err(VerificationError::TdxEventDigestMismatch);
            }
        }
        Ok(computed)
    } else {
        event
            .digest
            .as_slice()
            .try_into()
            .map_err(|_| VerificationError::TdxEventLogMismatch)
    }
}

/// What a verified event log binds this TD to, beyond the launch identity the
/// quote already carries: which application, which instance of it, launched
/// from which compose file on which OS image, keyed by which KMS.
///
/// `instance_id` is the piece the quote alone cannot give. `MRTD` and the
/// boot registers name an image, and every TD launched from that image shares
/// them; the instance id is extended into `RTMR3` and identifies the deployed
/// guest. Its seed persists on the data disk and can be cloned; it is not a
/// unique physical processor identity.
#[derive(Debug, Clone)]
pub(crate) struct TdxRuntimeBindings {
    pub(crate) compose_hash: [u8; 32],
    pub(crate) instance_id: Vec<u8>,
}

/// The named events a legacy dstack log must bind exactly once. Missing means the
/// log is not from a completed dstack boot; twice means someone appended a
/// second claim after the fact, and first-wins or last-wins would each make
/// one of those claims silently authoritative.
const REQUIRED_BINDING_EVENTS: [&str; 5] = [
    "compose-hash",
    "app-id",
    "instance-id",
    "os-image-hash",
    "mr-kms",
];

/// The register dstack extends named application events into.
const APP_EVENT_IMR: u32 = 3;

/// Verifies a runtime event log against the registers a verified quote signed
/// and returns what the log binds the TD to.
///
/// Two judgements. Every register the report carries must equal the fold of
/// the log's effective digests for it, from forty-eight zero bytes through
/// `sha384(register || digest)` per event, so the log accounts for exactly
/// what the TD extended: nothing missing, nothing invented, nothing
/// reordered. A runtime event's effective digest is derived from its own name
/// and payload (see [`effective_digest`]), so a payload cannot ride on the
/// digest of a different claim and survive the fold. And each required binding
/// must appear exactly once.
///
/// The caller compares the returned `compose_hash` against the workload it
/// expected; this function establishes what the TD is bound to, not whether
/// that binding is the one the caller wanted.
pub(crate) fn verify_tdx_event_log(
    report: &TdReport,
    events: &[TdxEvent],
    profile: TdxRuntimeProfile,
) -> Result<TdxRuntimeBindings, VerificationError> {
    verify_tdx_event_log_for_key_provider(report, events, profile, DstackKeyProvider::ManagedKms)
}

pub(crate) fn verify_tdx_event_log_for_key_provider(
    report: &TdReport,
    events: &[TdxEvent],
    profile: TdxRuntimeProfile,
    key_provider: DstackKeyProvider,
) -> Result<TdxRuntimeBindings, VerificationError> {
    if key_provider == DstackKeyProvider::EphemeralNone
        && !matches!(profile, TdxRuntimeProfile::Dstack061 { .. })
    {
        return Err(VerificationError::TdxUnknownLaunchMeasurement);
    }
    let mut folds = [[0u8; 48]; 4];
    for event in events {
        // dstack runtime events exclusively extend RTMR3. Treating an RTMR3
        // entry as opaque firmware would let a caller retain a real runtime
        // digest while hiding its name/payload from boot-identity checks.
        // Conversely, firmware occupies RTMR0..2, not the application register.
        if (event.imr == APP_EVENT_IMR) != (event.event_type == DSTACK_RUNTIME_EVENT_TYPE) {
            return Err(VerificationError::TdxEventLogMismatch);
        }
        let digest = effective_digest(event)?;
        let register = folds
            .get_mut(event.imr as usize)
            .ok_or(VerificationError::TdxEventLogMismatch)?;
        let mut hasher = Sha384::new();
        hasher.update(*register);
        hasher.update(digest);
        *register = hasher.finalize().into();
    }
    for (fold, quoted) in
        folds
            .iter()
            .zip([&report.rtmr0, &report.rtmr1, &report.rtmr2, &report.rtmr3])
    {
        if !bool::from(fold.ct_eq(quoted)) {
            return Err(VerificationError::TdxEventLogMismatch);
        }
    }

    if let TdxRuntimeProfile::Dstack061 { os_image_hash } = profile {
        return verify_dstack061_bindings(events, &os_image_hash, key_provider);
    }

    let mut bindings: Vec<(&str, &[u8])> = Vec::new();
    for event in events {
        if event.event_type != DSTACK_RUNTIME_EVENT_TYPE || event.imr != APP_EVENT_IMR {
            continue;
        }
        if REQUIRED_BINDING_EVENTS.contains(&event.name.as_str()) {
            bindings.push((&event.name, &event.payload));
        }
    }

    let exactly_one = |name: &str| -> Result<Vec<u8>, VerificationError> {
        let mut found = bindings.iter().filter(|(n, _)| *n == name);
        let first = found
            .next()
            .ok_or(VerificationError::TdxEventLogIncomplete)?;
        if found.next().is_some() {
            return Err(VerificationError::TdxEventLogIncomplete);
        }
        Ok(first.1.to_vec())
    };

    // The bindings not carried out of here are still held to exactly-once:
    // a log that binds the application register without naming its app, OS
    // image or KMS is not a completed dstack boot, whether or not a caller
    // reads those values yet.
    exactly_one("app-id")?;
    exactly_one("os-image-hash")?;
    exactly_one("mr-kms")?;
    let compose_hash: [u8; 32] = exactly_one("compose-hash")?
        .try_into()
        .map_err(|_| VerificationError::TdxEventLogIncomplete)?;
    Ok(TdxRuntimeBindings {
        compose_hash,
        instance_id: exactly_one("instance-id")?,
    })
}

/// Boot identities stop at system-ready; later events are app-controlled.
/// Keep replaying the entire log, but never let a later event supply a missing
/// boot identity or replace one. Key-provider admission is explicit; the
/// existing public verification path continues to require the managed KMS.
fn verify_dstack061_bindings(
    events: &[TdxEvent],
    expected_os_image_hash: &[u8; 32],
    key_provider: DstackKeyProvider,
) -> Result<TdxRuntimeBindings, VerificationError> {
    const REQUIRED: [&str; 5] = [
        "compose-hash",
        "app-id",
        "instance-id",
        "os-image-hash",
        "key-provider",
    ];
    let mut bindings: Vec<(&str, &[u8])> = Vec::new();
    let mut ready = false;
    for event in events {
        if event.imr != APP_EVENT_IMR || event.event_type != DSTACK_RUNTIME_EVENT_TYPE {
            continue;
        }
        if event.name == "system-ready" {
            if ready || !event.payload.is_empty() {
                return Err(VerificationError::TdxEventLogIncomplete);
            }
            ready = true;
        } else if event.name == "mr-kms" {
            return Err(VerificationError::TdxEventLogIncomplete);
        } else if REQUIRED.contains(&event.name.as_str())
            || (key_provider == DstackKeyProvider::EphemeralNone
                && matches!(event.name.as_str(), "storage-encrypted" | "storage-fs"))
        {
            if ready || bindings.iter().any(|(name, _)| *name == event.name) {
                return Err(VerificationError::TdxEventLogIncomplete);
            }
            bindings.push((&event.name, &event.payload));
        }
    }
    if !ready {
        return Err(VerificationError::TdxEventLogIncomplete);
    }
    let get = |name: &str| {
        bindings
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, p)| *p)
            .ok_or(VerificationError::TdxEventLogIncomplete)
    };
    let compose_hash = get("compose-hash")?
        .try_into()
        .map_err(|_| VerificationError::TdxEventLogIncomplete)?;
    if get("app-id")?.len() != 20 || get("instance-id")?.len() != 20 {
        return Err(VerificationError::TdxEventLogIncomplete);
    }
    // Pinned dstack 0.6.1 emits os-image-hash only on the KMS response path
    // (system_setup.rs request_app_keys_from_kms). None-mode has no such
    // response: its OS is authenticated by the complete independently pinned
    // MRTD+RTMR0/1/2 launch tuple selected before reaching this function.
    // Require absence in none-mode, rather than accepting an optional claim.
    let wrong_os_binding = match key_provider {
        DstackKeyProvider::ManagedKms => get("os-image-hash")? != expected_os_image_hash,
        DstackKeyProvider::EphemeralNone => {
            bindings.iter().any(|(name, _)| *name == "os-image-hash")
        }
    };
    if wrong_os_binding {
        return Err(VerificationError::TdxEventLogIncomplete);
    }
    if key_provider == DstackKeyProvider::EphemeralNone
        && (get("storage-encrypted")? != b"1" || get("storage-fs")? != b"ext4")
    {
        return Err(VerificationError::TdxEventLogIncomplete);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct KeyProvider {
        name: String,
        id: String,
    }
    let provider: KeyProvider = serde_json::from_slice(get("key-provider")?)
        .map_err(|_| VerificationError::TdxEventLogIncomplete)?;
    let allowed = match key_provider {
        DstackKeyProvider::ManagedKms => {
            provider.name == "kms"
                && !provider.id.is_empty()
                && provider.id.len() <= 1024
                && hex::decode(&provider.id).is_ok()
        }
        DstackKeyProvider::EphemeralNone => provider.name == "none" && provider.id.is_empty(),
    };
    if !allowed {
        return Err(VerificationError::TdxEventLogIncomplete);
    }
    Ok(TdxRuntimeBindings {
        compose_hash,
        instance_id: get("instance-id")?.to_vec(),
    })
}

/// Runs Intel's verification over the quote and the caller-supplied collateral
/// and reduces the outcome to the fields this crate goes on to judge.
///
/// `now` is the caller's clock in seconds since the epoch, checked against the
/// collateral's validity windows inside the DCAP walk. Expired CRLs or TCB
/// info refuse the quote outright: stale collateral is indistinguishable from
/// collateral chosen to hide a revocation.
pub(crate) fn verify_quote(
    quote: &[u8],
    collateral_json: &str,
    now_unix: u64,
) -> Result<TdReport, VerificationError> {
    let collateral: QuoteCollateralV3 = serde_json::from_str(collateral_json)
        .map_err(|_| VerificationError::TdxCollateralMalformed)?;

    let verified = dcap_verify(quote, &collateral, now_unix)
        .map_err(|_| VerificationError::TdxQuoteRejected)?;

    if verified.status != ACCEPTED_TCB_STATUS || !verified.advisory_ids.is_empty() {
        return Err(VerificationError::TdxTcbStatusNotAccepted);
    }

    let td = verified
        .report
        .as_td10()
        .ok_or(VerificationError::TdxNotTdQuote)?;

    Ok(TdReport {
        mr_td: td.mr_td,
        rtmr0: td.rt_mr0,
        rtmr1: td.rt_mr1,
        rtmr2: td.rt_mr2,
        rtmr3: td.rt_mr3,
        report_data: td.report_data,
    })
}
