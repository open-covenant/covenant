//! The bounty-verification core: pin acceptance criteria at open time,
//! scope the worker to a capability grant, then verify a submission against
//! the criteria AND its action-log evidence, producing a signed-able verdict
//! and a PayAI release decision.
//!
//! Trust boundary: Covenant decides *whether the work was done*. PayAI holds
//! and moves the money. The [`ReleaseDecision`] names the PayAI instruction
//! and which role signs it — it never moves funds itself.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::trail::{sha256_hex, AuditTrail};
use crate::validate;

/// One machine-checkable acceptance condition. Acceptance is the AND of all
/// criteria. Subjective bounties carry zero criteria and lean on a quorum of
/// verifiers grounded by the action log (see [`verify`]'s `scope_ok`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Criterion {
    /// The submitted result bytes hash to this sha256.
    ResultSha256 { hex: String },
    /// The result text contains this substring.
    ResultContains { needle: String },
    /// The result parsed as JSON has `value` at `pointer` (RFC 6901).
    ResultJsonEquals { pointer: String, value: Value },
    /// The action log contains at least `min` actions under `action_prefix`.
    AuditActionAtLeast { action_prefix: String, min: u32 },
}

impl Criterion {
    fn validate(&self) -> Result<(), String> {
        match self {
            Criterion::ResultSha256 { hex } => validate::hash_hex("result_sha256", hex),
            Criterion::ResultContains { needle } => validate::field("result_contains", needle),
            Criterion::ResultJsonEquals { pointer, .. } => {
                validate::field("result_json_pointer", pointer)
            }
            Criterion::AuditActionAtLeast { action_prefix, .. } => {
                validate::field("audit_action_prefix", action_prefix)
            }
        }
    }

    fn evaluate(&self, result: &str, trail: &AuditTrail) -> bool {
        match self {
            Criterion::ResultSha256 { hex } => &sha256_hex(result.as_bytes()) == hex,
            Criterion::ResultContains { needle } => result.contains(needle),
            Criterion::ResultJsonEquals { pointer, value } => serde_json::from_str::<Value>(result)
                .ok()
                .and_then(|v| v.pointer(pointer).cloned())
                .is_some_and(|got| &got == value),
            Criterion::AuditActionAtLeast { action_prefix, min } => {
                let n = trail
                    .entries
                    .iter()
                    .filter(|e| crate::trail::action_in_grant(&e.action, action_prefix))
                    .count();
                n as u32 >= *min
            }
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Criterion::ResultSha256 { .. } => "result_sha256",
            Criterion::ResultContains { .. } => "result_contains",
            Criterion::ResultJsonEquals { .. } => "result_json_equals",
            Criterion::AuditActionAtLeast { .. } => "audit_action_at_least",
        }
    }
}

/// The acceptance criteria for a bounty. Its [`AcceptanceCriteria::hash`] is
/// pinned in [`BountyOpened`] at open time so the poster cannot move the
/// goalposts after work starts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AcceptanceCriteria {
    pub criteria: Vec<Criterion>,
}

impl AcceptanceCriteria {
    pub fn validate(&self) -> Result<(), String> {
        for c in &self.criteria {
            c.validate()?;
        }
        Ok(())
    }

    /// Canonical sha256 over the criteria (JCS). Stable across key ordering.
    pub fn hash(&self) -> String {
        let canonical = serde_jcs::to_vec(self).expect("criteria serialise");
        sha256_hex(&canonical)
    }
}

/// The record Covenant signs when a bounty opens: it pins the criteria hash
/// to the escrow so verification later is against the agreed-upon bar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BountyOpened {
    pub bounty_id: String,
    pub poster: String,
    /// PayAI escrow contract reference (the on-chain contract PDA / cid).
    pub escrow_ref: String,
    pub criteria_hash: String,
}

