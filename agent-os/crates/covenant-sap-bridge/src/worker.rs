//! Subprocess transport to the TypeScript bridge worker.
//!
//! The worker (`@covenant/sap-bridge`, bin `covenant-sap-worker`) owns
//! the SAP SDK, the funding key, and all transaction building. This
//! module spawns it once per operation, writes a JSON payload to its
//! stdin, and parses the single JSON envelope it writes to stdout:
//!
//! ```json
//! { "ok": true,  "data":  <result> }
//! { "ok": false, "error": "<message>", "name": "<ErrorName>" }
//! ```
//!
//! The worker resolves its own config (cluster, RPC, program id) and
//! its signer (`COVENANT_SAP_KEYPAIR`) from the environment, which it
//! inherits from this process — so the Rust [`Config`] and the worker
//! stay consistent as long as both read the same `COVENANT_SAP_*`
//! variables.

use std::process::Stdio;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::config::Config;
use crate::{BridgeError, Result};

#[derive(serde::Deserialize)]
struct Envelope {
    ok: bool,
    #[serde(default)]
    data: serde_json::Value,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// Run one worker command. `payload` is serialized to JSON and piped to
/// the worker's stdin; the worker's `data` field is deserialized into
/// `T`. A `null` `data` deserializes cleanly into `Option<_>`, which is
/// how lookups signal "not found".
pub(crate) async fn invoke<P, T>(config: &Config, command: &str, payload: &P) -> Result<T>
where
    P: Serialize,
    T: DeserializeOwned,
{
    let mut parts = config.worker_command.iter();
    let program = parts
        .next()
        .ok_or_else(|| BridgeError::Invalid("worker command is empty".into()))?;

    let mut cmd = Command::new(program);
    cmd.args(parts)
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A timeout fires by dropping the wait_with_output future, which drops
        // the Child. Without kill_on_drop a dropped Child leaves the worker
        // running detached — defeating the timeout entirely. With it, the
        // subprocess is reaped automatically.
        .kill_on_drop(true);

    let body = serde_json::to_vec(payload).map_err(|e| BridgeError::Invalid(e.to_string()))?;

    let mut child = cmd
        .spawn()
        .map_err(|e| BridgeError::Worker(format!("spawn {program}: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&body)
            .await
            .map_err(|e| BridgeError::Worker(format!("write stdin: {e}")))?;
        // Drop stdin so the worker's stdin stream ends and it can run.
        stdin
            .shutdown()
            .await
            .map_err(|e| BridgeError::Worker(format!("close stdin: {e}")))?;
    }

    let output = match tokio::time::timeout(config.worker_timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(BridgeError::Worker(format!("wait: {e}"))),
        Err(_) => {
            return Err(BridgeError::Timeout {
                secs: config.worker_timeout.as_secs(),
            });
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The worker may emit diagnostics before the result; the envelope
    // is always the last non-empty line.
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(str::trim)
        .unwrap_or("");

    if line.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BridgeError::Worker(format!(
            "worker produced no output (exit {}, stderr: {})",
            output.status,
            stderr.trim()
        )));
    }

    let env: Envelope =
        serde_json::from_str(line).map_err(|e| BridgeError::Decode(format!("{e}: {line}")))?;

    if env.ok {
        serde_json::from_value(env.data).map_err(|e| BridgeError::Decode(e.to_string()))
    } else {
        let message = env
            .error
            .unwrap_or_else(|| "worker reported an error".into());
        // Preserve a meaningful upstream error name so callers can branch
        // on failure class. A bare "Error" (JS's default Error.name) or an
        // empty name carries no signal, so those collapse to Rpc as before.
        match env.name {
            Some(name) if !name.is_empty() && name != "Error" => {
                Err(BridgeError::Upstream { name, message })
            }
            _ => Err(BridgeError::Rpc(message)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Cluster;

    #[tokio::test]
    async fn invoke_rejects_empty_worker_command() {
        // worker_command supplies the program for Command::new; an empty
        // vec has nothing to spawn. invoke must reject it as a local
        // BridgeError::Invalid (documented as validation before any RPC)
        // rather than reach the OS spawn. Config::from_env filters empty
        // commands, so a directly-constructed Config is the only way to
        // hit this arm, and worker.rs otherwise has no inline test for it.
        let mut config = Config::disabled(Cluster::Devnet);
        config.worker_command.clear();
        let err = invoke::<serde_json::Value, serde_json::Value>(
            &config,
            "lookup",
            &serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, BridgeError::Invalid(m) if m.contains("worker command is empty")),
            "empty worker_command must surface as a pre-spawn Invalid error: {err:?}"
        );
    }
}
