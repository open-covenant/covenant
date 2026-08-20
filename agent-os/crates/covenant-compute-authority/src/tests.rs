use super::*;

const NOW: u64 = 1_000;
const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn principal(value: &str) -> PrincipalId {
    PrincipalId::new(value).expect("valid principal")
}

fn authority() -> (
    Arc<ReferenceMemoryStorage>,
    ComputeAuthority<ReferenceMemoryStorage>,
) {
    let storage = Arc::new(ReferenceMemoryStorage::new());
    let authority = ComputeAuthority::new_reference(Arc::clone(&storage));
    (storage, authority)
}

async fn account(
    authority: &ComputeAuthority<ReferenceMemoryStorage>,
    owner: &PrincipalId,
    cap_microusdc: u64,
) -> SpendAccount {
    authority
        .create_spend_account(
            CreateSpendAccount {
                owner: owner.clone(),
                cap_microusdc,
            },
            NOW,
        )
        .await
        .expect("create spend account")
}

async fn create_quote(
    authority: &ComputeAuthority<ReferenceMemoryStorage>,
    owner: &PrincipalId,
    offer_id: &str,
    total_microusdc: u64,
) -> Quote {
    authority
        .create_quote(
            CreateQuote {
                owner: owner.clone(),
                offer_id: offer_id.to_owned(),
                app_id: "gpu-workspace".to_owned(),
                workload_digest: DIGEST.to_owned(),
                rate_microusdc_per_hour: total_microusdc,
                duration_seconds: 3_600,
                expires_at_ms: NOW + 10_000,
            },
            NOW,
        )
        .await
        .expect("create quote")
}

async fn authorization(
    authority: &ComputeAuthority<ReferenceMemoryStorage>,
    owner: &PrincipalId,
    quote: &Quote,
    account: &SpendAccount,
) -> LaunchAuthorization {
    authority
        .authorize_launch(
            AuthorizeLaunch {
                owner: owner.clone(),
                quote_id: quote.id,
                spend_account_id: account.id,
                expires_at_ms: NOW + 5_000,
            },
            NOW,
        )
        .await
        .expect("authorize launch")
}

fn prepare(
    owner: &PrincipalId,
    quote: &Quote,
    account: &SpendAccount,
    authorization: &LaunchAuthorization,
    key: &str,
) -> PrepareLaunch {
    PrepareLaunch {
        owner: owner.clone(),
        quote_id: quote.id,
        authorization_id: authorization.id,
        spend_account_id: account.id,
        idempotency_key: IdempotencyKey::new(key).expect("valid idempotency key"),
    }
}

#[tokio::test]
async fn concurrent_launches_cannot_race_past_the_cap() {
    let (_storage, authority) = authority();
    let owner = principal("owner-a");
    let account = account(&authority, &owner, 100).await;
    let first_quote = create_quote(&authority, &owner, "offer-a", 60).await;
    let second_quote = create_quote(&authority, &owner, "offer-b", 60).await;
    let first_authorization = authorization(&authority, &owner, &first_quote, &account).await;
    let second_authorization = authorization(&authority, &owner, &second_quote, &account).await;

    let first = authority.clone();
    let second = authority.clone();
    let first_request = prepare(
        &owner,
        &first_quote,
        &account,
        &first_authorization,
        "launch-a",
    );
    let second_request = prepare(
        &owner,
        &second_quote,
        &account,
        &second_authorization,
        "launch-b",
    );
    let (first_result, second_result) = tokio::join!(
        first.prepare_launch(first_request, NOW + 1),
        second.prepare_launch(second_request, NOW + 1)
    );

    assert_eq!(
        usize::from(first_result.is_ok()) + usize::from(second_result.is_ok()),
        1
    );
    let error = first_result
        .err()
        .or_else(|| second_result.err())
        .expect("one launch must fail");
    assert!(matches!(
        error,
        AuthorityError::SpendCapExceeded {
            cap_microusdc: 100,
            committed_microusdc: 0,
            reserved_microusdc: 60,
            requested_microusdc: 60,
        }
    ));

    let account = authority
        .spend_account(account.id)
        .await
        .expect("read account");
    assert_eq!(account.reserved_microusdc, 60);
    assert_eq!(account.committed_microusdc, 0);
    assert_eq!(account.available_microusdc(), 40);
}

