// ER build of the settlement program. The whole program lives in the sibling
// crate's lib.rs; here we compile it verbatim with the `ephemeral` feature on so
// the delegate/commit/undelegate instructions and the `#[ephemeral]` macro are
// included. Keeping this a thin include means there is exactly one source of
// truth for the program logic.
include!("../../settlement/src/lib.rs");
