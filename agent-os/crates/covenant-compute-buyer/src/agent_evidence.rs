//! Authenticate complete checker outputs without trusting a relayed pass bit.
use crate::{BuyerConfig, BuyerError};
use base64::Engine as _;
use covenant_a2a::A2ATaskStatus;
use covenant_compute_protocol::{
    check_commands, parse_agent_check, parse_agent_check_verdict, parse_agent_task,
    parse_agent_task_output, sha256_hex, visible_commands, AgentCheckVerdict, AgentEvidenceBundle,
    AgentJobEvidence, AgentSkill, JobKind,
};
use covenant_identity::LocalIdentity;
use covenant_mcp::Content;
use serde::Serialize;
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct VerifiedAgentEvidence {
    pub evidence: AgentEvidenceBundle,
    pub final_patch_accepted: bool,
    pub checks: Vec<VerifiedAgentCheck>,
    /// These are boundaries of offline verification, not optional checks.
    pub inventory_completeness_verified: bool,
    pub settlement_verified: bool,
}

#[derive(Debug, Serialize)]
pub struct VerifiedAgentCheck {
    pub job_id: Uuid,
    pub patch_sha256: String,
    pub matches_final_patch: bool,
    pub receipt_verified: bool,
    pub execution_status: Option<A2ATaskStatus>,
    pub vote_verified: bool,
    pub verdict: Option<AgentCheckVerdict>,
}

fn bad(message: impl Into<String>) -> BuyerError {
    BuyerError::Verification(message.into())
}
fn protocol<T>(r: Result<T, covenant_compute_protocol::ProtocolError>) -> Result<T, BuyerError> {
    r.map_err(|e| bad(e.to_string()))
}

fn patch_matches(encoded: &str, digest: &str) -> Result<(), BuyerError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| bad("invalid patch encoding"))?;
    if sha256_hex(&bytes) != digest {
        return Err(bad("patch bytes do not match their digest"));
    }
    Ok(())
}

fn one_block(output: &[Content], key: &str) -> Result<(), BuyerError> {
    if output
        .iter()
        .filter(|c| matches!(c, Content::Json { value } if value.get(key).is_some()))
        .count()
        != 1
    {
        return Err(bad(format!("expected exactly one {key} output block")));
    }
    Ok(())
}

fn verify_job(row: &AgentJobEvidence) -> Result<bool, BuyerError> {
    protocol(row.envelope.verify())?;
    protocol(row.envelope.payload.validate_input())?;
    match (&row.receipt, &row.output) {
        (Some(receipt), Some(output)) => {
            crate::verify_receipt(&row.envelope, receipt, output)?;
            if receipt.receipt.operator.pubkey_base58() != row.coordinator.assigned_operator {
                return Err(bad("receipt operator differs from coordinator assignment"));
            }
            Ok(true)
        }
        (None, None) => Ok(false),
        _ => Err(bad("receipt and complete output must be present together")),
    }
}

