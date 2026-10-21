//! Signed operator control of the bounded outbound relay. No buyer credential
//! is put in a job, guest manifest, access grant or local provider journal.
use super::*;
use uuid::Uuid;

const MAX_RELAY_BYTES: usize = 768 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayResponse {
    version: u8,
    result: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayMetadata {
    schema_version: u8,
    job_id: Uuid,
    app_id: String,
    #[serde(default)]
    instance_id: Option<String>,
    relay_endpoint: String,
    expires_at_ms: u64,
}

fn hex_id(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl PhalaEvidenceClient {
    pub(super) fn ephemeral_spec(
        &self,
        job: &JobEnvelopePayload,
    ) -> Result<phala::PhalaEphemeralLeaseSpec, ExecutorError> {
        let spec = phala::parse_phala_ephemeral_lease_spec(&job.input)
            .map_err(failed)?
            .ok_or_else(|| failed("explicit ephemeral lease required"))?;
        spec.validate_for_job(job).map_err(failed)?;
        if spec.operator_pubkey_b58 != self.identity.agent_id().pubkey_base58()
            || self.relay_origin.as_deref() != Some(spec.relay.url.as_str())
        {
            return Err(failed(
                "signed relay origin or operator differs from configured native relay",
            ));
        }
        Ok(spec)
    }

    async fn relay_request(
        &self,
        job: &JobEnvelopePayload,
        operation: &str,
        arguments: Value,
    ) -> Result<Value, ExecutorError> {
        let spec = self.ephemeral_spec(job)?;
        let bytes = serde_json::to_vec(&json!({"version":1,"session_id":spec.relay.session_id,"operation":operation,"arguments":arguments})).map_err(failed)?;
        if bytes.len() > MAX_RELAY_BYTES {
            return Err(failed("relay request exceeds bound"));
        }
        let now = now_ms();
        if now >= spec.lease_expires_at_ms {
            return Err(failed("relay lease expired"));
        }
        let path = "/v1/relay";
        let signature =
            binding::sign_request(&self.identity, "POST", path, &bytes, now).map_err(failed)?;
        // One attempt. A failed submission never causes a replacement request
        // ID or challenge; the owning lease is closed and reconciled.
        let response = self
            .http
            .post(format!("{}{path}", spec.relay.url))
            .header(
                "x-covenant-hardware-actor",
                self.identity.agent_id().pubkey_base58(),
            )
            .header("x-covenant-hardware-signed-at", now.to_string())
            .header("x-covenant-hardware-signature", signature)
            .header("Content-Type", "application/json")
            .header("Accept-Encoding", "identity")
            .body(bytes)
            .send()
            .await
            .map_err(|_| failed("relay outcome unknown; original operation retained"))?;
        if response
            .headers()
            .get("Content-Encoding")
            .is_some_and(|v| v != "identity")
        {
            return Err(failed("encoded relay response refused"));
        }
        let (status, bytes) = body(response, MAX_RELAY_BYTES).await?;
        if status != 200 {
            return Err(failed(format!(
                "relay refused operator control (HTTP {status})"
            )));
        }
        let response: RelayResponse =
            serde_json::from_slice(&bytes).map_err(|_| failed("invalid relay response"))?;
        if response.version != 1 {
            return Err(failed("unsupported relay version"));
        }
        Ok(response.result)
    }

    pub(super) async fn open_relay_session(
        &self,
        job: &JobEnvelopePayload,
        app_id: &str,
    ) -> Result<(), ExecutorError> {
        if !hex_id(app_id) {
            return Err(failed("invalid provisioned app ID"));
        }
        let result = self
            .relay_request(job, "operator_open", json!({"app_id":app_id}))
            .await?;
        // Exact acknowledgement is checked against the coordinator contract;
        // no provider-returned token participates in relay authorization.
        if result != json!({"app_id":app_id}) {
            return Err(failed("relay changed provisioned app identity"));
        }
        self.relay_metadata(job, app_id, None).await?;
        Ok(())
    }

    async fn relay_metadata(
        &self,
        job: &JobEnvelopePayload,
        app_id: &str,
        expected_instance: Option<&str>,
    ) -> Result<Option<String>, ExecutorError> {
        let spec = self.ephemeral_spec(job)?;
        let path = format!("/federation/hardware/{}/relay", job.job_id);
        let now = now_ms();
        let signature =
            binding::sign_request(&self.identity, "GET", &path, &[], now).map_err(failed)?;
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .header(
                "x-covenant-hardware-actor",
                self.identity.agent_id().pubkey_base58(),
            )
            .header("x-covenant-hardware-signed-at", now.to_string())
            .header("x-covenant-hardware-signature", signature)
            .send()
            .await
            .map_err(|_| failed("relay metadata request failed"))?;
        let (status, bytes) = body(response, 8192).await?;
        if status != 200 {
            return Err(failed(format!(
                "relay session unavailable (HTTP {status}); no replacement permitted"
            )));
        }
        let value: RelayMetadata =
            serde_json::from_slice(&bytes).map_err(|_| failed("invalid relay metadata"))?;
        if value.schema_version != 1
            || value.job_id != job.job_id
            || value.app_id != app_id
            || value.relay_endpoint != spec.relay.url
            || value.expires_at_ms != spec.lease_expires_at_ms
            || value.instance_id.as_deref().is_some_and(|id| !hex_id(id))
            || expected_instance.is_some_and(|id| value.instance_id.as_deref() != Some(id))
        {
            return Err(failed(
                "relay metadata changed original lease or verified identity",
            ));
        }
        Ok(value.instance_id)
    }

    async fn relay_control(
        &self,
        job: &JobEnvelopePayload,
        kind: &str,
        payload: Value,
        request_id: Uuid,
        deadline_ms: u64,
    ) -> Result<Value, ExecutorError> {
        let id = request_id.to_string();
        let ack = self
            .relay_request(
                job,
                "buyer_submit",
                json!({"request_id":id,"kind":kind,"payload":payload}),
            )
            .await?;
        if ack != json!({"request_id":id}) {
            return Err(failed("relay control acknowledgement mismatch"));
        }
        loop {
            if now_ms() >= deadline_ms {
                return Err(failed("relay control outcome unknown at original deadline"));
            }
            let result = self
                .relay_request(job, "buyer_result", json!({"request_id":id}))
                .await?;
            let object = result
                .as_object()
                .ok_or_else(|| failed("invalid relay control result"))?;
            if object.len() != 2
                || object.get("request_id") != Some(&json!(id))
                || !object.contains_key("result")
            {
                return Err(failed("relay control result identity mismatch"));
            }
            if result["result"].is_null() {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            let value = result["result"]
                .as_object()
                .ok_or_else(|| failed("invalid guest control response"))?;
            if value.len() != 2
                || value.get("ok") != Some(&Value::Bool(true))
                || !value.contains_key("value")
            {
                return Err(failed("guest refused relay control"));
            }
            return Ok(value["value"].clone());
        }
    }

    fn run_directory(&self, job: Uuid, stage: &str) -> Result<PathBuf, ExecutorError> {
        let parent = self.evidence_root.join(job.to_string());
        private_directory(&parent)?;
        let directory = parent.join(stage);
        std::fs::create_dir(&directory).map_err(|_| {
            failed("ephemeral evidence stage already exists; reconcile without a new challenge")
        })?;
        private_directory(&directory)?;
        Ok(directory)
    }

    async fn relay_quote(
        &self,
        job: &JobEnvelopePayload,
        challenge: &HardwareChallenge,
        directory: &Path,
        app_id: &str,
        expected_instance: Option<&str>,
    ) -> Result<(HardwareEvidence, HardwareVerdict, String), ExecutorError> {
        save(
            &directory.join("quote-request.json"),
            &serde_json::to_vec(challenge).map_err(failed)?,
        )?;
        let raw = self
            .relay_control(
                job,
                "quote",
                serde_json::to_value(challenge).map_err(failed)?,
                challenge.challenge_id,
                challenge.expires_at_ms,
            )
            .await?;
        let bytes = serde_json::to_vec(&raw).map_err(failed)?;
        save(&directory.join("collector-response.json"), &bytes)?;
        let collected: Collected = serde_json::from_slice(&bytes).map_err(failed)?;
        let (quote, events) = prepare(&collected, challenge)?;
        let transport = IntelHttp {
            client: self.http.clone(),
            requests: Arc::new(AtomicUsize::new(0)),
        };
        let collateral = tokio::time::timeout(
            Duration::from_secs(70),
            CollateralClient::<DefaultConfig, _>::new(transport, INTEL_PCS_URL).fetch(&quote),
        )
        .await
        .map_err(|_| failed("Intel collateral fetch timed out"))?
        .map_err(|_| failed("Intel collateral fetch failed"))?;
        let collateral_json = serde_json::to_string(&collateral).map_err(failed)?;
        if collateral_json.len() > 128 * 1024 {
            return Err(failed("collateral exceeds bound"));
        }
        save(
            &directory.join("collateral.json"),
            collateral_json.as_bytes(),
        )?;
        let evidence = HardwareEvidence {
            challenge: challenge.clone(),
            guest: GuestEvidence::Tdx {
                quote_base64: STANDARD.encode(quote),
                collateral_json,
                events,
            },
            gpu_cc: None,
        };
        let exact = serde_json::to_vec(&evidence).map_err(failed)?;
        if exact.len() > MAX_EVIDENCE_BYTES {
            return Err(failed("evidence exceeds bound"));
        }
        let owned = evidence.clone();
        let verdict = tokio::task::spawn_blocking(move || {
            binding::verify_evidence_for_profile(
                &owned,
                now_ms(),
                HardwareProfile::PhalaEphemeralDstack061,
            )
        })
        .await
        .map_err(failed)?
        .map_err(failed)?;
        // Only inspect identities after the complete cryptographic event-log
        // verification above. Info and Cloud instance fields cannot replace it.
        let instance = verified_boot_identity(&evidence, &verdict, app_id, expected_instance)?;
        if collected.identity["guest_info"]["app_id"].as_str() != Some(app_id)
            || collected.identity["guest_info"]["instance_id"].as_str() != Some(instance.as_str())
        {
            return Err(failed("untrusted Info identity differs from verified boot"));
        }
        save(&directory.join("evidence.json"), &exact)?;
        save(
            &directory.join("local-verdict.json"),
            &serde_json::to_vec(&verdict).map_err(failed)?,
        )?;
        Ok((evidence, verdict, instance))
    }

    pub(super) async fn verify_ephemeral_readiness(
        &self,
        job: &JobEnvelopePayload,
        app_id: &str,
        previous: Option<&VerifiedPhalaAccess>,
    ) -> Result<VerifiedPhalaAccess, ExecutorError> {
        let spec = self.ephemeral_spec(job)?;
        if !hex_id(app_id) {
            return Err(failed("invalid independent Cloud app pin"));
        }
        let directory = self.run_directory(job.job_id, "admission")?;
        let registered_instance = self
            .relay_metadata(job, app_id, previous.map(|pin| pin.instance_id.as_str()))
            .await?;
        let request_id = Uuid::new_v4();
        save(
            &directory.join("identity-request.json"),
            &serde_json::to_vec(&json!({"request_id":request_id})).map_err(failed)?,
        )?;
        let identity = self
            .relay_control(
                job,
                "identity",
                json!({}),
                request_id,
                (now_ms() + 60_000).min(spec.lease_expires_at_ms),
            )
            .await?;
        save(
            &directory.join("unverified-identity.json"),
            &serde_json::to_vec(&identity).map_err(failed)?,
        )?;
        let access = unverified_access(&identity, &spec)?;
        if previous.is_some_and(|pin| pin.app_id != app_id || pin.access != access) {
            return Err(failed(
                "ephemeral SSH identity changed; replacement forbidden",
            ));
        }
        let scope = ephemeral_scope(job, &spec, &access)?;
        let challenge = HardwareChallenge::new(scope, now_ms()).map_err(failed)?;
        let (_, _, instance_id) = self
            .relay_quote(
                job,
                &challenge,
                &directory,
                app_id,
                previous
                    .map(|pin| pin.instance_id.as_str())
                    .or(registered_instance.as_deref()),
            )
            .await?;
        let pin = VerifiedPhalaAccess {
            access,
            app_id: app_id.into(),
            instance_id,
        };
        // Record the full verified identity before the remote immutable pin.
        save(
            &directory.join("verified-access.json"),
            &serde_json::to_vec(&pin).map_err(failed)?,
        )?;
        let ack = self
            .relay_request(
                job,
                "buyer_pin_instance",
                json!({"instance_id":pin.instance_id}),
            )
            .await?;
        if ack != json!({"instance_id":pin.instance_id}) {
            return Err(failed("relay instance pin acknowledgement mismatch"));
        }
        self.relay_metadata(job, app_id, Some(&pin.instance_id))
            .await?;
        Ok(pin)
    }

    pub(super) async fn submit_ephemeral_evidence(
        &self,
        job: &JobEnvelopePayload,
        access: &LeaseAccess,
        pin: &VerifiedPhalaAccess,
    ) -> Result<(), ExecutorError> {
        let spec = self.ephemeral_spec(job)?;
        if access.job_id != job.job_id || access.ssh.as_ref() != Some(&pin.access) {
            return Err(failed("ephemeral access differs from verified pin"));
        }
        self.relay_metadata(job, &pin.app_id, Some(&pin.instance_id))
            .await?;
        let scope = ephemeral_scope(job, &spec, &pin.access)?;
        let directory = self.run_directory(job.job_id, "coordinator")?;
        let path = format!("/federation/hardware/{}/challenge", job.job_id);
        let mut challenge = None;
        for _ in 0..20 {
            let (status, bytes) = self
                .coordinator(&path, br#"{"host_key_index":0}"#.to_vec())
                .await?;
            if status == 200 {
                let value: HardwareChallenge = serde_json::from_slice(&bytes).map_err(failed)?;
                value.validate(now_ms()).map_err(failed)?;
                if value.scope != scope {
                    return Err(failed("coordinator changed ephemeral challenge scope"));
                }
                save(&directory.join("challenge.json"), &bytes)?;
                challenge = Some(value);
                break;
            }
            let error: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            if status != 403
                || error["error"]
                    .as_str()
                    .is_none_or(|e| !e.ends_with("structured SSH identity is required"))
            {
                return Err(failed(format!(
                    "ephemeral challenge refused (HTTP {status})"
                )));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let challenge =
            challenge.ok_or_else(|| failed("coordinator did not record ephemeral access"))?;
        let (evidence, _, _) = self
            .relay_quote(
                job,
                &challenge,
                &directory,
                &pin.app_id,
                Some(&pin.instance_id),
            )
            .await?;
        let exact = serde_json::to_vec(&evidence).map_err(failed)?;
        let (status, bytes) = self
            .coordinator(
                &format!("/federation/hardware/{}/evidence", job.job_id),
                exact.clone(),
            )
            .await?;
        save(&directory.join("submission-response.json"), &bytes)?;
        if status != 200 {
            return Err(failed(format!(
                "ephemeral evidence submission refused (HTTP {status})"
            )));
        }
        let accepted: EvidenceResponse = serde_json::from_slice(&bytes).map_err(failed)?;
        if serde_json::to_vec(&accepted.evidence).map_err(failed)? != exact {
            return Err(failed("coordinator returned different ephemeral evidence"));
        }
        binding::verify_saved_verdict_for_profile(
            &accepted.evidence,
            &accepted.verdict,
            &scope,
            now_ms(),
            HardwareProfile::PhalaEphemeralDstack061,
        )
        .map_err(failed)?;
        verified_boot_identity(
            &accepted.evidence,
            &accepted.verdict,
            &pin.app_id,
            Some(&pin.instance_id),
        )?;
        Ok(())
    }
}

fn unverified_access(
    identity: &Value,
    spec: &phala::PhalaEphemeralLeaseSpec,
) -> Result<SshAccess, ExecutorError> {
    let host = spec.relay.host().map_err(failed)?;
    if identity["host"].as_str() != Some(host)
        || identity["port"].as_u64() != Some(443)
        || identity["username"].as_str() != Some("covenant")
        || identity["transport"] != json!({"kind":"outbound_relay"})
        || identity["guest_info"]["compose_hash"].as_str() != Some(spec.workload_sha256.as_str())
    {
        return Err(failed(
            "relay identity differs from original signed workload",
        ));
    }
    let key: SshHostKey =
        serde_json::from_value(identity["guest_channel_key"].clone()).map_err(failed)?;
    let access = SshAccess {
        host: host.into(),
        port: 443,
        username: "covenant".into(),
        lease_generation: spec.lease_generation.clone(),
        observed_host_keys: vec![key],
        transport: Some(SshTransport::OutboundRelay),
    };
    access.validate().map_err(failed)?;
    if access.observed_host_keys[0].algorithm != "ssh-ed25519" {
        return Err(failed("ephemeral guest requires Ed25519 SSH key"));
    }
    Ok(access)
}

fn ephemeral_scope(
    job: &JobEnvelopePayload,
    spec: &phala::PhalaEphemeralLeaseSpec,
    access: &SshAccess,
) -> Result<HardwareScope, ExecutorError> {
    access.validate().map_err(failed)?;
    if access.host != spec.relay.host().map_err(failed)?
        || access.port != 443
        || access.username != "covenant"
        || access.transport != Some(SshTransport::OutboundRelay)
        || access.lease_generation != spec.lease_generation
        || access.observed_host_keys.len() != 1
    {
        return Err(failed("ephemeral access scope mismatch"));
    }
    Ok(HardwareScope {
        profile: HardwareProfile::PhalaEphemeralDstack061,
        job_id: job.job_id,
        operator_pubkey_b58: spec.operator_pubkey_b58.clone(),
        buyer_pubkey_b58: job.buyer.pubkey_base58(),
        lease_generation: spec.lease_generation.clone(),
        host: access.host.clone(),
        port: access.port,
        transport: access.transport,
        username: access.username.clone(),
        guest_channel_key: access.observed_host_keys[0].clone(),
        workload_sha256: spec.workload_sha256.clone(),
        lease_expires_at_ms: spec.lease_expires_at_ms,
    })
}

// Must be called only after full quote verification has authenticated events.
fn verified_boot_identity(
    evidence: &HardwareEvidence,
    verdict: &HardwareVerdict,
    app_id: &str,
    expected_instance: Option<&str>,
) -> Result<String, ExecutorError> {
    if !hex_id(app_id) || expected_instance.is_some_and(|v| !hex_id(v)) {
        return Err(failed("invalid independent boot pin"));
    }
    if evidence.challenge.scope.profile != HardwareProfile::PhalaEphemeralDstack061 {
        return Err(failed("ephemeral Phala requires explicit verified profile"));
    }
    let identity = verdict
        .phala_identity
        .as_ref()
        .ok_or_else(|| failed("verified ephemeral boot identity missing"))?;
    if identity.app_id != app_id
        || expected_instance.is_some_and(|v| v != identity.instance_id)
        || verdict.guest_identity != format!("tdx/{}", identity.instance_id)
    {
        return Err(failed(
            "verified boot differs from immutable provision or instance pin",
        ));
    }
    Ok(identity.instance_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> phala::PhalaEphemeralLeaseSpec {
        let id = Uuid::new_v4();
        phala::PhalaEphemeralLeaseSpec {
            application_key_recipient: None,
            schema_version: 1,
            job_id: id,
            operator_pubkey_b58: bs58::encode([1; 32]).into_string(),
            lease_generation: format!("phala-ephemeral-{id}-1"),
            lease_expires_at_ms: now_ms() + 60_000,
            workload_sha256: "12".repeat(32),
            relay: phala::PhalaRelayConfig {
                url: "https://relay.example.com".into(),
                session_id: id.to_string(),
            },
        }
    }
    fn identity(spec: &phala::PhalaEphemeralLeaseSpec) -> Value {
        let mut wire = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
        wire.extend_from_slice(&[8; 32]);
        json!({"host":"relay.example.com","port":443,"username":"covenant","transport":{"kind":"outbound_relay"},"guest_channel_key":{"algorithm":"ssh-ed25519","key_base64":STANDARD.encode(wire)},"guest_info":{"compose_hash":spec.workload_sha256}})
    }

    #[test]
    fn relay_observation_cannot_change_signed_route_workload_or_key_type() {
        let spec = spec();
        let good = identity(&spec);
        assert!(unverified_access(&good, &spec).is_ok());
        for (field, value) in [
            ("host", json!("attacker.example.com")),
            ("port", json!(22)),
            ("username", json!("root")),
            ("transport", json!({"kind":"tls"})),
            (
                "guest_channel_key",
                json!({"algorithm":"ssh-ed25519","key_base64":STANDARD.encode([1;32])}),
            ),
        ] {
            let mut changed = good.clone();
            changed[field] = value;
            assert!(unverified_access(&changed, &spec).is_err(), "{field}");
        }
        let mut changed = good;
        changed["guest_info"]["compose_hash"] = json!("00".repeat(32));
        assert!(unverified_access(&changed, &spec).is_err());
    }

    #[test]
    fn relay_configuration_and_admission_stage_are_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let identity = Arc::new(
            LocalIdentity::load_or_create(&directory.path().join("identity.json"), "test").unwrap(),
        );
        let client = PhalaEvidenceClient::new(
            "https://coordinator.example.com",
            identity,
            directory.path().join("evidence"),
        )
        .unwrap();
        assert!(!client.ephemeral_available());
        let client = client
            .with_ephemeral_relay_origin("https://relay.example.com".into())
            .unwrap();
        assert!(client.ephemeral_available());
        let id = Uuid::new_v4();
        let stage = client.run_directory(id, "admission").unwrap();
        save(&stage.join("quote-request.json"), b"original-request").unwrap();
        assert!(
            client.run_directory(id, "admission").is_err(),
            "cannot replace a consumed or ambiguous quote stage"
        );
        assert_eq!(
            std::fs::read(stage.join("quote-request.json")).unwrap(),
            b"original-request"
        );
        assert!(
            client.run_directory(id, "coordinator").is_ok(),
            "the separately authorized coordinator challenge has its own durable stage"
        );
        assert!(client
            .with_ephemeral_relay_origin("http://relay.example.com".into())
            .is_err());
    }
}
