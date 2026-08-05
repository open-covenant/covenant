//! Guardrails on programmable land.
//!
//! ClawVille is making land do things rather than just look like things: a
//! building an agent can wire to run a shop, take deliveries, or open a door.
//! The moment land is programmable, the owner needs to grant one capability
//! without granting the parcel. Wiring a building to restock its shelves must
//! not also let it open the vault.
//!
//! This is the pre-execution half of that. Before an action runs, the guard
//! answers allow, refuse, or needs-owner, with a reason, and the answer is
//! shaped so it can be logged into the same [`crate::trail`] the bounty
//! verifier already reads. It is pure compute: no key, no network, no funds.
//! ClawVille stays the executor and keeps its own permissions; this is the
//! layer that says no before the action reaches them, and that sits alongside
//! the OOBE workflow rather than replacing it.
//!
//! Two rules carry the whole design:
//!
//! - **Deny by default.** An action absent from the grant is refused. There is
//!   no implicit inheritance from owning the parcel or from a neighbouring
//!   grant.
//! - **Reserved beats granted.** Actions on [`LandPolicy::owner_reserved`] can
//!   never be reached by a grant, including a wildcard one. A misconfigured or
//!   over-broad grant therefore widens the blast radius up to the reserved
//!   line and no further, which is what makes handing an agent a grant safe.

use serde::{Deserialize, Serialize};

use crate::validate;

/// Actions Covenant refuses to let a grant authorize on any parcel, so an
/// over-broad grant cannot reach them.
///
/// The list is deliberately about irreversibility rather than about money:
/// transferring the land, rewriting who may act on it, and emptying its
/// holdings are the three that cannot be undone by the owner afterwards.
/// ClawVille can extend this per deployment; it cannot be shortened by a
/// grant.
pub const DEFAULT_OWNER_RESERVED: [&str; 4] = [
    "land.transfer",
    "land.grant",
    "land.revoke",
    "vault.withdraw",
];

/// What the guard decided about one action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Refuse,
    /// Within the parcel's rules but reserved to the owner, so it waits for a
    /// human rather than being refused outright.
    NeedsOwner,
}

/// One capability the parcel owner has wired: an actor, a parcel, and the
/// actions they may run on it.
///
/// `allowed_actions` holds exact labels (`shop.restock`) or one-level
/// namespace wildcards (`shop.*`). A bare `*` grants every action that is not
/// owner-reserved, and is accepted so a deployment can express "this agent
/// runs the whole building" without that ever meaning "and can also sell it".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandGrant {
    pub parcel: String,
    /// The agent this grant is for.
    pub actor: String,
    pub allowed_actions: Vec<String>,
    /// Unix milliseconds. `None` never expires, which a deployment should
    /// treat as a smell rather than a default.
    pub expires_at_ms: Option<u64>,
}

impl LandGrant {
    pub fn new(
        parcel: impl Into<String>,
        actor: impl Into<String>,
        allowed_actions: Vec<String>,
        expires_at_ms: Option<u64>,
    ) -> Result<Self, String> {
        let parcel = parcel.into();
        let actor = actor.into();
        validate::field("parcel", &parcel)?;
        validate::pubkey("actor", &actor)?;
        if allowed_actions.is_empty() {
            return Err("grant must allow at least one action".into());
        }
        for a in &allowed_actions {
            validate::field("allowed_action", a)?;
        }
        Ok(Self {
            parcel,
            actor,
            allowed_actions,
            expires_at_ms,
        })
    }

    fn covers(&self, action: &str) -> bool {
        self.allowed_actions
            .iter()
            .any(|p| pattern_matches(p, action))
    }
}

/// `*` matches anything; `ns.*` matches within that namespace; otherwise the
/// label must match exactly.
///
/// Matching is prefix-based on purpose: `shop.*` must not match `shopfront.x`,
/// so the separator is required rather than implied.
fn pattern_matches(pattern: &str, action: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.strip_suffix(".*") {
        Some(ns) => action
            .strip_prefix(ns)
            .is_some_and(|rest| rest.starts_with('.') && rest.len() > 1),
        None => pattern == action,
    }
}

