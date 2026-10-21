use super::*;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use covenant_a2a::{A2ADuplicateSafety, A2AIdempotency};
use covenant_compute_protocol::phala::{
    phala_ephemeral_lease_input, phala_ephemeral_workload, phala_lease_input, phala_workload,
    reviewed_phala_ephemeral_wrapper, reviewed_phala_wrapper, PhalaRelayConfig, PHALA_CPU_MODEL,
    PHALA_EPHEMERAL_CPU_MODEL,
};
use covenant_compute_protocol::ssh::SshHostKey;
use covenant_compute_protocol::{lease_input, CapabilityRequirement, JobKind, LeaseTerms};
use covenant_types::AgentId;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Closed(AtomicBool);
#[async_trait]
impl LeaseCloseSource for Closed {
    async fn is_closed(&self, _: Uuid) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
struct Evidence;
#[async_trait]
impl PhalaEvidence for Evidence {
    async fn after_access(
        &self,
        _: &JobEnvelopePayload,
        _: &LeaseAccess,
        _: &str,
    ) -> Result<(), ExecutorError> {
        Ok(())
    }
}

struct VerifiedEvidence {
    prepared: AtomicUsize,
    verified: AtomicUsize,
    submitted: AtomicUsize,
    fail_verification: AtomicBool,
    fail_prepare: AtomicBool,
    change_key: AtomicBool,
    change_instance: AtomicBool,
}
impl VerifiedEvidence {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            prepared: AtomicUsize::new(0),
            verified: AtomicUsize::new(0),
            submitted: AtomicUsize::new(0),
            fail_verification: AtomicBool::new(false),
            fail_prepare: AtomicBool::new(false),
            change_key: AtomicBool::new(false),
            change_instance: AtomicBool::new(false),
        })
    }
}
#[async_trait]
impl PhalaEvidence for VerifiedEvidence {
    fn ephemeral_available(&self) -> bool {
        true
    }
    fn ephemeral_configured(&self, job: &JobEnvelopePayload) -> Result<(), ExecutorError> {
        if parse_phala_ephemeral_lease_spec(&job.input)
            .map_err(|e| failed(e.to_string()))?
            .is_some()
        {
            Ok(())
        } else {
            Err(failed("explicit ephemeral profile required"))
        }
    }
    async fn prepare_ephemeral(
        &self,
        _: &JobEnvelopePayload,
        app: &str,
    ) -> Result<(), ExecutorError> {
        assert_eq!(app, "ab".repeat(20));
        self.prepared.fetch_add(1, Ordering::SeqCst);
        if self.fail_prepare.load(Ordering::SeqCst) {
            Err(failed("relay tombstoned"))
        } else {
            Ok(())
        }
    }
    async fn ephemeral_ready(
        &self,
        job: &JobEnvelopePayload,
        app: &str,
        _: Option<&VerifiedPhalaAccess>,
    ) -> Result<VerifiedPhalaAccess, ExecutorError> {
        self.verified.fetch_add(1, Ordering::SeqCst);
        if self.fail_verification.load(Ordering::SeqCst) {
            return Err(failed("full quote rejected"));
        }
        let spec = parse_phala_ephemeral_lease_spec(&job.input)
            .unwrap()
            .unwrap();
        let mut wire = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
        wire.extend_from_slice(
            &[if self.change_key.load(Ordering::SeqCst) {
                9
            } else {
                8
            }; 32],
        );
        Ok(VerifiedPhalaAccess {
            app_id: app.into(),
            instance_id: if self.change_instance.load(Ordering::SeqCst) {
                "ef".repeat(20)
            } else {
                "cd".repeat(20)
            },
            access: SshAccess {
                host: spec.relay.host().unwrap().into(),
                port: 443,
                username: "covenant".into(),
                lease_generation: spec.lease_generation,
                observed_host_keys: vec![SshHostKey {
                    algorithm: "ssh-ed25519".into(),
                    key_base64: STANDARD.encode(wire),
                }],
                transport: Some(SshTransport::OutboundRelay),
            },
        })
    }
    async fn ephemeral_after_access(
        &self,
        _: &JobEnvelopePayload,
        access: &LeaseAccess,
        pin: &VerifiedPhalaAccess,
    ) -> Result<(), ExecutorError> {
        assert_eq!(access.ssh.as_ref(), Some(&pin.access));
        self.submitted.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn after_access(
        &self,
        _: &JobEnvelopePayload,
        _: &LeaseAccess,
        _: &str,
    ) -> Result<(), ExecutorError> {
        panic!("ephemeral evidence cannot downgrade to managed")
    }
}

fn verified_backend(
    provider: Arc<Provider>,
    evidence: Arc<VerifiedEvidence>,
    directory: &Path,
) -> Arc<PhalaSessionBackend> {
    PhalaSessionBackend::open_journal(
        provider,
        ephemeral_config(true),
        directory,
        Arc::new(Closed(AtomicBool::new(false))),
        evidence,
    )
    .unwrap()
}

#[tokio::test]
async fn ephemeral_native_readiness_requires_verified_identity_and_durable_pin() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let evidence = VerifiedEvidence::new();
    let backend = verified_backend(provider.clone(), evidence.clone(), directory.path());
    let job = ephemeral_job();
    let access = backend.open(&job).await.unwrap();
    let row = provider.durable(job.job_id);
    assert_eq!(row.phase, Phase::Ready);
    assert_eq!(row.access, access.ssh);
    assert_eq!(
        row.verified_ephemeral.as_ref().unwrap().instance_id,
        "cd".repeat(20)
    );
    assert!(
        row.observed_instance_id.is_none(),
        "null Cloud instance must not erase verified identity"
    );
    assert_eq!(evidence.prepared.load(Ordering::SeqCst), 1);
    assert_eq!(evidence.verified.load(Ordering::SeqCst), 1);
    backend.allocate_ephemeral(&job).await.unwrap();
    assert_eq!(
        provider.durable(job.job_id).phase,
        Phase::Ready,
        "an allocation receipt cannot downgrade already verified readiness"
    );
    backend.after_access(&job, &access).await.unwrap();
    assert_eq!(evidence.submitted.load(Ordering::SeqCst), 1);
    backend.close(job.job_id).await;
    assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
}

#[tokio::test]
async fn ephemeral_quote_failure_grants_no_access_and_cleans_original_resource() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let evidence = VerifiedEvidence::new();
    evidence.fail_verification.store(true, Ordering::SeqCst);
    let backend = verified_backend(provider.clone(), evidence, directory.path());
    let job = ephemeral_job();
    assert!(backend.open(&job).await.is_err());
    let row = provider.durable(job.job_id);
    assert!(row.access.is_none());
    assert!(row.verified_ephemeral.is_none());
    backend.reconcile().await.unwrap();
    assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
    assert!(backend.open(&job).await.is_err());
}

