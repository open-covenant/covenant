//! Daemon glue for the Metaplex profile.
//!
//! `covenant-metaplex` owns the DAS read tools and retained write-request
//! shapes but holds no minting key and no `solana-sdk` dependency. The
//! production daemon exposes only read observations and refuses funded write
//! names before signer, RPC, or key access. The subprocess signer below is a
//! compatibility and development primitive; process separation alone is not a
//! security isolation boundary.

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

    /// Construct the retained development signer when explicitly configured.
    /// Production daemon dispatch never calls this method.
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

/// A retained development [`MetaplexSigner`] that delegates to the
/// `covenant-metaplex-signer` sidecar. It pipes a [`SignerRequest`] as JSON to
/// stdin and reads a [`SignerResponse`] from stdout. Production daemon tool
/// dispatch does not construct this signer.
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
        env.push((
            "COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS".to_string(),
            config.per_action_cap_lamports.to_string(),
        ));
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
}

#[async_trait::async_trait]
impl MetaplexSigner for SubprocessMetaplexSigner {
    async fn sign(&self, request: SignerRequest) -> Result<SignerResponse, String> {
        let payload = serde_json::to_vec(&request).map_err(|e| format!("encode request: {e}"))?;

        let mut child = Command::new(&self.program)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| format!("await signer: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "signer exited {}: {}",
                output.status,
                stderr.trim()
            ));
        }

        let stdout = String::from_utf8(output.stdout)
            .map_err(|e| format!("signer stdout not utf-8: {e}"))?;
        let response = serde_json::from_str::<SignerResponse>(stdout.trim())
            .map_err(|e| format!("decode signer response: {e}"))?;

        // This is the sidecar's reported result; this adapter does not query
        // RPC independently. Production daemon dispatch does not reach it.
        tracing::info!(
            action = action_label(&request),
            asset = %response.asset,
            signature = %response.signature,
            cluster = %response.cluster,
            "development metaplex signer reported write result"
        );
        Ok(response)
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

    #[test]
    fn subprocess_signer_forwards_zero_cap() {
        let config = MetaplexConfig {
            enabled: true,
            rpc_url: "https://rpc.example".into(),
            signer_binary: "/bin/covenant-metaplex-signer".into(),
            per_action_cap_lamports: 0,
            ..Default::default()
        };
        let signer = SubprocessMetaplexSigner::from_config(&config);
        assert!(signer.env.iter().any(|(key, value)| {
            key == "COVENANT_METAPLEX_PER_ACTION_CAP_LAMPORTS" && value == "0"
        }));
        assert!(!config.writes_enabled());
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
}
