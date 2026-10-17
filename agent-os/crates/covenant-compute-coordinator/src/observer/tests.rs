use super::*;
use covenant_audit::InMemoryAuditLog;
use covenant_identity::LocalIdentity;

fn config() -> ObserverConfig {
    let encoded = |b| bs58::encode([b; 32]).into_string();
    ObserverConfig {
        genesis_hash: "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d".into(),
        rpc_url: "http://127.0.0.1:1".into(),
        er_rpc_url: "http://127.0.0.1:1".into(),
        program_id: encoded(1),
        renter: encoded(2),
        coordinator: encoded(3),
        mint: encoded(4),
        validator: encoded(5),
        interval: Duration::from_secs(60),
        grace: Duration::from_secs(120),
    }
}

fn account(owner: &str, bytes: &[u8]) -> Value {
    json!({"owner":owner, "executable":false, "data":[base64::engine::general_purpose::STANDARD.encode(bytes), "base64"]})
}

/// Serialize the Anchor declaration in field order, independently of the
/// decoder's offsets. Includes every flag and all three PDA bumps.
fn fixture() -> (ObserverConfig, String, Vec<u8>, Vec<u8>) {
    let config = config();
    let mut data = discriminator("LeaseTerms").to_vec();
    let job = Uuid::from_bytes([7; 16]);
    data.extend(job.as_bytes());
    for k in [
        key(&config.renter).unwrap(),
        [6; 32],
        key(&config.coordinator).unwrap(),
        key(&config.validator).unwrap(),
        key(&config.mint).unwrap(),
    ] {
        data.extend(k);
    }
    for n in [100u64, 600, 60_000, 100] {
        data.extend(n.to_le_bytes());
    }
    data.extend([0, 0, 0, 0, 255, 254, 253]);
    let address = address(
        &[b"lease", &key(&config.renter).unwrap(), job.as_bytes()],
        255,
        &key(&config.program_id).unwrap(),
    );
    let mut meter = discriminator("LeaseMeter").to_vec();
    meter.extend(key(&address).unwrap());
    meter.extend(key(&config.coordinator).unwrap());
    meter.extend(job.as_bytes());
    meter.extend(12_345u64.to_le_bytes());
    meter.extend([9; 32]);
    meter.extend([0, 254]);
    (config, address, data, meter)
}

fn state() -> CoordinatorState {
    CoordinatorState::new(
        LocalIdentity::generate("observer@test"),
        crate::CoordinatorConfig::default(),
        Arc::new(crate::NoReputation),
        Arc::new(crate::MockPayout::new()),
        Arc::new(InMemoryAuditLog::new()),
    )
}

#[test]
fn decodes_the_actual_anchor_field_order_and_binds_both_accounts() {
    let (config, address, bytes, meter) = fixture();
    assert_eq!(bytes.len(), TERMS_LEN);
    assert_eq!(meter.len(), METER_LEN);
    let terms =
        decode_terms_account(&address, &account(&config.program_id, &bytes), &config).unwrap();
    assert_eq!(terms.job_id, Uuid::from_bytes([7; 16]));
    assert_eq!(terms.opened_at, 100);
    assert_eq!(terms.charge(12_345), 1_235);
    assert_eq!(terms.charge(u64::MAX), 60_000);
    assert_eq!(
        decode_meter(
            &account(&config.program_id, &meter),
            &address,
            &terms,
            &config
        )
        .unwrap()
        .metered_ms,
        12_345
    );
    let mut wrong = meter;
    wrong[72] ^= 1;
    assert!(decode_meter(
        &account(&config.program_id, &wrong),
        &address,
        &terms,
        &config
    )
    .is_err());
}

#[test]
fn unknown_layout_boolean_owner_and_pins_never_become_healthy_accounts() {
    let (config, address, bytes, _) = fixture();
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(
        decode_terms_account(&address, &account(&config.program_id, &longer), &config).is_err()
    );
    let mut flag = bytes.clone();
    flag[219] = 2;
    assert!(decode_terms_account(&address, &account(&config.program_id, &flag), &config).is_err());
    assert!(decode_terms_account(&address, &account(DELEGATION_PROGRAM, &bytes), &config).is_err());
    let mut wrong = config.clone();
    wrong.mint = bs58::encode([10; 32]).into_string();
    assert!(decode_terms_account(&address, &account(&config.program_id, &bytes), &wrong).is_err());
    assert!(decode_terms_account(
        &config.renter,
        &account(&config.program_id, &bytes),
        &config
    )
    .is_err());
}