#[tokio::test]
async fn ephemeral_relay_tombstone_prevents_guest_creation() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let evidence = VerifiedEvidence::new();
    evidence.fail_prepare.store(true, Ordering::SeqCst);
    let backend = verified_backend(provider.clone(), evidence, directory.path());
    let job = ephemeral_job();
    assert!(backend.open(&job).await.is_err());
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 0);
    backend.reconcile().await.unwrap();
    assert_eq!(
        provider.durable(job.job_id).phase,
        Phase::ProvisionAbandoned
    );
}

#[tokio::test]
async fn ephemeral_verified_key_or_instance_change_is_terminal() {
    for change_key in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Provider::new(directory.path());
        let evidence = VerifiedEvidence::new();
        let backend = verified_backend(provider.clone(), evidence.clone(), directory.path());
        let job = ephemeral_job();
        backend.open(&job).await.unwrap();
        if change_key {
            evidence.change_key.store(true, Ordering::SeqCst);
        } else {
            evidence.change_instance.store(true, Ordering::SeqCst);
        }
        assert!(backend.open(&job).await.is_err());
        backend.reconcile().await.unwrap();
        assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
        assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn ephemeral_verified_pin_survives_restart_and_provider_change_closes_it() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let evidence = VerifiedEvidence::new();
    let job = ephemeral_job();
    let pin = {
        let backend = verified_backend(provider.clone(), evidence.clone(), directory.path());
        backend.open(&job).await.unwrap();
        provider.durable(job.job_id).verified_ephemeral.unwrap()
    };
    let backend = verified_backend(provider.clone(), evidence, directory.path());
    assert_eq!(
        backend.row(job.job_id).unwrap().verified_ephemeral.unwrap(),
        pin
    );
    provider.machine.lock().as_mut().unwrap().instance_id = Some("ef".repeat(20));
    backend.reconcile().await.unwrap();
    assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
}
struct Provider {
    directory: PathBuf,
    provision_count: AtomicUsize,
    create_count: AtomicUsize,
    delete_count: AtomicUsize,
    create_response_lost: AtomicBool,
    machine: Mutex<Option<Machine>>,
    preserve_after_delete: AtomicBool,
    delete_failure: AtomicBool,
    get_failure: AtomicBool,
    wrong_key: AtomicBool,
    create_hangs: AtomicBool,
    creating: tokio::sync::Notify,
    get_gate: Mutex<Option<Arc<GetGate>>>,
}
#[derive(Default)]
struct GetGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl Provider {
    fn new(path: &Path) -> Arc<Self> {
        Arc::new(Self {
            directory: path.into(),
            provision_count: AtomicUsize::new(0),
            create_count: AtomicUsize::new(0),
            delete_count: AtomicUsize::new(0),
            create_response_lost: AtomicBool::new(false),
            machine: Mutex::new(None),
            preserve_after_delete: AtomicBool::new(false),
            delete_failure: AtomicBool::new(false),
            get_failure: AtomicBool::new(false),
            wrong_key: AtomicBool::new(false),
            create_hangs: AtomicBool::new(false),
            creating: tokio::sync::Notify::new(),
            get_gate: Mutex::new(None),
        })
    }
    fn durable(&self, id: Uuid) -> Row {
        let snapshot: Snapshot =
            serde_json::from_slice(&std::fs::read(self.directory.join("leases.json")).unwrap())
                .unwrap();
        snapshot.rows[&id].clone()
    }
}
#[async_trait]
impl PhalaProvider for Provider {
    async fn health(&self) -> Result<(), ExecutorError> {
        Ok(())
    }
    async fn provision(&self, plan: &LeasePlan) -> Result<Provision, ExecutorError> {
        assert_eq!(self.durable(plan.job_id).phase, Phase::ProvisionIntent);
        self.provision_count.fetch_add(1, Ordering::SeqCst);
        Ok(Provision {
            app_id: "ab".repeat(20),
            compose_hash: plan.workload_sha256().into(),
            env_public_key: if plan.is_ephemeral() {
                String::new()
            } else {
                "01".repeat(32)
            },
            os_image_hash: OS_IMAGE_SHA256.into(),
            instance_type: plan.allowance.instance_type.clone(),
            hourly_micro_usd: 9000,
            teepod_id: 17,
            kms_id: if plan.is_ephemeral() {
                String::new()
            } else {
                "phala".into()
            },
            device_id: plan.is_ephemeral().then(|| "02".repeat(32)),
            provision_token: plan
                .is_ephemeral()
                .then(|| "private-provision-token".into()),
        })
    }
    async fn create(&self, plan: &LeasePlan, provision: &Provision) -> Result<Uuid, ExecutorError> {
        let row = self.durable(plan.job_id);
        assert_eq!(row.phase, Phase::CreateIntent);
        assert_eq!(row.plan.payload_sha256, plan.payload_sha256);
        self.create_count.fetch_add(1, Ordering::SeqCst);
        let id = Uuid::new_v4();
        *self.machine.lock() = Some(Machine {
            vm_uuid: id,
            app_id: provision.app_id.clone(),
            name: plan.name.clone(),
            compose_hash: plan.workload_sha256().into(),
            instance_id: (!plan.is_ephemeral()).then(|| "cd".repeat(20)),
            running: true,
            deleting: false,
        });
        self.creating.notify_one();
        if self.create_hangs.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.create_response_lost.load(Ordering::SeqCst) {
            Err(failed("lost create response"))
        } else {
            Ok(id)
        }
    }
    async fn find(&self, _: &str) -> Result<Option<Machine>, ExecutorError> {
        Ok(self.machine.lock().clone())
    }
    async fn get(&self, _: Uuid) -> Result<Option<Machine>, ExecutorError> {
        let gate = self.get_gate.lock().take();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        if self.get_failure.load(Ordering::SeqCst) {
            return Err(failed("provider GET failed"));
        }
        Ok(self.machine.lock().clone())
    }
    async fn identity(
        &self,
        plan: &LeasePlan,
        machine: &Machine,
    ) -> Result<SshAccess, ExecutorError> {
        assert!(
            !plan.is_ephemeral(),
            "allocation must not request unverified identity"
        );
        let mut wire = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
        wire.extend_from_slice(
            &[if self.wrong_key.load(Ordering::SeqCst) {
                9
            } else {
                8
            }; 32],
        );
        Ok(SshAccess {
            host: format!(
                "{}-1022.{PHALA_GATEWAY}",
                machine.instance_id.as_ref().unwrap()
            ),
            port: 443,
            username: "covenant".into(),
            lease_generation: plan.generation().into(),
            observed_host_keys: vec![SshHostKey {
                algorithm: "ssh-ed25519".into(),
                key_base64: STANDARD.encode(wire),
            }],
            transport: Some(SshTransport::Tls),
        })
    }
    async fn delete(&self, _: Uuid) -> Result<(), ExecutorError> {
        let snapshot: Snapshot =
            serde_json::from_slice(&std::fs::read(self.directory.join("leases.json")).unwrap())
                .unwrap();
        let row = snapshot.rows.values().next().unwrap();
        assert_eq!(row.phase, Phase::DeletePending);
        assert!(row.close_requested);
        assert_eq!(row.delete_status, Some(DeleteStatus::OutcomeUnknown));
        self.delete_count.fetch_add(1, Ordering::SeqCst);
        if self.delete_failure.load(Ordering::SeqCst) {
            return Err(failed("delete transport outcome unknown"));
        }
        if !self.preserve_after_delete.load(Ordering::SeqCst) {
            *self.machine.lock() = None;
        }
        Ok(())
    }
}

