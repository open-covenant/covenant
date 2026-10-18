use super::*;
use covenant_identity::LocalIdentity;

#[derive(Default)]
struct FakeObjects {
    objects: Mutex<BTreeMap<String, ObjectMetadata>>,
    deleted: Mutex<Vec<String>>,
    pause_head: std::sync::atomic::AtomicBool,
    head_entered: tokio::sync::Notify,
    head_continue: tokio::sync::Notify,
    fail_fence: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl WorkspaceObjects for FakeObjects {
    fn put_grant(
        &self,
        key: &str,
        _upload: &WorkspaceUpload,
        now: u64,
    ) -> Result<ObjectGrant, WorkspaceError> {
        Ok(ObjectGrant {
            method: "PUT".into(),
            url: key.into(),
            headers: BTreeMap::new(),
            expires_at_ms: now + GRANT_TTL_MS,
        })
    }
    fn get_grant(&self, key: &str, now: u64, ttl_ms: u64) -> Result<ObjectGrant, WorkspaceError> {
        Ok(ObjectGrant {
            method: "GET".into(),
            url: key.into(),
            headers: BTreeMap::new(),
            expires_at_ms: now + ttl_ms,
        })
    }
    async fn head(&self, key: &str, _now: u64) -> Result<ObjectMetadata, WorkspaceError> {
        if self
            .pause_head
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.head_entered.notify_one();
            self.head_continue.notified().await;
        }
        self.objects
            .lock()
            .get(key)
            .cloned()
            .ok_or(WorkspaceError::NotFound)
    }
    async fn delete(&self, key: &str, _now: u64) -> Result<(), WorkspaceError> {
        self.objects.lock().remove(key);
        self.deleted.lock().push(key.into());
        Ok(())
    }
    async fn fence(&self, key: &str, _now: u64) -> Result<(), WorkspaceError> {
        self.objects.lock().insert(
            key.into(),
            ObjectMetadata {
                ciphertext_bytes: 0,
                ciphertext_sha256: sha256_hex(b""),
            },
        );
        if self.fail_fence.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(WorkspaceError::Storage("uncertain fence".into()));
        }
        self.deleted.lock().push(key.into());
        Ok(())
    }
}

#[tokio::test]
async fn delayed_commit_cannot_adopt_replaced_same_id_reservation_or_orphan_it() {
    let directory = tempfile::tempdir().unwrap();
    let objects = Arc::new(FakeObjects::default());
    let service = Arc::new(
        WorkspaceService::open(
            &directory.path().join("book"),
            objects.clone(),
            coordinator(),
            4,
        )
        .unwrap(),
    );
    let workspace = service
        .create("owner", create_request(WorkspaceTrustFloor::Open))
        .unwrap();
    let upload = upload_request(0);
    let old = service
        .upload("owner", workspace.id, upload.clone(), 1000)
        .unwrap();
    objects.objects.lock().insert(
        old.grant.url.clone(),
        ObjectMetadata {
            ciphertext_bytes: 60,
            ciphertext_sha256: upload.ciphertext_sha256.clone(),
        },
    );
    objects
        .pause_head
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let blocked = {
        let service = service.clone();
        let commit = CommitWorkspace {
            upload_id: upload.upload_id,
            expected_version: 0,
        };
        tokio::spawn(async move { service.commit("owner", workspace.id, commit, 2000).await })
    };
    objects.head_entered.notified().await;
    let later = 1000 + UPLOAD_WINDOW_MS + 1;
    let new = service
        .upload("owner", workspace.id, upload.clone(), later)
        .unwrap();
    assert_ne!(new.grant.url, old.grant.url);
    objects.objects.lock().insert(
        new.grant.url.clone(),
        ObjectMetadata {
            ciphertext_bytes: 60,
            ciphertext_sha256: upload.ciphertext_sha256,
        },
    );
    objects.head_continue.notify_one();
    assert!(matches!(
        blocked.await.unwrap(),
        Err(WorkspaceError::Conflict(_))
    ));
    assert_eq!(service.get("owner", workspace.id).unwrap().version, 0);
    let committed = service
        .commit(
            "owner",
            workspace.id,
            CommitWorkspace {
                upload_id: upload.upload_id,
                expected_version: 0,
            },
            later + 1,
        )
        .await
        .unwrap();
    assert_eq!(committed.version, 1);
    service.cleanup(later + CLEANUP_SKEW_MS).await.unwrap();
    assert_eq!(objects.objects.lock()[&old.grant.url].ciphertext_bytes, 0);
    assert!(objects.objects.lock().contains_key(&new.grant.url));
    let download = service
        .download(
            "owner",
            workspace.id,
            DownloadWorkspace {
                version: 1,
                lease_job_id: None,
            },
            later + 2,
        )
        .unwrap();
    assert_eq!(download.grant.url, new.grant.url);
}

