//! Bounded Kani execution contracts and portable, receipt-covered evidence.
//! These checks authenticate and validate a checker's report. They do not
//! attest the checker's hardware or replace independently reproducing a proof.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{sha256_hex, ProtocolError};

pub const MAX_KANI_TIMEOUT_SECS: u32 = 7200;
pub const MAX_KANI_RAW_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_KANI_COMPRESSED_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KaniPlan {
    pub schema: String,
    pub harness: String,
    pub manifest_path: String,
    pub features: Vec<String>,
    pub tests: bool,
    pub timeout_secs: u32,
    pub unwind: Option<u32>,
    pub expected_outcome: String,
    pub expected_assertion: Option<String>,
    pub required_covers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KaniCheckSpec {
    pub plan: KaniPlan,
    pub memory_mib: u32,
    pub cpus: u32,
    pub scratch_mib: u32,
    /// Price of each coordinator-ordered proof attempt, included in the
    /// parent's maximum offer. Direct verification has its own signed price.
    pub check_price_micro_usdc: u64,
}

fn invalid(message: impl Into<String>) -> ProtocolError {
    ProtocolError::Invalid(message.into())
}

impl KaniPlan {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let identifier = |s: &str| {
            !s.is_empty()
                && s.len() <= 256
                && s.split("::").all(|part| {
                    part.as_bytes()
                        .first()
                        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
                        && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                })
        };
        if self.schema != "covenant.kani.plan.v1" || !identifier(&self.harness) {
            return Err(invalid(
                "Kani requires a versioned plan and an exact harness name",
            ));
        }
        let safe_component = |s: &str| {
            s.as_bytes()
                .first()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        };
        if self.manifest_path.len() > 256
            || self.manifest_path.rsplit('/').next() != Some("Cargo.toml")
            || !self.manifest_path.split('/').all(safe_component)
        {
            return Err(invalid(
                "Kani manifest must be a safe relative Cargo.toml path",
            ));
        }
        if self.timeout_secs == 0
            || self.timeout_secs > MAX_KANI_TIMEOUT_SECS
            || self.unwind.is_some_and(|n| n == 0 || n > 1_000_000)
        {
            return Err(invalid("Kani timeout or unwind bound is out of range"));
        }
        if self.features.len() > 32
            || self.features.iter().collect::<BTreeSet<_>>().len() != self.features.len()
            || self.features.iter().any(|s| {
                s.len() > 64
                    || !s
                        .as_bytes()
                        .first()
                        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
                    || !s
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_.+-".contains(&c))
            })
        {
            return Err(invalid(
                "Kani features must be unique bounded Cargo feature names",
            ));
        }
        if self.required_covers.len() > 32
            || self
                .required_covers
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 1024 || s.chars().any(char::is_control))
            || self.required_covers.iter().collect::<BTreeSet<_>>().len()
                != self.required_covers.len()
        {
            return Err(invalid(
                "Kani required covers must be unique bounded descriptions",
            ));
        }
        match self.expected_outcome.as_str() {
            "proven" if self.expected_assertion.is_none() && !self.required_covers.is_empty() => {}
            "refuted"
                if self.expected_assertion.as_ref().is_some_and(|s| {
                    !s.trim().is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control)
                }) => {}
            _ => {
                return Err(invalid(
                    "a proof needs required covers; a refutation needs its exact assertion",
                ))
            }
        }
        Ok(())
    }
}