fn successful_outcome() -> ExecutionOutcome {
    ExecutionOutcome {
        output: vec![],
        wall_ms: 10,
        tokens_in: None,
        tokens_out: None,
        finish_reason: None,
    }
}

#[tokio::test]
async fn accepted_delete_polls_once_across_stale_save_restart_and_delayed_absence() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let job = ephemeral_job();
    let cutoff = {
        let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
        backend.open(&job).await.unwrap();
        let stale = backend.row(job.job_id).unwrap();
        provider.preserve_after_delete.store(true, Ordering::SeqCst);
        backend.close(job.job_id).await;
        let pending = provider.durable(job.job_id);
        assert_eq!(pending.delete_status, Some(DeleteStatus::Accepted));
        assert_eq!(pending.delete_attempts, 1);
        backend.save(stale).unwrap();
        for _ in 0..3 {
            assert!(backend.reconcile().await.is_err());
            let row = provider.durable(job.job_id);
            assert_eq!(row.phase, Phase::DeletePending);
            assert_eq!(row.delete_status, Some(DeleteStatus::Accepted));
            assert_eq!(row.cleanup_deadline_at_ms, pending.cleanup_deadline_at_ms);
            assert!(!row.deletion_confirmed_in_time(now_ms()));
        }
        assert_eq!(provider.delete_count.load(Ordering::SeqCst), 1);
        pending.cleanup_deadline_at_ms
    };
    let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
    assert!(backend.reconcile().await.is_err());
    assert_eq!(provider.delete_count.load(Ordering::SeqCst), 1);
    // Asynchronous provider completion is independent of another DELETE.
    *provider.machine.lock() = None;
    backend.reconcile().await.unwrap();
    let row = provider.durable(job.job_id);
    assert_eq!(row.phase, Phase::Deleted);
    assert_eq!(row.delete_status, Some(DeleteStatus::Accepted));
    assert_eq!(row.cleanup_deadline_at_ms, cutoff);
    assert!(row.deletion_confirmed_in_time(now_ms()));
    assert_eq!(provider.delete_count.load(Ordering::SeqCst), 1);
    assert!(
        backend.admit(&ephemeral_job()).is_err(),
        "grant stays consumed"
    );
}

#[tokio::test]
async fn unknown_delete_or_crash_before_send_never_retries_running_vm_after_restart() {
    for crash_before_send in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Provider::new(directory.path());
        let job = ephemeral_job();
        let cutoff = {
            let backend =
                verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
            backend.open(&job).await.unwrap();
            if crash_before_send {
                let mut row = backend.row(job.job_id).unwrap();
                row.request_close_at(now_ms());
                row.phase = Phase::DeletePending;
                row.delete_attempts = 1;
                row.delete_status = Some(DeleteStatus::OutcomeUnknown);
                backend.save(row).unwrap();
            } else {
                provider.delete_failure.store(true, Ordering::SeqCst);
                backend.close(job.job_id).await;
            }
            let row = provider.durable(job.job_id);
            assert_eq!(row.delete_status, Some(DeleteStatus::OutcomeUnknown));
            row.cleanup_deadline_at_ms
        };
        provider.delete_failure.store(false, Ordering::SeqCst);
        let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
        for _ in 0..3 {
            assert!(provider.machine.lock().as_ref().unwrap().running);
            assert!(backend.reconcile().await.is_err());
            let row = provider.durable(job.job_id);
            assert_eq!(row.delete_status, Some(DeleteStatus::OutcomeUnknown));
            assert_eq!(row.cleanup_deadline_at_ms, cutoff);
        }
        assert_eq!(
            provider.delete_count.load(Ordering::SeqCst),
            usize::from(!crash_before_send)
        );
        // A later provider deleting observation can confirm acceptance, never absence.
        provider.machine.lock().as_mut().unwrap().deleting = true;
        assert!(backend.reconcile().await.is_err());
        assert_eq!(
            provider.durable(job.job_id).delete_status,
            Some(DeleteStatus::Accepted)
        );
        *provider.machine.lock() = None;
        backend.reconcile().await.unwrap();
        assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
        assert_eq!(
            provider.delete_count.load(Ordering::SeqCst),
            usize::from(!crash_before_send)
        );
    }
}

