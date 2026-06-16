//! Conformance golden vectors for the capability grammar.
//!
//! Each committed file under `tests/golden/capabilities/<namespace>.json` is the
//! canonical serialized form of one representative `Capability` grant for a
//! recognized capability namespace — the frozen contract third parties encode
//! and decode grants against without reading the Rust source. For every
//! namespace this suite re-serializes the in-code grant and asserts it is
//! byte-for-byte equal to the committed file, round-trips the file back to the
//! same value, asserts the frozen scope still passes the live `validate_scope`,
//! and checks the corpus covers exactly the namespace inventory.
//!
//! The action grammar is `namespace.method[.sub]` — a dot separator, two or
//! three segments. Scopes are JSON objects: `{}` is unscoped, and a non-empty
//! scope carries `version: 1` plus the namespace's recognized fields. See
//! `docs/capabilities.md`.
//!
//! Regenerate deliberately, only after reviewing the resulting diff:
//!
//! ```text
//! COVENANT_BLESS_CAPABILITY_GOLDEN=1 cargo test -p covenant-permissions \
//!   --test golden_capabilities grammar_vectors_match_committed_corpus
//! ```

use std::collections::BTreeSet;
use std::path::PathBuf;

use covenant_permissions::validate_scope;
use covenant_types::{AgentId, Capability};
use serde_json::json;

/// Set to (re)generate the committed corpus instead of asserting against it.
const BLESS_ENV: &str = "COVENANT_BLESS_CAPABILITY_GOLDEN";

/// Every recognized capability namespace, in `ScopeNamespace` declaration
/// order. The inline `scope_namespace_inventory_is_frozen` test in `src/lib.rs`
/// is the compile-time tripwire: adding a `ScopeNamespace` variant breaks the
/// build there, forcing a matching vector here and a bump to this list.
/// `capabilities.*` is intentionally absent — it is not a `ScopeNamespace` and
/// is unbound at grant time (the cutoff is enforced only at dispatch).
const EXPECTED_NAMESPACES: &[&str] = &[
    "intent",
    "tool",
    "memory",
    "agent",
    "a2a",
    "audit",
    "peers",
    "identity",
    "chain",
    "settlement",
    "x402",
    "secret",
];

struct GrammarVector {
    namespace: &'static str,
    capability: Capability,
}

fn grant(namespace: &'static str, action: &str, scope: serde_json::Value) -> GrammarVector {
    GrammarVector {
        namespace,
        capability: Capability {
            subject: AgentId::new("research@agent", [1u8; 32]),
            action: action.into(),
            scope,
            granted_by: AgentId::new("operator@local", [2u8; 32]),
            expires_at: Some(1_700_000_000_000),
        },
    }
}

/// One representative grant per recognized namespace. The `action` shows the
/// canonical dotted form; the `scope` shows that namespace's recognized fields.
/// Per-action dispatch predicates keep their own exhaustive crate-level tests —
/// these vectors freeze the grant envelope and the per-namespace scope syntax.
fn golden_vectors() -> Vec<GrammarVector> {
    vec![
        grant("intent", "intent.publish", json!({ "version": 1 })),
        grant(
            "tool",
            "tool.call.echo",
            json!({
                "version": 1,
                "tool": "echo",
                "arguments": { "allow": { "text": "hi" } }
            }),
        ),
        grant(
            "memory",
            "memory.read",
            json!({
                "version": 1,
                "tiers": ["working"],
                "record_id": null,
                "before_ms": null,
                "apply": false
            }),
        ),
        grant("agent", "agent.spawn", json!({ "version": 1 })),
        grant(
            "a2a",
            "a2a.repair.requeue",
            json!({
                "version": 1,
                "peer_pubkey_b58": "11111111111111111111111111111111",
                "task_id": null,
                "lease_id": null,
                "duplicate_risk": "idempotent"
            }),
        ),
        grant(
            "audit",
            "audit.purge",
            json!({
                "version": 1,
                "before_ms": 1_700_000_000_000u64,
                "include_integrity": true
            }),
        ),
        grant(
            "peers",
            "peers.revoke",
            json!({ "version": 1, "token_prefix": "abc123", "force": false }),
        ),
        grant(
            "identity",
            "identity.rotate",
            json!({ "version": 1, "self": true }),
        ),
        grant(
            "chain",
            "chain.receipts",
            json!({ "version": 1, "limit": 100, "resource": "compute" }),
        ),
        grant(
            "settlement",
            "settlement.backfill.apply",
            json!({ "version": 1, "apply": true, "before_ms": null }),
        ),
        grant(
            "x402",
            "x402.outbound.pay",
            json!({ "version": 1, "provider": "xona" }),
        ),
        grant(
            "secret",
            "secret.access",
            json!({ "version": 1, "name": "openai-api-key" }),
        ),
    ]
}

fn golden_dir() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("capabilities")
}