#[tokio::test]
async fn launch_replay_returns_the_original_job_without_a_second_reservation() {
    let (_storage, authority) = authority();
    let owner = principal("owner-a");
    let account = account(&authority, &owner, 100).await;
    let quote = create_quote(&authority, &owner, "offer-a", 40).await;
    let authorization = authorization(&authority, &owner, &quote, &account).await;
    let request = prepare(&owner, &quote, &account, &authorization, "launch-a");

    let created = authority
        .prepare_launch(request.clone(), NOW + 1)
        .await
        .expect("first launch");
    let replay = authority
        .prepare_launch(request, NOW + 2)
        .await
        .expect("replayed launch");

    assert_eq!(created.disposition, LaunchDisposition::Created);
    assert_eq!(replay.disposition, LaunchDisposition::Replay);
    assert_eq!(created.job.id, replay.job.id);
    assert_eq!(
        authority
            .spend_account(account.id)
            .await
            .expect("read account")
            .reserved_microusdc,
        40
    );

    let other_quote = create_quote(&authority, &owner, "offer-b", 10).await;
    let conflict = authority
        .prepare_launch(
            PrepareLaunch {
                quote_id: other_quote.id,
                idempotency_key: IdempotencyKey::new("launch-a").expect("valid key"),
                ..prepare(&owner, &quote, &account, &authorization, "unused-key")
            },
            NOW + 3,
        )
        .await
        .expect_err("a key cannot identify a different launch");
    assert_eq!(conflict, AuthorityError::IdempotencyConflict);

    let consumed = authority
        .prepare_launch(
            prepare(&owner, &quote, &account, &authorization, "launch-b"),
            NOW + 3,
        )
        .await
        .expect_err("authorization is one-shot");
    assert_eq!(consumed, AuthorityError::AuthorizationConsumed);
}

#[tokio::test]
async fn a_foreign_owner_cannot_cancel_a_job() {
    let (_storage, authority) = authority();
    let owner = principal("owner-a");
    let foreign = principal("owner-b");
    let account = account(&authority, &owner, 100).await;
    let quote = create_quote(&authority, &owner, "offer-a", 40).await;
    let authorization = authorization(&authority, &owner, &quote, &account).await;
    let job = authority
        .prepare_launch(
            prepare(&owner, &quote, &account, &authorization, "launch-a"),
            NOW + 1,
        )
        .await
        .expect("prepare launch")
        .job;
    authority
        .mark_running(job.id, "provider-job-a", NOW + 2)
        .await
        .expect("mark running");

    let error = authority
        .request_cancel(&foreign, job.id, NOW + 3)
        .await
        .expect_err("foreign cancellation must fail");
    assert_eq!(error, AuthorityError::ForeignOwner);
    assert_eq!(
        authority.job(job.id).await.expect("read job").state,
        JobState::Running
    );

    let cancelled = authority
        .request_cancel(&owner, job.id, NOW + 4)
        .await
        .expect("owner cancellation");
    assert_eq!(cancelled.state, JobState::CancelRequested);
    assert_eq!(
        authority
            .request_cancel(&owner, job.id, NOW + 5)
            .await
            .expect("cancel replay")
            .state,
        JobState::CancelRequested
    );
}

#[tokio::test]
async fn launch_failure_releases_the_full_reservation_but_not_the_authorization() {
    let (_storage, authority) = authority();
    let owner = principal("owner-a");
    let account = account(&authority, &owner, 100).await;
    let quote = create_quote(&authority, &owner, "offer-a", 70).await;
    let authorization = authorization(&authority, &owner, &quote, &account).await;
    let request = prepare(&owner, &quote, &account, &authorization, "launch-a");
    let job = authority
        .prepare_launch(request.clone(), NOW + 1)
        .await
        .expect("prepare launch")
        .job;

    let failed = authority
        .fail_launch(job.id, "provider_rejected", NOW + 2)
        .await
        .expect("record launch failure");
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.committed_microusdc, Some(0));
    let account = authority
        .spend_account(account.id)
        .await
        .expect("read account");
    assert_eq!(account.reserved_microusdc, 0);
    assert_eq!(account.committed_microusdc, 0);
    assert_eq!(account.available_microusdc(), 100);
    assert_eq!(
        authority
            .authorization(authorization.id)
            .await
            .expect("read authorization")
            .consumed_by,
        Some(job.id)
    );

    let replay = authority
        .prepare_launch(request, NOW + 3)
        .await
        .expect("failed launch replay");
    assert_eq!(replay.disposition, LaunchDisposition::Replay);
    assert_eq!(replay.job.state, JobState::Failed);
    assert_eq!(
        authority
            .spend_account(account.id)
            .await
            .expect("read account")
            .reserved_microusdc,
        0
    );
}

