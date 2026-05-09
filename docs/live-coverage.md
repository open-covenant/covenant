# Live Coverage Matrix

Default CI must stay deterministic, but Covenant still needs evidence that the operating layer survives real process, socket, CLI, model, and restart boundaries. Live tests are opt-in Rust tests whose names start with `live_` and whose test attributes include `#[ignore]`.

The machine-readable matrix lives at `agent-os/autonomy/live-coverage.json`. It maps protocol and runtime surfaces to:

- mock or fixture-driven tests;
- opt-in live test files;
- current coverage status;
- the next meaningful gap.

Validate the matrix without running live tests:

```bash
node agent-os/scripts/validate-live-coverage.mjs
bash agent-os/scripts/test-stats.sh
```

Run the live suite from `agent-os/`:

```bash
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_
```

Targeted live CLI tests execute `target/debug/covenant` from the workspace. Build the CLI first when running one of those tests directly:

```bash
cargo build -p covenant --locked
cargo test -p covenantd --test live_cli_version -- --ignored live_cli_version_reads_protocol_info_without_token
```

Linux gVisor runtime coverage has host prerequisites outside the default suite. Use the repeatable runner guide before interpreting a pass or failure as sandbox evidence:

```bash
cd agent-os
COVENANT_LIVE_GVISOR_ROOTFS=/path/to/rootfs \
  cargo test -p covenant-runtime --test live_gvisor -- --ignored live_gvisor_runner_dispatches_with_runsc
```

See [`docs/gvisor-live-runner.md`](gvisor-live-runner.md) for the required Linux host, `runsc`, rootfs, and CI adoption contract.

## Current Surface Map

| Surface | Status | Live coverage | Next gap |
| --- | --- | --- | --- |
| Daemon IPC core | Covered | daemon ping/intent, CLI ping JSON, CLI intent, CLI intent JSON, CLI resume JSON, CLI version | Resume-success fixture once budget refill semantics can be exercised without long sleeps. |
| State verifier | Covered | CLI `verify --json` healthy, drift, and targeted repair paths on a real daemon | Typed repair command hints once verifier repair action schemas stabilize. |
| Memory retention | Covered | CLI memory read JSON, CLI memory purge JSON | Live repair and compaction fixtures that assert memory/audit/receipt consistency. |
| HTTP gateway | Covered | health, version, bearer auth, tools call | High-risk mutation endpoints as retention and recovery policies stabilize. |
| CLI capability lifecycle | Covered | grant, grant JSON, grant with expiry, recent, recent JSON, revoke, revoke JSON, purge JSON | Purge failure-mode coverage for scoped retention limits once retention policy is stable. |
| CLI audit feed | Covered | audit purge JSON, audit recent, audit recent JSON, audit verify | Scoped audit purge rejection coverage once retention policy defaults are stable. |
| Ignore policy gate | Covered | CLI ignore check JSON | Live dispatch fixture proving ignored intents never write memory or receipts. |
| Peer authentication and token lifecycle | Covered | auth rejection, purge JSON, revoke, CLI revoke JSON, CLI self-revoke rejection, restart revoke, token rotation, CLI rotation JSON | Forced self-revoke recovery only with isolated temp-home fixtures. |
| Peer listing and status filters | Covered | list, JSON list/truncation, live-only, revoked-only | Prefix-filter JSON coverage if automation begins relying on prefix narrowing. |
| A2A mailbox and restart durability | Covered | duplex, admission gate, CLI compact JSON, CLI repair, CLI status JSON, restart replay | Stale-lease guard failure coverage. |
| MCP subprocess transport | Covered | CLI tools list JSON, CLI tools call JSON, stdio initialize/list/call | Third-party fixture once selection is stable. |
| Runtime and reference agent subprocess | Covered | research subprocess, malformed stdout rejection, daemon dispatch to research agent | Daemon-level dispatch failure assertions after operator-facing failure receipts are formalized. |
| Linux gVisor runtime dispatch | External service | `live_gvisor.rs` | Automate the documented Linux `runsc` runner on a pinned rootfs. |
| Budget enforcement | Covered | daemon rejection when budget exhausts, CLI resume, CLI resume JSON | Budget resume success after pause/resume policy lands. |
| Settlement receipts and chain gates | Covered | daemon dispatch writes/reads receipts after `chain.receipts`, CLI `chain status --json`, CLI `chain flush-receipts --json`, CLI `receipts recent --json`, CLI `chain receipt-batches --json` | Scoped receipt filter coverage once receipt query predicates become user-selectable. |
| Local model and full acceptance path | External service | Ollama and full acceptance tests | Model availability probes before more model coverage. |

## Rules

- Live tests must remain opt-in with `#[ignore]`.
- Live tests must explain required external services in the ignore reason or module doc.
- Credentialed, paid, or rate-limited APIs require human approval before adding tests.
- Improving the live-test ratio is not enough; the riskiest uncovered surface should drive the next test.