#[tokio::test]
async fn legacy_attempt_or_owned_provider_deleting_only_polls_without_new_delete() {
    for (attempts, deleting) in [(3, false), (3, true), (0, true)] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Provider::new(directory.path());
        let job = ephemeral_job();
        {
            let backend =
                verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
            backend.open(&job).await.unwrap();
            let mut row = backend.row(job.job_id).unwrap();
            row.request_close_at(now_ms());
            row.phase = Phase::DeletePending;
            row.delete_attempts = attempts;
            row.delete_status = None;
            backend.save(row).unwrap();
        }
        provider.machine.lock().as_mut().unwrap().deleting = deleting;
        let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
        assert!(backend.reconcile().await.is_err());
        let row = provider.durable(job.job_id);
        assert_eq!(row.delete_attempts, attempts);
        assert_eq!(
            row.delete_status,
            Some(if deleting {
                DeleteStatus::Accepted
            } else {
                DeleteStatus::OutcomeUnknown
            })
        );
        assert_eq!(provider.delete_count.load(Ordering::SeqCst), 0);
        *provider.machine.lock() = None;
        backend.reconcile().await.unwrap();
        assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
        assert_eq!(provider.delete_count.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn provider_deleting_never_adopts_foreign_uuid_or_app() {
    for foreign_uuid in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Provider::new(directory.path());
        let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
        let job = ephemeral_job();
        backend.open(&job).await.unwrap();
        let original_uuid = provider.durable(job.job_id).vm_uuid;
        {
            let mut machine = provider.machine.lock();
            let machine = machine.as_mut().unwrap();
            machine.deleting = true;
            if foreign_uuid {
                machine.vm_uuid = Uuid::new_v4();
            } else {
                machine.app_id = "ef".repeat(20);
            }
        }
        backend.request_close(job.job_id);
        assert!(backend.reconcile().await.is_err());
        let row = provider.durable(job.job_id);
        assert_eq!(row.vm_uuid, original_uuid);
        assert_eq!(row.delete_status, None);
        assert_eq!(provider.delete_count.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn deletion_journal_metadata_requires_original_ownership_and_close_intent() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
    let job = ephemeral_job();
    backend.open(&job).await.unwrap();
    provider.preserve_after_delete.store(true, Ordering::SeqCst);
    backend.close(job.job_id).await;
    let original = backend.row(job.job_id).unwrap();
    let mut journal = backend.journal.lock();
    let operator = journal.snapshot.operator.clone();
    journal.validate(&operator).unwrap();
    for mutation in 0..5 {
        let mut invalid = original.clone();
        match mutation {
            0 => invalid.close_requested = false,
            1 => invalid.phase = Phase::Ready,
            2 => invalid.vm_uuid = None,
            3 => invalid.provision = None,
            4 => {
                invalid.delete_attempts = 0;
                invalid.delete_status = Some(DeleteStatus::OutcomeUnknown);
            }
            _ => unreachable!(),
        }
        journal.snapshot.rows.insert(job.job_id, invalid);
        assert!(journal.validate(&operator).is_err());
    }
    journal.snapshot.rows.insert(job.job_id, original);
    journal.validate(&operator).unwrap();
}

#[tokio::test]
async fn ephemeral_success_waits_for_confirmed_delete_and_never_recreates() {
    for failure_mode in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let provider = Provider::new(directory.path());
        let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
        let job = ephemeral_job();
        backend.open(&job).await.unwrap();
        provider.preserve_after_delete.store(true, Ordering::SeqCst);
        provider
            .delete_failure
            .store(failure_mode == 1, Ordering::SeqCst);
        provider
            .get_failure
            .store(failure_mode == 2, Ordering::SeqCst);
        let executor = PhalaExecutor {
            lease: LeaseExecutor::new(backend.clone(), LeaseControl::new()),
            backend: backend.clone(),
        };
        let mut row = backend.row(job.job_id).unwrap();
        row.close_requested = true;
        // Exhausted grace never disables owned cleanup. The dedicated virtual
        // time test covers elapsed grace without racing filesystem latency.
        row.cleanup_deadline_at_ms = Some(0);
        backend.save(row).unwrap();
        assert!(backend.reconcile().await.is_err());
        let result = executor
            .finish_ephemeral(&job, Ok(successful_outcome()))
            .await;
        assert!(
            result.is_err(),
            "unconfirmed deletion must not become a payable successful receipt"
        );
        let pending = provider.durable(job.job_id);
        assert_eq!(
            pending.phase,
            if failure_mode == 2 {
                Phase::Ready
            } else {
                Phase::DeletePending
            }
        );
        assert!(pending.close_requested);
        assert!(pending.vm_uuid.is_some());
        assert!(pending.verified_ephemeral.is_some());
        assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
        // Later recovery resolves the original owned VM, without retroactively
        // converting the previously failed execution into a success.
        provider.delete_failure.store(false, Ordering::SeqCst);
        provider.get_failure.store(false, Ordering::SeqCst);
        provider
            .preserve_after_delete
            .store(false, Ordering::SeqCst);
        *provider.machine.lock() = None;
        backend.reconcile().await.unwrap();
        assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
        assert!(
            executor
                .finish_ephemeral(&job, Ok(successful_outcome()))
                .await
                .is_err(),
            "late cleanup cannot create a new payable success window"
        );
        assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn managed_success_retains_its_existing_cleanup_contract() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = backend(provider, directory.path(), true);
    let executor = PhalaExecutor {
        lease: LeaseExecutor::new(backend.clone(), LeaseControl::new()),
        backend,
    };
    assert!(executor
        .finish_ephemeral(&job(), Ok(successful_outcome()))
        .await
        .is_ok());
}

#[tokio::test]
async fn asynchronous_delete_uses_one_budget_across_initial_close_and_final_confirmation() {
    for elapsed in [82, 301] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Provider::new(directory.path());
        let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
        let job = ephemeral_job();
        backend.open(&job).await.unwrap();
        let gate = Arc::new(GetGate::default());
        *provider.get_gate.lock() = Some(gate.clone());
        let closing = {
            let backend = backend.clone();
            let id = job.job_id;
            tokio::spawn(async move { backend.close(id).await })
        };
        gate.entered.notified().await;
        let cutoff = provider.durable(job.job_id).cleanup_deadline_at_ms;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(elapsed)).await;
        if elapsed == 82 {
            assert!(!closing.is_finished(), "no success before provider absence");
            gate.release.notify_one();
        }
        closing.await.unwrap();
        let executor = PhalaExecutor {
            lease: LeaseExecutor::new(backend.clone(), LeaseControl::new()),
            backend: backend.clone(),
        };
        let result = executor
            .finish_ephemeral(&job, Ok(successful_outcome()))
            .await;
        assert_eq!(result.is_ok(), elapsed == 82);
        if let Ok(result) = result {
            assert_eq!(
                result.wall_ms, 10,
                "teardown does not extend receipt runtime"
            );
        } else {
            backend.reconcile().await.unwrap();
            assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
            assert!(
                executor
                    .finish_ephemeral(&job, Ok(successful_outcome()))
                    .await
                    .is_err(),
                "a second confirmation wait cannot refresh the first close budget"
            );
        }
        assert_eq!(provider.durable(job.job_id).cleanup_deadline_at_ms, cutoff);
        assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
        tokio::time::resume();
    }
}

#[tokio::test]
async fn deletion_observed_after_cutoff_is_cleanup_only_even_when_get_started_before_it() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
    let job = ephemeral_job();
    backend.open(&job).await.unwrap();
    let mut row = backend.row(job.job_id).unwrap();
    let cutoff = now_ms() + 50;
    row.close_requested = true;
    row.cleanup_deadline_at_ms = Some(cutoff);
    backend.save(row).unwrap();
    let gate = Arc::new(GetGate::default());
    *provider.get_gate.lock() = Some(gate.clone());
    let recovery = {
        let backend = backend.clone();
        tokio::spawn(async move { backend.reconcile().await })
    };
    gate.entered.notified().await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    *provider.machine.lock() = None;
    gate.release.notify_one();
    recovery.await.unwrap().unwrap();
    let row = provider.durable(job.job_id);
    assert_eq!(row.phase, Phase::Deleted);
    assert!(row.deleted_confirmed_at_ms.unwrap() >= cutoff);
    assert!(!row.deletion_confirmed_in_time(now_ms()));
    let executor = PhalaExecutor {
        lease: LeaseExecutor::new(backend.clone(), LeaseControl::new()),
        backend,
    };
    assert!(executor
        .finish_ephemeral(&job, Ok(successful_outcome()))
        .await
        .is_err());
}

