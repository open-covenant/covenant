//! Coordination layer for a permissionless keeper *network*.
//!
//! When N independent keepers all see the same actionable state, they
//! must not all submit the same transaction — that wastes CU and
//! invites races. This module provides a deterministic, communication-
//! free leader election so every keeper computes the same priority
//! ordering from public state plus a known peer set.
//!
//! Properties (Kani-amenable in shape):
//!
//! - **Uniqueness** — for any `(action_key, window, peers)`, exactly
//!   one peer is the leader. Ties on the priority hash are broken
//!   deterministically by lexicographic order of keeper id.
//! - **Determinism** — `should_lead` is a pure function of public
//!   inputs only.
//! - **Rotation** — across slot windows, the leader for the same
//!   `action_key` rotates (no permanent leader, no permanent loser).
//! - **No livelock** — non-leaders skip with a deterministic counter
//!   bump; they do not wait, hold, or block on another keeper.
//!
//! This is the same liveness shape spec §21 ("preemptible close
//! ownership ... a strict total order ... no hold-and-wait or
//! equal-priority livelock") requires of the protocol's bankrupt-
//! close ledger — applied to the keeper network around it.

use crate::state::KeeperAction;

/// 32-byte keeper identity. In production this is the agent's wallet
/// pubkey; in tests, any fixed 32-byte array.
pub type KeeperId = [u8; 32];

/// Coordination key for an action — what two keepers must agree to
/// coordinate on. Same `(kind, target_asset)` → same key, so any two
/// keepers seeing the same actionable state arrive at the same key
/// without communicating.
pub fn action_key(action: &KeeperAction) -> u64 {
    let kind: u64 = match action {
        KeeperAction::PushAuthMark { .. } => 1,
        KeeperAction::CrankAsset { .. } => 2,
        KeeperAction::RecoveryForfeitLeg { .. } => 3,
        KeeperAction::RecoveryRebalanceReduce { .. } => 4,
        KeeperAction::RecoveryFinalize { .. } => 5,
    };
    let target = action.target_asset().map(|a| a as u64 + 1).unwrap_or(0);
    (kind << 32) | target
}

/// FNV-1a 64-bit hash. Deterministic, dependency-free, sufficient for
/// keeper-network priority — *not* cryptographic. (For a sybil-
/// resistant priority the keeper's SAP-anchored identity is the trust
/// root; this hash only orders honest participants.)
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut h = OFFSET;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

fn priority_hash(action_key_v: u64, keeper: &KeeperId, window: u64) -> u64 {
    let mut buf = [0u8; 48];
    buf[..32].copy_from_slice(keeper);
    buf[32..40].copy_from_slice(&action_key_v.to_le_bytes());
    buf[40..48].copy_from_slice(&window.to_le_bytes());
    fnv1a_64(&buf)
}

