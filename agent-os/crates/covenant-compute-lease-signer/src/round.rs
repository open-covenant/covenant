//! Check-vote rounds on the settlement program: the checkers' signed
//! verdicts on one agent task, recorded on the rollup and settled into L1
//! history.
//!
//! `round` runs a whole round. Like the lease steps it reads the chain before
//! every write, so a repeat finishes an earlier run's work instead of redoing
//! it: open and delegate on L1 in one transaction, one rollup transaction per
//! vote (the voter's Ed25519 check, then `record_vote`), close on the rollup,
//! wait for the commit, then settle on L1, which logs every vote with its
//! signature and returns the rent.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_sdk::signer::{keypair::Keypair, Signer};
use solana_sdk::transaction::Transaction;
use tokio::time::{sleep, Instant};

use crate::lease::{
    discriminator, DELEGATION_PROGRAM, MAGIC_CONTEXT, MAGIC_PROGRAM, SYSTEM_PROGRAM,
};
use crate::rpc::{Rpc, SendError};
use crate::steps::{parse_hex, parse_pubkey, refused, unresolved, Failure};

pub const ED25519_PROGRAM: Pubkey =
    Pubkey::from_str_const("Ed25519SigVerify111111111111111111111111111");
pub const INSTRUCTIONS_SYSVAR: Pubkey =
    Pubkey::from_str_const("Sysvar1nstructions1111111111111111111111111");
/// The program's `MAX_ROUND_VOTES`.
pub const MAX_VOTES: usize = 5;

const VOTE_DOMAIN: &[u8; 31] = b"covenant.compute.agent-vote.v1\n";
const VOTE_PASS: u8 = 1;
const ROUND_OPEN: u8 = 0;
/// Discriminator plus the zero-copy `VoteRound`.
const ROUND_LEN: usize = 640;

const PRIORITY_MICRO_LAMPORTS: u64 = 100_000;
const OPEN_COMPUTE_UNITS: u32 = 150_000;
const SETTLE_COMPUTE_UNITS: u32 = 20_000;
const L1_CONFIRM: Duration = Duration::from_secs(60);
const ROLLUP_CONFIRM: Duration = Duration::from_secs(20);
const PICKUP: Duration = Duration::from_secs(30);
const COMMIT_WAIT: Duration = Duration::from_secs(90);
/// How far back to look for the settle of a round an earlier run finished.
const HISTORY: usize = 10;

/// The bytes a checker signs for one verdict, as `record_vote` rebuilds them.
pub fn vote_message(task_id: &[u8; 16], patch: &[u8; 32], passed: bool) -> [u8; 80] {
    let mut message = [0u8; 80];
    message[..31].copy_from_slice(VOTE_DOMAIN);
    message[31..47].copy_from_slice(task_id);
    message[47..79].copy_from_slice(patch);
    message[79] = u8::from(passed);
    message
}

/// One task's round and the instructions that move it. The coordinator is in
/// the seeds, so knowing a task id is not enough to occupy the address.
#[derive(Debug, Clone, Copy)]
pub struct Round {
    pub program: Pubkey,
    pub coordinator: Pubkey,
    pub task_id: [u8; 16],
    pub address: Pubkey,
}

impl Round {
    pub fn derive(program: Pubkey, coordinator: Pubkey, task_id: [u8; 16]) -> Self {
        let address =
            Pubkey::find_program_address(&[b"round", coordinator.as_ref(), &task_id], &program).0;
        Self {
            program,
            coordinator,
            task_id,
            address,
        }
    }

    pub fn open(&self, payer: Pubkey, validator: Pubkey, patch: [u8; 32]) -> Instruction {
        let mut data = discriminator("global", "open_round").to_vec();
        data.extend_from_slice(&self.task_id);
        data.extend_from_slice(&patch);
        self.instruction(
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new_readonly(self.coordinator, true),
                AccountMeta::new_readonly(validator, false),
                AccountMeta::new(self.address, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            ],
            data,
        )
    }

