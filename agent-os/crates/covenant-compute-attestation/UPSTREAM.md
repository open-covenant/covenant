# Upstream verifier provenance

Adapted from Prism Network, Apache-2.0, commit
`4e32eb5eb1398fd9d11dcd29aa5a96e3f2f646ec`, `crates/attestation`.
See LICENSE.prism and NOTICE.prism. Vendor roots, reference measurements and
test vectors retain upstream provenance; they are not Covenant production
acceptance or evidence of a live Covenant deployment.

Changes: crate/package imports, a minimal compatibility type module extracted
from upstream protocol, and separate Covenant UUID/session/authentication
bindings. Cryptographic verifier checks and compiled vendor references are
retained. The root source files carry upstream comments; references to Prism
services and rollout status describe upstream, not Covenant. Test private keys
are public synthetic fixtures solely for adversarial certificate tests.

Local test-only maintenance in the vendored `dcap-qvl`: collateral JSON
regression fixtures propagate JSON parse errors through the test result;
signature-encoding assertions omit redundant formatting borrows. These satisfy
the repository's Clippy gate without changing verifier behavior.

The dstack 0.6.1 port preserves explicit event versions and canonical V2
preimages, validates runtime/firmware register roles and boot identity
boundaries, and selects those semantics from a complete independently derived
production launch tuple. Source inputs and the reproducible measurement helper
are retained in `deploy/compute-attested-cpu/measurement/` at the repository
root. Existing Intel signature, TCB/advisory, debug and freshness policy remains
strict. The served Covenant trust ceiling remains `Open`.