fn coordinator() -> CoordinatorState {
    CoordinatorState::new(
        LocalIdentity::generate("workspace@coordinator"),
        crate::CoordinatorConfig::default(),
        Arc::new(crate::reputation::NoReputation),
        Arc::new(crate::MockPayout::new()),
        Arc::new(covenant_audit::InMemoryAuditLog::new()),
    )
}

fn create_request(floor: WorkspaceTrustFloor) -> CreateWorkspace {
    CreateWorkspace {
        id: Uuid::new_v4(),
        name: "checkpoint".into(),
        trust_floor: floor,
    }
}
fn upload_request(version: u64) -> WorkspaceUpload {
    WorkspaceUpload {
        upload_id: Uuid::new_v4(),
        expected_version: version,
        plaintext_bytes: 0,
        ciphertext_bytes: 60,
        ciphertext_sha256: "ab".repeat(32),
    }
}

async fn commit_fake(
    service: &WorkspaceService,
    objects: &FakeObjects,
    owner: &str,
    id: Uuid,
    version: u64,
    now: u64,
) -> WorkspaceView {
    let request = upload_request(version);
    let grant = service.upload(owner, id, request.clone(), now).unwrap();
    objects.objects.lock().insert(
        grant.grant.url,
        ObjectMetadata {
            ciphertext_bytes: 60,
            ciphertext_sha256: request.ciphertext_sha256,
        },
    );
    service
        .commit(
            owner,
            id,
            CommitWorkspace {
                upload_id: request.upload_id,
                expected_version: version,
            },
            now + 1,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn versions_survive_restart_stale_writes_and_rollback_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("workspaces.jsonl");
    let objects = Arc::new(FakeObjects::default());
    let service = WorkspaceService::open(&path, objects.clone(), coordinator(), 4).unwrap();
    let workspace = service
        .create("owner", create_request(WorkspaceTrustFloor::Open))
        .unwrap();
    let first = commit_fake(&service, &objects, "owner", workspace.id, 0, 1000).await;
    let second = commit_fake(&service, &objects, "owner", workspace.id, 1, 2000).await;
    assert_eq!(second.version, 2);
    assert!(matches!(
        service.upload("owner", workspace.id, upload_request(0), 2001),
        Err(WorkspaceError::Conflict(_))
    ));
    assert!(matches!(
        service.download(
            "owner",
            workspace.id,
            DownloadWorkspace {
                version: first.version,
                lease_job_id: None
            },
            2001
        ),
        Err(WorkspaceError::Conflict(_))
    ));
    drop(service);
    // A torn append never replaces the last synced version.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"torn\":")
        .unwrap();
    let service = WorkspaceService::open(&path, objects.clone(), coordinator(), 4).unwrap();
    assert_eq!(service.get("owner", workspace.id).unwrap(), second);
    assert!(matches!(
        service.get("other", workspace.id),
        Err(WorkspaceError::NotFound)
    ));
    assert_eq!(
        service
            .cleanup(2000 + GRANT_TTL_MS + CLEANUP_SKEW_MS)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        service
            .cleanup(2001 + GRANT_TTL_MS + CLEANUP_SKEW_MS)
            .await
            .unwrap(),
        1
    );
    service.delete("owner", workspace.id, 500_000).unwrap();
    assert!(service
        .create(
            "owner",
            CreateWorkspace {
                id: workspace.id,
                name: workspace.name,
                trust_floor: WorkspaceTrustFloor::Open
            }
        )
        .is_err());
    assert!(service.get("owner", workspace.id).is_err());
}

#[tokio::test]
async fn byte_limit_includes_pending_replaced_deleted_and_cleanup_until_success() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("book");
    let objects = Arc::new(FakeObjects::default());
    let service = WorkspaceService::open(&path, objects.clone(), coordinator(), 4)
        .unwrap()
        .with_byte_limit(120)
        .unwrap();
    let workspace = service
        .create("owner", create_request(WorkspaceTrustFloor::Open))
        .unwrap();
    commit_fake(&service, &objects, "owner", workspace.id, 0, 1000).await;
    let pending = upload_request(1);
    let grant = service
        .upload("owner", workspace.id, pending.clone(), 2000)
        .unwrap();
    assert_eq!(service.book.lock().reserved_bytes(), 120);
    let second = service
        .create("owner", create_request(WorkspaceTrustFloor::Open))
        .unwrap();
    assert!(matches!(
        service.upload("owner", second.id, upload_request(0), 2001),
        Err(WorkspaceError::Capacity)
    ));
    objects.objects.lock().insert(
        grant.grant.url,
        ObjectMetadata {
            ciphertext_bytes: 60,
            ciphertext_sha256: pending.ciphertext_sha256,
        },
    );
    service
        .commit(
            "owner",
            workspace.id,
            CommitWorkspace {
                upload_id: pending.upload_id,
                expected_version: 1,
            },
            2002,
        )
        .await
        .unwrap();
    assert_eq!(
        service.book.lock().reserved_bytes(),
        120,
        "superseded ciphertext remains reserved"
    );
    service.delete("owner", workspace.id, 3000).unwrap();
    assert_eq!(
        service.book.lock().reserved_bytes(),
        120,
        "deleted ciphertext remains reserved"
    );
    drop(service);
    let service = WorkspaceService::open(&path, objects, coordinator(), 4)
        .unwrap()
        .with_byte_limit(120)
        .unwrap();
    assert_eq!(
        service.book.lock().reserved_bytes(),
        120,
        "restart retains all obligations"
    );
    service
        .cleanup(3000 + GRANT_TTL_MS + CLEANUP_SKEW_MS)
        .await
        .unwrap();
    assert_eq!(service.book.lock().reserved_bytes(), 0);
    service
        .upload("owner", second.id, upload_request(0), 500_000)
        .unwrap();
}

