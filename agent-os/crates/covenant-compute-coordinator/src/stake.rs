//! The operator stake gate.
//!
//! A deployment can require every operator to hold CVNT staked for its node
//! identity before it wins work. The stake is the settlement program's own
//! `StakePosition`: the node's pubkey is the position's `agent_key`, the
//! tokens sit in a vault only the program moves, and the slash authority can
//! send them to the treasury. Only stake still locked past a lease's window
//! and its dispute window counts, so the tokens are there when a dispute
//! lands.
//!
//! The chain is read off the matcher's path: when an operator registers, and
//! on a timer for every registered operator. The matcher reads the verdict
//! the last read left on the operator's record, so a stake withdrawn at
//! unlock stops winning work within one refresh. A failed read keeps the last
//! verdict, and an operator never read is unstaked, so the gate fails closed
//! for newcomers and does not drop proven supply on an RPC blip.
//!
//! A deployment can also make proven faults cost stake. The same two
//! verdicts that slash a posted bond, a canary judged wrong and a
//! redundancy minority, then take a fixed amount of CVNT to the treasury
//! through `covenant-compute-stake slash`, run as a sidecar so this crate
//! never holds the slash authority's key. A buyer dispute never reaches
//! it. Each slash is journaled before the signer runs and carries its
//! fault's id in an on-chain memo, so neither a replayed verdict nor a
//! retried signer takes the stake twice.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use covenant_audit::AuditKind;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use uuid::Uuid;

use crate::journal::{StakeSlashState, StakeSlashStatus};
use crate::state::CoordinatorState;

#[derive(Debug, Clone)]
pub struct StakeRequirement {
    pub program_id: String,
    pub rpc_url: String,
    /// In the stake mint's base units.
    pub min_amount: u64,
    /// How long a position must still be locked, from now, to count.
    pub min_lock_remaining_secs: u64,
    /// Set when a proven fault also costs stake.
    pub slashing: Option<StakeSlashing>,
}

#[derive(Debug, Clone)]
pub struct StakeSlashing {
    /// Base units taken per proven fault, capped by what the operator's
    /// largest live position holds.
    pub per_fault: u64,
    /// `covenant-compute-stake`.
    pub signer_binary: PathBuf,
    /// The protocol slash authority's keypair file.
    pub keypair_path: String,
}

/// `StakePosition`'s size on chain: the 8-byte discriminator, then
/// agent_key, owner, amount, lock_until, vault, active, bump.
const POSITION_LEN: usize = 122;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    pub owner: [u8; 32],
    pub amount: u64,
    pub lock_until: u64,
    pub active: bool,
}

fn position_discriminator() -> [u8; 8] {
    let digest = Sha256::digest(b"account:StakePosition");
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

pub fn decode_position(data: &[u8]) -> Option<Position> {
    if data.len() != POSITION_LEN || data[..8] != position_discriminator() {
        return None;
    }
    let u64_at = |offset: usize| {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&data[offset..offset + 8]);
        u64::from_le_bytes(bytes)
    };
    let mut owner = [0u8; 32];
    owner.copy_from_slice(&data[40..72]);
    Some(Position {
        owner,
        amount: u64_at(72),
        lock_until: u64_at(80),
        active: data[120] == 1,
    })
}

/// The stake that counts: active positions still locked
/// `min_lock_remaining_secs` past `now_secs`.
pub fn counted_stake(positions: &[Position], now_secs: u64, min_lock_remaining_secs: u64) -> u64 {
    let horizon = now_secs.saturating_add(min_lock_remaining_secs);
    positions
        .iter()
        .filter(|p| p.active && p.lock_until >= horizon)
        .fold(0u64, |total, p| total.saturating_add(p.amount))
}

