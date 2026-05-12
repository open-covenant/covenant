//! Hermes runtime backend.
//!
//! Drives a remote Hermes-agent gateway (https://github.com/NousResearch/hermes-agent)
//! over its native `/v1/runs/*` HTTP surface. The covenant daemon dispatches
//! the intent, Hermes does the agent stepping, and we poll the run to
//! terminal state. Wall-clock budget comes from the agent manifest's
//! `resources.cpu_ms_per_task`; we issue `POST /v1/runs/{id}/stop` if
//! exceeded.
//!
//! The capability gate is enforced one level up: the daemon's
//! `dispatch_intent` runs `capability_check` before handing off to the
//! runner. This module's job is the wire protocol, not policy.
//!
//! Event-stream folding into the audit log is intentionally deferred to a
//! follow-up — this initial slice uses the polling status endpoint so we
//! can prove the round trip end-to-end before pulling in SSE.

use async_trait::async_trait;
use covenant_manifest::Runtime as RuntimeKind;
use covenant_router::AgentCard;
use covenant_types::Intent;
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

use crate::{AgentResult, Runner, RunnerError};

/// Default polling cadence while waiting for a run to reach a terminal
/// state. Hermes does not push us over the wire on the status endpoint,
/// so we sleep this long between GETs. Kept short so wall-clock budgets
/// are honored within reasonable precision.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Cap on a single HTTP request so a wedged Hermes gateway can't pin the
/// runner forever. Independent from the per-intent wall-clock budget.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Driver for a Hermes gateway. One instance is shared across dispatches
/// — the underlying `reqwest::Client` pools connections.
pub struct HermesRunner {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

impl HermesRunner {
    /// `base_url` is the gateway's API root including the version prefix,
    /// e.g. `http://127.0.0.1:8642/v1`. `api_key` is sent as a Bearer
    /// token; pass `None` only when the gateway is bound to loopback with
    /// no `API_SERVER_KEY` configured (Hermes permits unauthenticated
    /// loopback in that case).
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("build reqwest client");
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            http,
        }
    }

    fn ensure_allowed(&self, card: &AgentCard) -> Result<(), RunnerError> {
        if !matches!(card.manifest.agent.runtime, RuntimeKind::Hermes) {
            return Err(RunnerError::WrongRuntime {
                agent: card.id.clone(),
                expected: "hermes",
                got: runtime_name(card.manifest.agent.runtime),
            });
        }
        Ok(())
    }

    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => builder.bearer_auth(key),
            None => builder,
        }
    }

    async fn submit(&self, intent: &Intent) -> Result<String, RunnerError> {
        let url = format!("{}/runs", self.base_url);
        let body = json!({
            "input": intent.text,
            "session_id": intent.id.to_string(),
        });
        let resp = self
            .apply_auth(self.http.post(&url).json(&body))
            // Hermes dedupes identical submissions in a five-minute
            // window when an Idempotency-Key is present, which lets a
            // covenantd restart mid-dispatch reattach to the same run.
            .header("Idempotency-Key", intent.id.to_string())
            .send()
            .await
            .map_err(remote_err)?;
        let status = resp.status();
        let text = resp.text().await.map_err(remote_err)?;
        if !status.is_success() {
            return Err(RunnerError::Remote {
                status: status.as_u16(),
                message: truncate(&text, 512),
            });
        }
        let body: HermesRunCreated = serde_json::from_str(&text)
            .map_err(|source| RunnerError::MalformedStdout { source })?;
        Ok(body.run_id)
    }

    async fn poll(&self, run_id: &str) -> Result<HermesRunStatus, RunnerError> {
        let url = format!("{}/runs/{}", self.base_url, run_id);
        let resp = self
            .apply_auth(self.http.get(&url))
            .send()
            .await
            .map_err(remote_err)?;
        let status = resp.status();
        let text = resp.text().await.map_err(remote_err)?;
        if !status.is_success() {
            return Err(RunnerError::Remote {
                status: status.as_u16(),
                message: truncate(&text, 512),
            });
        }
        serde_json::from_str(&text).map_err(|source| RunnerError::MalformedStdout { source })
    }

    async fn cancel(&self, run_id: &str) {
        let url = format!("{}/runs/{}/stop", self.base_url, run_id);
        // Best-effort: we're already in an error/timeout path; log and
        // move on if the cancel itself fails.
        match self.apply_auth(self.http.post(&url)).send().await {
            Ok(resp) if resp.status().is_success() => {
                debug!(%run_id, "hermes run stop accepted");
            }
            Ok(resp) => {
                warn!(%run_id, status = %resp.status(), "hermes run stop returned non-success");
            }
            Err(e) => {
                warn!(%run_id, error = %e, "hermes run stop send failed");
            }
        }
    }
}

#[async_trait]
impl Runner for HermesRunner {
    async fn run(&self, card: &AgentCard, intent: &Intent) -> Result<AgentResult, RunnerError> {
        self.ensure_allowed(card)?;

        let budget = Duration::from_millis(card.manifest.resources.cpu_ms_per_task);
        let deadline = Instant::now() + budget;

        let run_id = self.submit(intent).await?;
        debug!(%run_id, agent = %card.id, "hermes run submitted");

        loop {
            if Instant::now() >= deadline {
                warn!(%run_id, agent = %card.id, ?budget, "hermes run timed out — stopping");
                self.cancel(&run_id).await;
                return Err(RunnerError::Timeout(budget));
            }
            let status = self.poll(&run_id).await?;
            match status.status.as_str() {
                "completed" => {
                    return Ok(AgentResult {
                        text: status.output.unwrap_or_default(),
                        sources: Vec::new(),
                    });
                }
                "failed" => {
                    return Err(RunnerError::Remote {
                        status: 0,
                        message: status
                            .error
                            .unwrap_or_else(|| "hermes run failed".to_string()),
                    });
                }
                "cancelled" => {
                    return Err(RunnerError::Remote {
                        status: 0,
                        message: "hermes run cancelled".to_string(),
                    });
                }
                // "queued" | "running" | "waiting_for_approval" | "stopping" → keep polling
                _ => {
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
        }
    }
}

fn runtime_name(r: RuntimeKind) -> &'static str {
    match r {
        RuntimeKind::Python3 => "python3",
        RuntimeKind::Node => "node",
        RuntimeKind::RustBin => "rust-bin",
        RuntimeKind::Hermes => "hermes",
    }
}

fn remote_err(e: reqwest::Error) -> RunnerError {
    RunnerError::Remote {
        status: e.status().map(|s| s.as_u16()).unwrap_or(0),
        message: e.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[derive(Debug, Deserialize)]
struct HermesRunCreated {
    run_id: String,
}

#[derive(Debug, Deserialize)]
struct HermesRunStatus {
    status: String,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    error: Option<String>,
}