#[test]
fn issuance_limits_and_single_writer_survive_restart_and_backward_clock() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("book");
    let objects = Arc::new(FakeObjects::default());
    let service = WorkspaceService::open(&path, objects.clone(), coordinator(), 4).unwrap();
    assert!(WorkspaceService::open(&path, objects.clone(), coordinator(), 4).is_err());
    let workspace = service
        .create("owner", create_request(WorkspaceTrustFloor::Open))
        .unwrap();
    let day = 2 * 86_400_000;
    for _ in 0..OBJECT_ACTIONS_PER_OWNER_DAY {
        service
            .book
            .lock()
            .reserve_object_action("owner", workspace.id, day)
            .unwrap();
    }
    assert!(matches!(
        service
            .book
            .lock()
            .reserve_object_action("owner", workspace.id, day),
        Err(WorkspaceError::Capacity)
    ));
    drop(service);
    let service = WorkspaceService::open(&path, objects, coordinator(), 4).unwrap();
    assert!(matches!(
        service
            .book
            .lock()
            .reserve_object_action("owner", workspace.id, 0),
        Err(WorkspaceError::Capacity)
    ));
    service
        .book
        .lock()
        .reserve_object_action("owner", workspace.id, day + 86_400_000)
        .unwrap();
}

#[tokio::test]
async fn compaction_and_additive_journal_do_not_rewrite_existing_compute_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("workspaces.jsonl");
    let jobs = directory.path().join("journal.jsonl");
    let original = b"existing settled jobs, running leases and buyer balances\n";
    std::fs::write(&jobs, original).unwrap();
    let objects = Arc::new(FakeObjects::default());
    let service = WorkspaceService::open(&path, objects.clone(), coordinator(), 4).unwrap();
    let workspace = service
        .create("owner", create_request(WorkspaceTrustFloor::Open))
        .unwrap();
    let committed = commit_fake(&service, &objects, "owner", workspace.id, 0, 1000).await;
    let usage = service
        .book
        .lock()
        .row("owner", workspace.id)
        .unwrap()
        .object_actions;
    service.book.lock().compact().unwrap();
    drop(service);
    // A pre-schema optional-field record can be replayed additively; unsupported
    // future schemas cannot silently be interpreted by this executable.
    let mut row: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    row.as_object_mut().unwrap().remove("schema_version");
    std::fs::write(&path, format!("{}\n", row)).unwrap();
    let service = WorkspaceService::open(&path, objects.clone(), coordinator(), 4).unwrap();
    assert_eq!(service.get("owner", workspace.id).unwrap(), committed);
    assert_eq!(
        service
            .book
            .lock()
            .row("owner", workspace.id)
            .unwrap()
            .object_actions,
        usage
    );
    assert_eq!(std::fs::read(&jobs).unwrap(), original);
    drop(service);
    row["schema_version"] = serde_json::json!(2);
    std::fs::write(&path, format!("{}\n", row)).unwrap();
    assert!(WorkspaceService::open(&path, objects, coordinator(), 4).is_err());
    assert_eq!(std::fs::read(&jobs).unwrap(), original);
}