#[tokio::test]
async fn settlement_commits_actual_spend_and_releases_the_remainder() {
    let (_storage, authority) = authority();
    let owner = principal("owner-a");
    let account = account(&authority, &owner, 100).await;
    let quote = create_quote(&authority, &owner, "offer-a", 80).await;
    let authorization = authorization(&authority, &owner, &quote, &account).await;
    let job = authority
        .prepare_launch(
            prepare(&owner, &quote, &account, &authorization, "launch-a"),
            NOW + 1,
        )
        .await
        .expect("prepare launch")
        .job;
    authority
        .mark_running(job.id, "provider-job-a", NOW + 2)
        .await
        .expect("mark running");

    let settled = authority
        .settle_job(
            job.id,
            JobSettlement {
                outcome: TerminalOutcome::Succeeded,
                charge_microusdc: 55,
                failure_code: None,
            },
            NOW + 3,
        )
        .await
        .expect("settle job");
    assert_eq!(settled.state, JobState::Succeeded);
    assert_eq!(settled.committed_microusdc, Some(55));

    let account = authority
        .spend_account(account.id)
        .await
        .expect("read account");
    assert_eq!(account.reserved_microusdc, 0);
    assert_eq!(account.committed_microusdc, 55);
    assert_eq!(account.available_microusdc(), 45);
}

#[tokio::test]
async fn cancellation_racing_provider_launch_preserves_the_cancel_request() {
    let (_storage, authority) = authority();
    let owner = principal("owner-a");
    let account = account(&authority, &owner, 100).await;
    let quote = create_quote(&authority, &owner, "offer-a", 40).await;
    let authorization = authorization(&authority, &owner, &quote, &account).await;
    let job = authority
        .prepare_launch(
            prepare(&owner, &quote, &account, &authorization, "launch-a"),
            NOW + 1,
        )
        .await
        .expect("prepare launch")
        .job;

    authority
        .request_cancel(&owner, job.id, NOW + 2)
        .await
        .expect("request cancel before provider returns");
    let job = authority
        .mark_running(job.id, "provider-job-a", NOW + 3)
        .await
        .expect("record provider job after cancel");
    assert_eq!(job.state, JobState::CancelRequested);
    assert_eq!(job.provider_job_id.as_deref(), Some("provider-job-a"));

    authority
        .settle_job(
            job.id,
            JobSettlement {
                outcome: TerminalOutcome::Cancelled,
                charge_microusdc: 5,
                failure_code: None,
            },
            NOW + 4,
        )
        .await
        .expect("settle cancellation");
    let account = authority
        .spend_account(account.id)
        .await
        .expect("read account");
    assert_eq!(account.reserved_microusdc, 0);
    assert_eq!(account.committed_microusdc, 5);
}

#[tokio::test]
async fn quote_records_are_immutable_and_reference_storage_is_rejected_in_production() {
    let (storage, authority) = authority();
    let owner = principal("owner-a");
    let quote = create_quote(&authority, &owner, "offer-a", 20).await;

    let duplicate = storage
        .insert_quote(Quote {
            total_microusdc: 30,
            rate_microusdc_per_hour: 30,
            ..quote
        })
        .await
        .expect_err("quote replacement must fail");
    assert_eq!(duplicate, AuthorityError::AlreadyExists(Resource::Quote));

    let result = ComputeAuthority::new(storage);
    assert!(matches!(
        result,
        Err(AuthorityError::EphemeralStorageRejected)
    ));
}

#[tokio::test]
async fn per_hour_quote_rounds_up_without_under_reserving() {
    let (_storage, authority) = authority();
    let quote = authority
        .create_quote(
            CreateQuote {
                owner: principal("owner-a"),
                offer_id: "offer-a".into(),
                app_id: "gpu-workspace".into(),
                workload_digest: DIGEST.into(),
                rate_microusdc_per_hour: 1,
                duration_seconds: 1,
                expires_at_ms: NOW + 10_000,
            },
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(quote.total_microusdc, 1);
}
