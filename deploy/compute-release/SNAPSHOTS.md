# CLI candidate snapshots

Pull-request CI runs `deploy/compute-release/dist.sh --snapshot <target>` for
all four supported platforms. It still performs both locked release builds,
extracts the archive, runs every binary's version/help checks, checks Linux
static linkage, and exercises the buyer's offline packaged features. Each
binary must match its own crate manifest; a snapshot may contain different
crate versions.

Snapshot archives start with `candidate-snapshot-<source-sha>-`. Their generated
README labels them unreleased. `SNAPSHOT_PROVENANCE.txt` records the source commit
and tree, dirty-working-tree status, target, per-binary manifest versions and
binary SHA-256 hashes. This metadata is informational, not a signed release
attestation. A dirty snapshot needs its source diff as well as its commit ID.
Snapshots have no release notes and are never installer or tag-release assets.

The default `dist.sh <target>` mode remains strict: all packaged crates must
share one version, release notes must exist, and the packaged README must link
to that version. Snapshot mode refuses tag refs. Only pushed `compute-v*` tags
can enter the publishing job, which also refuses candidate archive names.

Run `deploy/compute-release/test-dist.sh` for the isolated packaging-policy
harness. It substitutes Cargo to avoid compiling Rust, while exercising the
actual archive creation, extraction, binary checks and release/snapshot gates.
The four-platform workflow remains responsible for real builds.