#[tokio::test]
async fn disabled_admissions_preserve_existing_commit_restore_and_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("book");
    let objects = Arc::new(FakeObjects::default());
    let service = WorkspaceService::open(&path, objects.clone(), coordinator(), 4).unwrap();
    let workspace = service
        .create("owner", create_request(WorkspaceTrustFloor::Open))
        .unwrap();
    let request = upload_request(0);
    let grant = service
        .upload("owner", workspace.id, request.clone(), 1000)
        .unwrap();
    objects.objects.lock().insert(
        grant.grant.url,
        ObjectMetadata {
            ciphertext_bytes: 60,
            ciphertext_sha256: request.ciphertext_sha256,
        },
    );
    drop(service);
    let service = WorkspaceService::open(&path, objects, coordinator(), 4)
        .unwrap()
        .with_writes_enabled(false);
    assert!(service
        .create("owner", create_request(WorkspaceTrustFloor::Open))
        .is_err());
    service
        .commit(
            "owner",
            workspace.id,
            CommitWorkspace {
                upload_id: request.upload_id,
                expected_version: 0,
            },
            2000,
        )
        .await
        .unwrap();
    service
        .download(
            "owner",
            workspace.id,
            DownloadWorkspace {
                version: 1,
                lease_job_id: None,
            },
            2000,
        )
        .unwrap();
    assert!(service
        .upload("owner", workspace.id, upload_request(1), 2000)
        .is_err());
    service.delete("owner", workspace.id, 2000).unwrap();
    service
        .cleanup(2000 + GRANT_TTL_MS + CLEANUP_SKEW_MS)
        .await
        .unwrap();
    assert_eq!(service.book.lock().reserved_bytes(), 0);
}

#[tokio::test]
async fn commit_requires_verified_size_digest_and_reserved_version() {
    let directory = tempfile::tempdir().unwrap();
    let objects = Arc::new(FakeObjects::default());
    let service = WorkspaceService::open(
        &directory.path().join("book"),
        objects.clone(),
        coordinator(),
        4,
    )
    .unwrap();
    let workspace = service
        .create("owner", create_request(WorkspaceTrustFloor::Open))
        .unwrap();
    let request = upload_request(0);
    let grant = service
        .upload("owner", workspace.id, request.clone(), 1000)
        .unwrap();
    assert_eq!(
        service
            .upload("owner", workspace.id, request.clone(), 1001)
            .unwrap()
            .grant
            .url,
        grant.grant.url
    );
    assert!(service
        .upload("owner", workspace.id, upload_request(0), 1001)
        .is_err());
    let commit = CommitWorkspace {
        upload_id: request.upload_id,
        expected_version: 0,
    };
    assert!(matches!(
        service
            .commit("owner", workspace.id, commit.clone(), 1001)
            .await,
        Err(WorkspaceError::NotFound)
    ));
    objects.objects.lock().insert(
        grant.grant.url.clone(),
        ObjectMetadata {
            ciphertext_bytes: 60,
            ciphertext_sha256: "cd".repeat(32),
        },
    );
    assert!(service
        .commit("owner", workspace.id, commit.clone(), 1001)
        .await
        .is_err());
    objects.objects.lock().insert(
        grant.grant.url.clone(),
        ObjectMetadata {
            ciphertext_bytes: 59,
            ciphertext_sha256: request.ciphertext_sha256.clone(),
        },
    );
    assert!(service
        .commit("owner", workspace.id, commit.clone(), 1001)
        .await
        .is_err());
    objects.objects.lock().insert(
        grant.grant.url,
        ObjectMetadata {
            ciphertext_bytes: 60,
            ciphertext_sha256: request.ciphertext_sha256,
        },
    );
    let (a, b) = tokio::join!(
        service.commit("owner", workspace.id, commit.clone(), 1002),
        service.commit("owner", workspace.id, commit, 1002)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(service.get("owner", workspace.id).unwrap().version, 1);
}

#[tokio::test]
async fn abandoned_upload_cleanup_is_durable_and_waits_out_all_grants() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("book");
    let objects = Arc::new(FakeObjects::default());
    let service = WorkspaceService::open(&path, objects.clone(), coordinator(), 4).unwrap();
    let workspace = service
        .create("owner", create_request(WorkspaceTrustFloor::Open))
        .unwrap();
    let request = upload_request(0);
    let grant = service
        .upload("owner", workspace.id, request.clone(), 1000)
        .unwrap();
    objects.objects.lock().insert(
        grant.grant.url.clone(),
        ObjectMetadata {
            ciphertext_bytes: 60,
            ciphertext_sha256: request.ciphertext_sha256,
        },
    );
    assert_eq!(service.cleanup(1000 + UPLOAD_WINDOW_MS).await.unwrap(), 0);
    drop(service);
    let service = WorkspaceService::open(&path, objects.clone(), coordinator(), 4).unwrap();
    assert_eq!(
        service
            .cleanup(1000 + UPLOAD_WINDOW_MS + CLEANUP_SKEW_MS)
            .await
            .unwrap(),
        1
    );
    assert_eq!(objects.objects.lock()[&grant.grant.url].ciphertext_bytes, 0);
    assert_eq!(service.cleanup(2_000_000).await.unwrap(), 0);
}

