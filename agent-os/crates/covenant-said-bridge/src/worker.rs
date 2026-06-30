//! Subprocess transport to `@covenant/said-bridge`. JSON envelope:
//!
//! ```json
//! { "ok": true,  "data":  <result> }
//! { "ok": false, "error": "<message>", "name": "<ErrorName>" }
//! ```
//!
//! Worker config comes from the inherited env. Signer:
//! `COVENANT_SAID_KEYPAIR`.

#![allow(dead_code)]

use std::process::Stdio;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::config::Config;
use crate::{BridgeError, Result};

const MAX_WORKER_STDOUT_BYTES: usize = 1 << 20;
const MAX_WORKER_STDERR_BYTES: usize = 1 << 18;

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

    let mut child_stdout = child.stdout.take();
    let mut child_stderr = child.stderr.take();
    let read_streams = async {
        let stdout_fut = async {
            match child_stdout.as_mut() {
                Some(s) => {
                    let mut buf = Vec::new();
                    s.take(MAX_WORKER_STDOUT_BYTES as u64 + 1)
                        .read_to_end(&mut buf)
                        .await
                        .map_err(|e| BridgeError::Worker(format!("read stdout: {e}")))?;
                    if buf.len() > MAX_WORKER_STDOUT_BYTES {
                        return Err(BridgeError::Worker(format!(
                            "worker stdout exceeds {MAX_WORKER_STDOUT_BYTES} bytes"
                        )));
                    }
                    Ok(buf)
                }
                None => Ok(Vec::new()),
            }
        };
        let stderr_fut = async {
            match child_stderr.as_mut() {
                Some(s) => {
                    let mut buf = Vec::new();
                    s.take(MAX_WORKER_STDERR_BYTES as u64 + 1)
                        .read_to_end(&mut buf)
                        .await
                        .map_err(|e| BridgeError::Worker(format!("read stderr: {e}")))?;
                    if buf.len() > MAX_WORKER_STDERR_BYTES {
                        buf.truncate(MAX_WORKER_STDERR_BYTES);
                    }
                    Ok(buf)
                }
                None => Ok(Vec::new()),
            }
        };
        let (stdout, stderr) = tokio::join!(stdout_fut, stderr_fut);
        Ok::<_, BridgeError>((stdout?, stderr?))
    };

    let ((stdout_buf, stderr_buf), status) =
        match tokio::time::timeout(config.worker_timeout, async {
            let streams = read_streams.await?;
            let status = child
                .wait()
                .await
                .map_err(|e| BridgeError::Worker(format!("wait: {e}")))?;
            Ok::<_, BridgeError>((streams, status))
        })
        .await
        {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(BridgeError::Timeout {
                    secs: config.worker_timeout.as_secs(),
                });
            }
        };

    let stdout = String::from_utf8_lossy(&stdout_buf);
    let envelope = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .find_map(|l| serde_json::from_str::<Envelope>(l).ok().map(|e| (l, e)));

    let (line, env) = match envelope {
        Some(pair) => pair,
        None => {
            let stderr = String::from_utf8_lossy(&stderr_buf);
            return Err(BridgeError::Worker(format!(
                "worker produced no envelope (exit {status}, stderr: {})",
                stderr.trim()
            )));
        }
    };

    if env.ok {
        serde_json::from_value(env.data)
            .map_err(|e| BridgeError::Decode(format!("{e}: {line}")))
    } else {
        let message = env
            .error
            .unwrap_or_else(|| "worker reported an error".into());
        let name = env
            .name
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Worker".into());
        Err(BridgeError::Upstream { name, message })
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
