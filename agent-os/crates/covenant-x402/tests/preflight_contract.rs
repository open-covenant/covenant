use covenant_x402::preflight::{
    evaluate_preflight, payment_intent_hash, verify_preflight_receipt, PaymentIntentV1,
    PaymentPolicyV1, PreflightError, PreflightOutcome, PreflightReceiptV1,
};
use sha2::{Digest, Sha256};

const ACCEPTED: &str = include_str!("fixtures/preflight-v1/accepted.json");
const INTENT: &str = include_str!("fixtures/preflight-v1/intent.json");
const POLICY: &str = include_str!("fixtures/preflight-v1/policy.json");
const RECEIPT: &str = include_str!("fixtures/preflight-v1/advisory-receipt.json");
const EVALUATED_AT_MS: u64 = 1_785_456_001_000;
const JSON_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn intent() -> PaymentIntentV1 {
    serde_json::from_str(INTENT).expect("intent fixture")
}

fn policy() -> PaymentPolicyV1 {
    serde_json::from_str(POLICY).expect("policy fixture")
}

#[test]
fn golden_allow_receipt_is_pinned() {
    let actual = evaluate_preflight(&intent(), &policy(), EVALUATED_AT_MS).expect("evaluate");
    let expected: PreflightReceiptV1 = serde_json::from_str(RECEIPT).expect("receipt fixture");
    assert_eq!(actual, expected);
    assert_eq!(actual.outcome, PreflightOutcome::Allow);
    assert!(actual.reason_codes.is_empty());
    assert!(verify_preflight_receipt(&actual, &policy()).unwrap());
}

#[test]
fn intent_hash_is_stable_across_json_key_order() {
    let reordered = r#"{
      "expires_at_ms":1785456060000,
      "observed_at_ms":1785456000000,
      "payment":{
        "transaction_profile":"sponsored_v0_static_v1",
        "compute_unit_price_micro_lamports":1,
        "compute_unit_limit":20000,
        "memo":"pay_7d5d747be160e280504c099d984bcfe0",
        "destination_ata":"FaZxn8A41KgRZmm8vLrt4aZNVTDtRBBYS37FjCEKpXBs",
        "source_ata":"5SvmwUbZCqWGi4VdgPryvk7FDUPRzHY4AaXoS4w83h3k",
        "pay_to":"7G73PLhKvAPBGTzG5ESAE4coE7QrVeTTKfhTxQZbyGgC",
        "decimals":6,
        "transfer_amount":"80000",
        "required_amount":"80000",
        "token_program":"TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
        "mint":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        "fee_payer":"2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4",
        "funder":"9VaDVp1Wb78G4Wm6VuTiMrpESjrUymXefQTHcJGRSTEA",
        "network":"solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
      },
      "request":{
        "body_sha256":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "method":"POST",
        "url":"https://provider.example/resource?format=json"
      },
      "protocol":{
        "payment_identifier":"pay_7d5d747be160e280504c099d984bcfe0",
        "accepted_sha256":"sha256:29b617135032d52687ac97ccdb263ef14da4a0f7e264f23338c99ab2b7828781",
        "scheme":"exact",
        "version":2,
        "name":"x402"
      },
      "schema":"covenant.payment-intent.v1"
    }"#;
    let reordered: PaymentIntentV1 = serde_json::from_str(reordered).expect("reordered intent");
    assert_eq!(
        payment_intent_hash(&intent()).unwrap(),
        payment_intent_hash(&reordered).unwrap()
    );
}

#[test]
fn accepted_requirement_digest_is_pinned() {
    let accepted: serde_json::Value = serde_json::from_str(ACCEPTED).unwrap();
    let canonical = serde_jcs::to_vec(&accepted).unwrap();
    let digest = Sha256::digest(canonical);
    let actual = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        intent().protocol.accepted_sha256,
        format!("sha256:{actual}")
    );
}

#[test]
fn contracts_reject_unknown_and_missing_fields() {
    let mut unknown: serde_json::Value = serde_json::from_str(INTENT).unwrap();
    unknown["untrusted_extension"] = serde_json::json!(true);
    assert!(serde_json::from_value::<PaymentIntentV1>(unknown).is_err());

    let mut missing: serde_json::Value = serde_json::from_str(POLICY).unwrap();
    missing.as_object_mut().unwrap().remove("source_ata");
    assert!(serde_json::from_value::<PaymentPolicyV1>(missing).is_err());
}