#[tokio::test]
async fn unknown_fence_keeps_bytes_and_lifetime_allocation_survives_replay() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("book");
    let objects = Arc::new(FakeObjects::default());
    let service = WorkspaceService::open(&path, objects.clone(), coordinator(), 4).unwrap();
    let workspace = service
        .create("owner", create_request(WorkspaceTrustFloor::Open))
        .unwrap();
    service
        .upload("owner", workspace.id, upload_request(0), 1000)
        .unwrap();
    let cleanup_at = 1000 + UPLOAD_WINDOW_MS + CLEANUP_SKEW_MS;
    objects
        .fail_fence
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(service.cleanup(cleanup_at).await.is_err());
    assert_eq!(service.book.lock().reserved_bytes(), 60);
    assert_eq!(service.book.lock().allocated_objects(), 1);
    drop(service);
    let service = WorkspaceService::open(&path, objects.clone(), coordinator(), 4).unwrap();
    assert_eq!(service.book.lock().reserved_bytes(), 60);
    objects
        .fail_fence
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(service.cleanup(cleanup_at + 1).await.unwrap(), 1);
    assert_eq!(service.cleanup(cleanup_at + 2).await.unwrap(), 0);
    assert_eq!(service.book.lock().reserved_bytes(), 0);
    assert_eq!(service.book.lock().allocated_objects(), 1);
    {
        let mut book = service.book.lock();
        let mut row = book.row("owner", workspace.id).unwrap().clone();
        row.fenced_objects = MAX_ALLOCATED_OBJECTS;
        book.persist(row).unwrap();
    }
    drop(service);
    let service = WorkspaceService::open(&path, objects, coordinator(), 4).unwrap();
    assert_eq!(
        service.book.lock().allocated_objects(),
        MAX_ALLOCATED_OBJECTS
    );
    assert!(matches!(
        service.upload("owner", workspace.id, upload_request(0), cleanup_at + 3),
        Err(WorkspaceError::Capacity)
    ));
}

async fn accepted_lease(state: &CoordinatorState, buyer: &LocalIdentity, now: u64) -> Uuid {
    use covenant_compute_protocol::{
        CapabilityRequirement, EscrowHoldAttestation, FundingSource, JobEnvelopePayload, JobKind,
        LeaseTerms, SignedJobEnvelope,
    };
    let id = Uuid::new_v4();
    let identity = LocalIdentity::generate("coordinator@fixture");
    let envelope = SignedJobEnvelope::sign(
        JobEnvelopePayload {
            job_id: id,
            buyer: buyer.agent_id(),
            kind: JobKind::LeaseSession,
            capability_requirement: CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: None,
                kind: JobKind::LeaseSession,
                max_duration_secs: 600,
                min_reputation_bps: None,
            },
            input: vec![covenant_compute_protocol::lease_input(LeaseTerms {
                max_duration_secs: 600,
                rate_micro_usdc_per_sec: 1,
                client_public_key: None,
            })
            .unwrap()],
            price_micro_usdc: 600,
            deadline_ms: 900_000,
            idempotency: covenant_a2a::A2AIdempotency::new(
                covenant_a2a::A2ADuplicateSafety::Idempotent,
                "workspace-lease",
            ),
            issued_at_ms: now,
            referral_code: None,
            stream: false,
        },
        buyer,
    )
    .unwrap();
    let hold =
        EscrowHoldAttestation::sign(id, 600, FundingSource::Organic, now, &identity).unwrap();
    let record=serde_json::from_value(serde_json::json!({"operator_pubkey_b58":identity.agent_id().pubkey_base58(),"envelope":envelope,"escrow_hold":hold,
        "phase":"accepted","receipt":null,"output":null,"fee_micro_usdc":0,"accepted_at_ms":now})).unwrap();
    state.jobs().insert(id, record).unwrap();
    id
}

