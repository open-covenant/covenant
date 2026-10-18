//! Offline-only verifier: no identity, coordinator request, RPC, or purchase.
use covenant_compute_buyer::{verify_agent_evidence, verify_receipt};
use covenant_compute_protocol::{
    check_commands, parse_agent_check, parse_agent_check_verdict, validate_kani_check_output,
    AgentEvidenceBundle, AgentSkill, JobKind, SignedJobEnvelope, SignedWorkReceipt,
};
use covenant_mcp::Content;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{error::Error, fs};

#[derive(Deserialize)]
struct Record {
    envelope: SignedJobEnvelope,
    receipt: SignedWorkReceipt,
    output: Vec<Content>,
    expected: Expected,
    agent_evidence: Option<AgentEvidenceBundle>,
}
#[derive(Deserialize)]
struct Expected {
    job_id: String,
    buyer_pubkey: String,
    signed_repo_commit: String,
    tree_hash: String,
    role: String,
}
fn require(value: bool, reason: &str) -> Result<(), Box<dyn Error>> {
    if !value { return Err(reason.into()); }
    Ok(())
}
fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args().nth(1).ok_or("usage: verify-public-receipt RECORD.json")?;
    let r: Record = serde_json::from_slice(&fs::read(path)?)?;
    r.envelope.verify()?;
    let job = &r.envelope.payload;
    require(job.job_id.to_string() == r.expected.job_id, "unexpected job id")?;
    require(job.buyer.pubkey_base58() == r.expected.buyer_pubkey, "unexpected buyer")?;
    verify_receipt(&r.envelope, &r.receipt, &r.output)?;
    let receipt_status = serde_json::to_value(&r.receipt.receipt.status)?;
    let mut accepted = Value::Null;
    let mut final_patch_accepted = Value::Null;
    let mut kani = Value::Null;
    if job.kind == JobKind::AgentCheck {
        job.validate_input()?;
        let spec = parse_agent_check(&job.input)?;
        require(spec.repo.commit() == r.expected.signed_repo_commit, "source commit mismatch")?;
        require(spec.task_job_id == job.job_id, "public native check must be self-named")?;
        require(spec.acceptance.skill == AgentSkill::CodeChange, "unexpected native check skill")?;
        require(spec.hidden.is_none() && spec.reproduction.is_none(), "private or extra source overlays are not permitted")?;
        if r.expected.role == "verification" || r.expected.role == "canary" {
            require(spec.patch_b64.is_empty(), "freeze and verify the final source commit with no patch overlay")?;
        }
        if receipt_status == "ok" {
            require(r.output.iter().filter(|c| matches!(c, Content::Json { value } if value.get("agent_check_verdict").is_some())).count() == 1,
                    "expected exactly one check verdict")?;
            let v = parse_agent_check_verdict(&r.output)?;
            require(v.task_job_id == job.job_id && v.patch_sha256 == spec.patch_sha256 && v.skill == AgentSkill::CodeChange,
                    "check verdict does not match the exact order")?;
            if v.vote_signature_b58.is_some() {
                require(v.vote_signed_by(&r.receipt.receipt.operator.pubkey_base58()), "invalid checker vote")?;
            }
            let proof = validate_kani_check_output(job, &r.output)?;
            if proof.is_none() && v.passed {
                require(v.commands.iter().map(|c| c.command.as_str()).collect::<Vec<_>>() == check_commands(&spec.acceptance, None),
                        "passing generic verdict changed the ordered commands")?;
            }
            accepted = json!(v.passed);
            if spec.acceptance.kani.is_some() {
                let e = r.output.iter().find_map(|c| match c {
                    Content::Json { value } => value.get("kani_execution"), _ => None,
                }).ok_or("missing complete Kani execution")?;
                require(e["tree_hash"] == r.expected.tree_hash, "Kani executed another source tree")?;
                kani = json!({"harness":e["plan"]["harness"],"outcome":e["report"]["outcome"],
                    "accepted":proof,"image":e["image"],"image_id":e["image_id"],"tool_pin":e["tool_pin"],
                    "memory_mib":e["memory_mib"],"cpus":e["cpus"],"scratch_mib":e["scratch_mib"]});
            }
        }
    } else if job.kind == JobKind::AgentTask {
        if let Some(e) = r.agent_evidence {
            require(serde_json::to_value(&e.task.envelope)? == serde_json::to_value(&r.envelope)? &&
                serde_json::to_value(&e.task.receipt)? == serde_json::to_value(Some(&r.receipt))? &&
                serde_json::to_value(&e.task.output)? == serde_json::to_value(Some(&r.output))?, "agent inventory parent differs")?;
            let verified = verify_agent_evidence(e, job.job_id, &r.expected.buyer_pubkey)?;
            final_patch_accepted = json!(verified.final_patch_accepted);
        }
    } else { return Err("unsupported public evidence job kind".into()); }
    println!("{}", serde_json::to_string_pretty(&json!({
        "job_id":job.job_id,"buyer_pubkey":r.expected.buyer_pubkey,
        "envelope_signature_verified":true,"receipt_signature_verified":true,
        "exact_envelope_output_hashes_verified":true,"receipt_execution_status":receipt_status,
        "acceptance_passed":accepted,"kani":kani,"agent_final_patch_accepted":final_patch_accepted,
        "settlement_verified":false,"inventory_completeness_verified":false
    }))?);
    Ok(())
}
