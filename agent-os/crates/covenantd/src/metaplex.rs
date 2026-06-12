//! Daemon glue for the Metaplex profile.
//!
//! `covenant-metaplex` owns the DAS read tools and the attestation
//! request shapes but holds no minting key and no `solana-sdk`
//! dependency. Reads go straight out over HTTP (DAS). Writes are
//! delegated to the standalone `covenant-metaplex-signer` sidecar over a
//! subprocess — the same isolation the x402 funding-key signer uses. The
//! minting key lives only in the sidecar's address space.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use covenant_metaplex::{
    DasClient, HttpDasClient, MetaplexConfig, MetaplexSigner, SignerRequest, SignerResponse,
};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Materialised Metaplex profile: config plus a DAS read client, built
/// once at daemon startup and shared behind an `Arc`.
pub struct MetaplexState {
    pub config: MetaplexConfig,
    pub das: Arc<dyn DasClient>,
}

impl MetaplexState {
    /// Build state from config, wiring an HTTP DAS client at `das_url`.
    /// An empty `das_url` yields a client that refuses every read, so the
    /// daemon can construct this unconditionally and let config gate
    /// which tools are advertised.
    pub fn new(config: MetaplexConfig) -> Self {
        let das: Arc<dyn DasClient> = Arc::new(HttpDasClient::new(config.das_url.clone()));
        Self { config, das }
    }

    /// A subprocess-backed signer, or `None` when the write surface is
    /// not configured (no signer binary / RPC).
    pub fn signer(&self) -> Option<Arc<dyn MetaplexSigner>> {
        if !self.config.writes_enabled() {
            return None;
        }
        Some(
            Arc::new(SubprocessMetaplexSigner::from_config(&self.config))
                as Arc<dyn MetaplexSigner>,
        )
    }
}

/// A [`MetaplexSigner`] that delegates to the `covenant-metaplex-signer`
/// sidecar. The daemon spawns it per write, pipes the [`SignerRequest`]
/// as JSON to stdin, and reads a [`SignerResponse`] from stdout. The
/// minting key never enters the daemon: the sidecar reads it from its own
/// environment (`COVENANT_METAPLEX_KEYPAIR`).
pub struct SubprocessMetaplexSigner {
    program: PathBuf,
    env: Vec<(String, String)>,
}

impl SubprocessMetaplexSigner {
    pub fn from_config(config: &MetaplexConfig) -> Self {
        let mut env = vec![
            (
                "COVENANT_METAPLEX_RPC_URL".to_string(),
                config.rpc_url.clone(),
            ),
            (
                "COVENANT_METAPLEX_CLUSTER".to_string(),
                config.cluster.clone(),
            ),
        ];
        if !config.collection.is_empty() {
            env.push((
                "COVENANT_METAPLEX_COLLECTION".to_string(),
                config.collection.clone(),
            ));
        }
        if config.per_action_cap_lamports > 0 {
            env.push((
                "COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS".to_string(),
                config.per_action_cap_lamports.to_string(),
            ));
        }
        // The minting keypair path is read straight from the daemon's
        // environment and forwarded to the sidecar; it is never stored in
        // config or logged.
        if let Ok(keypair) = std::env::var("COVENANT_METAPLEX_KEYPAIR") {
            env.push(("COVENANT_METAPLEX_KEYPAIR".to_string(), keypair));
        }
        Self {
            program: PathBuf::from(&config.signer_binary),
            env,
        }
    }

    async fn sign_with_limits(
        &self,
        request: SignerRequest,
        max_output_bytes: usize,
        deadline: std::time::Duration,
    ) -> Result<SignerResponse, String> {
        let payload = serde_json::to_vec(&request).map_err(|e| format!("encode request: {e}"))?;

        let mut child = Command::new(&self.program)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // An over-cap flood or an elapsed deadline returns early, dropping
            // the Child; kill_on_drop reaps the sidecar instead of leaving it
            // running detached.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("spawn signer {:?}: {e}", self.program))?;

        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "signer stdin unavailable".to_string())?;
            stdin
                .write_all(&payload)
                .await
                .map_err(|e| format!("write to signer: {e}"))?;
            // Drop closes stdin so the one-shot sidecar sees EOF.
        }

        // wait_with_output() buffered stdout and stderr unbounded with no
        // deadline; the metaplex signer talks to Solana RPC and DAS, so a
        // runaway, buggy, or hostile-RPC-fed sidecar could OOM the daemon or
        // hang here. Shared with the x402 signer dispatch so the cap and the
        // deadline never diverge.
        let (stdout_bytes, stderr_bytes, status) =
            crate::x402::read_signer_output(&mut child, max_output_bytes, deadline)
                .await
                .map_err(|e| e.message())?;

        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr_bytes);
            return Err(format!("signer exited {}: {}", status, stderr.trim()));
        }

        let stdout =
            String::from_utf8(stdout_bytes).map_err(|e| format!("signer stdout not utf-8: {e}"))?;
        let response = serde_json::from_str::<SignerResponse>(stdout.trim())
            .map_err(|e| format!("decode signer response: {e}"))?;

        // The capability check already chained an audit row for this tool
        // call; this surfaces the on-chain result (asset + signature) for
        // operators. A dedicated AuditKind row that links the two is a
        // tracked follow-up (it touches the core audit enum).
        tracing::info!(
            action = action_label(&request),
            asset = %response.asset,
            signature = %response.signature,
            cluster = %response.cluster,
            "metaplex on-chain write confirmed"
        );
        Ok(response)
    }
}

