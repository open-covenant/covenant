//! Daemon glue for the SNS profile.
//!
//! `covenant-sns` owns `.sol` resolution (read) and the write-request shapes
//! but holds no key and no `solana-sdk`. Reads go straight out over HTTP to the
//! resolver. Writes are delegated to the `covenant-sns-signer` sidecar over a
//! subprocess (the same isolation the metaplex/x402 signers use): the parent
//! domain key lives only in the sidecar's address space.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use covenant_sns::{
    HttpSnsResolver, SignerRequest, SignerResponse, SnsConfig, SnsResolver, SnsSigner,
};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Materialised SNS profile: config plus a resolver client, shared behind an
/// `Arc`. An empty resolver URL yields a client that refuses every call, so
/// the daemon can build this unconditionally and let config gate the tools.
pub struct SnsState {
    pub config: SnsConfig,
    pub resolver: Arc<dyn SnsResolver>,
}

impl SnsState {
    pub fn new(config: SnsConfig) -> Self {
        let resolver: Arc<dyn SnsResolver> =
            Arc::new(HttpSnsResolver::new(config.resolver_url.clone()));
        Self { config, resolver }
    }

    /// A subprocess-backed signer, or `None` when the write surface is not
    /// configured (no signer binary / RPC).
    pub fn signer(&self) -> Option<Arc<dyn SnsSigner>> {
        if !self.config.writes_enabled() {
            return None;
        }
        Some(Arc::new(SubprocessSnsSigner::from_config(&self.config)) as Arc<dyn SnsSigner>)
    }
}

/// An [`SnsSigner`] that delegates to the `covenant-sns-signer` sidecar. The
/// daemon spawns it per write, pipes the [`SignerRequest`] as JSON to stdin,
/// and reads a [`SignerResponse`] from stdout. The parent-domain key never
/// enters the daemon: the sidecar reads it from its own environment
/// (`COVENANT_SNS_KEYPAIR`).
pub struct SubprocessSnsSigner {
    program: PathBuf,
    env: Vec<(String, String)>,
}

impl SubprocessSnsSigner {
    pub fn from_config(config: &SnsConfig) -> Self {
        let mut env = vec![("COVENANT_SNS_RPC_URL".to_string(), config.rpc_url.clone())];
        if !config.parent_domain.is_empty() {
            env.push((
                "COVENANT_SNS_PARENT_DOMAIN".to_string(),
                config.parent_domain.clone(),
            ));
        }
        // The parent-domain keypair path is read straight from the daemon's
        // environment and forwarded; it is never stored in config or logged.
        if let Ok(keypair) = std::env::var("COVENANT_SNS_KEYPAIR") {
            env.push(("COVENANT_SNS_KEYPAIR".to_string(), keypair));
        }
        Self {
            program: PathBuf::from(&config.signer_binary),
            env,
        }
    }
}

#[async_trait::async_trait]
impl SnsSigner for SubprocessSnsSigner {
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

        tracing::info!(
            domain = %response.domain,
            signature = %response.signature,
            cluster = %response.cluster,
            "sns on-chain write confirmed"
        );
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_config_has_no_signer() {
        let state = SnsState::new(SnsConfig {
            enabled: true,
            ..Default::default()
        });
        assert!(state.signer().is_none());
    }

    #[test]
    fn write_config_yields_a_signer() {
        let state = SnsState::new(SnsConfig {
            enabled: true,
            rpc_url: "https://api.devnet.solana.com".into(),
            signer_binary: "/bin/covenant-sns-signer".into(),
            parent_domain: "covenant.sol".into(),
            ..Default::default()
        });
        assert!(state.signer().is_some());
    }
}
