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
//! The worker receives a minimal environment: canonical config resolved by
//! Rust plus an allowlist of runtime paths, the one signer path needed by the
//! requested command, and the stats path. It never inherits the daemon's
//! ambient environment.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Stdio;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

/// Maximum bytes the bridge buffers from one worker subprocess stream. The
/// worker is an external process whose stdout and stderr reflect SAP responses
/// and its own diagnostics, so an unbounded read lets a runaway, buggy, or
/// hostile worker exhaust the daemon worker's memory before `worker_timeout`
/// fires. The result envelope is a single JSON line, so 16 MiB sits far above
/// any legitimate payload while still capping a flood — the memory-axis
/// sibling of the wall-clock timeout.
const MAX_WORKER_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// Ambient values the worker genuinely needs and Rust cannot derive from
/// [`Config`]. Signer values are paths, not key bytes. Only the signer class
/// required by the selected command crosses the boundary; read-only commands
/// receive neither signer path. The stats path is resolved in Rust, so neither
/// `HOME` nor `COVENANT_HOME` crosses the process boundary. `env_clear`
/// prevents accidental environment disclosure; it is not filesystem isolation
/// from another process running as the same OS user.
const PAYER_KEY_ENV: &str = "COVENANT_SAP_KEYPAIR";
const VERIFIER_KEY_ENV: &str = "COVENANT_SAP_VERIFIER_KEYPAIR";

fn signer_env_for_command(command: &str) -> Option<&'static str> {
    match command {
        "publish-agent" | "update-agent" | "attest-root" => Some(PAYER_KEY_ENV),
        "attest-agent" => Some(VERIFIER_KEY_ENV),
        _ => None,
    }
}

fn worker_environment<I>(
    config: &Config,
    command: &str,
    ambient: I,
) -> Result<Vec<(OsString, OsString)>>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let ambient = ambient.into_iter().collect::<Vec<_>>();
    let stats_path = worker_stats_path(&ambient)?;
    let signer_env = signer_env_for_command(command).map(OsStr::new);
    let mut env = ambient
        .into_iter()
        .filter(|(key, _)| key == OsStr::new("PATH") || signer_env == Some(key.as_os_str()))
        .collect::<Vec<_>>();
    env.extend([
        (
            OsString::from("COVENANT_SOLANA_CLUSTER"),
            OsString::from(config.cluster.as_str()),
        ),
        (
            OsString::from("COVENANT_SAP_ENABLED"),
            OsString::from(if config.enabled { "true" } else { "false" }),
        ),
        (
            OsString::from("COVENANT_SAP_PROGRAM_ID"),
            OsString::from(&config.program_id),
        ),
        (
            OsString::from("COVENANT_SAP_RPC_URL"),
            OsString::from(&config.rpc_url),
        ),
        (
            OsString::from("COVENANT_SAP_EXPLORER_URL"),
            OsString::from(&config.explorer_url),
        ),
        (
            OsString::from("COVENANT_SAP_STATS_PATH"),
            stats_path.into_os_string(),
        ),
    ]);
    Ok(env)
}

fn worker_stats_path(ambient: &[(OsString, OsString)]) -> Result<PathBuf> {
    let value = |key: &str| {
        ambient
            .iter()
            .find(|(candidate, _)| candidate == OsStr::new(key))
            .map(|(_, value)| value)
            .filter(|value| !os_value_is_blank(value))
            .map(|value| trim_os_value(value))
    };

    if let Some(path) = value("COVENANT_SAP_STATS_PATH") {
        return Ok(PathBuf::from(path));
    }
    if let Some(home) = value("COVENANT_HOME") {
        return Ok(PathBuf::from(home).join("sap-rpc-stats.json"));
    }
    if let Some(home) = value("HOME") {
        return Ok(PathBuf::from(home)
            .join(".covenant")
            .join("sap-rpc-stats.json"));
    }
    Err(BridgeError::Invalid(
        "SAP worker stats path requires COVENANT_SAP_STATS_PATH, COVENANT_HOME, or HOME".into(),
    ))
}

fn os_value_is_blank(value: &OsStr) -> bool {
    value.to_str().is_some_and(|value| value.trim().is_empty())
}

fn trim_os_value(value: &OsStr) -> OsString {
    value
        .to_str()
        .map(str::trim)
        .map(OsString::from)
        .unwrap_or_else(|| value.to_os_string())
}

