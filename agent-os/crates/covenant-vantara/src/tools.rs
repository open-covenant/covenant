//! Native Vantara tools.
//!
//! Three read-only tools over the explorer. `vantara.jobs` lists recent
//! provenance records and their attestations. `vantara.attest` resolves a
//! single job — by id, by output hash, or by hashing an output the caller
//! holds — and returns its content-free attestation. `vantara.payouts` reads
//! the on-chain settlement ledger. None of them run a job, hold a key, or
//! move money; the connector is a reader of Vantara's public trust surface.

use std::sync::Arc;

use async_trait::async_trait;
use covenant_mcp::{Content, Tool, ToolCallResult, ToolError};
use serde_json::{json, Value};

use crate::attest::{sha256_hex, VantaraAttestation};
use crate::client::{Lookup, VantaraClient};
use crate::config::VantaraConfig;

pub const JOBS_TOOL: &str = "vantara.jobs";
pub const ATTEST_TOOL: &str = "vantara.attest";
pub const PAYOUTS_TOOL: &str = "vantara.payouts";

/// Explorer page ceiling. The feed caps a page at 50 records.
const MAX_PAGE: u64 = 50;

/// Build the Vantara tool set permitted by `cfg.allow`, sharing one client.
/// Empty when the config is disabled.
pub fn vantara_tools(client: Arc<VantaraClient>, cfg: &VantaraConfig) -> Vec<Arc<dyn Tool>> {
    if !cfg.enabled {
        return Vec::new();
    }
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    if cfg.allows(JOBS_TOOL) {
        tools.push(Arc::new(JobsTool {
            client: client.clone(),
        }));
    }
    if cfg.allows(ATTEST_TOOL) {
        tools.push(Arc::new(AttestTool {
            client: client.clone(),
        }));
    }
    if cfg.allows(PAYOUTS_TOOL) {
        tools.push(Arc::new(PayoutsTool { client }));
    }
    tools
}

fn u32_arg(args: &Value, key: &str, default: u64, max: u64) -> u32 {
    args.get(key)
        .and_then(Value::as_u64)
        .unwrap_or(default)
        .clamp(1, max) as u32
}

