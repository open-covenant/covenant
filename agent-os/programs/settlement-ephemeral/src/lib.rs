// ER build of the settlement program. `_shared.rs` is a verbatim copy of the
// sibling `settlement/src/lib.rs` (the source of truth), kept here so this crate
// is self-contained (no cross-crate include!). That self-containment is what lets
// the reproducible / OtterSec build mount this crate directly. Regenerate after
// any settlement change: `cp ../../settlement/src/lib.rs src/_shared.rs` — CI
// diffs the two and fails if they drift.
include!("_shared.rs");