/// Canonical wire form: pretty-printed with a trailing newline, exactly as the
/// committed files are written.
fn canonical_json(cap: &Capability) -> String {
    let mut json = serde_json::to_string_pretty(cap).expect("capability serializes");
    json.push('\n');
    json
}

/// `Err(reason)` on drift — either the serialized bytes differ from the
/// committed vector, or the committed text does not deserialize back to the
/// same grant. The second leg catches a serialize/deserialize asymmetry that
/// byte-equality alone cannot see. Pure (no IO) so the suite can self-test that
/// the comparison actually bites.
fn detect_drift(committed: &str, cap: &Capability) -> Result<(), String> {
    let canonical = canonical_json(cap);
    if committed != canonical {
        return Err(format!(
            "serialized bytes differ from the committed vector\n  committed: {committed:?}\n  current:   {canonical:?}"
        ));
    }
    match serde_json::from_str::<Capability>(committed) {
        Ok(parsed) if parsed == *cap => Ok(()),
        Ok(parsed) => Err(format!(
            "committed bytes do not round-trip back to the grant\n  expected: {cap:?}\n  parsed:   {parsed:?}"
        )),
        Err(error) => Err(format!("committed bytes failed to deserialize: {error}")),
    }
}

#[test]
fn capability_grammar_vectors_match_committed_corpus() {
    let bless = std::env::var_os(BLESS_ENV).is_some();
    let dir = golden_dir();
    if bless {
        std::fs::create_dir_all(&dir).expect("create golden dir");
    }

    for vector in golden_vectors() {
        let path = dir.join(format!("{}.json", vector.namespace));

        if bless {
            std::fs::write(&path, canonical_json(&vector.capability))
                .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
            continue;
        }

        let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "missing golden vector {} ({error}); regenerate with {BLESS_ENV}=1",
                path.display()
            )
        });
        if let Err(reason) = detect_drift(&committed, &vector.capability) {
            panic!(
                "capability grammar vector '{}' drifted from its committed form ({}): {reason}\n\
                 Review the grammar change, then re-bless with {BLESS_ENV}=1 if it is intended.",
                vector.namespace,
                path.display()
            );
        }
    }
}

#[test]
fn capability_grammar_corpus_is_exhaustive() {
    let expected: BTreeSet<&str> = EXPECTED_NAMESPACES.iter().copied().collect();
    assert_eq!(
        expected.len(),
        EXPECTED_NAMESPACES.len(),
        "EXPECTED_NAMESPACES contains duplicate slugs"
    );

    let covered: BTreeSet<&str> = golden_vectors().iter().map(|v| v.namespace).collect();
    assert_eq!(
        covered.len(),
        golden_vectors().len(),
        "two grammar vectors share a namespace"
    );
    assert_eq!(
        covered, expected,
        "grammar vectors must cover exactly the recognized namespaces — every namespace needs one vector"
    );

    // The on-disk leg would race the corpus writer under bless (cargo runs a
    // binary's tests in parallel), so skip it while regenerating.
    if std::env::var_os(BLESS_ENV).is_some() {
        return;
    }

    let mut on_disk: BTreeSet<String> = BTreeSet::new();
    for entry in std::fs::read_dir(golden_dir()).expect("golden dir exists") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("golden file has a UTF-8 stem")
                .to_string();
            on_disk.insert(stem);
        }
    }
    let on_disk_refs: BTreeSet<&str> = on_disk.iter().map(String::as_str).collect();
    assert_eq!(
        on_disk_refs,
        expected,
        "committed files under {} must match the namespace inventory exactly — no orphans, none missing",
        golden_dir().display()
    );
}

#[test]
fn committed_grants_validate_against_live_scope_validator() {
    // The conformance tie. Every frozen grant's scope must still pass the live
    // grammar validator. Tightening `validate_scope` so it rejects a
    // previously valid grant fails here, forcing a deliberate, reviewed version
    // bump instead of a silent break of grants already in the field.
    for vector in golden_vectors() {
        validate_scope(&vector.capability.action, &vector.capability.scope).unwrap_or_else(
            |error| {
                panic!(
                    "frozen grant '{}' ({}) no longer validates against the live grammar: {error}",
                    vector.namespace, vector.capability.action
                )
            },
        );
    }
}

#[test]
fn capability_harness_detects_drift() {
    // Drives the real `detect_drift` so the suite proves the comparison bites:
    // an exact canonical vector is clean, and a committed vector that differs by
    // a single scope field is flagged as drift.
    let value = grant(
        "audit",
        "audit.purge",
        json!({ "version": 1, "before_ms": 1u64 }),
    );
    assert!(
        detect_drift(&canonical_json(&value.capability), &value.capability).is_ok(),
        "an exact canonical vector must not be reported as drift"
    );

    let tampered = canonical_json(
        &grant(
            "audit",
            "audit.purge",
            json!({ "version": 1, "before_ms": 2u64 }),
        )
        .capability,
    );
    assert!(
        detect_drift(&tampered, &value.capability).is_err(),
        "a committed vector that differs from its grant by one field must be flagged as drift"
    );
}