/// Reads up to `max` bytes from a worker stream, reporting whether the stream
/// had more to give. `take` stops the read at the cap instead of buffering an
/// unbounded body; reading one extra byte lets the caller tell an exact-fit
/// payload from a truncated flood. The returned buffer is clamped to `max`.
async fn read_capped<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    reader.take(max as u64 + 1).read_to_end(&mut buf).await?;
    let overflowed = buf.len() > max;
    buf.truncate(max);
    Ok((buf, overflowed))
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
    invoke_with_limits(config, command, payload, MAX_WORKER_OUTPUT_BYTES).await
}

async fn invoke_with_limits<P, T>(
    config: &Config,
    command: &str,
    payload: &P,
    max_output_bytes: usize,
) -> Result<T>
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
        .env_clear()
        .envs(worker_environment(config, command, std::env::vars_os())?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A timeout or an over-cap flood returns early, dropping the Child.
        // Without kill_on_drop a dropped Child leaves the worker running
        // detached — defeating the timeout entirely. With it, the subprocess is
        // reaped automatically.
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

    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");

    // wait_with_output() read both streams to EOF unbounded; cap each read
    // instead so a runaway or hostile worker cannot OOM the bridge before the
    // wall-clock budget fires. Read concurrently (not stdout-then-stderr) so a
    // worker that fills its stderr pipe before closing stdout cannot starve the
    // stdout reader. A worker that holds a pipe open without reaching EOF or the
    // cap still stalls here until worker_timeout — exactly as wait_with_output()
    // did — which the timeout below bounds.
    let read_both = async {
        tokio::join!(
            read_capped(&mut stdout, max_output_bytes),
            read_capped(&mut stderr, max_output_bytes),
        )
    };
    let (out, err) = match tokio::time::timeout(config.worker_timeout, read_both).await {
        Ok(pair) => pair,
        Err(_) => {
            return Err(BridgeError::Timeout {
                secs: config.worker_timeout.as_secs(),
            });
        }
    };
    let (stdout_bytes, stdout_overflowed) =
        out.map_err(|e| BridgeError::Worker(format!("read stdout: {e}")))?;
    let (stderr_bytes, stderr_overflowed) =
        err.map_err(|e| BridgeError::Worker(format!("read stderr: {e}")))?;

    // On overflow the worker may still be writing into a now-unread pipe and
    // will not exit on its own, so kill it to keep the reap prompt rather than
    // blocking until the timeout.
    if stdout_overflowed || stderr_overflowed {
        let _ = child.start_kill();
    }
    let status = match tokio::time::timeout(config.worker_timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => return Err(BridgeError::Worker(format!("wait: {e}"))),
        Err(_) => {
            return Err(BridgeError::Timeout {
                secs: config.worker_timeout.as_secs(),
            });
        }
    };
    if stdout_overflowed {
        return Err(BridgeError::Worker(format!(
            "worker stdout exceeded the {max_output_bytes}-byte cap"
        )));
    }

    let stdout = String::from_utf8_lossy(&stdout_bytes);
    // The worker may emit diagnostics before the result; the envelope
    // is always the last non-empty line.
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(str::trim)
        .unwrap_or("");

    if line.is_empty() {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        return Err(BridgeError::Worker(format!(
            "worker produced no output (exit {}, stderr: {})",
            status,
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

    #[test]
    fn worker_environment_is_allowlisted_and_config_is_canonical() {
        let mut config = Config::disabled(Cluster::Mainnet);
        config.enabled = true;
        config.program_id = "canonical-program".into();
        config.rpc_url = "https://canonical.example/rpc".into();
        config.explorer_url = "https://canonical.example/explorer".into();

        let env = worker_environment(
            &config,
            "publish-agent",
            [
                (OsString::from("PATH"), OsString::from("/usr/bin")),
                (
                    OsString::from("COVENANT_SAP_KEYPAIR"),
                    OsString::from("/keys/funder.json"),
                ),
                (
                    OsString::from("COVENANT_SAP_VERIFIER_KEYPAIR"),
                    OsString::from("/keys/verifier.json"),
                ),
                (
                    OsString::from("COVENANT_SAP_RPC_URL"),
                    OsString::from("https://ambient.example/rpc"),
                ),
                (
                    OsString::from("COVENANT_SAP_STATS_PATH"),
                    OsString::from("/var/lib/covenant/sap-stats.json"),
                ),
                (
                    OsString::from("COVENANT_HOME"),
                    OsString::from("/var/lib/covenant"),
                ),
                (OsString::from("HOME"), OsString::from("/users/operator")),
                (
                    OsString::from("UNRELATED_DAEMON_SECRET"),
                    OsString::from("must-not-leak"),
                ),
            ],
        )
        .expect("worker environment")
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();

        assert_eq!(
            env.get(OsStr::new("PATH")),
            Some(&OsString::from("/usr/bin"))
        );
        assert_eq!(
            env.get(OsStr::new("COVENANT_SAP_KEYPAIR")),
            Some(&OsString::from("/keys/funder.json"))
        );
        assert!(!env.contains_key(OsStr::new("COVENANT_SAP_VERIFIER_KEYPAIR")));
        assert_eq!(
            env.get(OsStr::new("COVENANT_SAP_RPC_URL")),
            Some(&OsString::from("https://canonical.example/rpc")),
            "resolved Rust config must override ambient config"
        );
        assert_eq!(
            env.get(OsStr::new("COVENANT_SOLANA_CLUSTER")),
            Some(&OsString::from("mainnet"))
        );
        assert_eq!(
            env.get(OsStr::new("COVENANT_SAP_ENABLED")),
            Some(&OsString::from("true"))
        );
        assert_eq!(
            env.get(OsStr::new("COVENANT_SAP_STATS_PATH")),
            Some(&OsString::from("/var/lib/covenant/sap-stats.json"))
        );
        assert!(!env.contains_key(OsStr::new("HOME")));
        assert!(!env.contains_key(OsStr::new("COVENANT_HOME")));
        assert!(
            !env.contains_key(OsStr::new("UNRELATED_DAEMON_SECRET")),
            "ambient daemon secrets must not cross the worker boundary"
        );
    }

    #[test]
    fn worker_environment_derives_stats_path_without_forwarding_home() {
        let config = Config::disabled(Cluster::Devnet);
        let env = worker_environment(
            &config,
            "find-agent",
            [
                (
                    OsString::from("COVENANT_HOME"),
                    OsString::from("  /var/lib/covenant  "),
                ),
                (OsString::from("HOME"), OsString::from("/users/operator")),
            ],
        )
        .expect("worker environment")
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();

        assert_eq!(
            env.get(OsStr::new("COVENANT_SAP_STATS_PATH")),
            Some(&OsString::from("/var/lib/covenant/sap-rpc-stats.json"))
        );
        assert!(!env.contains_key(OsStr::new("HOME")));
        assert!(!env.contains_key(OsStr::new("COVENANT_HOME")));
    }

    #[test]
    fn worker_environment_separates_signers_by_command() {
        let config = Config::disabled(Cluster::Devnet);
        let ambient = || {
            [
                (OsString::from("PATH"), OsString::from("/usr/bin")),
                (
                    OsString::from(PAYER_KEY_ENV),
                    OsString::from("/keys/payer.json"),
                ),
                (
                    OsString::from(VERIFIER_KEY_ENV),
                    OsString::from("/keys/verifier.json"),
                ),
                (
                    OsString::from("COVENANT_SAP_STATS_PATH"),
                    OsString::from("/var/lib/covenant/sap-stats.json"),
                ),
            ]
        };

        for command in ["publish-agent", "update-agent", "attest-root"] {
            let env = worker_environment(&config, command, ambient())
                .expect("payer command environment")
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>();
            assert!(env.contains_key(OsStr::new(PAYER_KEY_ENV)), "{command}");
            assert!(!env.contains_key(OsStr::new(VERIFIER_KEY_ENV)), "{command}");
        }

        let env = worker_environment(&config, "attest-agent", ambient())
            .expect("verifier command environment")
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert!(!env.contains_key(OsStr::new(PAYER_KEY_ENV)));
        assert!(env.contains_key(OsStr::new(VERIFIER_KEY_ENV)));

        for command in [
            "find-agent",
            "describe-agent",
            "find-by-protocol",
            "status",
            "stats",
            "unknown",
        ] {
            let env = worker_environment(&config, command, ambient())
                .expect("non-signing command environment")
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>();
            assert!(!env.contains_key(OsStr::new(PAYER_KEY_ENV)), "{command}");
            assert!(!env.contains_key(OsStr::new(VERIFIER_KEY_ENV)), "{command}");
            assert_eq!(
                env.get(OsStr::new("COVENANT_SAP_STATS_PATH")),
                Some(&OsString::from("/var/lib/covenant/sap-stats.json")),
                "{command}"
            );
        }
    }

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

    #[tokio::test]
    async fn invoke_with_limits_rejects_an_oversized_worker_stdout() {
        // A worker that floods stdout past the cap must surface a Worker error
        // naming the cap instead of buffering the whole stream and OOMing the
        // bridge before worker_timeout fires. The fake worker drains stdin
        // first so the payload write does not race a broken pipe, then writes
        // 200 bytes; a 64-byte cap forces the overflow branch.
        let mut config = Config::disabled(Cluster::Devnet);
        config.worker_command = vec![
            "sh".into(),
            "-c".into(),
            "cat >/dev/null; head -c 200 /dev/zero".into(),
        ];
        let err = invoke_with_limits::<serde_json::Value, serde_json::Value>(
            &config,
            "lookup",
            &serde_json::json!({}),
            64,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, BridgeError::Worker(m) if m.contains("exceeded") && m.contains("cap")),
            "an over-cap worker stdout must surface as a cap-breach Worker error: {err:?}"
        );
    }

    #[tokio::test]
    async fn invoke_with_limits_decodes_an_under_cap_envelope() {
        // The bounded read must still parse a normal single-line envelope that
        // fits under the cap, proving the cap did not regress the happy path.
        let mut config = Config::disabled(Cluster::Devnet);
        config.worker_command = vec![
            "sh".into(),
            "-c".into(),
            "cat >/dev/null; printf '{\"ok\":true,\"data\":42}'".into(),
        ];
        let value: i64 = invoke_with_limits(&config, "lookup", &serde_json::json!({}), 4096)
            .await
            .expect("under-cap envelope decodes through the bounded read");
        assert_eq!(value, 42);
    }

    #[tokio::test]
    async fn invoke_with_limits_routes_a_named_upstream_error_to_upstream() {
        // An ok:false envelope carrying a meaningful upstream error name must
        // surface as BridgeError::Upstream with the name preserved verbatim, not
        // flattened into a string. BridgeError documents this as load-bearing:
        // reconciliation loops branch on the failure class (a TransactionFailed /
        // blockheight-exceeded send failure, BridgeSignerRequiredError) instead of
        // substring-matching a message. The whole ok:false routing is otherwise
        // uncovered -- the existing tests are ok:true happy decode, pre-spawn
        // validation, and the stdout cap -- so a refactor that collapsed the
        // match env.name arm into the Rpc fold would erase the branchable name
        // with no failing test. The fake worker drains stdin first (so the
        // payload write does not race a broken pipe), then prints the envelope.
        let mut config = Config::disabled(Cluster::Devnet);
        config.worker_command = vec![
            "sh".into(),
            "-c".into(),
            "cat >/dev/null; printf '{\"ok\":false,\"name\":\"TransactionFailed\",\"error\":\"blockhash expired\"}'".into(),
        ];
        let err = invoke_with_limits::<serde_json::Value, serde_json::Value>(
            &config,
            "send",
            &serde_json::json!({}),
            4096,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, BridgeError::Upstream { name, message }
                if name == "TransactionFailed" && message == "blockhash expired"),
            "a named upstream failure must surface as Upstream with its name intact: {err:?}"
        );
    }

    #[tokio::test]
    async fn invoke_with_limits_folds_a_nameless_failure_to_rpc() {
        // A failure name that carries no signal -- absent, empty, or JS's default
        // "Error" -- must collapse to BridgeError::Rpc, never a useless
        // Upstream { name: "" | "Error" } that a reconciliation loop would mistake
        // for a branchable failure class. All three no-signal shapes share one
        // message, so the only variable under test is the name guard
        // (!name.is_empty() && name != "Error"); loosening it would surface here.
        let nameless = [
            "cat >/dev/null; printf '{\"ok\":false,\"error\":\"node unreachable\"}'",
            "cat >/dev/null; printf '{\"ok\":false,\"name\":\"\",\"error\":\"node unreachable\"}'",
            "cat >/dev/null; printf '{\"ok\":false,\"name\":\"Error\",\"error\":\"node unreachable\"}'",
        ];
        for worker in nameless {
            let mut config = Config::disabled(Cluster::Devnet);
            config.worker_command = vec!["sh".into(), "-c".into(), worker.into()];
            let err = invoke_with_limits::<serde_json::Value, serde_json::Value>(
                &config,
                "send",
                &serde_json::json!({}),
                4096,
            )
            .await
            .unwrap_err();
            assert!(
                matches!(&err, BridgeError::Rpc(m) if m == "node unreachable"),
                "a no-signal name must fold to Rpc, got {err:?} for worker `{worker}`"
            );
        }
    }

    #[tokio::test]
    async fn invoke_with_limits_rejects_a_worker_that_produces_no_output() {
        // A worker that exits successfully with no stdout yields an empty
        // envelope line; invoke must surface a Worker error naming the exit
        // status and stderr so an operator can tell a worker that crashed
        // silently apart from one that emitted unparseable garbage. The fake
        // worker drains stdin first so the payload write does not race a
        // broken pipe, then prints nothing and exits 0.
        let mut config = Config::disabled(Cluster::Devnet);
        config.worker_command = vec!["sh".into(), "-c".into(), "cat >/dev/null; true".into()];
        let err = invoke_with_limits::<serde_json::Value, serde_json::Value>(
            &config,
            "lookup",
            &serde_json::json!({}),
            4096,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, BridgeError::Worker(m)
                if m.contains("produced no output") && m.contains("exit")),
            "a worker that produces no output must surface a Worker error naming the exit status, got {err:?}"
        );
    }

    #[tokio::test]
    async fn invoke_with_limits_surfaces_a_malformed_envelope_line() {
        // A worker whose last stdout line is not valid JSON (a stack trace,
        // a stray log line) must surface a Decode error preserving the
        // offending line, so the operator sees what the worker printed
        // rather than an opaque parse failure.
        let mut config = Config::disabled(Cluster::Devnet);
        config.worker_command = vec![
            "sh".into(),
            "-c".into(),
            "cat >/dev/null; printf 'not-json-garbage'".into(),
        ];
        let err = invoke_with_limits::<serde_json::Value, serde_json::Value>(
            &config,
            "lookup",
            &serde_json::json!({}),
            4096,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, BridgeError::Decode(m) if m.contains("not-json-garbage")),
            "a non-JSON envelope line must surface a Decode error carrying the offending line, got {err:?}"
        );
    }

    #[tokio::test]
    async fn invoke_with_limits_surfaces_an_inner_data_type_mismatch() {
        // A well-formed envelope whose `data` field will not deserialize
        // into T (here a string where i64 is expected) must surface a Decode
        // error rather than silently coercing — catching a worker/protocol
        // schema drift at the boundary instead of letting a wrong-typed
        // value propagate downstream.
        let mut config = Config::disabled(Cluster::Devnet);
        config.worker_command = vec![
            "sh".into(),
            "-c".into(),
            "cat >/dev/null; printf '{\"ok\":true,\"data\":\"not-a-number\"}'".into(),
        ];
        let err = invoke_with_limits::<serde_json::Value, i64>(
            &config,
            "lookup",
            &serde_json::json!({}),
            4096,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, BridgeError::Decode(_)),
            "a wrong-typed data field must surface a Decode error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn read_capped_treats_an_exact_max_fill_as_an_exact_fit_not_overflow() {
        // read_capped reads through take(max + 1) and sets
        // overflowed = buf.len() > max (worker.rs:59), so a stream of exactly
        // max bytes is an exact fit (overflowed false, buffer returned whole)
        // and only max + 1 is a truncated flood (overflowed true, buffer
        // clamped to max) — the discriminator the extra byte exists to enable.
        // The invoke_with_limits cap tests bracket this from far away: 200
        // bytes against a 64-byte cap fills the buffer to max + 1 (so `> max`
        // and `>= max` agree), and the under-cap envelope is 21 bytes against
        // 4096 (far under). Neither lands on buf.len() == max, where a
        // `> max` -> `>= max` slip flips an exact-fit payload to a spurious
        // overflow and discards a legal worst-case worker response. Pin the
        // inclusive endpoint directly.
        let max = 64;

        let exact = vec![b'x'; max];
        let mut reader = exact.as_slice();
        let (buf, overflowed) = read_capped(&mut reader, max)
            .await
            .expect("reading an in-memory slice cannot fail");
        assert!(
            !overflowed,
            "a stream of exactly max bytes is an exact fit, not a flood"
        );
        assert_eq!(
            buf, exact,
            "an exact-fit body must be returned whole, not truncated"
        );

        let flood = vec![b'x'; max + 1];
        let mut reader = flood.as_slice();
        let (buf, overflowed) = read_capped(&mut reader, max)
            .await
            .expect("reading an in-memory slice cannot fail");
        assert!(
            overflowed,
            "a stream of max + 1 bytes is a truncated flood and must report overflow"
        );
        assert_eq!(
            buf.len(),
            max,
            "an over-cap read must clamp the returned buffer to max"
        );
    }
}