    pub fn delegate(&self, payer: Pubkey, validator: Pubkey) -> Instruction {
        let buffer =
            Pubkey::find_program_address(&[b"buffer", self.address.as_ref()], &self.program).0;
        let record = Pubkey::find_program_address(
            &[b"delegation", self.address.as_ref()],
            &DELEGATION_PROGRAM,
        )
        .0;
        let metadata = Pubkey::find_program_address(
            &[b"delegation-metadata", self.address.as_ref()],
            &DELEGATION_PROGRAM,
        )
        .0;
        self.instruction(
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new_readonly(self.coordinator, true),
                AccountMeta::new_readonly(validator, false),
                AccountMeta::new(self.address, false),
                AccountMeta::new(buffer, false),
                AccountMeta::new(record, false),
                AccountMeta::new(metadata, false),
                AccountMeta::new_readonly(self.program, false),
                AccountMeta::new_readonly(DELEGATION_PROGRAM, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            ],
            discriminator("global", "delegate_round").to_vec(),
        )
    }

    pub fn record(&self, passed: bool) -> Instruction {
        let mut data = discriminator("global", "record_vote").to_vec();
        data.push(u8::from(passed));
        self.instruction(
            vec![
                AccountMeta::new(self.address, false),
                AccountMeta::new_readonly(self.coordinator, true),
                AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR, false),
            ],
            data,
        )
    }

    pub fn close(&self, payer: Pubkey) -> Instruction {
        self.instruction(
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(self.address, false),
                AccountMeta::new_readonly(self.coordinator, true),
                AccountMeta::new_readonly(MAGIC_PROGRAM, false),
                AccountMeta::new(MAGIC_CONTEXT, false),
            ],
            discriminator("global", "close_round").to_vec(),
        )
    }

    pub fn settle(&self, payer: Pubkey) -> Instruction {
        self.instruction(
            vec![
                AccountMeta::new(self.address, false),
                AccountMeta::new_readonly(self.coordinator, true),
                AccountMeta::new(payer, false),
            ],
            discriminator("global", "settle_round").to_vec(),
        )
    }

    fn instruction(&self, accounts: Vec<AccountMeta>, data: Vec<u8>) -> Instruction {
        Instruction {
            program_id: self.program,
            accounts,
            data,
        }
    }
}

/// The Ed25519 program instruction carrying one vote's signature, with every
/// offset pointing into its own data: the only layout `record_vote` accepts.
pub fn signature_check(voter: &Pubkey, signature: &[u8; 64], message: &[u8; 80]) -> Instruction {
    let here = u16::MAX;
    let mut data = vec![1u8, 0];
    // signature, key and message offsets, each with its instruction index
    for field in [48, here, 16, here, 112, message.len() as u16, here] {
        data.extend_from_slice(&field.to_le_bytes());
    }
    data.extend_from_slice(voter.as_ref());
    data.extend_from_slice(signature);
    data.extend_from_slice(message);
    Instruction {
        program_id: ED25519_PROGRAM,
        accounts: vec![],
        data,
    }
}

/// `VoteRound`, read from its zero-copy layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundState {
    pub coordinator: Pubkey,
    pub validator: Pubkey,
    pub payer: Pubkey,
    pub task_id: [u8; 16],
    pub patch: [u8; 32],
    pub votes: Vec<(Pubkey, bool)>,
    pub result: u8,
}

impl RoundState {
    pub fn decode(data: &[u8]) -> Result<Self, String> {
        if data.len() < ROUND_LEN || data[..8] != discriminator("account", "VoteRound") {
            return Err("account is not a VoteRound".into());
        }
        let count = usize::from(data[637]).min(MAX_VOTES);
        Ok(Self {
            coordinator: pubkey_at(data, 8),
            validator: pubkey_at(data, 40),
            payer: pubkey_at(data, 72),
            task_id: bytes_at(data, 104),
            patch: bytes_at(data, 120),
            votes: (0..count)
                .map(|i| (pubkey_at(data, 152 + 32 * i), data[632 + i] == VOTE_PASS))
                .collect(),
            result: data[638],
        })
    }
}

/// What a settled round logged, decoded from `RoundSettled`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledRound {
    pub task_id: [u8; 16],
    pub coordinator: Pubkey,
    pub patch: [u8; 32],
    pub result: u8,
    pub votes: Vec<(Pubkey, bool)>,
}