/// The action a parcel is about to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandAction {
    pub parcel: String,
    pub actor: String,
    /// Label in ClawVille's vocabulary, e.g. `shop.restock`.
    pub action: String,
    /// Digest of the action's parameters. The guard binds the parameters
    /// without reading them, so ClawVille can change its payload shape freely
    /// and nothing about a parcel's contents reaches Covenant.
    pub params_hash: String,
}

impl LandAction {
    pub fn new(
        parcel: impl Into<String>,
        actor: impl Into<String>,
        action: impl Into<String>,
        params_hash: impl Into<String>,
    ) -> Result<Self, String> {
        let parcel = parcel.into();
        let actor = actor.into();
        let action = action.into();
        let params_hash = params_hash.into();
        validate::field("parcel", &parcel)?;
        validate::pubkey("actor", &actor)?;
        validate::field("action", &action)?;
        validate::hash_hex("params_hash", &params_hash)?;
        if action.contains('*') {
            return Err("action must be a concrete label, not a pattern".into());
        }
        Ok(Self {
            parcel,
            actor,
            action,
            params_hash,
        })
    }
}

/// The bar a parcel's actions are judged against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandPolicy {
    /// Actions no grant can authorize. See [`DEFAULT_OWNER_RESERVED`].
    pub owner_reserved: Vec<String>,
}

impl Default for LandPolicy {
    fn default() -> Self {
        Self::conservative()
    }
}

impl LandPolicy {
    /// The default bar: the irreversible actions stay with the owner.
    pub fn conservative() -> Self {
        Self {
            owner_reserved: DEFAULT_OWNER_RESERVED
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }

    /// Reserve additional actions. There is deliberately no method to
    /// un-reserve one, because the guard's value is that a grant cannot widen
    /// past this line.
    pub fn reserving(mut self, actions: impl IntoIterator<Item = String>) -> Self {
        self.owner_reserved.extend(actions);
        self.owner_reserved.sort();
        self.owner_reserved.dedup();
        self
    }

    fn reserves(&self, action: &str) -> bool {
        self.owner_reserved
            .iter()
            .any(|p| pattern_matches(p, action))
    }
}

/// The guard's answer, with each gate reported separately so a refusal names
/// which one closed rather than just saying no.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandVerdict {
    pub parcel: String,
    pub actor: String,
    pub action: String,
    pub params_hash: String,
    pub decision: Decision,
    /// Why, whenever the answer is not `Allow`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The grant is for this parcel and this actor.
    pub grant_applies: bool,
    /// The grant had not expired at the decision time.
    pub grant_fresh: bool,
    /// The action is not owner-reserved.
    pub not_reserved: bool,
    /// The action is inside the grant.
    pub in_grant: bool,
    pub decided_at_ms: u64,
}

impl LandVerdict {
    pub fn allowed(&self) -> bool {
        self.decision == Decision::Allow
    }
}