#[tokio::test]
async fn first_close_cutoff_survives_stale_rows_reopen_and_legacy_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let job = ephemeral_job();
    let cutoff = {
        let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
        backend.open(&job).await.unwrap();
        let stale = backend.row(job.job_id).unwrap();
        backend.request_close(job.job_id);
        let closed = backend.row(job.job_id).unwrap();
        assert_eq!(
            closed.plan.result_deadline_at_ms,
            Some(job.issued_at_ms + job.deadline_ms)
        );
        backend.save(stale).unwrap();
        backend.request_close(job.job_id);
        assert!(provider.durable(job.job_id).close_requested);
        assert_eq!(
            provider.durable(job.job_id).cleanup_deadline_at_ms,
            closed.cleanup_deadline_at_ms
        );
        closed.cleanup_deadline_at_ms
    };
    {
        let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
        backend.request_close(job.job_id);
        assert_eq!(provider.durable(job.job_id).cleanup_deadline_at_ms, cutoff);
    }
    // Legacy closing rows remain readable for teardown, without inventing a
    // result deadline or awarding a fresh grace after restart.
    let path = directory.path().join("leases.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let row = value["rows"][job.job_id.to_string()]
        .as_object_mut()
        .unwrap();
    row.remove("cleanup_deadline_at_ms");
    row["plan"]
        .as_object_mut()
        .unwrap()
        .remove("result_deadline_at_ms");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
    backend.reconcile().await.unwrap();
    let row = provider.durable(job.job_id);
    assert_eq!(row.phase, Phase::Deleted);
    assert_eq!(row.cleanup_deadline_at_ms, Some(0));
    let executor = PhalaExecutor {
        lease: LeaseExecutor::new(backend.clone(), LeaseControl::new()),
        backend,
    };
    assert!(executor
        .finish_ephemeral(&job, Ok(successful_outcome()))
        .await
        .is_err());
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn old_ephemeral_order_cannot_allocate_but_remains_historically_verifiable() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = verified_backend(provider.clone(), VerifiedEvidence::new(), directory.path());
    let mut job = ephemeral_job();
    job.deadline_ms = 120_000;
    job.validate_input().unwrap();
    assert!(backend.open(&job).await.is_err());
    assert_eq!(provider.provision_count.load(Ordering::SeqCst), 0);
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 0);
}

#[test]
fn cleanup_cutoff_caps_at_signed_deadline_and_refuses_clock_rollback_or_missing_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = verified_backend(provider, VerifiedEvidence::new(), directory.path());
    let job = ephemeral_job();
    let plan = backend.plan(&job).unwrap();
    let mut row = Row {
        plan,
        phase: Phase::Provisioned,
        provision: None,
        vm_uuid: None,
        access: None,
        close_requested: false,
        cleanup_deadline_at_ms: None,
        deleted_confirmed_at_ms: None,
        last_observed_ms: 1000,
        delete_attempts: 0,
        delete_status: None,
        observed_instance_id: None,
        verified_ephemeral: None,
    };
    row.plan.result_deadline_at_ms = Some(200_000);
    row.request_close_at(50_000);
    assert_eq!(row.cleanup_deadline_at_ms, Some(140_000));
    assert_eq!(row.last_observed_ms, 50_000);
    assert!(row.cleanup_remaining(49_999).is_err());
    row.request_close_at(80_000);
    assert_eq!(row.cleanup_deadline_at_ms, Some(140_000));
    assert!(row.cleanup_remaining(140_000).is_err());
    row.close_requested = false;
    row.cleanup_deadline_at_ms = None;
    row.plan.result_deadline_at_ms = Some(59_999);
    row.request_close_at(90_000);
    assert_eq!(row.cleanup_deadline_at_ms, Some(0));
}

fn operator() -> String {
    bs58::encode([3; 32]).into_string()
}
fn config(enabled: bool) -> PhalaConfig {
    PhalaConfig {
        operator_pubkey_b58: operator(),
        wrapper_template: reviewed_phala_wrapper(),
        image_digest: format!("sha256:{}", "12".repeat(32)),
        allow_managed_kms_creation: enabled,
        allow_ephemeral_creation: false,
        creation_allowance: Some(CreationAllowance {
            schema_version: 1,
            review_id: "disposable-test-review".into(),
            operator_pubkey_b58: operator(),
            instance_type: "tdx.test".into(),
            max_hourly_micro_usd: 10000,
            max_runtime_secs: 60,
            max_new_resources: 1,
            planning_reserve_micro_usd: 10000,
            valid_until_ms: now_ms() + 120_000,
            job_id: None,
            buyer_pubkey_b58: None,
        }),
        ready_timeout: Duration::from_millis(50),
        poll_interval: Duration::from_millis(1),
    }
}
fn job() -> JobEnvelopePayload {
    let mut wire = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
    wire.extend_from_slice(&[7; 32]);
    let mut job = JobEnvelopePayload {
        job_id: Uuid::new_v4(),
        buyer: AgentId::new("buyer@local", [2; 32]),
        kind: JobKind::LeaseSession,
        capability_requirement: CapabilityRequirement {
            inference_tariff: None,
            gpu_class: Some("cpu".into()),
            min_vram_gb: None,
            model_id: Some(PHALA_CPU_MODEL.into()),
            kind: JobKind::LeaseSession,
            max_duration_secs: 60,
            min_reputation_bps: None,
        },
        input: vec![lease_input(LeaseTerms {
            max_duration_secs: 60,
            rate_micro_usdc_per_sec: 10,
            client_public_key: Some(format!("ssh-ed25519 {}", STANDARD.encode(wire))),
        })
        .unwrap()],
        price_micro_usdc: 600,
        deadline_ms: 120_000,
        idempotency: A2AIdempotency::new(A2ADuplicateSafety::Idempotent, "phala-test"),
        issued_at_ms: now_ms(),
        referral_code: None,
        stream: true,
    };
    let c = config(true);
    let (_, spec) =
        phala_workload(&job, &operator(), &c.wrapper_template, &c.image_digest).unwrap();
    job.input.push(phala_lease_input(spec).unwrap());
    job
}
fn backend(provider: Arc<Provider>, path: &Path, enabled: bool) -> Arc<PhalaSessionBackend> {
    PhalaSessionBackend::open_journal(
        provider,
        config(enabled),
        path,
        Arc::new(Closed(AtomicBool::new(false))),
        Arc::new(Evidence),
    )
    .unwrap()
}

fn ephemeral_config(enabled: bool) -> PhalaConfig {
    let mut config = config(false);
    config.wrapper_template = reviewed_phala_ephemeral_wrapper();
    config.allow_ephemeral_creation = enabled;
    config
}
fn ephemeral_job() -> JobEnvelopePayload {
    let mut job = job();
    job.deadline_ms = 60_000 + covenant_compute_protocol::phala::PHALA_EPHEMERAL_RESULT_SLACK_MS;
    job.input.pop();
    job.capability_requirement.model_id = Some(PHALA_EPHEMERAL_CPU_MODEL.into());
    let config = ephemeral_config(true);
    let relay = PhalaRelayConfig {
        url: "https://relay.example.com".into(),
        session_id: job.job_id.to_string(),
    };
    let (_, spec) = phala_ephemeral_workload(
        &job,
        &operator(),
        &config.wrapper_template,
        &config.image_digest,
        &relay,
    )
    .unwrap();
    job.input.push(phala_ephemeral_lease_input(spec).unwrap());
    job
}
fn ephemeral_backend(
    provider: Arc<Provider>,
    path: &Path,
    enabled: bool,
) -> Arc<PhalaSessionBackend> {
    PhalaSessionBackend::open_journal(
        provider,
        ephemeral_config(enabled),
        path,
        Arc::new(Closed(AtomicBool::new(false))),
        Arc::new(Evidence),
    )
    .unwrap()
}

