//! Verification-only purchases: a checker executes the pinned source and
//! returns its signed verdict/report. The execution is paid even if it fails
//! the requested property; no artificial model-authored patch is needed.
use base64::Engine as _;
use covenant_compute_protocol::{
    agent_check_input, parse_agent_check_verdict, sha256_hex, AgentCheckSpec, AgentCheckVerdict,
    AgentSkill, JobKind,
};
use covenant_identity::LocalIdentity;
use uuid::Uuid;

use crate::{BuyerConfig, BuyerError, DispatchOutcome, JobRequest, PreparedTask};

pub struct CheckOutcome {
    pub execution: DispatchOutcome,
    pub verdict: AgentCheckVerdict,
}

fn request(
    task: &PreparedTask,
    patch: &[u8],
    id: Uuid,
    price: u64,
    deadline_ms: u64,
) -> Result<JobRequest, BuyerError> {
    if price == 0 {
        return Err(BuyerError::Protocol("check offer must be positive".into()));
    }
    if task.spec.acceptance.skill != AgentSkill::CodeChange || task.spec.reproduction.is_some() {
        return Err(BuyerError::Protocol(
            "direct checks require code.change with no reproduction".into(),
        ));
    }
    if deadline_ms < (u64::from(task.spec.acceptance.timeout_secs) + 300) * 1000 {
        return Err(BuyerError::Protocol(
            "check deadline must include its timeout plus 300 seconds of scheduling slack".into(),
        ));
    }
    if task
        .spec
        .acceptance
        .kani
        .as_ref()
        .is_some_and(|k| k.check_price_micro_usdc != price)
    {
        return Err(BuyerError::Protocol(
            "direct Kani check price must match its explicit offer".into(),
        ));
    }
    let spec = AgentCheckSpec {
        task_job_id: id,
        repo: task.spec.repo.clone(),
        acceptance: task.spec.acceptance.clone(),
        patch_b64: base64::engine::general_purpose::STANDARD.encode(patch),
        patch_sha256: sha256_hex(patch),
        hidden: task.hidden.clone(),
        reproduction: None,
    };
    let input = agent_check_input(spec).map_err(|e| BuyerError::Protocol(e.to_string()))?;
    Ok(JobRequest {
        kind: JobKind::AgentCheck,
        input: vec![input],
        model: Some(task.spec.acceptance.checker_model()),
        gpu_class: None,
        min_vram_gb: None,
        min_reputation_bps: None,
        price_micro_usdc: price,
        deadline_ms,
    })
}

pub fn check_preview(
    task: &PreparedTask,
    patch: &[u8],
    price: u64,
    deadline_ms: u64,
) -> Result<serde_json::Value, BuyerError> {
    // Validate the same payload without signing, uploading or reserving it.
    request(task, patch, Uuid::nil(), price, deadline_ms)?;
    Ok(
        serde_json::json!({"dry_run":true,"spends_nothing":true,"kind":"agent_check",
        "commit":task.spec.repo.commit(),"patch_sha256":sha256_hex(patch),
        "price_micro_usdc":price,"deadline_ms":deadline_ms,
        "checker_model":task.spec.acceptance.checker_model(),"acceptance":task.spec.acceptance,
        "payment_basis":"checker execution, including a reported failed property",
        "stored_bundle_bytes":task.bundle.as_ref().map_or(0,Vec::len)}),
    )
}

#[allow(clippy::too_many_arguments)] // Mirrors the caller's explicit signed purchase fields.
pub async fn check_pinned_task(
    http: &reqwest::Client,
    config: &BuyerConfig,
    identity: &LocalIdentity,
    task: PreparedTask,
    patch: &[u8],
    price: u64,
    deadline_ms: u64,
    idempotency_key: Option<&str>,
) -> Result<CheckOutcome, BuyerError> {
    let purchase = idempotency_key
        .map(|key| crate::agent_purchase::AgentPurchase::open(config, identity, key))
        .transpose()?;
    let id = purchase.as_ref().map_or_else(Uuid::new_v4, |p| p.job_id());
    let request = request(&task, patch, id, price, deadline_ms)?;
    let envelope = match &purchase {
        Some(p) => p.resolve(config, identity, request, id)?,
        None => crate::build_envelope(config, identity, request, false, id)?,
    };
    if let Some(bundle) = &task.bundle {
        crate::agent::store_bundle(http, config, identity, bundle).await?;
    }
    let execution = crate::dispatch_signed(http, config, identity, envelope).await?;
    if execution.output.iter().filter(|c| matches!(c, covenant_mcp::Content::Json { value } if value.get("agent_check_verdict").is_some())).count() != 1 {
        return Err(BuyerError::Verification("checker output must carry exactly one verdict".into()));
    }
    covenant_compute_protocol::validate_kani_check_output(
        &execution.envelope.payload,
        &execution.output,
    )
    .map_err(|e| BuyerError::Verification(e.to_string()))?;
    let verdict = parse_agent_check_verdict(&execution.output)
        .map_err(|e| BuyerError::Verification(e.to_string()))?;
    if verdict.task_job_id != id
        || verdict.patch_sha256 != sha256_hex(patch)
        || verdict.skill != AgentSkill::CodeChange
    {
        return Err(BuyerError::Verification(
            "signed checker verdict does not match this check order".into(),
        ));
    }
    if verdict.passed && task.spec.acceptance.kani.is_none() {
        let expected =
            covenant_compute_protocol::check_commands(&task.spec.acceptance, task.hidden.as_ref());
        let ran = verdict
            .commands
            .iter()
            .map(|c| c.command.as_str())
            .collect::<Vec<_>>();
        if ran != expected {
            return Err(BuyerError::Verification(
                "passing checker verdict omits or changes an ordered command".into(),
            ));
        }
    }
    if verdict.vote_signature_b58.is_some()
        && !verdict.vote_signed_by(&execution.receipt.receipt.operator.pubkey_base58())
    {
        return Err(BuyerError::Verification(
            "checker verdict vote does not match its receipt signer".into(),
        ));
    }
    if let Some(p) = purchase {
        p.settle(id)?;
    }
    Ok(CheckOutcome { execution, verdict })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_check_binds_its_own_id_and_empty_patch() {
        let args: crate::AgentArgs = serde_json::from_value(serde_json::json!({
            "repo":"https://github.com/example/repo", "commit":"a".repeat(40),
            "task":"verify", "accept":["true"], "check_image":"python:3.12-slim",
            "check_timeout_secs":60
        }))
        .unwrap();
        let task = crate::prepare_agent_task(&args).unwrap();
        let id = Uuid::new_v4();
        let r = request(&task, b"", id, 100000, 360000).unwrap();
        let spec = covenant_compute_protocol::parse_agent_check(&r.input).unwrap();
        assert_eq!(spec.task_job_id, id);
        assert_eq!(spec.patch_sha256, sha256_hex(b""));
        assert!(spec.patch_b64.is_empty());
        assert!(request(&task, b"", id, 100000, 359999).is_err());
    }
}