impl SettledRound {
    /// The `RoundSettled` event among one transaction's logs.
    pub fn from_logs(logs: &[String]) -> Option<Self> {
        let tag = discriminator("event", "RoundSettled");
        logs.iter()
            .filter_map(|line| line.strip_prefix("Program data: "))
            .filter_map(|b64| BASE64.decode(b64).ok())
            .find(|data| data.len() >= 575 && data[..8] == tag)
            .map(|data| {
                let count = usize::from(data[89]).min(MAX_VOTES);
                Self {
                    task_id: bytes_at(&data, 8),
                    coordinator: pubkey_at(&data, 24),
                    patch: bytes_at(&data, 56),
                    result: data[88],
                    votes: (0..count)
                        .map(|i| (pubkey_at(&data, 90 + 32 * i), data[250 + i] == VOTE_PASS))
                        .collect(),
                }
            })
    }
}

/// What the coordinator writes on stdin for `round`.
#[derive(Debug, Deserialize)]
pub struct RoundRequest {
    pub program_id: String,
    pub er_validator: String,
    /// The task's uuid as 32 hex characters.
    pub task_id: String,
    pub patch_sha256: String,
    pub votes: Vec<VoteRequest>,
}

#[derive(Debug, Deserialize)]
pub struct VoteRequest {
    pub voter: String,
    pub passed: bool,
    /// The checker's ed25519 signature on the vote message, base58.
    pub signature: String,
}

/// What the chain counted.
#[derive(Debug, Serialize)]
pub struct RoundOutcome {
    /// `passed`, `failed`, `split`, or `open` for a round that never closed.
    pub result: &'static str,
    /// The settle transaction, whose log carries every vote and signature.
    pub signature: String,
    pub round: String,
    pub votes: Vec<CountedVote>,
}

#[derive(Debug, Serialize)]
pub struct CountedVote {
    pub voter: String,
    pub passed: bool,
}

fn result_name(result: u8) -> &'static str {
    match result {
        1 => "passed",
        2 => "failed",
        3 => "split",
        _ => "open",
    }
}

struct Vote {
    voter: Pubkey,
    passed: bool,
    signature: [u8; 64],
}

pub struct RoundSession {
    renter: Keypair,
    coordinator: Keypair,
    l1: Rpc,
    rollup: Rpc,
    round: Round,
    validator: Pubkey,
    patch: [u8; 32],
    votes: Vec<Vote>,
}

impl RoundSession {
    /// Checks every vote's signature before anything is sent: a vote that
    /// does not verify here would only fail on the rollup.
    pub fn new(
        renter: Keypair,
        coordinator: Keypair,
        l1_url: &str,
        rollup_url: &str,
        request: &RoundRequest,
    ) -> Result<Self, Failure> {
        if renter.pubkey() == coordinator.pubkey() {
            return Err(refused(
                "the renter and coordinator keys are the same key; rounds keep them apart as \
                 leases do",
            ));
        }
        let program = parse_pubkey("program_id", &request.program_id)?;
        let validator = parse_pubkey("er_validator", &request.er_validator)?;
        let task_id: [u8; 16] = parse_hex("task_id", &request.task_id)?;
        let patch: [u8; 32] = parse_hex("patch_sha256", &request.patch_sha256)?;
        if request.votes.is_empty() || request.votes.len() > MAX_VOTES {
            return Err(refused(format!(
                "a round holds 1 to {MAX_VOTES} votes, not {}",
                request.votes.len()
            )));
        }
        let mut votes: Vec<Vote> = Vec::with_capacity(request.votes.len());
        for vote in &request.votes {
            let voter = parse_pubkey("voter", &vote.voter)?;
            if votes.iter().any(|v| v.voter == voter) {
                return Err(refused(format!("{voter} votes twice")));
            }
            let signature: [u8; 64] = bs58_64(&vote.signature)
                .ok_or_else(|| refused(format!("the vote by {voter} has no 64-byte signature")))?;
            let message = vote_message(&task_id, &patch, vote.passed);
            if !Signature::from(signature).verify(voter.as_ref(), &message) {
                return Err(refused(format!(
                    "the vote by {voter} does not verify against its verdict"
                )));
            }
            votes.push(Vote {
                voter,
                passed: vote.passed,
                signature,
            });
        }
        let round = Round::derive(program, coordinator.pubkey(), task_id);
        Ok(Self {
            renter,
            coordinator,
            l1: Rpc::new(l1_url),
            rollup: Rpc::new(rollup_url),
            round,
            validator,
            patch,
            votes,
        })
    }

