//! Subprocess transport to the SAID bridge worker (`@covenant/said-bridge`).
//!
//! Mirrors the JSON envelope contract used by `covenant-sap-bridge`:
//!
//! ```json
//! { "ok": true,  "data":  <result> }
//! { "ok": false, "error": "<message>", "name": "<ErrorName>" }
//! ```
//!
//! The worker resolves its own cluster, RPC, program id, and signer from
//! the inherited environment. The SAID owner keypair lives at
//! `COVENANT_SAID_KEYPAIR`.

#![allow(dead_code)]

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
        match env.name {
            Some(name) if !name.is_empty() && name != "Error" => {
                Err(BridgeError::Upstream { name, message })
            }
            _ => Err(BridgeError::Rest(message)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Cluster;

    #[tokio::test]
    async fn invoke_rejects_empty_worker_command() {
        let mut config = Config::disabled(Cluster::Devnet);
        config.worker_command.clear();
        let err = invoke::<serde_json::Value, serde_json::Value>(
            &config,
            "status",
            &serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, BridgeError::Invalid(m) if m.contains("worker command is empty")),
            "empty worker_command must surface as Invalid: {err:?}"
        );
    }
}
