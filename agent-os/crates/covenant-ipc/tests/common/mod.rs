//! Shared harness for the IPC conformance golden-vector suites
//! (`golden_requests`, `golden_responses`).
//!
//! The committed files under `tests/golden/<subdir>/` are the frozen wire
//! contract; these helpers generate them, assert byte-exact drift, and check
//! the corpus is exhaustive. See `tests/golden/README.md`.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Set to (re)generate the committed corpus instead of asserting against it.
pub const BLESS_ENV: &str = "COVENANT_BLESS_IPC_GOLDEN";

pub fn golden_subdir(name: &str) -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(name)
}

/// Canonical wire form: pretty-printed with a trailing newline, exactly as the
/// committed files are written.
pub fn canonical_json<T: Serialize>(value: &T) -> String {
    let mut json = serde_json::to_string_pretty(value).expect("message serializes");
    json.push('\n');
    json
}

/// The `kind` discriminator a message serializes to — also its file name.
pub fn wire_kind<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value).expect("message serializes") {
        serde_json::Value::Object(map) => map
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .expect("message wire form carries a string 'kind' discriminator")
            .to_string(),
        other => panic!("message must serialize as a JSON object, got {other}"),
    }
}

/// Compare committed bytes against a value's canonical form. `Err(reason)` on
/// drift — either the serialized bytes differ, or the canonical text does not
/// deserialize back to the same value. The second leg catches a type whose
/// canonical form does not round-trip — e.g. a `serialize_with`/`deserialize_with`
/// pair that are not inverses, or a `skip_deserializing` field that still
/// serializes — asymmetry byte-equality alone cannot see. Pure (no IO) so the
/// suites can self-test that the harness actually detects drift.
pub fn detect_drift<T>(committed: &str, value: &T) -> Result<(), String>
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let canonical = canonical_json(value);
    if committed != canonical {
        return Err(format!(
            "serialized bytes differ from the committed vector\n  committed: {committed:?}\n  current:   {canonical:?}"
        ));
    }
    match serde_json::from_str::<T>(committed) {
        Ok(parsed) if parsed == *value => Ok(()),
        Ok(parsed) => Err(format!(
            "committed bytes do not round-trip back to the value\n  expected: {value:?}\n  parsed:   {parsed:?}"
        )),
        Err(error) => Err(format!("committed bytes failed to deserialize: {error}")),
    }
}

/// Assert every vector matches its committed golden file (via [`detect_drift`])
/// — or, under [`BLESS_ENV`], (re)write the files. Drift fails with a re-bless
/// hint instead of silently breaking downstream consumers.
pub fn check_corpus<T>(subdir: &str, vectors: &[T])
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bless = std::env::var_os(BLESS_ENV).is_some();
    let dir = golden_subdir(subdir);
    if bless {
        std::fs::create_dir_all(&dir).expect("create golden dir");
    }

    for value in vectors {
        let kind = wire_kind(value);
        let path = dir.join(format!("{kind}.json"));

        if bless {
            std::fs::write(&path, canonical_json(value))
                .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
            continue;
        }

        let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "missing golden vector {} ({error}); regenerate with {BLESS_ENV}=1",
                path.display()
            )
        });
        if let Err(reason) = detect_drift(&committed, value) {
            panic!(
                "IPC message '{kind}' drifted from its committed golden vector ({}): \
                 {reason}\nReview the wire-shape change, then re-bless with \
                 {BLESS_ENV}=1 if it is intended.",
                path.display()
            );
        }
    }
}

/// Assert the generated vectors and the committed files each cover exactly
/// `expected_kinds` — no duplicate, missing, or orphaned variant.
pub fn check_exhaustive<T: Serialize>(subdir: &str, expected_kinds: &[&str], vectors: &[T]) {
    let expected: BTreeSet<&str> = expected_kinds.iter().copied().collect();
    assert_eq!(
        expected.len(),
        expected_kinds.len(),
        "expected kind list contains duplicate slugs"
    );

    let generated: BTreeSet<String> = vectors.iter().map(|value| wire_kind(value)).collect();
    assert_eq!(
        generated.len(),
        vectors.len(),
        "two golden vectors share a wire kind"
    );
    let generated_refs: BTreeSet<&str> = generated.iter().map(String::as_str).collect();
    assert_eq!(
        generated_refs, expected,
        "golden vectors must cover exactly the expected kinds — every variant needs one vector"
    );

    // The on-disk leg is meaningless while files are being regenerated, and
    // reading the directory would race the corpus writer (cargo runs a binary's
    // tests in parallel), so skip it under bless.
    if std::env::var_os(BLESS_ENV).is_some() {
        return;
    }

    let mut on_disk: BTreeSet<String> = BTreeSet::new();
    for entry in std::fs::read_dir(golden_subdir(subdir)).expect("golden dir exists") {
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
        "committed golden files under {} must match the expected kinds exactly — \
         no orphans, none missing",
        golden_subdir(subdir).display()
    );
}