#[test]
fn an_orphan_is_found_from_chain_without_any_local_job() {
    let (config, address, bytes, _) = fixture();
    let terms =
        decode_terms_account(&address, &account(&config.program_id, &bytes), &config).unwrap();
    assert!(compare_lease(&terms, None, None, 101_000, config.grace).is_empty());
    assert_eq!(
        compare_lease(&terms, None, None, 300_000, config.grace),
        vec![FindingKind::OrphanedFundedLease]
    );
    assert_eq!(
        compare_lease(&terms, None, None, 900_000, config.grace),
        vec![
            FindingKind::ExpiredUnsettledLease,
            FindingKind::OrphanedFundedLease
        ]
    );
}

#[test]
fn only_committed_meter_bytes_size_a_settled_charge() {
    let (config, address, mut bytes, _) = fixture();
    bytes[216] = 1;
    bytes[217] = 1;
    bytes[219] = 1;
    let terms =
        decode_terms_account(&address, &account(&config.program_id, &bytes), &config).unwrap();
    let local = LocalLease {
        phase: JobPhase::Refunded,
        concluded_at_ms: Some(110_000),
        expected_terms: Some((100, 600, terms.operator.clone())),
        settled_charge: Some(0),
    };
    assert!(compare_lease(
        &terms,
        Some(MeterEvidence::Uncommitted),
        Some(&local),
        300_000,
        config.grace
    )
    .is_empty());
    assert_eq!(
        compare_lease(
            &terms,
            Some(MeterEvidence::Committed(12_345)),
            Some(&local),
            300_000,
            config.grace
        ),
        vec![FindingKind::SettlementMismatch]
    );
    // An unreadable meter means unknown, never an invented zero settlement.
    assert!(compare_lease(&terms, None, Some(&local), 300_000, config.grace).is_empty());
}

#[test]
fn terminal_local_books_do_not_hide_a_funded_chain_vault() {
    let (config, address, bytes, _) = fixture();
    let terms =
        decode_terms_account(&address, &account(&config.program_id, &bytes), &config).unwrap();
    let local = LocalLease {
        phase: JobPhase::Refunded,
        concluded_at_ms: Some(110_000),
        expected_terms: Some((100, 600, terms.operator.clone())),
        settled_charge: Some(0),
    };
    assert!(compare_lease(&terms, None, Some(&local), 115_000, config.grace).is_empty());
    assert_eq!(
        compare_lease(&terms, None, Some(&local), 300_000, config.grace),
        vec![FindingKind::LocalTerminalChainOpen]
    );
}

#[test]
fn status_is_stale_or_blind_without_making_up_an_insolvency() {
    let mut report = ObserverReport {
        schema_version: 1,
        observed_at_ms: 1_000,
        scan_complete: true,
        l1_slot: Some(8),
        scanned_leases: 4,
        findings: Vec::new(),
    };
    assert_eq!(report.status(), ObserverStatus::Healthy);
    report.push(FindingKind::RpcUnavailable, None, None);
    assert_eq!(report.status(), ObserverStatus::Blind);
    assert_eq!(
        public_observation(Some(&report), 182_000, Duration::from_secs(60)).status,
        ObserverStatus::Stale
    );
    let public = serde_json::to_value(public_observation(
        Some(&report),
        2_000,
        Duration::from_secs(60),
    ))
    .unwrap();
    assert!(public.get("findings").is_none());
    assert_eq!(public["finding_counts"]["rpc_unavailable"], 1);
}

