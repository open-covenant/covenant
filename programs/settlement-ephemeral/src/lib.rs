// ER build of the settlement program. `_shared.rs` is a verbatim copy of the
// sibling `settlement/src/lib.rs` (the source of truth), kept here so this crate
// is self-contained (no cross-crate include!). That self-containment is what lets
// the reproducible / OtterSec build mount this crate directly. Regenerate after
// any settlement change: `cp ../settlement/src/lib.rs src/_shared.rs`, then diff
// the two. A drift here ships a mainnet binary that is not the reviewed source.
include!("_shared.rs");