/// Deterministic leadership: `me` is the leader for `(action, window)`
/// iff `me` has the lowest priority hash among `peers ∪ {me}`. Ties on
/// the hash break lex on the keeper id, so the function is total and
/// uniqueness holds.
///
/// `window_slots == 0` collapses every slot into the same window —
/// useful in tests; in production a non-zero window rotates
/// leadership across windows.
///
/// Empty peer set (`peers` does not contain `me`, length 0) → always
/// the leader. Operators running a single keeper see no coordination
/// overhead.
pub fn should_lead(
    me: &KeeperId,
    action: &KeeperAction,
    slot: u64,
    window_slots: u64,
    peers: &[KeeperId],
) -> bool {
    let window = if window_slots == 0 { 0 } else { slot / window_slots };
    let k = action_key(action);
    let my_h = priority_hash(k, me, window);
    // Dedup peer list. Operators sometimes pass duplicated entries
    // (merged config files, registry overlaps); a duplicate would
    // count twice against `me` on an equal-hash compare, flipping
    // leadership unfairly. Bounded scan: in practice the keeper
    // network is O(10s), not O(thousands).
    let mut seen: Vec<KeeperId> = Vec::with_capacity(peers.len());
    for peer in peers {
        if peer == me {
            continue;
        }
        if seen.iter().any(|s| s == peer) {
            continue;
        }
        seen.push(*peer);
        let p_h = priority_hash(k, peer, window);
        if p_h < my_h {
            return false;
        }
        if p_h == my_h && peer < me {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::KeeperAction;

    fn kid(b: u8) -> KeeperId {
        [b; 32]
    }

    #[test]
    fn single_keeper_with_no_peers_always_leads() {
        let me = kid(1);
        let action = KeeperAction::CrankAsset { asset_index: 0 };
        assert!(should_lead(&me, &action, 100, 50, &[]));
        assert!(should_lead(&me, &action, 7_000_000, 1, &[]));
    }

    #[test]
    fn three_keepers_exactly_one_is_leader_in_any_window() {
        // Sweep multiple windows; uniqueness must hold in every one.
        let peers = [kid(1), kid(2), kid(3)];
        let action = KeeperAction::PushAuthMark {
            asset_index: 7,
            mark_e6: 1_000_000,
        };
        for slot in (0..1000).step_by(37) {
            let mut leaders = 0;
            for me in &peers {
                if should_lead(me, &action, slot, 50, &peers) {
                    leaders += 1;
                }
            }
            assert_eq!(leaders, 1, "exactly one leader per window (slot={slot})");
        }
    }

    #[test]
    fn leadership_rotates_across_windows() {
        let peers = [kid(11), kid(22), kid(33)];
        let action = KeeperAction::PushAuthMark {
            asset_index: 0,
            mark_e6: 1,
        };
        let mut who_leads = std::collections::HashSet::new();
        for window in 0..200u64 {
            for me in &peers {
                if should_lead(me, &action, window * 50, 50, &peers) {
                    who_leads.insert(*me);
                }
            }
        }
        assert!(
            who_leads.len() >= 2,
            "leadership must rotate across windows; saw {} unique leaders",
            who_leads.len()
        );
    }

    #[test]
    fn different_action_keys_can_have_different_leaders_in_the_same_window() {
        // Distinct (kind, target) → distinct priority hash → can elect
        // a different leader. The network thus naturally parallelizes
        // across non-conflicting work in the same window.
        let peers = [kid(5), kid(6), kid(7)];
        let crank = KeeperAction::CrankAsset { asset_index: 3 };
        let push = KeeperAction::PushAuthMark {
            asset_index: 3,
            mark_e6: 1,
        };
        let crank_leader = peers
            .iter()
            .find(|m| should_lead(m, &crank, 100, 50, &peers))
            .unwrap();
        let push_leader = peers
            .iter()
            .find(|m| should_lead(m, &push, 100, 50, &peers))
            .unwrap();
        // Not asserting they differ in every config (might collide
        // depending on the hash) — only that the function is total
        // and produces a leader for each independently.
        let _ = (crank_leader, push_leader);
    }

    #[test]
    fn determinism_pure_function_of_public_inputs() {
        let peers = [kid(1), kid(2), kid(3)];
        let action = KeeperAction::CrankAsset { asset_index: 0 };
        let a = should_lead(&kid(2), &action, 1234, 50, &peers);
        let b = should_lead(&kid(2), &action, 1234, 50, &peers);
        assert_eq!(a, b);
    }

    /// Peer-list deduplication: a duplicate entry must not affect
    /// the leader election. Without dedup, the same peer counted
    /// twice could push `me` out of leadership on a hash tie.
    #[test]
    fn duplicate_peers_do_not_change_leadership() {
        let action = KeeperAction::CrankAsset { asset_index: 1 };
        let me = kid(2);
        let peers_clean = vec![kid(1), kid(3)];
        let peers_dup = vec![kid(1), kid(1), kid(3), kid(3), kid(1)];
        let clean = should_lead(&me, &action, 100, 50, &peers_clean);
        let dup = should_lead(&me, &action, 100, 50, &peers_dup);
        assert_eq!(clean, dup, "duplicates must not change leadership");
    }

    /// Per-asset cranks have distinct action keys, so different
    /// peers can simultaneously lead cranks on different assets in
    /// the same window. This is the win of the per-asset crank fan-out:
    /// the keeper network parallelizes naturally across the asset axis.
    #[test]
    fn per_asset_cranks_have_distinct_keys() {
        let a0 = action_key(&KeeperAction::CrankAsset { asset_index: 0 });
        let a1 = action_key(&KeeperAction::CrankAsset { asset_index: 1 });
        let a2 = action_key(&KeeperAction::CrankAsset { asset_index: 2 });
        assert_ne!(a0, a1);
        assert_ne!(a1, a2);
        assert_ne!(a0, a2);
    }
}