#[test]
fn incident_record_requires_disclosure_and_chronology() {
    assert!(validate_incidents(&incident_record()).is_ok());
    let mut r = IncidentRecord {
        schema_version: 1,
        incidents: vec![Incident {
            id: "example".into(),
            started_at_ms: 1_000,
            resolved_at_ms: Some(3_000),
            effect: "Rentals could not start.".into(),
            cause: "Provider API unavailable.".into(),
            fix: "Provider recovered.".into(),
            updates: vec![IncidentUpdate {
                at_ms: 2_000,
                message: "Investigating.".into(),
            }],
        }],
    };
    assert!(validate_incidents(&r).is_ok());
    let mut duplicate = r.clone();
    duplicate.incidents.push(duplicate.incidents[0].clone());
    assert!(validate_incidents(&duplicate).is_err());
    r.incidents[0].updates[0].at_ms = 4_000;
    assert!(validate_incidents(&r).is_err());
    r.incidents[0].updates[0].at_ms = 2_000;
    r.incidents[0].cause.clear();
    assert!(validate_incidents(&r).is_err());
    r.schema_version = 2;
    assert!(validate_incidents(&r).is_err());
}

async fn rpc_fixture(
    delegated: bool,
    wrong_validator: bool,
) -> (ObserverConfig, tokio::task::JoinHandle<()>) {
    let (mut config, address, mut terms, meter) = fixture();
    if delegated {
        terms[219] = 1;
    }
    let program = config.program_id.clone();
    let validator = config.validator.clone();
    let handler = move |Json(request): Json<Value>| {
        let (address, terms, meter, program, validator) = (
            address.clone(),
            terms.clone(),
            meter.clone(),
            program.clone(),
            validator.clone(),
        );
        async move {
            let result = match request["method"].as_str().unwrap() {
                "getGenesisHash" => json!("5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"),
                "getBlockTime" => json!(100),
                "getAccountInfo" if request["params"][0] == program => {
                    json!({"context":{"slot":100},"value":{"executable":true}})
                }
                "getProgramAccounts" => {
                    json!({"context":{"slot":100},"value":[{"pubkey":address,"account":account(&program, &terms)}]})
                }
                "getMultipleAccounts" => {
                    json!({"context":{"slot":101},"value":[account(&program, &terms),
                    if delegated { account(DELEGATION_PROGRAM, &[]) } else { account(&program, &meter) }]})
                }
                "getIdentity" => {
                    json!({"identity":if wrong_validator { "wrong".to_owned() } else { validator }})
                }
                "getAccountInfo" => json!({"context":{"slot":3},"value":account(&program, &meter)}),
                _ => panic!("unexpected RPC"),
            };
            Json(json!({"jsonrpc":"2.0","id":1,"result":result}))
        }
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    config.rpc_url = format!("http://{}", listener.local_addr().unwrap());
    config.er_rpc_url = config.rpc_url.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().route("/", axum::routing::post(handler)),
        )
        .await
        .unwrap();
    });
    (config, server)
}

#[tokio::test]
async fn real_rpc_path_scans_chain_first_and_finds_missing_local_funds() {
    let (config, server) = rpc_fixture(false, false).await;
    let observer = ChainObserver::new(config).unwrap();
    let report = observer.observe(&state(), 300_000).await;
    assert!(report.scan_complete);
    assert_eq!(report.scanned_leases, 1);
    assert_eq!(report.status(), ObserverStatus::Degraded);
    assert_eq!(report.findings[0].kind, FindingKind::OrphanedFundedLease);
    server.abort();
}

#[tokio::test]
async fn delegated_zeroed_l1_meter_reads_the_pinned_rollup() {
    let (config, server) = rpc_fixture(true, false).await;
    let observer = ChainObserver::new(config).unwrap();
    let report = observer.observe(&state(), 101_000).await;
    assert_eq!(report.status(), ObserverStatus::Healthy);
    assert_eq!(report.scanned_leases, 1);
    server.abort();
}

#[tokio::test]
async fn wrong_rollup_identity_and_unreachable_rpc_are_blind() {
    let (config, server) = rpc_fixture(true, true).await;
    let observer = ChainObserver::new(config).unwrap();
    let report = observer.observe(&state(), 101_000).await;
    assert_eq!(report.status(), ObserverStatus::Blind);
    assert!(!report.scan_complete);
    server.abort();
    let observer = ChainObserver::new(super::tests::config()).unwrap();
    let report = observer.observe(&state(), 101_000).await;
    assert_eq!(report.status(), ObserverStatus::Blind);
    assert_eq!(report.findings[0].kind, FindingKind::RpcUnavailable);
}