/// The owners of the positions [`counted_stake`] counts, base58, deduplicated.
pub fn counted_owners(
    positions: &[Position],
    now_secs: u64,
    min_lock_remaining_secs: u64,
) -> Vec<String> {
    let horizon = now_secs.saturating_add(min_lock_remaining_secs);
    let mut owners: Vec<String> = positions
        .iter()
        .filter(|p| p.active && p.lock_until >= horizon)
        .map(|p| bs58::encode(p.owner).into_string())
        .collect();
    owners.sort();
    owners.dedup();
    owners
}

impl StakeRequirement {
    /// Every stake position naming this operator's node identity.
    pub async fn positions(
        &self,
        http: &reqwest::Client,
        operator_pubkey_b58: &str,
    ) -> Result<Vec<Position>, String> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getProgramAccounts",
            "params": [self.program_id, {
                "encoding": "base58",
                "commitment": "confirmed",
                "filters": [
                    {"dataSize": POSITION_LEN},
                    {"memcmp": {"offset": 0, "bytes": bs58::encode(position_discriminator()).into_string()}},
                    {"memcmp": {"offset": 8, "bytes": operator_pubkey_b58}},
                ],
            }],
        });
        let response = http
            .post(&self.rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("getProgramAccounts: {e}"))?;
        let value: Value = response
            .json()
            .await
            .map_err(|e| format!("getProgramAccounts: decode response: {e}"))?;
        parse_positions(&value)
    }
}

/// The positions in a `getProgramAccounts` answer, base58-encoded.
pub fn parse_positions(value: &Value) -> Result<Vec<Position>, String> {
    if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
        return Err(format!("getProgramAccounts: {error}"));
    }
    let accounts = value["result"]
        .as_array()
        .ok_or_else(|| format!("getProgramAccounts: no result in {value}"))?;
    accounts
        .iter()
        .map(|account| {
            let data = account["account"]["data"][0]
                .as_str()
                .ok_or("getProgramAccounts: account without data")?;
            let bytes = bs58::decode(data)
                .into_vec()
                .map_err(|e| format!("getProgramAccounts: bad base58: {e}"))?;
            decode_position(&bytes)
                .ok_or_else(|| "getProgramAccounts: not a stake position".to_string())
        })
        .collect()
}

/// Reads one operator's stake and records the verdict on its registry entry.
pub async fn refresh_operator(
    state: &CoordinatorState,
    http: &reqwest::Client,
    operator_pubkey_b58: &str,
) {
    let Some(requirement) = state.config().stake.as_ref() else {
        return;
    };
    let now_secs = crate::epoch_ms() / 1_000;
    match requirement.positions(http, operator_pubkey_b58).await {
        Ok(positions) => {
            let counted = counted_stake(&positions, now_secs, requirement.min_lock_remaining_secs);
            let staked = counted >= requirement.min_amount;
            state.registry().set_staked(operator_pubkey_b58, staked);
            state.registry().set_stake_owners(
                operator_pubkey_b58,
                counted_owners(&positions, now_secs, requirement.min_lock_remaining_secs),
            );
            if !staked {
                tracing::info!(
                    operator = %operator_pubkey_b58,
                    counted,
                    required = requirement.min_amount,
                    "operator stake below the requirement; not matched until it is staked"
                );
            }
        }
        Err(error) => tracing::warn!(
            operator = %operator_pubkey_b58,
            %error,
            "operator stake read failed; keeping the last verdict"
        ),
    }
}

/// Re-reads every registered operator's stake on `interval`.
pub fn spawn_periodic_stake_refresh(
    state: CoordinatorState,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let http = stake_client();
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            for (key, _) in state.registry().snapshot() {
                refresh_operator(&state, &http, &key).await;
            }
        }
    })
}