    pub async fn run(&self) -> Result<RoundOutcome, Failure> {
        let identity = self.rollup.identity().await.map_err(refused)?;
        if identity != self.validator.to_string() {
            return Err(refused(format!(
                "the rollup endpoint answers as {identity}, not the pinned validator {}",
                self.validator
            )));
        }

        match self
            .l1
            .account(&self.round.address)
            .await
            .map_err(refused)?
        {
            None => {
                if let Some(outcome) = self.settled_earlier().await? {
                    return Ok(outcome);
                }
                self.send_l1(
                    &[
                        self.round
                            .open(self.renter.pubkey(), self.validator, self.patch),
                        self.round.delegate(self.renter.pubkey(), self.validator),
                    ],
                    OPEN_COMPUTE_UNITS,
                )
                .await
                .map_err(|e| sent("open_round", e))?;
            }
            Some(account) if account.owner == DELEGATION_PROGRAM => {}
            Some(account) if account.owner == self.round.program => {
                let state = RoundState::decode(&account.data).map_err(refused)?;
                self.check(&state)?;
                if state.result != ROUND_OPEN {
                    return self.settle(&state).await;
                }
                self.send_l1(
                    &[self.round.delegate(self.renter.pubkey(), self.validator)],
                    OPEN_COMPUTE_UNITS,
                )
                .await
                .map_err(|e| sent("delegate_round", e))?;
            }
            Some(account) => {
                return Err(refused(format!(
                    "the round address is owned by {}",
                    account.owner
                )))
            }
        }

        let state = self.await_pickup().await?;
        if state.result == ROUND_OPEN {
            for vote in &self.votes {
                if state.votes.iter().any(|(voter, _)| *voter == vote.voter) {
                    continue;
                }
                let message = vote_message(&self.round.task_id, &self.patch, vote.passed);
                self.send_rollup(&[
                    signature_check(&vote.voter, &vote.signature, &message),
                    self.round.record(vote.passed),
                ])
                .await
                .map_err(|e| sent("record_vote", e))?;
            }
            self.send_rollup(&[self.round.close(self.renter.pubkey())])
                .await
                .map_err(|e| sent("close_round", e))?;
        }
        let state = self.await_commit().await?;
        self.settle(&state).await
    }

    /// The round at this address must be the one this request describes.
    fn check(&self, state: &RoundState) -> Result<(), Failure> {
        if state.coordinator != self.round.coordinator
            || state.validator != self.validator
            || state.task_id != self.round.task_id
            || state.patch != self.patch
        {
            return Err(refused(
                "the round on chain was opened for another validator or patch",
            ));
        }
        Ok(())
    }

    async fn settle(&self, state: &RoundState) -> Result<RoundOutcome, Failure> {
        match self
            .send_l1(&[self.round.settle(state.payer)], SETTLE_COMPUTE_UNITS)
            .await
        {
            Ok(signature) => Ok(self.outcome(state.result, &state.votes, signature)),
            Err(SendError::Refused(message)) => match self.settled_earlier().await? {
                Some(outcome) => Ok(outcome),
                None => Err(refused(format!("settle_round: {message}"))),
            },
            Err(SendError::Unknown { signature, message }) => Err(unresolved(
                format!("settle_round: {message}"),
                Some(signature),
            )),
        }
    }

    /// A round whose account is gone was settled, and its settle logged the
    /// count.
    async fn settled_earlier(&self) -> Result<Option<RoundOutcome>, Failure> {
        let history = self
            .l1
            .recent_logs(&self.round.address, HISTORY)
            .await
            .map_err(refused)?;
        Ok(history.into_iter().find_map(|(signature, logs)| {
            SettledRound::from_logs(&logs)
                .filter(|s| {
                    s.task_id == self.round.task_id && s.coordinator == self.round.coordinator
                })
                .map(|s| self.outcome(s.result, &s.votes, signature))
        }))
    }