#[tokio::test]
async fn public_status_does_not_call_empty_capacity_an_outage_or_disabled_observation_healthy() {
    let (headers, Json(value)) = public_status(State(state()), None).await;
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(value["chain_observer"]["status"], "disabled");
    assert!(value["latest_recorded_lease_payout_at_ms"].is_null());
    assert_eq!(value["journal_writable"], true);
}

#[test]
fn settled_chain_records_missing_from_journal_or_escrow_are_not_clean() {
    let (config, address, mut bytes, _) = fixture();
    bytes[216] = 1;
    bytes[217] = 1;
    let terms =
        decode_terms_account(&address, &account(&config.program_id, &bytes), &config).unwrap();
    assert_eq!(
        compare_lease(&terms, None, None, 300_000, config.grace),
        vec![FindingKind::OrphanedSettledLease]
    );
    let local = LocalLease {
        phase: JobPhase::Completed,
        concluded_at_ms: Some(110_000),
        expected_terms: Some((100, 600, terms.operator.clone())),
        settled_charge: None,
    };
    assert_eq!(
        compare_lease(
            &terms,
            Some(MeterEvidence::Committed(1000)),
            Some(&local),
            300_000,
            config.grace
        ),
        vec![FindingKind::LocalSettlementMissing]
    );
}

#[tokio::test]
async fn wrong_genesis_and_missing_executable_program_cannot_pass_empty_scan() {
    let handler = |Json(request): Json<Value>| async move {
        let result = match request["method"].as_str().unwrap() {
            "getGenesisHash" => json!("5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"),
            "getAccountInfo" => json!({"context":{"slot":100},"value":null}),
            "getProgramAccounts" => panic!("must reject wrong pins before empty scan"),
            _ => panic!("unexpected RPC"),
        };
        Json(json!({"jsonrpc":"2.0","id":1,"result":result}))
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = config();
    config.rpc_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().route("/", axum::routing::post(handler)),
        )
        .await
        .unwrap();
    });
    let observer = ChainObserver::new(config.clone()).unwrap();
    let result = observer.observe(&state(), 1000).await;
    assert_eq!(result.status(), ObserverStatus::Blind);
    config.genesis_hash = "11111111111111111111111111111111".into();
    let observer = ChainObserver::new(config).unwrap();
    assert_eq!(
        observer.observe(&state(), 1000).await.status(),
        ObserverStatus::Blind
    );
    server.abort();
}

#[tokio::test]
async fn public_report_hides_accounts_jobs_rpc_endpoints_and_secrets() {
    let (mut config, server) = rpc_fixture(false, false).await;
    config.rpc_url.push_str("/?api-key=secret-sentinel");
    let observer = Arc::new(ChainObserver::new(config).unwrap());
    let state = state();
    observer.observe(&state, 300_000).await;
    let (_, Json(value)) = public_status(State(state), Some(Extension(observer))).await;
    let bytes = serde_json::to_string(&value).unwrap();
    assert!(!bytes.contains("secret-sentinel"));
    assert!(!bytes.contains("127.0.0.1"));
    assert!(!bytes.contains(&Uuid::from_bytes([7; 16]).to_string()));
    assert!(value["chain_observer"].get("findings").is_none());
    assert_eq!(value["mode"], "alert_only");
    server.abort();
}

#[tokio::test]
async fn indefinitely_cached_rpc_scan_is_blind_even_when_all_accounts_decode() {
    let (config, server) = rpc_fixture(false, false).await;
    let observer = ChainObserver::new(config).unwrap();
    let report = observer.observe(&state(), 500_001).await;
    assert_eq!(report.status(), ObserverStatus::Blind);
    assert!(!report.scan_complete);
    assert_eq!(report.findings[0].kind, FindingKind::InvalidChainResponse);
    server.abort();
}

#[tokio::test]
async fn future_rpc_block_time_is_not_treated_as_current_observation() {
    let (config, server) = rpc_fixture(false, false).await;
    let observer = ChainObserver::new(config).unwrap();
    let report = observer.observe(&state(), 1_000).await;
    assert_eq!(report.status(), ObserverStatus::Blind);
    assert_eq!(report.findings[0].kind, FindingKind::InvalidChainResponse);
    server.abort();
}