impl BountyOpened {
    pub fn new(
        bounty_id: impl Into<String>,
        poster: impl Into<String>,
        escrow_ref: impl Into<String>,
        criteria: &AcceptanceCriteria,
    ) -> Result<Self, String> {
        let bounty_id = bounty_id.into();
        let poster = poster.into();
        let escrow_ref = escrow_ref.into();
        validate::field("bounty_id", &bounty_id)?;
        validate::pubkey("poster", &poster)?;
        validate::field("escrow_ref", &escrow_ref)?;
        criteria.validate()?;
        Ok(Self {
            bounty_id,
            poster,
            escrow_ref,
            criteria_hash: criteria.hash(),
        })
    }
}

/// The capability grant a worker acts under for one bounty. Mirrors the
/// semantics of `covenant_types::Capability` (subject, allowed actions,
/// expiry) without coupling this verification crate to the signer; the
/// daemon mints the signed `Capability` from this.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BountyGrant {
    pub bounty_id: String,
    pub worker: String,
    /// Action labels (or namespaces) the worker may exercise for this bounty.
    pub allowed_actions: Vec<String>,
    pub expires_at_ms: Option<u64>,
}

impl BountyGrant {
    pub fn new(
        bounty_id: impl Into<String>,
        worker: impl Into<String>,
        allowed_actions: Vec<String>,
        expires_at_ms: Option<u64>,
    ) -> Result<Self, String> {
        let bounty_id = bounty_id.into();
        let worker = worker.into();
        validate::field("bounty_id", &bounty_id)?;
        validate::pubkey("worker", &worker)?;
        if allowed_actions.is_empty() {
            return Err("grant must allow at least one action".into());
        }
        for a in &allowed_actions {
            validate::field("allowed_action", a)?;
        }
        Ok(Self {
            bounty_id,
            worker,
            allowed_actions,
            expires_at_ms,
        })
    }
}

/// What the worker submits: the result plus its action-log evidence and the
/// audit root it claims was anchored on-chain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Submission {
    pub bounty_id: String,
    pub worker: String,
    pub result: String,
    pub audit_root: String,
    pub trail: AuditTrail,
}

impl Submission {
    pub fn validate(&self) -> Result<(), String> {
        validate::field("bounty_id", &self.bounty_id)?;
        validate::pubkey("worker", &self.worker)?;
        validate::hash_hex("audit_root", &self.audit_root)?;
        self.trail.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CriterionResult {
    pub label: String,
    pub pass: bool,
}

/// The verdict. Covenant signs this; PayAI release keys on `pass`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub bounty_id: String,
    pub worker: String,
    pub criteria_hash: String,
    pub audit_root: String,
    /// Recomputed trail root equals the claimed `audit_root` (no tampering).
    pub evidence_ok: bool,
    /// Every logged action was within the worker's grant.
    pub scope_ok: bool,
    pub criterion_results: Vec<CriterionResult>,
    pub out_of_scope: Vec<String>,
    pub pass: bool,
}

/// Verify a submission against the bounty. Three independent gates, all
/// required to pass:
///   1. evidence integrity — recomputed trail root == anchored `audit_root`;
///   2. scope — every logged action within `grant.allowed_actions`;
///   3. criteria — all acceptance criteria evaluate true.
///
/// `expected_criteria_hash` is the hash pinned at open time; a mismatch
/// (criteria changed under the worker) fails verification outright.
pub fn verify(
    grant: &BountyGrant,
    criteria: &AcceptanceCriteria,
    expected_criteria_hash: &str,
    submission: &Submission,
) -> Result<Verdict, String> {
    submission.validate()?;
    criteria.validate()?;

    if criteria.hash() != expected_criteria_hash {
        return Err("criteria hash does not match the hash pinned at bounty open".into());
    }
    if submission.bounty_id != grant.bounty_id {
        return Err("submission bounty_id does not match the grant".into());
    }
    if submission.worker != grant.worker {
        return Err("submission worker does not match the grant".into());
    }

    let evidence_ok = submission.trail.root() == submission.audit_root;

    let out_of_scope: Vec<String> = submission
        .trail
        .out_of_scope(&grant.allowed_actions)
        .into_iter()
        .map(str::to_string)
        .collect();
    let scope_ok = out_of_scope.is_empty();

    let criterion_results: Vec<CriterionResult> = criteria
        .criteria
        .iter()
        .map(|c| CriterionResult {
            label: c.label().to_string(),
            pass: c.evaluate(&submission.result, &submission.trail),
        })
        .collect();
    let criteria_ok = criterion_results.iter().all(|r| r.pass);

    Ok(Verdict {
        bounty_id: submission.bounty_id.clone(),
        worker: submission.worker.clone(),
        criteria_hash: expected_criteria_hash.to_string(),
        audit_root: submission.audit_root.clone(),
        evidence_ok,
        scope_ok,
        criterion_results,
        out_of_scope,
        pass: evidence_ok && scope_ok && criteria_ok,
    })
}

/// What PayAI should do, given a verdict. Covenant emits this; it never
/// signs a PayAI instruction. `release_payment` is buyer-or-admin with no
/// on-chain condition, so a pass routes through the poster (buyer); a fail
/// routes a refund through the escrow admin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "decision",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ReleaseDecision {
    /// Pay the worker. Poster (buyer) is already an authorized signer.
    Release {
        bounty_id: String,
        instruction: String,
        signer_role: String,
    },
    /// Refund the poster. `refund_buyer` is admin-only today.
    Refund {
        bounty_id: String,
        instruction: String,
        signer_role: String,
    },
}