    fn outcome(&self, result: u8, votes: &[(Pubkey, bool)], signature: String) -> RoundOutcome {
        RoundOutcome {
            result: result_name(result),
            signature,
            round: self.round.address.to_string(),
            votes: votes
                .iter()
                .map(|(voter, passed)| CountedVote {
                    voter: voter.to_string(),
                    passed: *passed,
                })
                .collect(),
        }
    }

    async fn await_pickup(&self) -> Result<RoundState, Failure> {
        let deadline = Instant::now() + PICKUP;
        loop {
            if let Ok(Some(account)) = self.rollup.account(&self.round.address).await {
                if account.owner == self.round.program {
                    return RoundState::decode(&account.data).map_err(refused);
                }
            }
            if Instant::now() >= deadline {
                return Err(refused("the rollup has not picked up the round"));
            }
            sleep(Duration::from_secs(1)).await;
        }
    }

    async fn await_commit(&self) -> Result<RoundState, Failure> {
        let deadline = Instant::now() + COMMIT_WAIT;
        loop {
            if let Ok(Some(account)) = self.l1.account(&self.round.address).await {
                if account.owner == self.round.program {
                    if let Ok(state) = RoundState::decode(&account.data) {
                        if state.result != ROUND_OPEN {
                            return Ok(state);
                        }
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(unresolved(
                    format!(
                        "the round did not come back to L1 within {}s",
                        COMMIT_WAIT.as_secs()
                    ),
                    None,
                ));
            }
            sleep(Duration::from_secs(1)).await;
        }
    }

    /// The renter pays every fee and every rent; the coordinator co-signs.
    async fn send(
        &self,
        rpc: &Rpc,
        instructions: &[Instruction],
        timeout: Duration,
    ) -> Result<String, SendError> {
        let blockhash = rpc.latest_blockhash().await.map_err(SendError::Refused)?;
        let mut tx = Transaction::new_with_payer(instructions, Some(&self.renter.pubkey()));
        tx.try_sign(&[&self.renter, &self.coordinator], blockhash)
            .map_err(|e| SendError::Refused(format!("sign: {e}")))?;
        rpc.send_and_confirm(&tx, timeout).await
    }

    async fn send_l1(
        &self,
        instructions: &[Instruction],
        compute_units: u32,
    ) -> Result<String, SendError> {
        let mut all = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(compute_units),
            ComputeBudgetInstruction::set_compute_unit_price(PRIORITY_MICRO_LAMPORTS),
        ];
        all.extend_from_slice(instructions);
        self.send(&self.l1, &all, L1_CONFIRM).await
    }

    /// Rollup transactions are gasless, so they carry no compute budget.
    async fn send_rollup(&self, instructions: &[Instruction]) -> Result<String, SendError> {
        self.send(&self.rollup, instructions, ROLLUP_CONFIRM).await
    }
}

/// Nothing a round sends moves money, so an unknown outcome is only ever a
/// reason to read the chain again; the stage still says which it was.
fn sent(step: &str, error: SendError) -> Failure {
    match error {
        SendError::Refused(message) => refused(format!("{step}: {message}")),
        SendError::Unknown { signature, message } => {
            unresolved(format!("{step}: {message}"), Some(signature))
        }
    }
}

fn bs58_64(value: &str) -> Option<[u8; 64]> {
    let signature: Signature = value.parse().ok()?;
    Some(signature.into())
}

fn pubkey_at(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::new_from_array(bytes_at(data, offset))
}

fn bytes_at<const N: usize>(data: &[u8], offset: usize) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&data[offset..offset + N]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> Pubkey {
        Pubkey::new_from_array([n; 32])
    }

    #[test]
    fn a_vote_message_matches_the_program_layout() {
        let message = vote_message(&[7u8; 16], &[9u8; 32], true);
        assert_eq!(&message[..31], b"covenant.compute.agent-vote.v1\n");
        assert_eq!(&message[31..47], &[7u8; 16]);
        assert_eq!(&message[47..79], &[9u8; 32]);
        assert_eq!(message[79], 1);
    }