#[tokio::test]
async fn ephemeral_allocation_is_single_boot_without_secret_or_access_grant() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = ephemeral_backend(provider.clone(), directory.path(), true);
    let job = ephemeral_job();
    let first = backend.allocate_ephemeral(&job).await.unwrap();
    let second = backend.allocate_ephemeral(&job).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
    assert_eq!(provider.provision_count.load(Ordering::SeqCst), 1);
    let row = provider.durable(job.job_id);
    assert_eq!(row.phase, Phase::Created);
    assert!(row.plan.spec.is_none());
    assert!(row.plan.ephemeral_spec.is_some());
    assert!(row.plan.collector_token.is_empty());
    assert!(row.access.is_none());
    assert!(row.observed_instance_id.is_none());
    let provision = row.provision.unwrap();
    assert_eq!(
        provision.provision_token.as_deref(),
        Some("private-provision-token")
    );
    assert!(provision.kms_id.is_empty());
    assert!(provision.env_public_key.is_empty());
    // An allocation cannot be mistaken for served native readiness.
    assert!(backend.open(&job).await.is_err());
    assert!(
        backend.allocate_ephemeral(&job).await.is_err(),
        "queued cancellation is terminal before reconciliation"
    );
    backend.reconcile().await.unwrap();
    assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
    assert!(backend.allocate_ephemeral(&job).await.is_err());
    assert!(backend.allocate_ephemeral(&ephemeral_job()).await.is_err());
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ephemeral_advertising_and_managed_enablement_cannot_enable_native_readiness() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let mut config = ephemeral_config(false);
    config.allow_managed_kms_creation = true;
    let backend = PhalaSessionBackend::open_journal(
        provider.clone(),
        config,
        directory.path(),
        Arc::new(Closed(AtomicBool::new(false))),
        Arc::new(Evidence),
    )
    .unwrap();
    let job = ephemeral_job();
    assert!(backend.allocate_ephemeral(&job).await.is_err());
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 0);
    drop(backend);
    let backend = ephemeral_backend(provider.clone(), directory.path(), true);
    let executor = PhalaExecutor {
        lease: LeaseExecutor::new(backend.clone(), LeaseControl::new()),
        backend,
    };
    assert!(executor.admit(&job, Duration::from_secs(60)).is_err());
    assert!(executor.health().await.is_err());
    assert!(executor
        .backend
        .allocate_ephemeral(&super::tests::job())
        .await
        .is_err());
    assert_eq!(provider.provision_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn ephemeral_lost_create_recovers_same_app_and_cleanup_without_second_create() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    provider.create_response_lost.store(true, Ordering::SeqCst);
    let job = ephemeral_job();
    {
        let backend = ephemeral_backend(provider.clone(), directory.path(), true);
        assert!(backend.allocate_ephemeral(&job).await.is_err());
        assert_eq!(provider.durable(job.job_id).phase, Phase::CreateIntent);
    }
    let backend = ephemeral_backend(provider.clone(), directory.path(), false);
    // The failed attempt's cancellation is durable, so restart only reconciles
    // its original resource; it cannot recover an allocation/access grant.
    assert!(provider.durable(job.job_id).close_requested);
    assert!(backend.allocate_ephemeral(&job).await.is_err());
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
    provider.preserve_after_delete.store(true, Ordering::SeqCst);
    backend.close(job.job_id).await;
    assert_eq!(provider.durable(job.job_id).phase, Phase::DeletePending);
    drop(backend);
    provider
        .preserve_after_delete
        .store(false, Ordering::SeqCst);
    *provider.machine.lock() = None;
    let backend = ephemeral_backend(provider.clone(), directory.path(), false);
    backend.reconcile().await.unwrap();
    assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ephemeral_unknown_create_and_foreign_app_never_recreate_or_grant_cleanup_authority() {
    for foreign in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Provider::new(directory.path());
        provider.create_response_lost.store(true, Ordering::SeqCst);
        let job = ephemeral_job();
        {
            let backend = ephemeral_backend(provider.clone(), directory.path(), true);
            assert!(backend.allocate_ephemeral(&job).await.is_err());
        }
        if foreign {
            provider.machine.lock().as_mut().unwrap().app_id = "ff".repeat(20);
        } else {
            *provider.machine.lock() = None;
        }
        let backend = ephemeral_backend(provider.clone(), directory.path(), true);
        assert!(backend.allocate_ephemeral(&job).await.is_err());
        assert!(backend.reconcile().await.is_err());
        assert!(backend.allocate_ephemeral(&ephemeral_job()).await.is_err());
        assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
        assert_eq!(provider.delete_count.load(Ordering::SeqCst), 0);
        assert_eq!(backend.shutdown_status().unknown_creates, 1);
    }
}

#[tokio::test]
async fn ephemeral_observed_instance_change_is_terminal_and_cannot_restart() {
    for disappear in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Provider::new(directory.path());
        let backend = ephemeral_backend(provider.clone(), directory.path(), true);
        let job = ephemeral_job();
        backend.allocate_ephemeral(&job).await.unwrap();
        provider.machine.lock().as_mut().unwrap().instance_id = Some("cd".repeat(20));
        backend.reconcile().await.unwrap();
        assert_eq!(
            provider.durable(job.job_id).observed_instance_id,
            Some("cd".repeat(20))
        );
        provider.machine.lock().as_mut().unwrap().instance_id =
            (!disappear).then(|| "ef".repeat(20));
        backend.reconcile().await.unwrap();
        assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
        assert!(backend.allocate_ephemeral(&job).await.is_err());
        assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn ephemeral_cancellation_while_create_is_unknown_only_reconciles_owned_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    provider.create_hangs.store(true, Ordering::SeqCst);
    let backend = ephemeral_backend(provider.clone(), directory.path(), true);
    let job = ephemeral_job();
    let id = job.job_id;
    let worker = backend.clone();
    let task = tokio::spawn(async move { worker.allocate_ephemeral(&job).await });
    tokio::time::timeout(Duration::from_secs(1), provider.creating.notified())
        .await
        .unwrap();
    task.abort();
    let _ = task.await;
    backend.reconcile().await.unwrap();
    assert_eq!(provider.durable(id).phase, Phase::Deleted);
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
}

