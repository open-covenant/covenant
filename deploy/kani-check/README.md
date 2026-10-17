# deploy/kani-check — Covenant Kani verifier runner

`runner.py` (Python stdlib only) validates a `covenant.kani.plan.v1` plan, runs the pinned
`/usr/local/cargo/bin/cargo-kani` directly (no cargo plugin discovery, no shell) with a fixed argv,
captures raw stdout/stderr/Kani export as files, and classifies the export into
`covenant.kani.result.v1` (`proven`, `refuted`, `timeout`, `out_of_memory`, `tool_error`,
`inconclusive`). `accepted` is true only when the outcome equals the plan's `expected_outcome`;
the CLI exits 0 only then. The final report is one JSON document on stdout; diagnostics go to stderr.

```
python3 /opt/covenant/kani/runner.py run --plan /request/plan.json --workdir /work --outdir /evidence
```

Pinned toolchain on the build host: Kani 0.68.0, CBMC 6.11.0, nightly-2026-08-21
(rustc 1.100.0-nightly 8925ea358). Kani 0.68 has no `--test <target>`; only `--tests` is supported.

## Behaviour and bounds

- Rejected before running: unknown/oversized plan fields, manifests outside `/work` (symlinks resolved),
  `.cargo/config[.toml]` outside the exact parsed allowlist `{env: {RUST_MIN_STACK: "8388608"}}`, unsafe
  `[package|workspace.metadata.kani.flags]` (`no-*`, `only-codegen`, `prove-safety-only`,
  `ignore-global-asm`, `cbmc-args`, ...), and environment overriding `RUSTC*`/`RUSTFLAGS`/`CARGO_TARGET_*`.
- Environment: `PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin`, `RUSTUP_HOME=/usr/local/rustup`,
  `KANI_HOME=/opt/kani`, `HOME=/tmp`, `CARGO_NET_OFFLINE=true`, colors off,
  `CARGO_TARGET_DIR=/scratch/target`, `TMPDIR=/scratch/tmp`, `CARGO_HOME=/deps/cargo` if present.
- Evidence (`stdout.log`, `stderr.log`, `kani-export.json`, `result.json`) is written to `--outdir`;
  stale files are removed first and symlinks are never followed. Artifact entries carry `{name,bytes,sha256}`
  over the original bytes; nothing is truncated. More than 16 MiB of raw evidence kills the process group
  and yields `tool_error`. Wall timeout is `timeout_secs` plus a 300 s build grace; the process group is
  killed and reaped.
- Memory, CPU, PID, network and mount limits are enforced by the node/Docker, not by this runner. The node
  supplies authoritative OOM information. The report is not itself cryptographic proof: the node embeds
  the complete artifact bytes in its signed output (4 MiB compressed cap). Full evidence is not stored on chain.

## Build (operator, needs network; not performed by the author)

The inspected host base is `rust@sha256:81015672d6939b0265f09adf96d5de125482bb1330106076956b0518e74edc90`.
The operator must first verify that this digest resolves in the registry for the target platform
(e.g. `docker buildx imagetools inspect rust@sha256:8101...edc90`); a tag is not an immutability claim.

```
docker build --build-arg BASE_IMAGE=rust@sha256:81015672d6939b0265f09adf96d5de125482bb1330106076956b0518e74edc90 \
  --build-arg KANI_VERSION=0.68.0 --iidfile kani-check.iid -t covenant-kani-check:build deploy/kani-check
IMAGE="$(cat kani-check.iid)"   # immutable local image ID (sha256:...)
# after pushing: record the registry digest, never the tag
docker push <registry>/covenant-kani-check:build
docker inspect --format '{{index .RepoDigests 0}}' <registry>/covenant-kani-check:build
```

## Offline bounded smoke tests (operator)

```
docker run --rm --network none --read-only --user 12345:12345 --memory 2g --pids-limit 256 --cpus 2 \
  --tmpfs /tmp --mount type=tmpfs,dst=/scratch,tmpfs-mode=1777 \
  --mount type=bind,src=$PWD/fixture,dst=/work,readonly \
  --mount type=bind,src=$PWD/req,dst=/request,readonly --mount type=bind,src=$PWD/evidence,dst=/evidence \
  "$IMAGE"
docker run --rm --network none --read-only --entrypoint cat "$IMAGE" /opt/covenant/kani/versions.txt
```

Wrap with `timeout 900`. `/evidence` must be writable by the chosen UID.

## Tests

```
python3 .covenant-kani-acceptance/acceptance_test.py
python3 -m unittest discover -s deploy/kani-check/tests -v
```

These validate the classifier against synthetic, source-derived fixtures and the runner against stub
subprocesses. They are fixture validation only: the Docker image build and live Kani/CBMC runs have not
been performed, and prior Percolator proofs and buyer fixtures are not claimed as Covenant authorship.

## Integration review

The network-authored source is retained with its paid job patch. Local bootstrap
review corrected the Docker build ARG scope, allowed the exact fixed node/image
environment values, and ensured rejected invocations preserve empty output streams.
The initial workflow uses reviewed, pinned trees and locked, reviewed dependencies,
including procedural macros. Arbitrary hostile compile-time code shares the
compiler's writable paths and requires further driver isolation; a receipt
authenticates the operator's statement rather than providing independent trusted
execution of malicious repositories.
