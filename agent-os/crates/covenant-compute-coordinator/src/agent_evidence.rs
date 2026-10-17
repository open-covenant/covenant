//! Private, on-demand export of the original signed checker evidence.
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use covenant_compute_protocol::{
    parse_agent_check, AgentEvidenceBundle, AgentEvidenceMetadata, AgentEvidencePayout,
    AgentJobEvidence, JobKind,
};
use uuid::Uuid;

use crate::{
    http::{verify_signed_read, ApiError},
    jobs::{JobPhase, JobRecord},
    CoordinatorState,
};

pub(crate) async fn evidence(
    State(state): State<CoordinatorState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<AgentEvidenceBundle>, ApiError> {
    let task = state
        .jobs()
        .get(job_id)
        .ok_or_else(|| ApiError::NotFound("no such job".into()))?;
    verify_signed_read(
        &headers,
        &task.envelope.payload.buyer.pubkey_base58(),
        &format!("/federation/jobs/{job_id}/evidence"),
    )?;
    if task.envelope.payload.kind != JobKind::AgentTask {
        return Err(ApiError::BadRequest(
            "evidence export requires an agent task".into(),
        ));
    }
    // A child envelope contains the builder's entire patch. Releasing it before
    // the parent is paid would bypass the existing receipt route's privacy gate.
    if task.phase != JobPhase::Completed {
        return Err(ApiError::Conflict(
            "full agent evidence is available once the parent is paid".into(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut checks = Vec::with_capacity(task.check_jobs.len());
    for id in &task.check_jobs {
        let child = state
            .jobs()
            .get(*id)
            .ok_or_else(|| ApiError::Internal("linked check record is missing".into()))?;
        if !seen.insert(*id)
            || child.envelope.payload.job_id != *id
            || child.envelope.payload.kind != JobKind::AgentCheck
            || child.checks_task != Some(job_id)
            || parse_agent_check(&child.envelope.payload.input)
                .map(|s| s.task_job_id)
                .ok()
                != Some(job_id)
        {
            return Err(ApiError::Internal(
                "linked check record does not belong to this task".into(),
            ));
        }
        checks.push(project(&state, child));
    }
    Ok(Json(AgentEvidenceBundle {
        schema_version: 1,
        task: project(&state, task),
        checks,
    }))
}

fn project(state: &CoordinatorState, record: JobRecord) -> AgentJobEvidence {
    let payout = record.payout.as_ref().map(|p| AgentEvidencePayout {
        amount_micro_usdc: p.amount_micro_usdc,
        tx_signature: p.tx_signature.clone(),
        memo: record.receipt.as_ref().map(|r| r.payout_memo()),
    });
    let coordinator = AgentEvidenceMetadata {
        assigned_operator: record.operator_pubkey_b58,
        phase: record.phase.as_str().to_owned(),
        charged_micro_usdc: state
            .escrow()
            .settled_charge_micro_usdc(record.envelope.payload.job_id),
        payout,
    };
    AgentJobEvidence {
        envelope: record.envelope,
        receipt: record.receipt,
        output: record.output,
        coordinator,
    }
}