/// Decide whether `action` may run under `grant`.
///
/// Gate order is load-bearing. The reserved check runs before the grant check,
/// so a wildcard grant can never reach a reserved action; reversing them would
/// make `*` mean "everything including selling the land", which is the failure
/// this whole module exists to prevent.
pub fn authorize(
    action: &LandAction,
    grant: &LandGrant,
    policy: &LandPolicy,
    now_ms: u64,
) -> LandVerdict {
    let grant_applies = grant.parcel == action.parcel && grant.actor == action.actor;
    let grant_fresh = grant.expires_at_ms.is_none_or(|exp| now_ms < exp);
    let not_reserved = !policy.reserves(&action.action);
    let in_grant = grant.covers(&action.action);

    let (decision, reason) = if !grant_applies {
        (
            Decision::Refuse,
            Some(format!(
                "no grant for actor {} on parcel {}",
                action.actor, action.parcel
            )),
        )
    } else if !grant_fresh {
        (
            Decision::Refuse,
            Some(format!(
                "grant expired at {}",
                grant.expires_at_ms.unwrap_or_default()
            )),
        )
    } else if !not_reserved {
        (
            Decision::NeedsOwner,
            Some(format!(
                "{} is reserved to the parcel owner and cannot be granted",
                action.action
            )),
        )
    } else if !in_grant {
        (
            Decision::Refuse,
            Some(format!("{} is not in the grant", action.action)),
        )
    } else {
        (Decision::Allow, None)
    };

    LandVerdict {
        parcel: action.parcel.clone(),
        actor: action.actor.clone(),
        action: action.action.clone(),
        params_hash: action.params_hash.clone(),
        decision,
        reason,
        grant_applies,
        grant_fresh,
        not_reserved,
        in_grant,
        decided_at_ms: now_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trail::{ActionEntry, AuditTrail};

    const PARCEL: &str = "parcel-42";
    const ACTOR: &str = "Ep7dD7biX7rZ6NSVzy8uEpgEEYipVfQ8ofwHzZmRM8dF";
    const OTHER: &str = "24i43XkDyJAJJBi7X3ARRCt3WBh16uJuSfVRLKXVEYBQ";

    fn hash() -> String {
        "a".repeat(64)
    }

    fn grant(actions: &[&str]) -> LandGrant {
        LandGrant::new(
            PARCEL,
            ACTOR,
            actions.iter().map(|s| (*s).to_string()).collect(),
            Some(2_000),
        )
        .unwrap()
    }

    fn action(name: &str) -> LandAction {
        LandAction::new(PARCEL, ACTOR, name, hash()).unwrap()
    }

    #[test]
    fn a_granted_action_is_allowed() {
        let v = authorize(
            &action("shop.restock"),
            &grant(&["shop.restock"]),
            &LandPolicy::conservative(),
            1_000,
        );
        assert_eq!(v.decision, Decision::Allow);
        assert!(v.allowed());
        assert_eq!(v.reason, None);
    }

    #[test]
    fn an_ungranted_action_is_refused_with_the_reason() {
        let v = authorize(
            &action("door.open"),
            &grant(&["shop.restock"]),
            &LandPolicy::conservative(),
            1_000,
        );
        assert_eq!(v.decision, Decision::Refuse);
        assert!(v.reason.unwrap().contains("not in the grant"));
        assert!(!v.in_grant);
    }

    #[test]
    fn wiring_a_building_to_run_a_shop_does_not_let_it_sell_the_land() {
        // The property ClawVille asked for, stated as a test: a wildcard grant
        // over the whole parcel still cannot reach a reserved action.
        let everything = grant(&["*"]);
        let policy = LandPolicy::conservative();

        assert_eq!(
            authorize(&action("shop.restock"), &everything, &policy, 1_000).decision,
            Decision::Allow
        );
        for reserved in DEFAULT_OWNER_RESERVED {
            let v = authorize(&action(reserved), &everything, &policy, 1_000);
            assert_eq!(v.decision, Decision::NeedsOwner, "{reserved} was grantable");
            assert!(!v.not_reserved);
            assert!(
                v.in_grant,
                "the grant does cover it; the policy is what stops it"
            );
        }
    }

    #[test]
    fn a_namespace_wildcard_stays_inside_its_namespace() {
        let g = grant(&["shop.*"]);
        let policy = LandPolicy::conservative();
        assert_eq!(
            authorize(&action("shop.restock"), &g, &policy, 1_000).decision,
            Decision::Allow
        );
        // Neither a different namespace nor a lookalike prefix.
        for outside in ["door.open", "shopfront.paint", "shop"] {
            assert_eq!(
                authorize(&action(outside), &g, &policy, 1_000).decision,
                Decision::Refuse,
                "{outside} matched shop.*"
            );
        }
    }

    #[test]
    fn a_grant_for_another_actor_or_parcel_does_not_apply() {
        let policy = LandPolicy::conservative();
        let g = grant(&["shop.restock"]);

        let other_actor = LandAction::new(PARCEL, OTHER, "shop.restock", hash()).unwrap();
        let v = authorize(&other_actor, &g, &policy, 1_000);
        assert_eq!(v.decision, Decision::Refuse);
        assert!(!v.grant_applies);

        let other_parcel = LandAction::new("parcel-9", ACTOR, "shop.restock", hash()).unwrap();
        assert!(!authorize(&other_parcel, &g, &policy, 1_000).grant_applies);
    }

    #[test]
    fn an_expired_grant_stops_working() {
        let g = grant(&["shop.restock"]);
        let policy = LandPolicy::conservative();
        assert!(authorize(&action("shop.restock"), &g, &policy, 1_999).allowed());

        let v = authorize(&action("shop.restock"), &g, &policy, 2_000);
        assert_eq!(v.decision, Decision::Refuse);
        assert!(!v.grant_fresh);
        assert!(v.reason.unwrap().contains("expired"));
    }

    #[test]
    fn a_grant_without_an_expiry_stays_fresh() {
        let g = LandGrant::new(PARCEL, ACTOR, vec!["shop.restock".into()], None).unwrap();
        assert!(authorize(
            &action("shop.restock"),
            &g,
            &LandPolicy::conservative(),
            u64::MAX
        )
        .allowed());
    }

    #[test]
    fn a_deployment_can_reserve_more_but_the_defaults_survive() {
        let policy = LandPolicy::conservative().reserving(["door.open".to_string()]);
        let everything = grant(&["*"]);

        assert_eq!(
            authorize(&action("door.open"), &everything, &policy, 1_000).decision,
            Decision::NeedsOwner
        );
        assert_eq!(
            authorize(&action("land.transfer"), &everything, &policy, 1_000).decision,
            Decision::NeedsOwner
        );
        assert_eq!(
            authorize(&action("shop.restock"), &everything, &policy, 1_000).decision,
            Decision::Allow
        );
    }

    #[test]
    fn reserving_the_same_action_twice_does_not_duplicate_it() {
        let policy = LandPolicy::conservative()
            .reserving(["door.open".to_string()])
            .reserving(["door.open".to_string()]);
        assert_eq!(
            policy
                .owner_reserved
                .iter()
                .filter(|a| *a == "door.open")
                .count(),
            1
        );
    }

    #[test]
    fn a_reserved_namespace_covers_its_actions() {
        let policy = LandPolicy::conservative().reserving(["vault.*".to_string()]);
        for a in ["vault.withdraw", "vault.rotate_key"] {
            assert_eq!(
                authorize(&action(a), &grant(&["*"]), &policy, 1_000).decision,
                Decision::NeedsOwner,
                "{a}"
            );
        }
    }

    #[test]
    fn malformed_input_is_refused_at_construction() {
        assert!(LandGrant::new(PARCEL, "not-a-pubkey", vec!["a".into()], None).is_err());
        assert!(LandGrant::new(PARCEL, ACTOR, vec![], None).is_err());
        assert!(LandAction::new(PARCEL, ACTOR, "shop.restock", "nothex").is_err());
        // A pattern is a grant concept; an action being run is always concrete.
        assert!(LandAction::new(PARCEL, ACTOR, "shop.*", hash()).is_err());
    }

    #[test]
    fn a_verdict_logs_into_the_same_trail_the_verifier_reads() {
        let v = authorize(
            &action("shop.restock"),
            &grant(&["shop.*"]),
            &LandPolicy::conservative(),
            1_000,
        );
        let trail = AuditTrail::new(vec![ActionEntry {
            seq: 0,
            action: v.action.clone(),
            detail_hash: v.params_hash.clone(),
        }]);
        trail
            .validate()
            .expect("a verdict is a well-formed trail entry");
        assert_eq!(trail.root().len(), 64);
    }
}