/// Takes stake for one proven fault, off the verdict's path. The attempt
/// is journaled first; a `slash_id` already on record is never sent again.
pub fn slash_for_fault(
    state: &CoordinatorState,
    slash_id: &str,
    job_id: Uuid,
    operator_pubkey_b58: &str,
    reason: &str,
) {
    let Some(slashing) = state
        .config()
        .stake
        .as_ref()
        .and_then(|stake| stake.slashing.as_ref())
    else {
        return;
    };
    if state.stake_slash(slash_id).is_some() {
        return;
    }
    let record = StakeSlashState {
        slash_id: slash_id.to_string(),
        operator_pubkey_b58: operator_pubkey_b58.to_string(),
        job_id,
        amount: slashing.per_fault,
        reason: reason.to_string(),
        status: StakeSlashStatus::Attempted,
        tx_signature: None,
        detail: String::new(),
    };
    if let Err(error) = state.record_stake_slash(record.clone()) {
        tracing::error!(
            %job_id,
            operator = %operator_pubkey_b58,
            %error,
            "stake slash could not be made durable, so it was not sent"
        );
        return;
    }
    tokio::spawn(drive_slash(state.clone(), record));
}

/// Drives every slash a previous run left `Attempted`, once `delay` has
/// passed: long enough that a transaction the last run sent has either
/// landed, and is found by its memo, or can no longer land.
pub fn resume_stake_slashes(state: &CoordinatorState, delay: Duration) {
    let pending = state.attempted_stake_slashes();
    if pending.is_empty() {
        return;
    }
    tracing::warn!(
        count = pending.len(),
        "stake slashes with no recorded outcome; driving them again after the resume delay"
    );
    for record in pending {
        let state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            drive_slash(state, record).await;
        });
    }
}

pub(crate) async fn drive_slash(state: CoordinatorState, mut record: StakeSlashState) {
    let Some(requirement) = state.config().stake.clone() else {
        return;
    };
    let Some(slashing) = requirement.slashing.as_ref() else {
        return;
    };
    let operator = record.operator_pubkey_b58.clone();
    let job_id = record.job_id;
    match run_slasher(&requirement, slashing, &record).await {
        SlashRun::Landed { signature, amount } => {
            record.status = StakeSlashStatus::Slashed;
            record.tx_signature = Some(signature.clone());
            if let Err(error) = state.record_stake_slash(record.clone()) {
                tracing::error!(%job_id, %operator, %error, "landed stake slash could not be journaled");
            }
            state
                .record_audit(AuditKind::ComputeStakeSlashed {
                    operator_pubkey_b58: operator.clone(),
                    amount,
                    job_id,
                    reason: record.reason.clone(),
                    tx_signature: signature.clone(),
                })
                .await;
            tracing::warn!(%job_id, %operator, ?amount, %signature, "operator CVNT stake slashed for a proven fault");
            refresh_operator(&state, &stake_client(), &operator).await;
        }
        SlashRun::Refused(detail) => {
            tracing::warn!(%job_id, %operator, %detail, "stake slash refused; nothing moved");
            record.status = StakeSlashStatus::Refused;
            record.detail = detail;
            if let Err(error) = state.record_stake_slash(record) {
                tracing::error!(%job_id, %operator, %error, "refused stake slash could not be journaled");
            }
        }
        SlashRun::Unresolved(detail) => {
            tracing::error!(
                %job_id,
                %operator,
                %detail,
                "stake slash outcome unknown; the next boot drives it again"
            );
            record.detail = detail;
            let _ = state.record_stake_slash(record);
        }
    }
}

enum SlashRun {
    /// `amount` is absent when a retry found the slash its earlier run
    /// already landed.
    Landed {
        signature: String,
        amount: Option<u64>,
    },
    /// Nothing was submitted.
    Refused(String),
    /// Something may have been submitted.
    Unresolved(String),
}

