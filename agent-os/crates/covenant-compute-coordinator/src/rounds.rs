//! Check-vote rounds: the checkers' own signed votes on an agent task,
//! counted by the settlement program on the MagicBlock rollup.
//!
//! When a deployment runs rounds, a task's payout needs two answers that
//! agree: the coordinator's decision over the verdicts it holds, and the
//! chain's count of the same verdicts as the checkers signed them. Either
//! one saying no means no payment. A round that cannot run at all leaves the
//! coordinator's decision standing, which is how every task settled before
//! rounds existed; the task record then carries no round.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::onchain_meter::{call_signer, LeaseMeterError, SidecarLeaseMeterConfig};

/// The most votes one round holds, as the program caps it.
pub const MAX_ROUND_VOTES: usize = 5;

/// One checker's vote as it signed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundVote {
    pub voter: String,
    pub passed: bool,
    pub signature: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundResult {
    Passed,
    Failed,
    /// A tie: the evidence settles nothing, so nobody is paid.
    Split,
    /// The round never closed.
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountedVote {
    pub voter: String,
    pub passed: bool,
}

/// What the chain counted for one task. Kept on the task record and shown
/// with its status, so a buyer can check the settle transaction, which logs
/// every vote with its signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundRecord {
    pub result: RoundResult,
    /// The round's account address on the settlement program.
    pub round: String,
    /// The L1 transaction that settled the round.
    pub signature: String,
    pub votes: Vec<CountedVote>,
}

#[async_trait]
pub trait VoteRounds: Send + Sync {
    /// Runs one task's round to the end: open, every vote, close, settle.
    /// Repeating a call finishes or reports the round an earlier call
    /// started.
    async fn run(
        &self,
        task_id: Uuid,
        patch_sha256: &str,
        votes: &[RoundVote],
    ) -> Result<RoundRecord, String>;

    fn describe(&self) -> String;
}

/// The real rounds, through the lease signer's `round` step: the same
/// binary, keys, program and rollup as the lease meter.
pub struct SidecarVoteRounds {
    config: Arc<SidecarLeaseMeterConfig>,
}

impl SidecarVoteRounds {
    pub fn new(config: SidecarLeaseMeterConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

#[async_trait]
impl VoteRounds for SidecarVoteRounds {
    async fn run(
        &self,
        task_id: Uuid,
        patch_sha256: &str,
        votes: &[RoundVote],
    ) -> Result<RoundRecord, String> {
        let request = serde_json::json!({
            "program_id": self.config.program_id,
            "er_validator": self.config.er_validator,
            "task_id": task_id.simple().to_string(),
            "patch_sha256": patch_sha256,
            "votes": votes,
        });
        call_signer::<RoundRecord>(&self.config, "round", task_id, request)
            .await
            .map_err(|e| match e {
                LeaseMeterError::Backend(m) => m,
                LeaseMeterError::Unresolved { message, .. } => message,
            })
    }

    fn describe(&self) -> String {
        format!(
            "sidecar rounds on {} via {}",
            self.config.program_id, self.config.er_validator
        )
    }
}

/// Counts every round itself and records what it was asked, for drills and
/// tests. A result forced with [`NoopVoteRounds::answer`] overrides the
/// count; [`NoopVoteRounds::fail`] makes every round unavailable.
#[derive(Default)]
pub struct NoopVoteRounds {
    asked: Mutex<Vec<(Uuid, String, Vec<RoundVote>)>>,
    forced: Mutex<Option<Result<RoundResult, String>>>,
}

impl NoopVoteRounds {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn answer(&self, result: RoundResult) {
        *self.forced.lock() = Some(Ok(result));
    }

    pub fn fail(&self, error: &str) {
        *self.forced.lock() = Some(Err(error.into()));
    }

    pub fn asked(&self) -> Vec<(Uuid, String, Vec<RoundVote>)> {
        self.asked.lock().clone()
    }
}

#[async_trait]
impl VoteRounds for NoopVoteRounds {
    async fn run(
        &self,
        task_id: Uuid,
        patch_sha256: &str,
        votes: &[RoundVote],
    ) -> Result<RoundRecord, String> {
        self.asked
            .lock()
            .push((task_id, patch_sha256.to_string(), votes.to_vec()));
        let result = match self.forced.lock().clone() {
            Some(forced) => forced?,
            None => {
                let passes = votes.iter().filter(|v| v.passed).count();
                match passes.cmp(&(votes.len() - passes)) {
                    std::cmp::Ordering::Greater => RoundResult::Passed,
                    std::cmp::Ordering::Less => RoundResult::Failed,
                    std::cmp::Ordering::Equal => RoundResult::Split,
                }
            }
        };
        Ok(RoundRecord {
            result,
            round: format!("noop-round-{}", task_id.simple()),
            signature: format!("noop-settle-{}", task_id.simple()),
            votes: votes
                .iter()
                .map(|v| CountedVote {
                    voter: v.voter.clone(),
                    passed: v.passed,
                })
                .collect(),
        })
    }

    fn describe(&self) -> String {
        "noop rounds (counted in process, nothing on chain)".into()
    }
}

/// The payout decision once the chain has counted: paid only when both
/// say pass, refunded as a failed check only when both say fail, and with
/// any disagreement, nobody paid and nobody faulted.
pub fn agreed(decided: Option<bool>, counted: RoundResult) -> Option<bool> {
    match (decided, counted) {
        (Some(true), RoundResult::Passed) => Some(true),
        (Some(false), RoundResult::Failed) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payout_needs_the_chain_and_the_coordinator_to_agree() {
        assert_eq!(agreed(Some(true), RoundResult::Passed), Some(true));
        assert_eq!(agreed(Some(false), RoundResult::Failed), Some(false));
        assert_eq!(agreed(Some(true), RoundResult::Failed), None);
        assert_eq!(agreed(Some(true), RoundResult::Split), None);
        assert_eq!(agreed(Some(false), RoundResult::Passed), None);
        assert_eq!(agreed(None, RoundResult::Passed), None);
        assert_eq!(agreed(Some(true), RoundResult::Open), None);
    }

    #[test]
    fn the_sidecar_answer_decodes_into_a_record() {
        let answer = r#"{"result":"split","round":"R","signature":"S","votes":[{"voter":"A","passed":true},{"voter":"B","passed":false}]}"#;
        let record: RoundRecord = serde_json::from_str(answer).unwrap();
        assert_eq!(record.result, RoundResult::Split);
        assert_eq!(record.votes.len(), 2);
    }
}