#[tokio::test]
async fn lease_restore_checks_owner_live_window_and_actual_open_trust() {
    let directory = tempfile::tempdir().unwrap();
    let objects = Arc::new(FakeObjects::default());
    let state = coordinator();
    let service = WorkspaceService::open(
        &directory.path().join("book"),
        objects.clone(),
        state.clone(),
        4,
    )
    .unwrap();
    let buyer = LocalIdentity::generate("buyer@fixture");
    let owner = buyer.agent_id().pubkey_base58();
    let open = service
        .create(&owner, create_request(WorkspaceTrustFloor::Open))
        .unwrap();
    let private = service
        .create(&owner, create_request(WorkspaceTrustFloor::Confidential))
        .unwrap();
    commit_fake(&service, &objects, &owner, open.id, 0, 1000).await;
    commit_fake(&service, &objects, &owner, private.id, 0, 1000).await;
    let lease = accepted_lease(&state, &buyer, 1000).await;
    let request = DownloadWorkspace {
        version: 1,
        lease_job_id: Some(lease),
    };
    assert!(service
        .download(&owner, open.id, request.clone(), 2000)
        .is_ok());
    let bounded = service
        .download(&owner, open.id, request.clone(), 590_000)
        .unwrap();
    assert_eq!(
        bounded.grant.expires_at_ms, 601_000,
        "grant cannot outlive the paid lease"
    );
    assert!(matches!(
        service.download(&owner, private.id, request.clone(), 2000),
        Err(WorkspaceError::Forbidden(_))
    ));
    assert!(matches!(
        service.download(&owner, open.id, request, 601_000),
        Err(WorkspaceError::Forbidden(_))
    ));
    // Owner backup recovery is always possible locally, independent of what
    // hardware the network can currently prove.
    assert!(service
        .download(
            &owner,
            private.id,
            DownloadWorkspace {
                version: 1,
                lease_job_id: None
            },
            2000
        )
        .is_ok());
    let other = LocalIdentity::generate("other@fixture");
    let foreign = accepted_lease(&state, &other, 1000).await;
    assert!(service
        .download(
            &owner,
            open.id,
            DownloadWorkspace {
                version: 1,
                lease_job_id: Some(foreign)
            },
            2000
        )
        .is_err());
}

#[derive(Clone, Default)]
struct S3Fixture(Arc<Mutex<BTreeMap<String, Vec<u8>>>>);
async fn s3_put(
    State(objects): State<S3Fixture>,
    RoutePath(key): RoutePath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    use base64::Engine;
    let expected = base64::engine::general_purpose::STANDARD.encode(sha2_10::Sha256::digest(&body));
    let conditional = headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some("*");
    if (!conditional && !body.is_empty())
        || headers
            .get("x-amz-checksum-sha256")
            .and_then(|v| v.to_str().ok())
            != Some(expected.as_str())
    {
        return StatusCode::BAD_REQUEST;
    }
    let mut entries = objects.0.lock();
    if conditional && entries.contains_key(&key) {
        return StatusCode::PRECONDITION_FAILED;
    }
    entries.insert(key, body.to_vec());
    StatusCode::OK
}
async fn s3_get(State(objects): State<S3Fixture>, RoutePath(key): RoutePath<String>) -> Response {
    use base64::Engine;
    let Some(bytes) = objects.0.lock().get(&key).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let checksum =
        base64::engine::general_purpose::STANDARD.encode(sha2_10::Sha256::digest(&bytes));
    (
        [
            ("x-amz-checksum-sha256", checksum),
            ("content-length", bytes.len().to_string()),
        ],
        bytes,
    )
        .into_response()
}
async fn s3_delete(
    State(objects): State<S3Fixture>,
    RoutePath(key): RoutePath<String>,
) -> StatusCode {
    objects.0.lock().remove(&key);
    StatusCode::NO_CONTENT
}