    #[test]
    fn a_signature_check_points_every_offset_at_its_own_data() {
        let message = vote_message(&[1u8; 16], &[2u8; 32], false);
        let ix = signature_check(&key(3), &[4u8; 64], &message);
        let d = &ix.data;
        let field = |at: usize| u16::from_le_bytes([d[at], d[at + 1]]);
        assert_eq!(
            (d[0], field(4), field(8), field(14)),
            (1, u16::MAX, u16::MAX, u16::MAX)
        );
        assert_eq!(&d[usize::from(field(6))..][..32], key(3).as_ref());
        assert_eq!(&d[usize::from(field(2))..][..64], &[4u8; 64]);
        assert_eq!(
            &d[usize::from(field(10))..][..usize::from(field(12))],
            &message
        );
    }

    #[test]
    fn a_round_decodes_from_its_zero_copy_layout() {
        let mut data = vec![0u8; ROUND_LEN];
        data[..8].copy_from_slice(&discriminator("account", "VoteRound"));
        data[8..40].copy_from_slice(key(1).as_ref());
        data[40..72].copy_from_slice(key(2).as_ref());
        data[72..104].copy_from_slice(key(3).as_ref());
        data[104..120].copy_from_slice(&[4u8; 16]);
        data[120..152].copy_from_slice(&[5u8; 32]);
        data[152..184].copy_from_slice(key(6).as_ref());
        data[184..216].copy_from_slice(key(7).as_ref());
        data[632] = 1;
        data[633] = 2;
        data[637] = 2;
        data[638] = 3;
        let state = RoundState::decode(&data).unwrap();
        assert_eq!(state.coordinator, key(1));
        assert_eq!(state.validator, key(2));
        assert_eq!(state.payer, key(3));
        assert_eq!(state.task_id, [4u8; 16]);
        assert_eq!(state.patch, [5u8; 32]);
        assert_eq!(state.votes, vec![(key(6), true), (key(7), false)]);
        assert_eq!(result_name(state.result), "split");
        data[0] ^= 1;
        assert!(RoundState::decode(&data).is_err());
    }

    #[test]
    fn a_settled_round_is_read_back_from_its_event() {
        let mut event = discriminator("event", "RoundSettled").to_vec();
        event.extend_from_slice(&[4u8; 16]);
        event.extend_from_slice(key(1).as_ref());
        event.extend_from_slice(&[5u8; 32]);
        event.extend_from_slice(&[1, 1]);
        for i in 0..MAX_VOTES {
            event.extend_from_slice(key(10 + i as u8).as_ref());
        }
        event.extend_from_slice(&[1, 0, 0, 0, 0]);
        event.extend_from_slice(&[0u8; 64 * MAX_VOTES]);
        let logs = vec![
            "Program log: Instruction: SettleRound".to_string(),
            format!("Program data: {}", BASE64.encode(&event)),
        ];
        let settled = SettledRound::from_logs(&logs).unwrap();
        assert_eq!(settled.task_id, [4u8; 16]);
        assert_eq!(settled.coordinator, key(1));
        assert_eq!(settled.result, 1);
        assert_eq!(settled.votes, vec![(key(10), true)]);
        assert!(SettledRound::from_logs(&logs[..1]).is_none());
    }

    #[test]
    fn a_request_is_refused_unless_every_vote_verifies() {
        let checker = Keypair::new();
        let task = "07".repeat(16);
        let patch = "09".repeat(32);
        let signature = checker.sign_message(&vote_message(&[7u8; 16], &[9u8; 32], true));
        let request = |passed: bool, signature: String| RoundRequest {
            program_id: key(1).to_string(),
            er_validator: key(2).to_string(),
            task_id: task.clone(),
            patch_sha256: patch.clone(),
            votes: vec![VoteRequest {
                voter: checker.pubkey().to_string(),
                passed,
                signature,
            }],
        };
        let session = |r: &RoundRequest| {
            RoundSession::new(Keypair::new(), Keypair::new(), "http://l1", "http://er", r)
        };
        assert!(session(&request(true, signature.to_string())).is_ok());
        // The same signature offered for the opposite verdict.
        assert!(session(&request(false, signature.to_string())).is_err());
        assert!(session(&request(true, "not-a-signature".into())).is_err());
    }
}