#[test]
fn journal_compatibility_preserves_managed_rows_and_rejects_mixed_profiles_or_secrets() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = backend(provider, directory.path(), true);
    let mut managed = backend.plan(&job()).unwrap();
    managed.collector_token = "01".repeat(32);
    let old_wire = serde_json::to_value(&managed).unwrap();
    assert!(old_wire.get("ephemeral_spec").is_none());
    assert!(old_wire["spec"].is_object());
    let restored: LeasePlan = serde_json::from_value(old_wire.clone()).unwrap();
    restored.validate_spec().unwrap();
    assert_eq!(serde_json::to_value(restored).unwrap(), old_wire);
    drop(backend);
    let provider = Provider::new(directory.path());
    let backend = ephemeral_backend(provider, directory.path(), true);
    let mut ephemeral = backend.plan(&ephemeral_job()).unwrap();
    ephemeral.validate_spec().unwrap();
    ephemeral.collector_token = "01".repeat(32);
    assert!(ephemeral.validate_spec().is_err());
    ephemeral.collector_token.clear();
    ephemeral.spec = managed.spec.clone();
    assert!(ephemeral.validate_spec().is_err());
    ephemeral.spec = None;
    ephemeral.ephemeral_spec = None;
    assert!(ephemeral.validate_spec().is_err());
}

#[tokio::test]
async fn signed_lease_creates_once_and_delete_requires_observed_absence() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = backend(provider.clone(), directory.path(), true);
    let job = job();
    assert!(backend.shutdown_status().is_resolved());
    let first = backend.open(&job).await.unwrap();
    assert_eq!(backend.shutdown_status().pending_leases, 1);
    assert!(!backend.shutdown_status().is_resolved());
    assert_eq!(
        first.ssh.as_ref().unwrap().transport,
        Some(SshTransport::Tls)
    );
    assert_eq!(backend.open(&job).await.unwrap().ssh, first.ssh);
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
    provider.preserve_after_delete.store(true, Ordering::SeqCst);
    backend.close(job.job_id).await;
    assert_eq!(provider.durable(job.job_id).phase, Phase::DeletePending);
    assert_eq!(backend.shutdown_status().pending_deletes, 1);
    assert!(!backend.shutdown_status().is_resolved());
    assert!(backend.admit(&super::tests::job()).is_err());
    provider
        .preserve_after_delete
        .store(false, Ordering::SeqCst);
    *provider.machine.lock() = None;
    backend.reconcile().await.unwrap();
    assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
    assert!(backend.shutdown_status().is_resolved());
    assert_eq!(provider.delete_count.load(Ordering::SeqCst), 1);
    assert!(backend.open(&job).await.is_err());
    assert!(
        backend.open(&super::tests::job()).await.is_err(),
        "confirmed deletion must not silently refund the planning reservation"
    );
}

#[tokio::test]
async fn lost_create_response_restarts_by_exact_app_without_second_create() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    provider.create_response_lost.store(true, Ordering::SeqCst);
    let job = job();
    {
        let backend = backend(provider.clone(), directory.path(), true);
        assert!(backend.open(&job).await.is_err());
        assert_eq!(provider.durable(job.job_id).phase, Phase::CreateIntent);
    }
    let restarted = backend(provider.clone(), directory.path(), false);
    let access = restarted.open(&job).await.unwrap();
    assert_eq!(access.job_id, job.job_id);
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
    restarted.close(job.job_id).await;
    assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
}

#[tokio::test]
async fn unknown_create_absence_never_releases_capacity_or_recreates() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    provider.create_response_lost.store(true, Ordering::SeqCst);
    let original = job();
    {
        let backend = backend(provider.clone(), directory.path(), true);
        assert!(backend.open(&original).await.is_err());
    }
    *provider.machine.lock() = None;
    let backend = backend(provider.clone(), directory.path(), true);
    assert!(backend.open(&original).await.is_err());
    assert!(backend.reconcile().await.is_err());
    assert!(backend.open(&job()).await.is_err());
    assert_eq!(backend.shutdown_status().unknown_creates, 1);
    assert!(!backend.shutdown_status().is_resolved());
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
    assert_eq!(provider.delete_count.load(Ordering::SeqCst), 0);
    assert!(provider.durable(original.job_id).close_requested);
}

#[tokio::test]
async fn foreign_resource_is_neither_adopted_nor_deleted_after_unknown_create() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    provider.create_response_lost.store(true, Ordering::SeqCst);
    let job = job();
    {
        let backend = backend(provider.clone(), directory.path(), true);
        assert!(backend.open(&job).await.is_err());
    }
    provider.machine.lock().as_mut().unwrap().name = "existing-diagnostic-cvm".into();
    let backend = backend(provider.clone(), directory.path(), true);
    assert!(backend.open(&job).await.is_err());
    assert!(backend.reconcile().await.is_err());
    assert_eq!(provider.delete_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn owned_uuid_cleanup_survives_workload_and_name_drift() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = backend(provider.clone(), directory.path(), true);
    let job = job();
    backend.open(&job).await.unwrap();
    {
        let mut state = provider.machine.lock();
        let machine = state.as_mut().unwrap();
        machine.compose_hash = String::new();
        machine.name = "changed-by-control-plane".into();
    }
    backend.close(job.job_id).await;
    assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
    assert_eq!(provider.delete_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn disabled_creation_still_polls_owned_cleanup_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let job = job();
    {
        let backend = backend(provider.clone(), directory.path(), true);
        backend.open(&job).await.unwrap();
        provider.preserve_after_delete.store(true, Ordering::SeqCst);
        backend.close(job.job_id).await;
    }
    provider
        .preserve_after_delete
        .store(false, Ordering::SeqCst);
    *provider.machine.lock() = None;
    let backend = backend(provider.clone(), directory.path(), false);
    backend.reconcile().await.unwrap();
    assert_eq!(provider.durable(job.job_id).phase, Phase::Deleted);
    assert!(backend.open(&super::tests::job()).await.is_err());
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn changed_payload_or_key_and_unsigned_leases_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = backend(provider.clone(), directory.path(), true);
    let mut unsigned = job();
    unsigned.input.pop();
    assert!(backend.open(&unsigned).await.is_err());
    assert_eq!(provider.provision_count.load(Ordering::SeqCst), 0);
    let original = job();
    backend.open(&original).await.unwrap();
    let mut changed = original.clone();
    changed.price_micro_usdc += 1;
    assert!(backend.open(&changed).await.is_err());
    provider.wrong_key.store(true, Ordering::SeqCst);
    assert!(backend.open(&original).await.is_err());
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_during_create_reconciles_and_deletes_without_second_create() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    provider.create_hangs.store(true, Ordering::SeqCst);
    let backend = backend(provider.clone(), directory.path(), true);
    let executor = PhalaExecutor::new(backend.clone(), Arc::new(Closed(AtomicBool::new(false))));
    let job = job();
    let id = job.job_id;
    let (sink, _receiver) = tokio::sync::mpsc::channel(2);
    let task = tokio::spawn(async move {
        executor
            .execute_streaming(&job, Duration::from_secs(60), sink)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), provider.creating.notified())
        .await
        .unwrap();
    task.abort();
    let _ = task.await;
    backend.reconcile().await.unwrap();
    assert_eq!(provider.durable(id).phase, Phase::Deleted);
    assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn journal_lock_and_invalid_history_stop_before_provider_access() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let first = backend(provider.clone(), directory.path(), true);
    assert!(PhalaSessionBackend::open_journal(
        provider.clone(),
        config(true),
        directory.path(),
        Arc::new(Closed(AtomicBool::new(false))),
        Arc::new(Evidence)
    )
    .is_err());
    drop(first);
    std::fs::write(directory.path().join("leases.json"), b"not-json").unwrap();
    assert!(PhalaSessionBackend::open_journal(
        provider.clone(),
        config(true),
        directory.path(),
        Arc::new(Closed(AtomicBool::new(false))),
        Arc::new(Evidence)
    )
    .is_err());
    assert_eq!(provider.provision_count.load(Ordering::SeqCst), 0);
}

#[test]
fn io_failure_poison_prevents_later_mutations() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = backend(provider, directory.path(), true);
    let mut plan = backend.plan(&job()).unwrap();
    plan.collector_token = "01".repeat(32);
    std::fs::create_dir(directory.path().join("leases.json")).unwrap();
    let row = Row {
        plan,
        phase: Phase::ProvisionIntent,
        provision: None,
        vm_uuid: None,
        access: None,
        close_requested: false,
        cleanup_deadline_at_ms: None,
        deleted_confirmed_at_ms: None,
        last_observed_ms: now_ms(),
        delete_attempts: 0,
        delete_status: None,
        observed_instance_id: None,
        verified_ephemeral: None,
    };
    assert!(backend.save(row.clone()).is_err());
    std::fs::remove_dir(directory.path().join("leases.json")).unwrap();
    assert!(backend.save(row).is_err());
    assert!(backend.admit(&job()).is_err());
    assert!(backend.shutdown_status().journal_poisoned);
    assert!(!backend.shutdown_status().is_resolved());
}

