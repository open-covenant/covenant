//! Attestation for application-key recipients, independent of SSH admission.
//!
//! This verifies vendor evidence only. The caller must derive report data from
//! its retained challenge, authorize the tenant/key/lease, and compare both
//! returned Cloud identities. A successful call is not production qualification.

use p384::elliptic_curve::subtle::ConstantTimeEq;

use crate::{
    Policy, VerificationError,
    binding::{PhalaEphemeralIdentity, parse_events},
    tdx::{self, DstackKeyProvider},
    vendor_types::TdxEventEntry,
};

/// Verify a fresh application-key quote using the compiled OS launch tuple,
/// Intel DCAP signatures and current collateral, full RTMR replay, explicit
/// `key_provider:none`, and the caller's exact report-data/workload pins.
///
/// The time must come from a trusted clock or a fresh nonce-bound trusted time
/// assertion. Neither the guest's clock nor unsigned host input is sufficient.
/// This API deliberately accepts no caller-supplied launch policy/test roots.
pub fn verify_phala_ephemeral_key_attestation(
    quote: &[u8],
    collateral_json: &str,
    events: &[TdxEventEntry],
    expected_report_data: &[u8; 64],
    expected_workload: &[u8; 32],
    now_ms: u64,
) -> Result<PhalaEphemeralIdentity, VerificationError> {
    if quote.is_empty()
        || quote.len() > 24 * 1024
        || collateral_json.len() > 128 * 1024
        || events.is_empty()
        || events.len() > 256
        || now_ms == 0
        || now_ms > (1_u64 << 53) - 1
    {
        return Err(VerificationError::MalformedEvidence);
    }
    let parsed = parse_events(events).map_err(|_| VerificationError::MalformedEvidence)?;
    let report = tdx::verify_quote(quote, collateral_json, now_ms / 1000)?;
    if !bool::from(report.report_data.ct_eq(expected_report_data)) {
        return Err(VerificationError::TdxReportDataMismatch);
    }
    let policy = Policy::default();
    let profile = policy
        .tdx_measurements()
        .runtime_profile(&report)
        .ok_or(VerificationError::TdxUnknownLaunchMeasurement)?;
    let bindings = tdx::verify_tdx_event_log_for_key_provider(
        &report,
        &parsed,
        profile,
        DstackKeyProvider::EphemeralNone,
    )?;
    if !bool::from(bindings.compose_hash.ct_eq(expected_workload)) {
        return Err(VerificationError::TdxComposeHashMismatch);
    }
    // These names were checked exactly once, before system-ready, by the
    // authenticated dstack061 event-log verifier above. Never use Info hints.
    let app = parsed
        .iter()
        .find(|event| event.imr == 3 && event.name == "app-id")
        .ok_or(VerificationError::TdxEventLogIncomplete)?;
    Ok(PhalaEphemeralIdentity {
        app_id: hex::encode(&app.payload),
        instance_id: hex::encode(&bindings.instance_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_missing_or_oversized_evidence_before_vendor_parsing() {
        let event = TdxEventEntry {
            imr: 3,
            event_type: 0x0800_0001,
            event: "system-ready".into(),
            digest: String::new(),
            event_payload: String::new(),
            version: 1,
            preimage: None,
        };
        for (quote, events, time) in [
            (vec![], vec![event.clone()], 1),
            (vec![0; 24 * 1024 + 1], vec![event.clone()], 1),
            (vec![0], vec![], 1),
            (vec![0], vec![event.clone(); 257], 1),
            (vec![0], vec![event.clone()], 0),
            (vec![0], vec![event], 1_u64 << 53),
        ] {
            assert_eq!(
                verify_phala_ephemeral_key_attestation(
                    &quote, "{}", &events, &[0; 64], &[0; 32], time
                ),
                Err(VerificationError::MalformedEvidence)
            );
        }
    }
}