/// Verify signatures, exact input/output hashes and task/patch relationships.
/// Payout metadata and inventory completeness remain coordinator assertions.
pub fn verify_agent_evidence(
    evidence: AgentEvidenceBundle,
    expected_job_id: Uuid,
    expected_buyer_pubkey: &str,
) -> Result<VerifiedAgentEvidence, BuyerError> {
    let parent = &evidence.task;
    if evidence.schema_version != 1
        || parent.envelope.payload.job_id != expected_job_id
        || parent.envelope.payload.kind != JobKind::AgentTask
        || parent.envelope.payload.buyer.pubkey_base58() != expected_buyer_pubkey
    {
        return Err(bad("agent evidence names another version, task or buyer"));
    }
    if !verify_job(parent)? {
        return Err(bad("parent signed receipt/output is missing"));
    }
    if parent.receipt.as_ref().unwrap().receipt.status != A2ATaskStatus::Ok {
        return Err(bad("parent execution did not produce an authored artifact"));
    }
    let spec = protocol(parse_agent_task(&parent.envelope.payload.input))?;
    let output = parent.output.as_ref().unwrap();
    one_block(output, "agent_task_result")?;
    let built = protocol(parse_agent_task_output(output))?;
    patch_matches(&built.patch_b64, &built.patch_sha256)?;
    if built.base_commit != spec.repo.commit() {
        return Err(bad("parent output is on another base commit"));
    }
    let mut seen = HashSet::new();
    let mut checks = Vec::with_capacity(evidence.checks.len());
    for child in &evidence.checks {
        let id = child.envelope.payload.job_id;
        if id == expected_job_id
            || !seen.insert(id)
            || child.envelope.payload.kind != JobKind::AgentCheck
        {
            return Err(bad("duplicate or non-check child evidence"));
        }
        let receipt_verified = verify_job(child)?;
        let check = protocol(parse_agent_check(&child.envelope.payload.input))?;
        if check.task_job_id != expected_job_id
            || check.repo != spec.repo
            || check.acceptance != spec.acceptance
        {
            return Err(bad("child check does not match the signed parent contract"));
        }
        if check.patch_b64.is_empty() {
            return Err(bad("authorship check has no patch"));
        }
        patch_matches(&check.patch_b64, &check.patch_sha256)?;
        match (&spec.reproduction, &check.reproduction) {
            (Some(reference), Some(patch)) if reference.patch_sha256 == patch.patch_sha256 => {
                patch_matches(&patch.patch_b64, &patch.patch_sha256)?
            }
            (None, None) => {}
            _ => return Err(bad("child reproduction differs from parent commitment")),
        }
        let status = child.receipt.as_ref().map(|r| r.receipt.status.clone());
        let mut vote_verified = false;
        let verdict = if status == Some(A2ATaskStatus::Ok) {
            let content = child.output.as_ref().unwrap();
            one_block(content, "agent_check_verdict")?;
            let verdict = protocol(parse_agent_check_verdict(content))?;
            if verdict.task_job_id != expected_job_id
                || verdict.patch_sha256 != check.patch_sha256
                || verdict.skill != check.acceptance.skill
            {
                return Err(bad("child verdict does not match its signed check input"));
            }
            let checker = child
                .receipt
                .as_ref()
                .unwrap()
                .receipt
                .operator
                .pubkey_base58();
            if checker
                == parent
                    .receipt
                    .as_ref()
                    .unwrap()
                    .receipt
                    .operator
                    .pubkey_base58()
            {
                return Err(bad("the builder signed its own check"));
            }
            if verdict.vote_signature_b58.is_some() {
                if !verdict.vote_signed_by(&checker) {
                    return Err(bad("checker vote signature does not verify"));
                }
                vote_verified = true;
            }
            if check.acceptance.kani.is_some() {
                protocol(covenant_compute_protocol::validate_kani_check_output(
                    &child.envelope.payload,
                    content,
                ))?;
            }
            if verdict.passed {
                let ran = |v: &[covenant_compute_protocol::CommandOutcome]| {
                    v.iter().map(|o| o.command.clone()).collect::<Vec<_>>()
                };
                let visible = visible_commands(&check.acceptance)
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let every = check_commands(&check.acceptance, check.hidden.as_ref())
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let caught = ran(&verdict.baseline) == visible
                    && visible.starts_with(&ran(&verdict.commands));
                let exact = match check.acceptance.skill {
                    AgentSkill::CodeChange => ran(&verdict.commands) == every,
                    AgentSkill::CodeTests | AgentSkill::CodeFix => {
                        caught && ran(&verdict.fixed) == every
                    }
                    AgentSkill::CodeRepro => caught,
                };
                if !exact && check.acceptance.kani.is_none() {
                    return Err(bad("passing verdict did not execute the ordered commands"));
                }
            }
            Some(verdict)
        } else {
            None
        };
        checks.push(VerifiedAgentCheck {
            job_id: id,
            patch_sha256: check.patch_sha256.clone(),
            matches_final_patch: check.patch_sha256 == built.patch_sha256,
            receipt_verified,
            execution_status: status,
            vote_verified,
            verdict,
        });
    }
    let final_patch_accepted = checks
        .iter()
        .any(|c| c.matches_final_patch && c.verdict.as_ref().is_some_and(|v| v.passed));
    if !final_patch_accepted {
        return Err(bad(
            "no authenticated passing check accepts the final patch",
        ));
    }
    Ok(VerifiedAgentEvidence {
        evidence,
        checks,
        final_patch_accepted,
        inventory_completeness_verified: false,
        settlement_verified: false,
    })
}

pub async fn fetch_agent_evidence(
    http: &reqwest::Client,
    config: &BuyerConfig,
    buyer: &LocalIdentity,
    job_id: Uuid,
) -> Result<VerifiedAgentEvidence, BuyerError> {
    let path = format!("/federation/jobs/{job_id}/evidence");
    let at = crate::epoch_ms();
    let signature = protocol(covenant_compute_protocol::sign_read(buyer, &path, at))?;
    let response = http
        .get(format!(
            "{}{}",
            config.coordinator_url.trim_end_matches('/'),
            path
        ))
        .header(
            covenant_compute_protocol::READ_SIGNED_AT_HEADER,
            at.to_string(),
        )
        .header(covenant_compute_protocol::READ_SIGNATURE_HEADER, signature)
        .send()
        .await
        .map_err(|e| BuyerError::unreachable(&config.coordinator_url, "read agent evidence", &e))?;
    let value = crate::json_or_error("agent evidence", response).await?;
    let evidence: AgentEvidenceBundle =
        serde_json::from_value(value).map_err(|e| bad(format!("agent evidence decode: {e}")))?;
    verify_agent_evidence(evidence, job_id, &buyer.agent_id().pubkey_base58())
}