#[tokio::test]
async fn expired_signed_lease_refuses_without_provider_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = backend(provider.clone(), directory.path(), true);
    let mut job = job();
    job.input.pop();
    job.issued_at_ms = now_ms() - 61_000;
    let c = config(true);
    let (_, spec) =
        phala_workload(&job, &operator(), &c.wrapper_template, &c.image_digest).unwrap();
    job.input.push(phala_lease_input(spec).unwrap());
    assert!(backend.open(&job).await.is_err());
    assert_eq!(provider.provision_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn future_signed_lease_and_bool_without_allowance_refuse_before_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Provider::new(directory.path());
    let backend = backend(provider.clone(), directory.path(), true);
    let mut future = job();
    future.input.pop();
    future.issued_at_ms = now_ms() + 60_000;
    let c = config(true);
    let (_, spec) =
        phala_workload(&future, &operator(), &c.wrapper_template, &c.image_digest).unwrap();
    future.input.push(phala_lease_input(spec).unwrap());
    assert!(backend.open(&future).await.is_err());
    drop(backend);
    let mut c = config(true);
    c.creation_allowance = None;
    let backend = PhalaSessionBackend::open_journal(
        provider.clone(),
        c,
        directory.path(),
        Arc::new(Closed(AtomicBool::new(false))),
        Arc::new(Evidence),
    )
    .unwrap();
    assert!(backend.open(&job()).await.is_err());
    assert_eq!(provider.provision_count.load(Ordering::SeqCst), 0);
}

#[test]
fn allowance_requires_one_resource_and_covers_reviewed_runtime() {
    let mut a = config(true).creation_allowance.unwrap();
    a.max_new_resources = 2;
    assert!(a.validate(&operator()).is_err());
    a.max_new_resources = 1;
    a.planning_reserve_micro_usd = 1;
    assert!(a.validate(&operator()).is_err());
    a.planning_reserve_micro_usd = 10000;
    a.max_runtime_secs = 3601;
    assert!(a.validate(&operator()).is_err());
}

#[test]
fn allowance_optional_canary_pins_require_a_complete_valid_pair() {
    let mut allowance = config(true).creation_allowance.unwrap();
    let legacy = serde_json::to_value(&allowance).unwrap();
    assert!(legacy.get("job_id").is_none());
    assert!(legacy.get("buyer_pubkey_b58").is_none());
    let restored: CreationAllowance = serde_json::from_value(legacy).unwrap();
    assert_eq!(restored, allowance);
    allowance.job_id = Some(Uuid::new_v4());
    assert!(allowance.validate(&operator()).is_err());
    allowance.buyer_pubkey_b58 = Some("malformed".into());
    assert!(allowance.validate(&operator()).is_err());
    allowance.buyer_pubkey_b58 = Some(bs58::encode([2; 32]).into_string());
    assert!(allowance.validate(&operator()).is_ok());
    allowance.job_id = Some(Uuid::nil());
    assert!(allowance.validate(&operator()).is_err());
    allowance.job_id = None;
    assert!(allowance.validate(&operator()).is_err());
}

#[tokio::test]
async fn pinned_canary_rejects_other_job_or_buyer_without_spending_allowance() {
    for wrong_buyer in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Provider::new(directory.path());
        let allowed_job = ephemeral_job();
        let mut config = ephemeral_config(true);
        let allowance = config.creation_allowance.as_mut().unwrap();
        allowance.job_id = Some(allowed_job.job_id);
        allowance.buyer_pubkey_b58 = Some(allowed_job.buyer.pubkey_base58());
        let mut rejected = if wrong_buyer {
            allowed_job.clone()
        } else {
            ephemeral_job()
        };
        if wrong_buyer {
            rejected.buyer = AgentId::new("other@local", [6; 32]);
            rejected.input.pop();
            let relay = PhalaRelayConfig {
                url: "https://relay.example.com".into(),
                session_id: rejected.job_id.to_string(),
            };
            let (_, spec) = phala_ephemeral_workload(
                &rejected,
                &operator(),
                &config.wrapper_template,
                &config.image_digest,
                &relay,
            )
            .unwrap();
            rejected
                .input
                .push(phala_ephemeral_lease_input(spec).unwrap());
        }
        let backend = PhalaSessionBackend::open_journal(
            provider.clone(),
            config,
            directory.path(),
            Arc::new(Closed(AtomicBool::new(false))),
            Arc::new(Evidence),
        )
        .unwrap();
        assert!(backend.allocate_ephemeral(&rejected).await.is_err());
        assert_eq!(provider.provision_count.load(Ordering::SeqCst), 0);
        assert_eq!(provider.create_count.load(Ordering::SeqCst), 0);
        assert_eq!(provider.delete_count.load(Ordering::SeqCst), 0);
        assert!(backend.journal.lock().snapshot.rows.is_empty());
        backend.allocate_ephemeral(&allowed_job).await.unwrap();
        let row = provider.durable(allowed_job.job_id);
        assert_eq!(row.plan.allowance.job_id, Some(allowed_job.job_id));
        assert_eq!(
            row.plan.allowance.buyer_pubkey_b58.as_deref(),
            Some(allowed_job.buyer.pubkey_base58().as_str())
        );
        assert_eq!(provider.provision_count.load(Ordering::SeqCst), 1);
        assert_eq!(provider.create_count.load(Ordering::SeqCst), 1);
    }
}