impl KaniCheckSpec {
    pub fn validate(&self, image: &str, timeout_secs: u32) -> Result<(), ProtocolError> {
        self.plan.validate()?;
        let digest = image
            .strip_prefix("sha256:")
            .or_else(|| image.rsplit_once("@sha256:").map(|(_, d)| d));
        if !digest.is_some_and(|d| {
            d.len() == 64
                && d.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err(invalid(
                "Kani requires an immutable SHA-256 image reference",
            ));
        }
        if !(1024..=262144).contains(&self.memory_mib)
            || !(1..=256).contains(&self.cpus)
            || !(256..=65536).contains(&self.scratch_mib)
            || self.scratch_mib >= self.memory_mib
            || self.plan.timeout_secs > timeout_secs
            || timeout_secs > MAX_KANI_TIMEOUT_SECS
            || !(50_000..=1_000_000).contains(&self.check_price_micro_usdc)
        {
            return Err(invalid(
                "Kani resource, duration or check-price bounds are invalid",
            ));
        }
        Ok(())
    }
}

/// A versioned exact capability label; generic image support is insufficient.
pub fn kani_checker_model(image: &str, memory_mib: u32, cpus: u32, scratch_mib: u32) -> String {
    format!(
        "kani-v1:{}",
        sha256_hex(format!("{image}\n{memory_mib}\n{cpus}\n{scratch_mib}\n").as_bytes())
    )
}

/// A fixed command label, not buyer-supplied shell. All plan details are bound.
pub fn kani_command_label(plan: &KaniPlan) -> String {
    format!(
        "covenant-kani-v1 {} {}",
        plan.harness,
        sha256_hex(&serde_json::to_vec(plan).expect("KaniPlan serializes"))
    )
}

/// Decode and check the complete evidence package with bounded decompression.
pub fn kani_artifacts(evidence: &Value) -> Result<BTreeMap<String, Vec<u8>>, ProtocolError> {
    let encoded = evidence["artifacts_gzip_b64"]
        .as_str()
        .ok_or_else(|| invalid("missing Kani artifacts"))?;
    if encoded.len() > MAX_KANI_COMPRESSED_BYTES.div_ceil(3) * 4 {
        return Err(invalid("Kani compressed evidence exceeds its limit"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| invalid("invalid Kani evidence base64"))?;
    if bytes.len() > MAX_KANI_COMPRESSED_BYTES
        || evidence["artifacts_sha256"].as_str() != Some(&sha256_hex(&bytes))
    {
        return Err(invalid("Kani compressed evidence digest mismatch"));
    }
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .take(24 * 1024 * 1024 + 1)
        .read_to_end(&mut decoded)
        .map_err(|_| invalid("invalid Kani evidence gzip"))?;
    if decoded.len() > 24 * 1024 * 1024 {
        return Err(invalid("Kani evidence decompression limit exceeded"));
    }
    let packed: BTreeMap<String, String> = serde_json::from_slice(&decoded)?;
    let mut artifacts = BTreeMap::new();
    let mut total = 0u64;
    for (name, encoded) in packed {
        if ![
            "result.json",
            "stdout.log",
            "stderr.log",
            "kani-export.json",
        ]
        .contains(&name.as_str())
        {
            return Err(invalid("unexpected Kani artifact name"));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| invalid("invalid Kani artifact encoding"))?;
        total += bytes.len() as u64;
        if total > MAX_KANI_RAW_BYTES {
            return Err(invalid("Kani raw evidence limit exceeded"));
        }
        artifacts.insert(name, bytes);
    }
    if evidence["raw_bytes"].as_u64() != Some(total) {
        return Err(invalid("Kani raw evidence length mismatch"));
    }
    Ok(artifacts)
}

/// Structural/result validation shared by settlement and offline buyers.
/// Returns whether the requested property outcome was actually reported.
pub fn validate_kani_evidence(
    spec: &KaniCheckSpec,
    image: &str,
    evidence: &Value,
) -> Result<bool, ProtocolError> {
    if evidence["schema"] != "covenant.kani.execution.v1"
        || evidence["plan"] != serde_json::to_value(&spec.plan)?
        || evidence["image"] != image
        || evidence["memory_mib"].as_u64() != Some(u64::from(spec.memory_mib))
        || evidence["cpus"].as_u64() != Some(u64::from(spec.cpus))
        || evidence["scratch_mib"].as_u64() != Some(u64::from(spec.scratch_mib))
    {
        return Err(invalid(
            "Kani evidence does not match its ordered plan, image or resources",
        ));
    }
    let report = &evidence["report"];
    if report["schema"] != "covenant.kani.result.v1"
        || report["harness"] != spec.plan.harness
        || report["reason"].as_str().is_none_or(|s| s.is_empty())
    {
        return Err(invalid("malformed Kani result report"));
    }
    let outcome = report["outcome"]
        .as_str()
        .ok_or_else(|| invalid("missing Kani outcome"))?;
    if ![
        "proven",
        "refuted",
        "timeout",
        "out_of_memory",
        "tool_error",
        "inconclusive",
    ]
    .contains(&outcome)
    {
        return Err(invalid("unknown Kani outcome"));
    }
    let accepted = outcome == spec.plan.expected_outcome;
    if report["accepted"].as_bool() != Some(accepted) {
        return Err(invalid("Kani acceptance contradicts its outcome"));
    }
    let timed_out = report["timed_out"]
        .as_bool()
        .ok_or_else(|| invalid("missing Kani timeout status"))?;
    let oom_killed = report["oom_killed"]
        .as_bool()
        .ok_or_else(|| invalid("missing Kani OOM status"))?;
    if report["exit_code"].as_i64().is_none() {
        return Err(invalid("missing Kani process exit status"));
    }
    let artifacts = kani_artifacts(evidence)?;
    let raw_report: Value = serde_json::from_slice(
        artifacts
            .get("result.json")
            .ok_or_else(|| invalid("missing Kani result artifact"))?,
    )?;
    if &raw_report != report {
        return Err(invalid("Kani report differs from its preserved artifact"));
    }
    let listed = report["artifacts"]
        .as_array()
        .ok_or_else(|| invalid("missing Kani artifact manifest"))?;
    let mut names = BTreeSet::new();
    for item in listed {
        let name = item["name"]
            .as_str()
            .ok_or_else(|| invalid("invalid artifact name"))?;
        if !names.insert(name) || name == "result.json" {
            return Err(invalid("duplicate or recursive artifact manifest"));
        }
        let bytes = artifacts
            .get(name)
            .ok_or_else(|| invalid("missing declared artifact"))?;
        if item["bytes"].as_u64() != Some(bytes.len() as u64)
            || item["sha256"].as_str() != Some(&sha256_hex(bytes))
        {
            return Err(invalid("Kani artifact digest or length mismatch"));
        }
    }
    if !names.contains("stdout.log")
        || !names.contains("stderr.log")
        || names.len() + 1 != artifacts.len()
    {
        return Err(invalid("incomplete Kani artifact manifest"));
    }
    // Outcome classification is an evidence claim even when the buyer requested
    // another outcome. Acceptance controls the task, not how much to verify.
    match outcome {
        "proven" | "refuted" => {
            if timed_out || oom_killed || report["evidence_complete"] == false {
                return Err(invalid(
                    "a resource failure or incomplete evidence cannot establish a Kani proof",
                ));
            }
            let export: Value = serde_json::from_slice(
                artifacts
                    .get("kani-export.json")
                    .ok_or_else(|| invalid("proof has no raw Kani export"))?,
            )?;
            validate_completed_export(&spec.plan, outcome, &export, report["exit_code"].as_i64())?;
        }
        "timeout" | "out_of_memory" => {
            let outer_failure = if outcome == "timeout" {
                timed_out
            } else {
                oom_killed
            };
            if !outer_failure {
                let export: Value =
                    serde_json::from_slice(artifacts.get("kani-export.json").ok_or_else(
                        || invalid("resource outcome has no outer or solver evidence"),
                    )?)?;
                validate_resource_export(&spec.plan, &export, outcome)?;
            }
        }
        _ if timed_out || oom_killed => {
            return Err(invalid(
                "resource failure is mislabeled as a Kani tool result",
            ))
        }
        _ => {}
    }
    Ok(accepted)
}

/// Enforce job/source/patch and verdict bindings before paying for a Kani
/// attempt, including a directly purchased verification order.
pub fn validate_kani_check_output(
    job: &crate::JobEnvelopePayload,
    output: &[covenant_mcp::Content],
) -> Result<Option<bool>, ProtocolError> {
    let spec = crate::parse_agent_check(&job.input)?;
    let Some(kani) = &spec.acceptance.kani else {
        return Ok(None);
    };
    let evidence: Vec<_> = output
        .iter()
        .filter_map(|content| match content {
            covenant_mcp::Content::Json { value } => value.get("kani_execution"),
            _ => None,
        })
        .collect();
    if evidence.len() != 1 {
        return Err(invalid("Kani output requires exactly one evidence block"));
    }
    let e = evidence[0];
    if e["job_id"] != job.job_id.to_string()
        || e["task_job_id"] != spec.task_job_id.to_string()
        || e["repo_commit"] != spec.repo.commit()
        || e["patch_sha256"] != spec.patch_sha256
        || e["hidden_sha256"] != serde_json::to_value(&spec.acceptance.hidden_sha256)?
        || e["tree_hash"]
            .as_str()
            .is_none_or(|h| h.len() != 40 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
        || e["image_id"].as_str().is_none_or(|h| {
            h.len() != 71
                || !h.starts_with("sha256:")
                || !h[7..].bytes().all(|b| b.is_ascii_hexdigit())
        })
    {
        return Err(invalid(
            "Kani output does not bind the exact job, source, patch and image",
        ));
    }
    if spec.acceptance.image.starts_with("sha256:") && e["image_id"] != spec.acceptance.image {
        return Err(invalid(
            "Kani resolved image differs from its ordered config digest",
        ));
    }
    let accepted = validate_kani_evidence(kani, &spec.acceptance.image, e)?;
    if output
        .iter()
        .filter(|content| {
            matches!(content,
        covenant_mcp::Content::Json { value } if value.get("agent_check_verdict").is_some())
        })
        .count()
        != 1
    {
        return Err(invalid("Kani output requires exactly one verdict block"));
    }
    let verdict = crate::parse_agent_check_verdict(output)?;
    if verdict.task_job_id != spec.task_job_id
        || verdict.patch_sha256 != spec.patch_sha256
        || verdict.skill != crate::AgentSkill::CodeChange
        || verdict.passed != accepted
    {
        return Err(invalid(
            "Kani verdict contradicts its ordered proof evidence",
        ));
    }
    if accepted {
        let mut commands = spec.acceptance.setup.clone();
        commands.push(kani_command_label(&kani.plan));
        if verdict
            .commands
            .iter()
            .map(|c| c.command.clone())
            .collect::<Vec<_>>()
            != commands
        {
            return Err(invalid(
                "passing Kani verdict did not execute its ordered plan",
            ));
        }
    }
    Ok(Some(accepted))
}

// Kani can time out or exhaust memory inside CBMC while its outer process
// exits normally. Authenticate that classification from the selected harness.
fn validate_resource_export(
    plan: &KaniPlan,
    doc: &Value,
    outcome: &str,
) -> Result<(), ProtocolError> {
    let fail = || invalid("Kani export does not establish the reported resource failure");
    if doc["metadata"]["version"] != "1.0" || doc["metadata"]["kani_version"] != "0.68.0" {
        return Err(fail());
    }
    let one = |field: &Value, key: &str| -> Result<Value, ProtocolError> {
        let entries = field.as_array().ok_or_else(fail)?;
        if entries.len() != 1 || entries[0][key] != plan.harness {
            return Err(fail());
        }
        Ok(entries[0].clone())
    };
    one(&doc["harness_metadata"], "pretty_name")?;
    let result = one(&doc["verification_results"]["results"], "harness_id")?;
    let errors = one(&doc["error_details"], "harness_id")?;
    let summary = &doc["verification_results"]["summary"];
    if result["status"] != "Failure"
        || errors["has_errors"] != true
        || errors["exit_status"] != outcome
        || summary["status"] != "completed"
        || summary["total_harnesses"].as_u64() != Some(1)
        || summary["executed"].as_u64() != Some(1)
        || summary["successful"].as_u64() != Some(0)
        || summary["failed"].as_u64() != Some(1)
    {
        return Err(fail());
    }
    Ok(())
}

/// Check the underlying solver export as well as the runner's classification.
fn validate_completed_export(
    plan: &KaniPlan,
    outcome: &str,
    doc: &Value,
    exit: Option<i64>,
) -> Result<(), ProtocolError> {
    let fail = || invalid("Kani export does not establish the reported property outcome");
    if outcome == "refuted" && plan.expected_assertion.is_none() {
        return Err(fail());
    }
    if doc["metadata"]["version"] != "1.0" || doc["metadata"]["kani_version"] != "0.68.0" {
        return Err(fail());
    }
    let one = |field: &Value| -> Result<Value, ProtocolError> {
        let entries = field.as_array().ok_or_else(fail)?;
        if entries.len() != 1 {
            return Err(fail());
        }
        Ok(entries[0].clone())
    };
    let metadata = one(&doc["harness_metadata"])?;
    if metadata["pretty_name"] != plan.harness || metadata["attributes"]["should_panic"] != false {
        return Err(fail());
    }
    let result = one(&doc["verification_results"]["results"])?;
    let errors = one(&doc["error_details"])?;
    let properties = one(&doc["property_details"])?;
    for entry in [&result, &errors, &properties] {
        if entry["harness_id"] != plan.harness {
            return Err(fail());
        }
    }
    let summary = &doc["verification_results"]["summary"];
    if summary["status"] != "completed"
        || summary["total_harnesses"].as_u64() != Some(1)
        || summary["executed"].as_u64() != Some(1)
    {
        return Err(fail());
    }
    let checks = result["checks"].as_array().ok_or_else(fail)?;
    if checks.is_empty() {
        return Err(fail());
    }
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut reachable_assertions = 0;
    let mut failed_assertions = 0;
    let mut ids = BTreeSet::new();
    for check in checks {
        let id = check["id"].as_u64().filter(|id| *id > 0).ok_or_else(fail)?;
        if !ids.insert(id) || check["description"].as_str().is_none() {
            return Err(fail());
        }
        let status = check["status"].as_str().ok_or_else(fail)?;
        let category = check["category"].as_str().ok_or_else(fail)?;
        let counter = match status {
            "Success" => "passed",
            "Failure" => "failed",
            "Unreachable" => "unreachable",
            "Satisfied" => "satisfied",
            "Unsatisfiable" => "unsatisfiable",
            _ => return Err(fail()),
        };
        *counts.entry(counter).or_default() += 1;
        if category == "assertion" && status == "Success" {
            reachable_assertions += 1;
        }
        if category != "cover" && !["Success", "Unreachable"].contains(&status) {
            if outcome != "refuted"
                || status != "Failure"
                || category != "assertion"
                || check["description"].as_str() != plan.expected_assertion.as_deref()
            {
                return Err(fail());
            }
            failed_assertions += 1;
        }
        if category == "cover" && !["Satisfied", "Unsatisfiable", "Unreachable"].contains(&status) {
            return Err(fail());
        }
        if outcome == "proven"
            && category != "cover"
            && status == "Unreachable"
            && plan.required_covers.is_empty()
        {
            return Err(fail());
        }
    }
    let p = &properties["property_details"];
    for key in [
        "passed",
        "failed",
        "unreachable",
        "undetermined",
        "solver_error",
        "satisfied",
        "unsatisfiable",
        "covered",
        "uncovered",
    ] {
        if p[key].as_u64() != Some(*counts.get(key).unwrap_or(&0)) {
            return Err(fail());
        }
    }
    if p["total_properties"].as_u64() != Some(checks.len() as u64) {
        return Err(fail());
    }
    for cover in &plan.required_covers {
        let matching: Vec<_> = checks
            .iter()
            .filter(|c| c["category"] == "cover" && c["description"] == *cover)
            .collect();
        if matching.len() != 1 || matching[0]["status"] != "Satisfied" {
            return Err(fail());
        }
    }
    if outcome == "proven" {
        if exit != Some(0)
            || result["status"] != "Success"
            || errors["has_errors"] != false
            || summary["successful"].as_u64() != Some(1)
            || summary["failed"].as_u64() != Some(0)
            || reachable_assertions == 0
            || failed_assertions != 0
        {
            return Err(fail());
        }
    } else if exit.is_none_or(|e| e <= 0 || e >= 125)
        || result["status"] != "Failure"
        || summary["successful"].as_u64() != Some(0)
        || summary["failed"].as_u64() != Some(1)
        || failed_assertions == 0
        || errors["has_errors"] != true
        || errors["error_type"] != "assertion_failure"
        || errors["failed_properties_type"] != "PanicsOnly"
        || errors["exit_status"] != "properties_failed"
    {
        return Err(fail());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn cases() -> Vec<Value> {
        serde_json::from_str(include_str!("../tests/fixtures/kani-0.68-cases.json")).unwrap()
    }

    fn evidence(case: &Value) -> (KaniCheckSpec, String, Value) {
        let plan: KaniPlan = serde_json::from_value(case["plan"].clone()).unwrap();
        let spec = KaniCheckSpec {
            plan: plan.clone(),
            memory_mib: 28672,
            cpus: 6,
            scratch_mib: 8192,
            check_price_micro_usdc: 100_000,
        };
        let image = format!("sha256:{}", "a".repeat(64));
        // Deliberately claim success even for broken solver exports. The
        // portable validator must independently reject these false claims.
        let mut report = json!({"schema":"covenant.kani.result.v1", "harness":plan.harness,
            "outcome":plan.expected_outcome, "accepted":true, "reason":"fixture claim",
            "exit_code":case["execution"]["returncode"],
            "timed_out":case["execution"]["timed_out"],
            "oom_killed":case["execution"]["oom_killed"]});
        let mut artifacts = BTreeMap::from([
            ("stdout.log".to_string(), b"complete stdout\n".to_vec()),
            ("stderr.log".to_string(), Vec::new()),
            (
                "kani-export.json".to_string(),
                serde_json::to_vec(&case["document"]).unwrap(),
            ),
        ]);
        report["artifacts"] = json!(artifacts
            .iter()
            .map(|(name, bytes)| json!({
                "name":name, "bytes":bytes.len(), "sha256":sha256_hex(bytes)
            }))
            .collect::<Vec<_>>());
        artifacts.insert("result.json".into(), serde_json::to_vec(&report).unwrap());
        let raw_bytes: usize = artifacts.values().map(Vec::len).sum();
        let packed: BTreeMap<_, _> = artifacts
            .iter()
            .map(|(name, bytes)| {
                (
                    name,
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                )
            })
            .collect();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&serde_json::to_vec(&packed).unwrap())
            .unwrap();
        let compressed = gzip.finish().unwrap();
        let evidence = json!({"schema":"covenant.kani.execution.v1", "plan":plan,
            "image":image, "memory_mib":28672, "cpus":6, "scratch_mib":8192,
            "report":report, "artifacts_gzip_b64":base64::engine::general_purpose::STANDARD.encode(&compressed),
            "artifacts_sha256":sha256_hex(&compressed), "raw_bytes":raw_bytes});
        (spec, image, evidence)
    }

    #[test]
    fn kani_evidence_rejects_false_success_for_every_negative_solver_fixture() {
        for case in cases() {
            let (spec, image, e) = evidence(&case);
            spec.validate(&image, 1800).unwrap();
            let result = validate_kani_evidence(&spec, &image, &e);
            let should_accept = case["expected"] == case["plan"]["expected_outcome"];
            assert_eq!(
                result.is_ok_and(|v| v),
                should_accept,
                "case {}",
                case["name"]
            );
        }
    }

    #[test]
    fn kani_evidence_detects_artifact_plan_and_resource_tampering() {
        let (spec, image, e) = evidence(&cases()[0]);
        assert!(validate_kani_evidence(&spec, &image, &e).unwrap());
        for (field, value) in [
            ("memory_mib", json!(1024)),
            ("cpus", json!(1)),
            ("scratch_mib", json!(512)),
            ("image", json!("rust:latest")),
            ("raw_bytes", json!(0)),
            ("artifacts_sha256", json!("0".repeat(64))),
            ("artifacts_gzip_b64", json!("AAAA")),
        ] {
            let mut bad = e.clone();
            bad[field] = value;
            assert!(
                validate_kani_evidence(&spec, &image, &bad).is_err(),
                "{field}"
            );
        }
        let mut bad = e.clone();
        bad["report"]["accepted"] = json!(false);
        assert!(validate_kani_evidence(&spec, &image, &bad).is_err());
        let mut wrong_spec = spec.clone();
        wrong_spec.plan.harness.push_str("_other");
        assert!(validate_kani_evidence(&wrong_spec, &image, &e).is_err());
    }

    /// Re-sign the artifact manifest in a fixture after changing the report or
    /// export, so negative cases exercise semantics rather than a stale hash.
    fn repack(e: &mut Value, mut artifacts: BTreeMap<String, Vec<u8>>) {
        artifacts.remove("result.json");
        e["report"]["artifacts"] = json!(artifacts
            .iter()
            .map(|(name, bytes)| json!({
                "name":name, "bytes":bytes.len(), "sha256":sha256_hex(bytes)
            }))
            .collect::<Vec<_>>());
        artifacts.insert(
            "result.json".into(),
            serde_json::to_vec(&e["report"]).unwrap(),
        );
        e["raw_bytes"] = json!(artifacts.values().map(Vec::len).sum::<usize>());
        let packed: BTreeMap<_, _> = artifacts
            .iter()
            .map(|(name, bytes)| {
                (
                    name,
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                )
            })
            .collect();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&serde_json::to_vec(&packed).unwrap())
            .unwrap();
        let compressed = gzip.finish().unwrap();
        e["artifacts_sha256"] = json!(sha256_hex(&compressed));
        e["artifacts_gzip_b64"] =
            json!(base64::engine::general_purpose::STANDARD.encode(compressed));
    }

    #[test]
    fn conclusive_outcomes_are_validated_even_when_not_accepted() {
        let (mut spec, image, mut e) = evidence(&cases()[0]);
        spec.plan.expected_outcome = "refuted".into();
        spec.plan.expected_assertion = Some("assertion failed: balance_is_conserved".into());
        e["plan"] = serde_json::to_value(&spec.plan).unwrap();
        e["report"]["accepted"] = json!(false);
        let artifacts = kani_artifacts(&e).unwrap();
        repack(&mut e, artifacts.clone());
        // A completed proof is valid evidence even when the order wanted a
        // counterexample. It must never become a passing acceptance vote.
        assert!(!validate_kani_evidence(&spec, &image, &e).unwrap());
        let mut missing = e.clone();
        let mut no_export = artifacts.clone();
        no_export.remove("kani-export.json");
        repack(&mut missing, no_export);
        assert!(validate_kani_evidence(&spec, &image, &missing).is_err());
        for (field, value) in [
            ("timed_out", json!(true)),
            ("oom_killed", json!(true)),
            ("evidence_complete", json!(false)),
            ("exit_code", json!(137)),
        ] {
            let mut bad = e.clone();
            bad["report"][field] = value;
            repack(&mut bad, artifacts.clone());
            assert!(
                validate_kani_evidence(&spec, &image, &bad).is_err(),
                "{field}"
            );
        }
        let (mut spec, image, mut e) = evidence(&cases()[2]);
        spec.plan.expected_outcome = "proven".into();
        spec.plan.expected_assertion = None;
        e["plan"] = serde_json::to_value(&spec.plan).unwrap();
        e["report"]["accepted"] = json!(false);
        let artifacts = kani_artifacts(&e).unwrap();
        repack(&mut e, artifacts);
        assert!(
            validate_kani_evidence(&spec, &image, &e).is_err(),
            "an unrequested assertion failure must be reported as inconclusive"
        );
    }

    #[test]
    fn resource_results_require_outer_flags_or_matching_solver_evidence() {
        for index in [5, 6, 28, 29] {
            let case = &cases()[index];
            let (spec, image, mut e) = evidence(case);
            e["report"]["outcome"] = case["expected"].clone();
            e["report"]["accepted"] = json!(false);
            let artifacts = kani_artifacts(&e).unwrap();
            repack(&mut e, artifacts.clone());
            assert!(
                !validate_kani_evidence(&spec, &image, &e).unwrap(),
                "{}",
                case["name"]
            );
            let mut no_support = e.clone();
            no_support["report"]["timed_out"] = json!(false);
            no_support["report"]["oom_killed"] = json!(false);
            let mut no_export = artifacts.clone();
            no_export.remove("kani-export.json");
            repack(&mut no_support, no_export);
            assert!(validate_kani_evidence(&spec, &image, &no_support).is_err());
            let mut wrong_kind = e.clone();
            wrong_kind["report"]["outcome"] = json!(if case["expected"] == "timeout" {
                "out_of_memory"
            } else {
                "timeout"
            });
            repack(&mut wrong_kind, artifacts.clone());
            assert!(validate_kani_evidence(&spec, &image, &wrong_kind).is_err());
            if index < 7 {
                let mut mislabeled = e.clone();
                mislabeled["report"]["outcome"] = json!("tool_error");
                repack(&mut mislabeled, artifacts);
                assert!(validate_kani_evidence(&spec, &image, &mislabeled).is_err());
            }
        }
    }

    #[test]
    fn conclusive_export_rejects_malformed_ids_and_missing_descriptions() {
        for (field, value) in [
            ("id", json!("1")),
            ("id", json!(0)),
            ("id", json!(true)),
            ("description", Value::Null),
        ] {
            let mut case = cases()[0].clone();
            case["document"]["verification_results"]["results"][0]["checks"][0][field] = value;
            let (spec, image, e) = evidence(&case);
            assert!(
                validate_kani_evidence(&spec, &image, &e).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn typed_output_binds_the_job_hidden_files_image_and_unique_verdict() {
        let (kani, image, mut e) = evidence(&cases()[0]);
        let id = uuid::Uuid::new_v4();
        let identity = covenant_identity::LocalIdentity::generate("proof@test");
        let commit = "a".repeat(40);
        let patch_sha256 = sha256_hex(b"");
        let acceptance = crate::AcceptanceSpec {
            skill: crate::AgentSkill::CodeChange,
            image: image.clone(),
            commands: vec![],
            timeout_secs: 120,
            protected_paths: vec![],
            hidden_sha256: None,
            setup: vec![],
            kani: Some(kani.clone()),
        };
        let check = crate::AgentCheckSpec {
            task_job_id: id,
            repo: crate::RepoSource::Bundle {
                bundle_b64: "QUJD".into(),
                commit: commit.clone(),
            },
            acceptance: acceptance.clone(),
            patch_b64: String::new(),
            patch_sha256: patch_sha256.clone(),
            hidden: None,
            reproduction: None,
        };
        let job = crate::JobEnvelopePayload {
            job_id: id,
            buyer: identity.agent_id(),
            kind: crate::JobKind::AgentCheck,
            capability_requirement: crate::CapabilityRequirement {
                gpu_class: None,
                min_vram_gb: None,
                model_id: Some(acceptance.checker_model()),
                kind: crate::JobKind::AgentCheck,
                max_duration_secs: 120,
                min_reputation_bps: None,
            },
            input: vec![crate::agent_check_input(check).unwrap()],
            price_micro_usdc: 100_000,
            deadline_ms: 600_000,
            issued_at_ms: 1,
            referral_code: None,
            stream: false,
            idempotency: covenant_a2a::A2AIdempotency::new(
                covenant_a2a::A2ADuplicateSafety::Idempotent,
                id.to_string(),
            ),
        };
        e["job_id"] = json!(id);
        e["task_job_id"] = json!(id);
        e["repo_commit"] = json!(commit);
        e["patch_sha256"] = json!(patch_sha256);
        e["hidden_sha256"] = Value::Null;
        e["tree_hash"] = json!("b".repeat(40));
        e["image_id"] = json!(image);
        let verdict = crate::AgentCheckVerdict::new(
            id,
            patch_sha256,
            true,
            vec![],
            vec![crate::CommandOutcome {
                command: kani_command_label(&kani.plan),
                exit_code: 0,
                duration_ms: 1,
                timed_out: false,
                output_tail: String::new(),
            }],
        );
        let mut output = crate::agent_check_output(verdict).unwrap();
        output.push(covenant_mcp::Content::Json {
            value: json!({"kani_execution":e}),
        });
        assert_eq!(
            validate_kani_check_output(&job, &output).unwrap(),
            Some(true)
        );
        for (field, value) in [
            ("job_id", json!(uuid::Uuid::new_v4())),
            ("task_job_id", json!(uuid::Uuid::new_v4())),
            ("repo_commit", json!("0".repeat(40))),
            ("patch_sha256", json!("0".repeat(64))),
            ("hidden_sha256", json!("0".repeat(64))),
            ("image_id", json!(format!("sha256:{}", "b".repeat(64)))),
            ("tree_hash", json!("not-a-tree")),
        ] {
            let mut bad = output.clone();
            if let covenant_mcp::Content::Json { value: block } = &mut bad[1] {
                block["kani_execution"][field] = value;
            }
            assert!(validate_kani_check_output(&job, &bad).is_err(), "{field}");
        }
        for index in [0, 1] {
            let mut duplicate = output.clone();
            duplicate.push(output[index].clone());
            assert!(validate_kani_check_output(&job, &duplicate).is_err());
        }
    }

    #[test]
    fn plan_admission_rejects_inputs_the_pinned_runner_would_reject() {
        let (spec, _, _) = evidence(&cases()[0]);
        for harness in ["proofs:one", "proofs::", "1proof", "::proof"] {
            let mut p = spec.plan.clone();
            p.harness = harness.into();
            assert!(p.validate().is_err());
        }
        for path in [
            "space dir/Cargo.toml",
            "./Cargo.toml",
            "x//Cargo.toml",
            "/Cargo.toml",
        ] {
            let mut p = spec.plan.clone();
            p.manifest_path = path.into();
            assert!(p.validate().is_err());
        }
        for features in [
            vec!["x".into(), "x".into()],
            vec!["dep/feature".into()],
            vec!["x".repeat(65)],
        ] {
            let mut p = spec.plan.clone();
            p.features = features;
            assert!(p.validate().is_err());
        }
        let mut p = spec.plan.clone();
        p.features = vec!["feat+enabled".into()];
        assert!(p.validate().is_ok());
    }

    #[test]
    fn kani_plan_needs_bounds_and_nonvacuous_property() {
        let (spec, image, _) = evidence(&cases()[0]);
        assert!(spec.validate(&image, 1800).is_ok());
        assert!(spec.validate("rust:latest", 1800).is_err());
        let mut s = spec.clone();
        s.plan.required_covers.clear();
        assert!(s.validate(&image, 1800).is_err());
        let mut s = spec.clone();
        s.plan.manifest_path = "../Cargo.toml".into();
        assert!(s.validate(&image, 1800).is_err());
        let mut s = spec.clone();
        s.plan.timeout_secs = 7201;
        assert!(s.validate(&image, 7201).is_err());
        let mut s = spec.clone();
        s.scratch_mib = s.memory_mib;
        assert!(s.validate(&image, 1800).is_err());
        assert_ne!(
            kani_checker_model(&image, 28672, 6, 8192),
            kani_checker_model(&image, 1024, 6, 8192)
        );
    }
}