impl ReleaseDecision {
    pub fn from_verdict(v: &Verdict) -> Self {
        if v.pass {
            ReleaseDecision::Release {
                bounty_id: v.bounty_id.clone(),
                instruction: "release_payment".into(),
                signer_role: "buyer".into(),
            }
        } else {
            ReleaseDecision::Refund {
                bounty_id: v.bounty_id.clone(),
                instruction: "refund_buyer".into(),
                signer_role: "admin".into(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trail::ActionEntry;

    const WORKER: &str = "9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH";
    const POSTER: &str = "96GsGo69kVfPZffudCexfnsSi5EuhAyd278MuJPwzGdu";

    fn entry(seq: u64, action: &str) -> ActionEntry {
        ActionEntry {
            seq,
            action: action.into(),
            detail_hash: "c".repeat(64),
        }
    }

    fn trail() -> AuditTrail {
        AuditTrail::new(vec![
            entry(0, "tool.call.fs.read"),
            entry(1, "tool.call.fs.write"),
        ])
    }

    fn criteria() -> AcceptanceCriteria {
        AcceptanceCriteria {
            criteria: vec![
                Criterion::ResultContains {
                    needle: "done".into(),
                },
                Criterion::AuditActionAtLeast {
                    action_prefix: "tool.call.fs".into(),
                    min: 2,
                },
            ],
        }
    }

    fn submission(result: &str, t: AuditTrail) -> Submission {
        let audit_root = t.root();
        Submission {
            bounty_id: "b1".into(),
            worker: WORKER.into(),
            result: result.into(),
            audit_root,
            trail: t,
        }
    }

    fn grant() -> BountyGrant {
        BountyGrant::new("b1", WORKER, vec!["tool.call.fs".into()], None).unwrap()
    }

    #[test]
    fn criteria_hash_is_stable_and_pins_at_open() {
        let c = criteria();
        let opened = BountyOpened::new("b1", POSTER, "escrow-cid-1", &c).unwrap();
        assert_eq!(opened.criteria_hash, c.hash());
        assert_eq!(c.hash().len(), 64);
    }

    #[test]
    fn clean_submission_passes_all_three_gates() {
        let c = criteria();
        let v = verify(&grant(), &c, &c.hash(), &submission("task done", trail())).unwrap();
        assert!(v.evidence_ok && v.scope_ok && v.pass, "{v:?}");
        assert!(v.criterion_results.iter().all(|r| r.pass));
        assert!(v.out_of_scope.is_empty());
    }

    #[test]
    fn tampered_evidence_fails_on_integrity() {
        let c = criteria();
        let mut s = submission("task done", trail());
        s.trail.entries.push(entry(2, "tool.call.fs.delete")); // edit after anchoring
        let v = verify(&grant(), &c, &c.hash(), &s).unwrap();
        assert!(!v.evidence_ok, "edited trail must break the anchored root");
        assert!(!v.pass);
    }

    #[test]
    fn out_of_scope_action_fails_even_if_criteria_met() {
        let c = criteria();
        let t = AuditTrail::new(vec![
            entry(0, "tool.call.fs.read"),
            entry(1, "tool.call.fs.write"),
            entry(2, "settlement.pay"), // not in grant
        ]);
        let v = verify(&grant(), &c, &c.hash(), &submission("task done", t)).unwrap();
        assert!(!v.scope_ok);
        assert_eq!(v.out_of_scope, vec!["settlement.pay"]);
        assert!(
            !v.pass,
            "in-scope criteria can't rescue an out-of-scope action"
        );
    }

    #[test]
    fn unmet_criterion_fails() {
        let c = criteria();
        // "task" lacks "done" → ResultContains fails.
        let v = verify(&grant(), &c, &c.hash(), &submission("task", trail())).unwrap();
        assert!(
            v.evidence_ok && v.scope_ok,
            "only the criterion should fail"
        );
        assert!(!v.pass);
        assert!(v
            .criterion_results
            .iter()
            .any(|r| r.label == "result_contains" && !r.pass));
    }

    #[test]
    fn moved_goalposts_are_rejected() {
        let opened_hash = criteria().hash();
        let changed = AcceptanceCriteria {
            criteria: vec![Criterion::ResultContains {
                needle: "anything".into(),
            }],
        };
        let err = verify(
            &grant(),
            &changed,
            &opened_hash,
            &submission("anything", trail()),
        )
        .unwrap_err();
        assert!(err.contains("criteria hash"), "{err}");
    }

    #[test]
    fn release_decision_follows_the_verdict() {
        let c = criteria();
        let pass = verify(&grant(), &c, &c.hash(), &submission("task done", trail())).unwrap();
        match ReleaseDecision::from_verdict(&pass) {
            ReleaseDecision::Release {
                instruction,
                signer_role,
                ..
            } => {
                assert_eq!(instruction, "release_payment");
                assert_eq!(signer_role, "buyer");
            }
            other => panic!("expected Release, got {other:?}"),
        }
        let fail = verify(&grant(), &c, &c.hash(), &submission("nope", trail())).unwrap();
        match ReleaseDecision::from_verdict(&fail) {
            ReleaseDecision::Refund {
                instruction,
                signer_role,
                ..
            } => {
                assert_eq!(instruction, "refund_buyer");
                assert_eq!(signer_role, "admin");
            }
            other => panic!("expected Refund, got {other:?}"),
        }
    }

    #[test]
    fn result_sha256_and_json_criteria_evaluate() {
        let body = r#"{"status":"ok","n":3}"#;
        let c = AcceptanceCriteria {
            criteria: vec![
                Criterion::ResultSha256 {
                    hex: sha256_hex(body.as_bytes()),
                },
                Criterion::ResultJsonEquals {
                    pointer: "/status".into(),
                    value: Value::String("ok".into()),
                },
            ],
        };
        let v = verify(&grant(), &c, &c.hash(), &submission(body, trail())).unwrap();
        assert!(v.pass, "{v:?}");
    }

    #[test]
    fn grant_and_submission_must_agree() {
        let c = criteria();
        let mut s = submission("task done", trail());
        s.bounty_id = "other".into();
        assert!(verify(&grant(), &c, &c.hash(), &s)
            .unwrap_err()
            .contains("bounty_id"));
    }
}