#[test]
fn receipt_cannot_claim_signer_enforcement() {
    let mut receipt: serde_json::Value = serde_json::from_str(RECEIPT).unwrap();
    receipt["enforcement"]["decision_consumed_by_signer"] = serde_json::json!(true);
    assert!(serde_json::from_value::<PreflightReceiptV1>(receipt).is_err());
}

#[test]
fn wire_contract_rejects_integers_above_the_published_schema_maximum() {
    let above = serde_json::json!(JSON_SAFE_INTEGER + 1);

    for path in ["observed_at_ms", "expires_at_ms"] {
        let mut value: serde_json::Value = serde_json::from_str(INTENT).unwrap();
        value[path] = above.clone();
        assert!(
            serde_json::from_value::<PaymentIntentV1>(value).is_err(),
            "intent accepted out-of-schema {path}"
        );
    }

    let mut intent_value: serde_json::Value = serde_json::from_str(INTENT).unwrap();
    intent_value["payment"]["compute_unit_price_micro_lamports"] = above.clone();
    assert!(serde_json::from_value::<PaymentIntentV1>(intent_value).is_err());

    for path in [
        "max_compute_unit_price_micro_lamports",
        "max_intent_lifetime_ms",
    ] {
        let mut value: serde_json::Value = serde_json::from_str(POLICY).unwrap();
        value[path] = above.clone();
        assert!(
            serde_json::from_value::<PaymentPolicyV1>(value).is_err(),
            "policy accepted out-of-schema {path}"
        );
    }

    let mut receipt_value: serde_json::Value = serde_json::from_str(RECEIPT).unwrap();
    receipt_value["evaluated_at_ms"] = above;
    assert!(serde_json::from_value::<PreflightReceiptV1>(receipt_value).is_err());
}

#[test]
fn programmatic_out_of_schema_values_cannot_emit_a_receipt() {
    let mut invalid = intent();
    invalid.observed_at_ms = JSON_SAFE_INTEGER + 1;

    assert!(matches!(
        evaluate_preflight(&invalid, &policy(), EVALUATED_AT_MS),
        Err(PreflightError::JsonSafeInteger {
            field: "observed_at_ms",
            value
        }) if value == JSON_SAFE_INTEGER + 1
    ));
    assert!(serde_json::to_value(&invalid).is_err());

    assert!(matches!(
        evaluate_preflight(&intent(), &policy(), JSON_SAFE_INTEGER + 1),
        Err(PreflightError::JsonSafeInteger {
            field: "evaluated_at_ms",
            value
        }) if value == JSON_SAFE_INTEGER + 1
    ));
}

#[test]
fn receipt_verification_detects_tampering() {
    let mut receipt = evaluate_preflight(&intent(), &policy(), EVALUATED_AT_MS).unwrap();
    receipt.intent.payment.transfer_amount = "80001".into();
    assert!(!verify_preflight_receipt(&receipt, &policy()).unwrap());
}

#[test]
fn published_schemas_are_valid_json_and_closed_at_the_root() {
    let schemas = [
        include_str!("../../../../docs/schemas/payment-intent-v1.schema.json"),
        include_str!("../../../../docs/schemas/payment-policy-v1.schema.json"),
        include_str!("../../../../docs/schemas/preflight-receipt-v1.schema.json"),
    ];
    for source in schemas {
        let schema: serde_json::Value = serde_json::from_str(source).expect("schema JSON");
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(schema["additionalProperties"], false);
        assert_schema_numeric_bounds_are_js_safe(&schema);
    }
}

fn assert_schema_numeric_bounds_are_js_safe(value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(maximum) = fields.get("maximum").and_then(serde_json::Value::as_u64) {
                assert!(
                    maximum <= JSON_SAFE_INTEGER,
                    "schema numeric maximum {maximum} exceeds JavaScript's exact integer range"
                );
            }
            for child in fields.values() {
                assert_schema_numeric_bounds_are_js_safe(child);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                assert_schema_numeric_bounds_are_js_safe(child);
            }
        }
        _ => {}
    }
}