use sha2_10::Digest;

#[tokio::test]
async fn real_router_presigner_and_buyer_roundtrip_with_write_once_s3_semantics() {
    let object_state = S3Fixture::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let storage_url = format!("http://{}", listener.local_addr().unwrap());
    let storage_router = Router::new()
        .route(
            "/bucket/*key",
            axum::routing::put(s3_put).get(s3_get).delete(s3_delete),
        )
        .layer(DefaultBodyLimit::max(CHUNK_BYTES * 3))
        .with_state(object_state.clone());
    let storage_task = tokio::spawn(async move {
        axum::serve(listener, storage_router).await.unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let objects = S3WorkspaceObjects::new(S3WorkspaceConfig {
        endpoint: storage_url,
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        prefix: "".into(),
        access_key: "test".into(),
        secret_key: "test-secret".into(),
        session_token: None,
    })
    .unwrap();
    let service = Arc::new(
        WorkspaceService::open(
            &directory.path().join("book"),
            Arc::new(objects),
            coordinator(),
            4,
        )
        .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(service);
    let service_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let buyer = LocalIdentity::generate("buyer@workspace");
    let http = reqwest::Client::new();
    let config = covenant_compute_buyer::BuyerConfig {
        coordinator_url: base.clone(),
        poll_interval: std::time::Duration::from_millis(1),
        referral_code: None,
        rpc_url: None,
    };
    let client = covenant_compute_buyer::workspace::WorkspaceClient::new(&http, &config, &buyer);
    let key = covenant_compute_protocol::VaultKey::random();
    let workspace = client
        .create(Uuid::new_v4(), "weights", WorkspaceTrustFloor::Open)
        .await
        .unwrap();
    let source = directory.path().join("source");
    let plaintext = vec![42u8; CHUNK_BYTES + 31];
    std::fs::write(&source, &plaintext).unwrap();
    let prepared = client
        .prepare_snapshot(&workspace, &key, &source)
        .await
        .unwrap();
    let prepared = prepared.persist(&directory.path().join("pending")).unwrap();
    let upload_id = prepared.upload.upload_id;
    drop(prepared);
    let prepared = covenant_compute_buyer::workspace::PreparedWorkspaceSnapshot::load(
        &directory.path().join("pending"),
    )
    .unwrap();
    assert_eq!(
        prepared.upload.upload_id, upload_id,
        "process restart preserves reservation and ciphertext"
    );
    let committed = client.upload_snapshot(&prepared).await.unwrap();
    assert_eq!(committed.version, 1);
    assert_eq!(
        client.upload_snapshot(&prepared).await.unwrap(),
        committed,
        "uncertain upload/commit retries are idempotent"
    );
    // No plaintext is stored at the object endpoint.
    assert!(object_state
        .0
        .lock()
        .values()
        .all(|b| !b.windows(128).any(|w| w == [42u8; 128])));
    let output = directory.path().join("restored");
    client
        .restore_file(&committed, &key, &output, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), plaintext);
    assert!(
        client
            .restore_file(&committed, &key, &output, None)
            .await
            .is_err(),
        "must never overwrite an existing destination"
    );
    let wrong = directory.path().join("wrong-key");
    assert!(client
        .restore_file(
            &committed,
            &covenant_compute_protocol::VaultKey::random(),
            &wrong,
            None
        )
        .await
        .is_err());
    assert!(!wrong.exists());
    let unsigned = http
        .get(format!(
            "{base}/workspaces/{}",
            buyer.agent_id().pubkey_base58()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), StatusCode::UNAUTHORIZED);
    let other = LocalIdentity::generate("other@workspace");
    let path = format!("/workspaces/{}", buyer.agent_id().pubkey_base58());
    let stamp = now_ms();
    let forged = sign_workspace(&other, "GET", &path, &[], stamp).unwrap();
    let response = http
        .get(format!("{base}{path}"))
        .header(WORKSPACE_SIGNED_AT_HEADER, stamp)
        .header(WORKSPACE_SIGNATURE_HEADER, forged)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    service_task.abort();
    storage_task.abort();
}