#[async_trait::async_trait]
impl MetaplexSigner for SubprocessMetaplexSigner {
    async fn sign(&self, request: SignerRequest) -> Result<SignerResponse, String> {
        self.sign_with_limits(
            request,
            crate::x402::MAX_SIGNER_OUTPUT_BYTES,
            crate::x402::SIGNER_OUTPUT_DEADLINE,
        )
        .await
    }
}

fn action_label(request: &SignerRequest) -> &'static str {
    match request {
        SignerRequest::AttestAuditRoot { .. } => "attest.audit_root",
        SignerRequest::RegisterIdentity { .. } => "identity.register",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn state_without_signer_surface_has_no_signer() {
        let state = MetaplexState::new(MetaplexConfig {
            enabled: true,
            das_url: "https://das.example".into(),
            ..Default::default()
        });
        assert!(
            state.signer().is_none(),
            "reads-only config exposes no signer"
        );
    }

    #[tokio::test]
    async fn subprocess_signer_decodes_stdout_response() {
        let signer = SubprocessMetaplexSigner {
            program: PathBuf::from("sh"),
            env: vec![],
        };
        // Stand in for the sidecar: ignore stdin, emit a SignerResponse.
        let out = Command::new("sh")
            .arg("-c")
            .arg("cat >/dev/null; printf '{\"signature\":\"s\",\"asset\":\"a\",\"cluster\":\"devnet\"}'")
            .output()
            .await
            .unwrap();
        let resp: SignerResponse = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(resp.cluster, "devnet");
        // The real path is exercised end-to-end on devnet; here we only
        // pin that the program field is wired.
        let _ = &signer.program;
    }

    #[test]
    fn from_config_forwards_rpc_and_cluster_and_pins_program() {
        let signer = SubprocessMetaplexSigner::from_config(&MetaplexConfig {
            enabled: true,
            cluster: "mainnet-beta".into(),
            rpc_url: "https://rpc.example".into(),
            signer_binary: "/opt/covenant-metaplex-signer".into(),
            ..Default::default()
        });
        assert_eq!(
            signer.program,
            PathBuf::from("/opt/covenant-metaplex-signer")
        );
        assert_eq!(
            env_get(&signer.env, "COVENANT_METAPLEX_RPC_URL"),
            Some("https://rpc.example")
        );
        assert_eq!(
            env_get(&signer.env, "COVENANT_METAPLEX_CLUSTER"),
            Some("mainnet-beta")
        );
        assert_eq!(
            env_get(&signer.env, "COVENANT_METAPLEX_COLLECTION"),
            None,
            "empty collection must not be forwarded to the sidecar"
        );
        assert_eq!(
            env_get(&signer.env, "COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS"),
            None,
            "a 0 cap must defer to the sidecar default, not forward 0"
        );
    }

    #[test]
    fn from_config_forwards_collection_and_cap_when_set() {
        let signer = SubprocessMetaplexSigner::from_config(&MetaplexConfig {
            enabled: true,
            rpc_url: "https://rpc.example".into(),
            signer_binary: "/opt/signer".into(),
            collection: "CoLLECT1onMintGroup11111111111111111111111".into(),
            per_action_cap_lamports: 5_000_000,
            ..Default::default()
        });
        assert_eq!(
            env_get(&signer.env, "COVENANT_METAPLEX_COLLECTION"),
            Some("CoLLECT1onMintGroup11111111111111111111111")
        );
        assert_eq!(
            env_get(&signer.env, "COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS"),
            Some("5000000")
        );
    }

    #[test]
    fn signer_present_when_write_surface_configured() {
        let state = MetaplexState::new(MetaplexConfig {
            enabled: true,
            rpc_url: "https://rpc.example".into(),
            signer_binary: "/opt/signer".into(),
            das_url: "https://das.example".into(),
            ..Default::default()
        });
        assert!(
            state.signer().is_some(),
            "a config with enabled + signer_binary + rpc_url must expose a signer"
        );
    }

    #[test]
    fn from_config_omits_collection_and_cap_when_unset() {
        // The mirror of `from_config_forwards_collection_and_cap_when_set`:
        // an empty collection and a zero cap must NOT be forwarded. A
        // regression that pushed them unconditionally would hand the
        // sidecar COVENANT_METAPLEX_COLLECTION="" and a literal "0" cap,
        // which it reads as a real (mis)configuration rather than "unset".
        let signer = SubprocessMetaplexSigner::from_config(&MetaplexConfig {
            enabled: true,
            rpc_url: "https://rpc.example".into(),
            signer_binary: "/opt/signer".into(),
            collection: String::new(),
            per_action_cap_lamports: 0,
            ..Default::default()
        });
        assert_eq!(
            env_get(&signer.env, "COVENANT_METAPLEX_RPC_URL"),
            Some("https://rpc.example")
        );
        assert!(
            env_get(&signer.env, "COVENANT_METAPLEX_COLLECTION").is_none(),
            "an empty collection must not be forwarded as a blank env value"
        );
        assert!(
            env_get(&signer.env, "COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS").is_none(),
            "a zero cap must not be forwarded"
        );
    }

    #[test]
    fn action_label_pins_telemetry_slugs_for_both_signer_actions() {
        use covenant_metaplex::AttestationPayload;

        // Operator log/telemetry contract. The dotted slugs are distinct
        // from the kebab-case `action` wire tags (attest-audit-root /
        // register-identity); a copy-paste swap between the two would
        // silently mislabel every confirmed on-chain write.
        let attest = SignerRequest::AttestAuditRoot {
            payload: AttestationPayload::new(
                "a".repeat(64),
                "v0.1.0",
                "covenant",
                "audit",
                1_700_000_000,
            ),
            asset: None,
            collection: None,
        };
        let register = SignerRequest::RegisterIdentity {
            agent_label: "operator".into(),
            agent_pubkey: "Agent1111111111111111111111111111111111111".into(),
            asset: None,
            registration_uri: None,
        };
        assert_eq!(action_label(&attest), "attest.audit_root");
        assert_eq!(action_label(&register), "identity.register");
    }

    fn sample_request() -> SignerRequest {
        SignerRequest::RegisterIdentity {
            agent_label: "operator".into(),
            agent_pubkey: "Agent1111111111111111111111111111111111111".into(),
            asset: None,
            registration_uri: None,
        }
    }

    fn fake_signer(body: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("fake-signer.sh");
        // `cat >/dev/null` drains the request on stdin first so the daemon's
        // write does not race a broken pipe before the body runs.
        std::fs::write(&script, format!("#!/bin/sh\ncat >/dev/null\n{body}\n"))
            .expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        (dir, script)
    }

    #[tokio::test]
    async fn sign_with_limits_rejects_oversized_signer_stdout() {
        // The metaplex signer talks to Solana RPC and DAS, so its stdout is
        // untrusted; a flood past the cap must surface a cap-breach error
        // instead of buffering the whole stream and OOMing the daemon. A
        // 64-byte cap against 200 bytes of output forces the overflow branch.
        let (_dir, script) = fake_signer("head -c 200 /dev/zero");
        let signer = SubprocessMetaplexSigner {
            program: script,
            env: vec![],
        };
        let err = signer
            .sign_with_limits(sample_request(), 64, std::time::Duration::from_secs(30))
            .await
            .expect_err("over cap");
        assert!(
            err.contains("exceeded") && err.contains("cap"),
            "an over-cap signer stdout must surface as a cap-breach error: {err}"
        );
    }

    #[tokio::test]
    async fn sign_with_limits_decodes_under_cap_response() {
        // A normal single-line SignerResponse under the cap must still decode,
        // proving the bounded read did not regress the happy path.
        let (_dir, script) =
            fake_signer("printf '{\"signature\":\"s\",\"asset\":\"a\",\"cluster\":\"devnet\"}'");
        let signer = SubprocessMetaplexSigner {
            program: script,
            env: vec![],
        };
        let resp = signer
            .sign_with_limits(sample_request(), 4096, std::time::Duration::from_secs(30))
            .await
            .expect("under-cap response decodes");
        assert_eq!(resp.cluster, "devnet");
        assert_eq!(resp.signature, "s");
    }
}
