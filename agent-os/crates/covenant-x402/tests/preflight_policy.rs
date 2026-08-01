use covenant_x402::preflight::{
    evaluate_preflight, PaymentIntentV1, PaymentPolicyV1, PreflightOutcome, PreflightReasonCode,
};

const INTENT: &str = include_str!("fixtures/preflight-v1/intent.json");
const POLICY: &str = include_str!("fixtures/preflight-v1/policy.json");
const NOW: u64 = 1_785_456_001_000;

fn intent() -> PaymentIntentV1 {
    serde_json::from_str(INTENT).unwrap()
}

fn policy() -> PaymentPolicyV1 {
    serde_json::from_str(POLICY).unwrap()
}

fn assert_denied(
    intent: &PaymentIntentV1,
    policy: &PaymentPolicyV1,
    now: u64,
    expected: PreflightReasonCode,
) {
    let receipt = evaluate_preflight(intent, policy, now).unwrap();
    assert_eq!(receipt.outcome, PreflightOutcome::Deny);
    assert!(
        receipt.reason_codes.contains(&expected),
        "missing {expected:?} in {:?}",
        receipt.reason_codes
    );
}

#[test]
fn protocol_and_evidence_mutations_deny() {
    let policy = policy();

    let mut changed = intent();
    changed.protocol.version = 1;
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::UnsupportedX402Version,
    );

    let mut changed = intent();
    changed.protocol.accepted_sha256 = "sha256:ABC".into();
    assert_denied(&changed, &policy, NOW, PreflightReasonCode::DigestInvalid);

    let mut changed = intent();
    changed.protocol.payment_identifier = "short".into();
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::PaymentIdentifierInvalid,
    );

    let mut changed = intent();
    changed.request.url = "https://other.example/resource?format=json".into();
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::ResourceNotAllowed,
    );

    let mut changed = intent();
    changed.request.method = "GET".into();
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::ResourceNotAllowed,
    );

    let mut changed = intent();
    changed.request.url = "https://provider.example/resource?format=xml".into();
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::ResourceNotAllowed,
    );
}

#[test]
fn solana_scope_mutations_deny() {
    let policy = policy();

    let mut changed = intent();
    changed.payment.network = covenant_x402::preflight::SOLANA_DEVNET.into();
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::NetworkNotAllowed,
    );

    let mut changed = intent();
    changed.payment.mint = covenant_x402::preflight::USDC_DEVNET_MINT.into();
    assert_denied(&changed, &policy, NOW, PreflightReasonCode::MintNotAllowed);

    let mut changed = intent();
    changed.payment.token_program = "TokenzQdYJ2qgG3AqYpS1JvJp8JrQV9GkqQ7sm5F".into();
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::TokenProgramNotAllowed,
    );

    let mut changed = intent();
    changed.payment.decimals = 9;
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::DecimalsMismatch,
    );

    let mut changed = intent();
    changed.payment.funder = changed.payment.pay_to.clone();
    assert_denied(&changed, &policy, NOW, PreflightReasonCode::FunderMismatch);

    let mut changed = intent();
    changed.payment.fee_payer = changed.payment.pay_to.clone();
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::FeePayerNotAllowed,
    );

    let mut changed = intent();
    changed.payment.pay_to = changed.payment.funder.clone();
    assert_denied(&changed, &policy, NOW, PreflightReasonCode::PayToMismatch);

    let mut changed = intent();
    changed.payment.source_ata = changed.payment.destination_ata.clone();
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::SourceAtaMismatch,
    );

    let mut changed = intent();
    changed.payment.destination_ata = changed.payment.source_ata.clone();
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::DestinationAtaMismatch,
    );

    let mut changed = intent();
    changed.payment.funder = "not-base58".into();
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::SolanaAddressInvalid,
    );
}

#[test]
fn amount_memo_compute_and_time_mutations_deny() {
    let policy = policy();

    for value in ["0", "080000", "-1", "18446744073709551616"] {
        let mut changed = intent();
        changed.payment.transfer_amount = value.into();
        assert_denied(&changed, &policy, NOW, PreflightReasonCode::AmountInvalid);
    }

    let mut changed = intent();
    changed.payment.transfer_amount = "80001".into();
    assert_denied(&changed, &policy, NOW, PreflightReasonCode::AmountMismatch);

    let mut changed = intent();
    changed.payment.required_amount = "100001".into();
    changed.payment.transfer_amount = "100001".into();
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::AmountExceedsLimit,
    );

    let mut changed = intent();
    changed.payment.memo = "different-payment-reference".into();
    assert_denied(&changed, &policy, NOW, PreflightReasonCode::MemoInvalid);

    let mut changed = intent();
    changed.payment.compute_unit_limit = 20_001;
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::ComputeBudgetExceeded,
    );

    let mut changed = intent();
    changed.expires_at_ms = changed.observed_at_ms;
    assert_denied(
        &changed,
        &policy,
        NOW,
        PreflightReasonCode::TimeWindowInvalid,
    );

    assert_denied(
        &intent(),
        &policy,
        1_785_455_999_999,
        PreflightReasonCode::NotYetValid,
    );
    assert_denied(
        &intent(),
        &policy,
        1_785_456_060_000,
        PreflightReasonCode::Expired,
    );
}

#[test]
fn malformed_or_expanded_policy_denies_closed() {
    let intent = intent();

    let mut changed = policy();
    changed
        .allowed_fee_payers
        .push(changed.allowed_fee_payers[0].clone());
    assert_denied(&intent, &changed, NOW, PreflightReasonCode::PolicyInvalid);

    let mut changed = policy();
    changed.network = "solana:unknown".into();
    assert_denied(&intent, &changed, NOW, PreflightReasonCode::PolicyInvalid);

    let mut changed = policy();
    changed.routes[0].origin = "http://provider.example".into();
    assert_denied(&intent, &changed, NOW, PreflightReasonCode::PolicyInvalid);

    let mut changed = policy();
    changed.routes.push(changed.routes[0].clone());
    assert_denied(&intent, &changed, NOW, PreflightReasonCode::PolicyInvalid);
}

#[test]
fn denial_reason_order_is_pinned() {
    let policy = policy();
    let mut changed = intent();
    changed.protocol.version = 1;
    changed.protocol.accepted_sha256 = "invalid".into();
    changed.protocol.payment_identifier = "short".into();
    changed.request.url = "https://other.example/resource".into();
    changed.payment.network = covenant_x402::preflight::SOLANA_DEVNET.into();

    let receipt = evaluate_preflight(&changed, &policy, NOW).unwrap();
    assert_eq!(
        receipt.reason_codes,
        vec![
            PreflightReasonCode::UnsupportedX402Version,
            PreflightReasonCode::DigestInvalid,
            PreflightReasonCode::PaymentIdentifierInvalid,
            PreflightReasonCode::ResourceNotAllowed,
            PreflightReasonCode::NetworkNotAllowed,
            PreflightReasonCode::MemoInvalid,
        ]
    );
}