fn offset_arg(args: &Value) -> u32 {
    args.get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

struct JobsTool {
    client: Arc<VantaraClient>,
}

#[async_trait]
impl Tool for JobsTool {
    fn name(&self) -> &str {
        JOBS_TOOL
    }
    fn description(&self) -> &str {
        "List recent Vantara provenance records (content-free: hashes only). Returns the raw feed page and a Covenant attestation for each job."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "description": "Records to return, 1-50 (default 20)." },
                "offset": { "type": "integer", "description": "Feed offset (default 0)." }
            },
            "additionalProperties": false
        })
    }
    async fn call(&self, args: Value) -> Result<ToolCallResult, ToolError> {
        let limit = u32_arg(&args, "limit", 20, MAX_PAGE);
        let offset = offset_arg(&args);
        match self.client.jobs(limit, offset).await {
            Ok(page) => {
                let attestations: Vec<Value> = page
                    .jobs
                    .iter()
                    .map(|j| {
                        VantaraAttestation::from_job(
                            &page.cluster,
                            &page.hash_algorithm,
                            page.signing.as_ref(),
                            j,
                        )
                        .to_json()
                    })
                    .collect();
                Ok(ToolCallResult::ok(vec![
                    Content::json(serde_json::to_value(&page).unwrap_or(Value::Null)),
                    Content::json(json!({ "attestations": attestations })),
                ]))
            }
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

struct AttestTool {
    client: Arc<VantaraClient>,
}

#[async_trait]
impl Tool for AttestTool {
    fn name(&self) -> &str {
        ATTEST_TOOL
    }
    fn description(&self) -> &str {
        "Attest a Vantara job by `jobId`, by `outputHash`, or by `output` (hashed locally to its sha256). Returns a content-free Covenant attestation binding the output hash to the model and node that produced it, or an error if no job matches."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "jobId": { "type": "string", "description": "Vantara job id to attest." },
                "outputHash": { "type": "string", "description": "SHA-256 hex of an output to locate in the feed." },
                "output": { "type": "string", "description": "Raw output; hashed locally to its sha256, never sent." }
            },
            "additionalProperties": false
        })
    }
    async fn call(&self, args: Value) -> Result<ToolCallResult, ToolError> {
        let matcher = Matcher::from_args(&args)?;
        let result = self.client.find_job(|j| matcher.matches(j)).await;
        match result {
            Ok(Lookup::Found {
                job,
                cluster,
                hash_algorithm,
                signing,
            }) => {
                let att =
                    VantaraAttestation::from_job(&cluster, &hash_algorithm, signing.as_ref(), &job);
                Ok(ToolCallResult::ok(vec![Content::json(
                    json!({ "attestation": att.to_json() }),
                )]))
            }
            Ok(Lookup::Missing {
                scanned,
                total,
                truncated,
            }) => {
                let mut msg = format!(
                    "no vantara job matches {} (scanned {scanned} of {total} records)",
                    matcher.describe()
                );
                if truncated {
                    msg.push_str("; scan bound reached before the end of the feed");
                }
                Ok(ToolCallResult::error(msg))
            }
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

enum Matcher {
    JobId(String),
    OutputHash(String),
}

impl Matcher {
    fn from_args(args: &Value) -> Result<Self, ToolError> {
        let str_arg = |k: &str| {
            args.get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        };
        if let Some(id) = str_arg("jobId") {
            return Ok(Matcher::JobId(id.to_string()));
        }
        if let Some(h) = str_arg("outputHash") {
            return Ok(Matcher::OutputHash(h.to_ascii_lowercase()));
        }
        if let Some(output) = str_arg("output") {
            return Ok(Matcher::OutputHash(sha256_hex(output.as_bytes())));
        }
        Err(ToolError::InvalidArguments(
            "provide one of `jobId`, `outputHash`, or `output`".to_string(),
        ))
    }

    fn matches(&self, job: &crate::types::Job) -> bool {
        match self {
            Matcher::JobId(id) => &job.job_id == id,
            Matcher::OutputHash(h) => job.output_hash.eq_ignore_ascii_case(h),
        }
    }

    fn describe(&self) -> String {
        match self {
            Matcher::JobId(id) => format!("jobId={id}"),
            Matcher::OutputHash(h) => format!("outputHash={h}"),
        }
    }
}

struct PayoutsTool {
    client: Arc<VantaraClient>,
}

#[async_trait]
impl Tool for PayoutsTool {
    fn name(&self) -> &str {
        PAYOUTS_TOOL
    }
    fn description(&self) -> &str {
        "Read the Vantara on-chain settlement ledger: per-wallet USDC payouts, each with a Solana transaction signature, plus lifetime totals. This is the payment anchor; payouts are hourly aggregates and carry no job id."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "description": "Payouts to return, 1-50 (default 10)." },
                "offset": { "type": "integer", "description": "Feed offset (default 0)." }
            },
            "additionalProperties": false
        })
    }
    async fn call(&self, args: Value) -> Result<ToolCallResult, ToolError> {
        let limit = u32_arg(&args, "limit", 10, MAX_PAGE);
        let offset = offset_arg(&args);
        match self.client.payouts(limit, offset).await {
            Ok(page) => Ok(ToolCallResult::ok(vec![Content::json(
                serde_json::to_value(&page).unwrap_or(Value::Null),
            )])),
            Err(e) => Ok(ToolCallResult::error(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn enabled(base: &str) -> VantaraConfig {
        VantaraConfig {
            enabled: true,
            base_url: base.to_string(),
            ..Default::default()
        }
    }

    const OUTPUT_HASH: &str = "557be7eca214f1889cdb6dfa348eb7c937648c9d6be72bfc1b8204adf7552a43";

    fn jobs_body(total: u64, jobs: &str) -> String {
        format!(
            r#"{{"cluster":"mainnet-beta","hashAlgorithm":"sha256","note":"",
                "pagination":{{"limit":50,"offset":0,"total":{total}}},"jobs":[{jobs}]}}"#
        )
    }

    fn one_job() -> String {
        format!(
            r#"{{"jobId":"7f3c1a92","nodeId":null,"model":"claude","outputHash":"{OUTPUT_HASH}",
                "proofSignature":null,"completedAt":"2026-07-01T18:04:12.311Z"}}"#
        )
    }

    /// A one-job feed with a real signing block and a `vantaraSignature`
    /// signed under a known key, plus a settled reward.
    fn signed_jobs_body() -> String {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let canonical = format!(
            "vantara-job-receipt-v1|7f3c1a92|-|claude|{OUTPUT_HASH}|2026-07-01T18:04:12.311Z"
        );
        let sig = STANDARD.encode(sk.sign(canonical.as_bytes()).to_bytes());
        let pk = bs58::encode(sk.verifying_key().as_bytes()).into_string();
        format!(
            r#"{{"cluster":"mainnet-beta","hashAlgorithm":"sha256","note":"",
                "signing":{{"field":"vantaraSignature","scheme":"ed25519","publicKey":"{pk}",
                    "publicKeyEncoding":"base58","signatureEncoding":"base64",
                    "canonical":"vantara-job-receipt-v1|<jobId>|<nodeId or '-'>|<model>|<outputHash>|<completedAt ISO>"}},
                "pagination":{{"limit":50,"offset":0,"total":1}},
                "jobs":[{{"jobId":"7f3c1a92","nodeId":null,"model":"claude","outputHash":"{OUTPUT_HASH}",
                    "proofSignature":null,"vantaraSignature":"{sig}","completedAt":"2026-07-01T18:04:12.311Z",
                    "settlement":{{"settled":true,"settledAt":"2026-07-02T08:00:00.000Z"}}}}]}}"#
        )
    }

    #[tokio::test]
    async fn jobs_attestation_reports_verified_signature_and_settlement() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/explorer/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_string(signed_jobs_body()))
            .mount(&server)
            .await;

        let client = Arc::new(VantaraClient::new(server.uri()));
        let res = JobsTool { client }.call(json!({})).await.unwrap();
        match &res.content[1] {
            Content::Json { value } => {
                let att = &value.get("attestations").and_then(Value::as_array).unwrap()[0];
                assert_eq!(att["signature"]["status"].as_str(), Some("verified"));
                assert_eq!(att["settlement"]["settled"].as_bool(), Some(true));
            }
            other => panic!("expected attestations json, got {other:?}"),
        }
    }

    #[test]
    fn disabled_registers_nothing_enabled_registers_three() {
        let client = Arc::new(VantaraClient::new("http://x"));
        assert!(vantara_tools(client.clone(), &VantaraConfig::default()).is_empty());

        let mut names: Vec<String> = vantara_tools(client, &enabled("http://x"))
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        names.sort();
        assert_eq!(names, vec![ATTEST_TOOL, JOBS_TOOL, PAYOUTS_TOOL]);
    }

    #[test]
    fn allowlist_filters_registration() {
        let client = Arc::new(VantaraClient::new("http://x"));
        let mut cfg = enabled("http://x");
        cfg.allow = Some(vec![ATTEST_TOOL.to_string()]);
        let names: Vec<String> = vantara_tools(client, &cfg)
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        assert_eq!(names, vec![ATTEST_TOOL]);
    }

    #[tokio::test]
    async fn jobs_returns_page_then_attestations() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/explorer/jobs"))
            .and(query_param("limit", "20"))
            .respond_with(ResponseTemplate::new(200).set_body_string(jobs_body(1, &one_job())))
            .mount(&server)
            .await;

        let client = Arc::new(VantaraClient::new(server.uri()));
        let tool = JobsTool { client };
        let res = tool.call(json!({})).await.unwrap();
        assert!(!res.is_error);
        assert_eq!(res.content.len(), 2);
        match &res.content[1] {
            Content::Json { value } => {
                let atts = value.get("attestations").and_then(Value::as_array).unwrap();
                assert_eq!(atts.len(), 1);
                assert_eq!(
                    atts[0].get("provider").and_then(Value::as_str),
                    Some("vantara")
                );
                assert_eq!(atts[0].get("model").and_then(Value::as_str), Some("claude"));
                assert_eq!(
                    atts[0].get("outputHash").and_then(Value::as_str),
                    Some(OUTPUT_HASH)
                );
            }
            other => panic!("expected attestations json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attest_by_output_hashes_locally_and_binds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/explorer/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_string(jobs_body(1, &one_job())))
            .mount(&server)
            .await;

        let client = Arc::new(VantaraClient::new(server.uri()));
        let tool = AttestTool { client };
        // "covenant" hashes to OUTPUT_HASH in the sample fixture.
        let res = tool
            .call(json!({ "outputHash": OUTPUT_HASH }))
            .await
            .unwrap();
        assert!(!res.is_error);
        match &res.content[0] {
            Content::Json { value } => {
                let att = value.get("attestation").unwrap();
                assert_eq!(att.get("jobId").and_then(Value::as_str), Some("7f3c1a92"));
                assert_eq!(
                    att.get("cluster").and_then(Value::as_str),
                    Some("mainnet-beta")
                );
            }
            other => panic!("expected attestation json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attest_missing_is_error_with_scan_detail() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/explorer/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_string(jobs_body(1, &one_job())))
            .mount(&server)
            .await;

        let client = Arc::new(VantaraClient::new(server.uri()));
        let tool = AttestTool { client };
        let res = tool
            .call(json!({ "jobId": "does-not-exist" }))
            .await
            .unwrap();
        assert!(res.is_error);
        match &res.content[0] {
            Content::Text { text } => {
                assert!(text.contains("no vantara job matches"));
                assert!(text.contains("scanned 1 of 1"));
            }
            other => panic!("expected error text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn attest_without_selector_is_invalid_arguments() {
        let client = Arc::new(VantaraClient::new("http://x"));
        let tool = AttestTool { client };
        let err = tool.call(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn payouts_returns_the_ledger_page() {
        let body = r#"{"cluster":"mainnet-beta","pagination":{"limit":10,"offset":0,"total":3982},
            "totals":{"USDC":{"total":367.96,"count":3982}},
            "payouts":[{"id":"fb7b3983","walletAddress":"HoRpjJmY","asset":"USDC","amount":0.38,
                "reason":"hourly-aggregate-capped","reasonLabel":"hourly-aggregate-capped",
                "txSignature":"2B9MTUz9","createdAt":"2026-07-02T08:29:25.319Z",
                "periodStart":"2026-07-02T07:29:25.315Z","periodEnd":"2026-07-02T08:29:25.315Z"}]}"#;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/explorer/payouts"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = Arc::new(VantaraClient::new(server.uri()));
        let tool = PayoutsTool { client };
        let res = tool.call(json!({})).await.unwrap();
        assert!(!res.is_error);
        match &res.content[0] {
            Content::Json { value } => {
                assert_eq!(
                    value.get("payouts").and_then(Value::as_array).map(Vec::len),
                    Some(1)
                );
                let sig = value["payouts"][0]["txSignature"].as_str().unwrap();
                assert_eq!(sig, "2B9MTUz9");
            }
            other => panic!("expected payouts json, got {other:?}"),
        }
    }
}