async fn run_slasher(
    requirement: &StakeRequirement,
    slashing: &StakeSlashing,
    record: &StakeSlashState,
) -> SlashRun {
    let reason_hash: String = Sha256::digest(record.slash_id.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let child = Command::new(&slashing.signer_binary)
        .arg("slash")
        .arg(&record.operator_pubkey_b58)
        .arg(record.amount.to_string())
        .args([
            "--reason",
            &reason_hash,
            "--program",
            &requirement.program_id,
        ])
        .env_clear()
        .env("COVENANT_COMPUTE_STAKE_RPC_URL", &requirement.rpc_url)
        .env("COVENANT_COMPUTE_STAKE_KEYPAIR", &slashing.keypair_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let child = match child {
        Ok(child) => child,
        Err(e) => {
            return SlashRun::Refused(format!(
                "spawn stake signer {:?}: {e}",
                slashing.signer_binary
            ))
        }
    };
    let output = match child.wait_with_output().await {
        Ok(output) => output,
        Err(e) => return SlashRun::Unresolved(format!("await stake signer: {e}")),
    };
    let answer = String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line).ok());
    match answer {
        Some(answer) if output.status.success() => match answer["signature"].as_str() {
            Some(signature) => SlashRun::Landed {
                signature: signature.to_string(),
                amount: answer["amount"].as_u64(),
            },
            None => SlashRun::Unresolved(format!(
                "stake signer answered without a signature: {answer}"
            )),
        },
        Some(answer) => {
            let error = answer["error"]
                .as_str()
                .unwrap_or("no error given")
                .to_string();
            if answer["stage"] == "not_submitted" {
                SlashRun::Refused(error)
            } else {
                SlashRun::Unresolved(error)
            }
        }
        None => SlashRun::Unresolved(format!(
            "stake signer exited {} without an answer: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

pub(crate) fn stake_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payout::MockPayout;
    use crate::reputation::NoReputation;
    use crate::state::CoordinatorConfig;
    use covenant_audit::InMemoryAuditLog;
    use covenant_identity::LocalIdentity;
    use std::sync::Arc;

    /// A shell stand-in for `covenant-compute-stake`: appends its argv to
    /// `calls` and answers with `body`.
    fn stub_signer(dir: &std::path::Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(format!("stake-{}.sh", Uuid::new_v4()));
        let calls = dir.join("calls");
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"$@\" >> {}\n{body}\n", calls.display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn slashing_state(signer_binary: PathBuf) -> CoordinatorState {
        let config = CoordinatorConfig {
            stake: Some(StakeRequirement {
                program_id: "program".into(),
                rpc_url: "http://127.0.0.1:9".into(),
                min_amount: 1_000,
                min_lock_remaining_secs: 0,
                slashing: Some(StakeSlashing {
                    per_fault: 50,
                    signer_binary,
                    keypair_path: "slash.json".into(),
                }),
            }),
            ..CoordinatorConfig::default()
        };
        CoordinatorState::new(
            LocalIdentity::generate("coordinator@test"),
            config,
            Arc::new(NoReputation),
            Arc::new(MockPayout::new()),
            Arc::new(InMemoryAuditLog::new()),
        )
    }

    fn attempted(state: &CoordinatorState, slash_id: &str) -> StakeSlashState {
        let record = StakeSlashState {
            slash_id: slash_id.into(),
            operator_pubkey_b58: "op".into(),
            job_id: Uuid::new_v4(),
            amount: 50,
            reason: "wrong answer".into(),
            status: StakeSlashStatus::Attempted,
            tx_signature: None,
            detail: String::new(),
        };
        state.record_stake_slash(record.clone()).unwrap();
        record
    }

    #[tokio::test]
    async fn a_landed_slash_is_recorded_audited_and_never_sent_again() {
        let dir = tempfile::tempdir().unwrap();
        let signer = stub_signer(dir.path(), r#"echo '{"signature":"sig-1","amount":50}'"#);
        let state = slashing_state(signer);
        let record = attempted(&state, "canary:j1:op");

        drive_slash(state.clone(), record.clone()).await;
        let landed = state.stake_slash("canary:j1:op").unwrap();
        assert_eq!(landed.status, StakeSlashStatus::Slashed);
        assert_eq!(landed.tx_signature.as_deref(), Some("sig-1"));

        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        let reason: String = Sha256::digest(b"canary:j1:op")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            calls.trim(),
            format!("slash op 50 --reason {reason} --program program")
        );
        let events = state.audit().recent(10).await.unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            AuditKind::ComputeStakeSlashed { amount: Some(50), tx_signature, .. } if tx_signature == "sig-1"
        )));

        slash_for_fault(&state, "canary:j1:op", record.job_id, "op", "wrong answer");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls"))
                .unwrap()
                .lines()
                .count(),
            1,
            "a slash on record is not sent again"
        );
    }

    #[tokio::test]
    async fn a_refusal_is_final_and_an_unknown_outcome_stays_attempted() {
        let dir = tempfile::tempdir().unwrap();
        let refused = stub_signer(
            dir.path(),
            r#"echo '{"error":"no live stake","stage":"not_submitted"}'; exit 1"#,
        );
        let state = slashing_state(refused);
        drive_slash(state.clone(), attempted(&state, "a")).await;
        let a = state.stake_slash("a").unwrap();
        assert_eq!(
            (a.status, a.detail.as_str()),
            (StakeSlashStatus::Refused, "no live stake")
        );

        let maybe = stub_signer(
            dir.path(),
            r#"echo '{"error":"timed out","stage":"maybe_submitted"}'; exit 1"#,
        );
        let state = slashing_state(maybe);
        drive_slash(state.clone(), attempted(&state, "b")).await;
        assert_eq!(
            state.stake_slash("b").unwrap().status,
            StakeSlashStatus::Attempted
        );

        let silent = stub_signer(dir.path(), "exit 1");
        let state = slashing_state(silent);
        drive_slash(state.clone(), attempted(&state, "c")).await;
        assert_eq!(
            state.stake_slash("c").unwrap().status,
            StakeSlashStatus::Attempted
        );
        assert_eq!(state.attempted_stake_slashes().len(), 1);
    }

    fn position_bytes(amount: u64, lock_until: u64, active: bool) -> Vec<u8> {
        let mut data = position_discriminator().to_vec();
        data.extend_from_slice(&[7u8; 32]);
        data.extend_from_slice(&[9u8; 32]);
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&lock_until.to_le_bytes());
        data.extend_from_slice(&[3u8; 32]);
        data.push(u8::from(active));
        data.push(254);
        data
    }

    #[test]
    fn a_position_decodes_at_the_program_offsets() {
        let position =
            decode_position(&position_bytes(1_000_000_000_000, 1_900_000_000, true)).unwrap();
        assert_eq!(position.owner, [9u8; 32]);
        assert_eq!(position.amount, 1_000_000_000_000);
        assert_eq!(position.lock_until, 1_900_000_000);
        assert!(position.active);
        assert!(decode_position(&position_bytes(1, 1, true)[..121]).is_none());
        let mut wrong = position_bytes(1, 1, true);
        wrong[0] ^= 1;
        assert!(
            decode_position(&wrong).is_none(),
            "another account type is not a stake"
        );
    }

    #[test]
    fn only_active_stake_locked_past_the_horizon_counts() {
        let now = 1_800_000_000;
        let day = 86_400;
        let positions = [
            decode_position(&position_bytes(600, now + 30 * day, true)).unwrap(),
            decode_position(&position_bytes(500, now + 30 * day, true)).unwrap(),
            decode_position(&position_bytes(900, now + day, true)).unwrap(),
            decode_position(&position_bytes(700, now + 30 * day, false)).unwrap(),
        ];
        assert_eq!(counted_stake(&positions, now, 2 * day), 1_100);
        assert_eq!(counted_stake(&positions, now, 0), 2_000);
        assert_eq!(counted_stake(&[], now, 0), 0);
    }

    #[test]
    fn an_rpc_answer_parses_into_positions() {
        let answer = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": [{"pubkey": "P", "account": {"data": [
                bs58::encode(position_bytes(400, 4_000_000_000, true)).into_string(), "base58"
            ]}}],
        });
        let positions = parse_positions(&answer).unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].amount, 400);
        assert!(parse_positions(&json!({"error": {"code": -32010}})).is_err());
        assert!(
            parse_positions(&json!({"result": [{"account": {"data": ["zzz", "base58"]}}]}))
                .is_err()
        );
    }
}
