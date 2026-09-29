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

use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::state::CoordinatorState;

#[derive(Debug, Clone)]
pub struct StakeRequirement {
    pub program_id: String,
    pub rpc_url: String,
    /// In the stake mint's base units.
    pub min_amount: u64,
    /// How long a position must still be locked, from now, to count.
    pub min_lock_remaining_secs: u64,
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

pub(crate) fn stake_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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
