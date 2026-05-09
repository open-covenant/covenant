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

## Current Surface Map

| Surface | Status | Live coverage | Next gap |
| --- | --- | --- | --- |
| Daemon IPC core | Covered | daemon ping/intent, CLI intent, CLI version | Resume-intent coverage after repair semantics settle. |
| HTTP gateway | Covered | health, version, bearer auth, tools call | High-risk mutation endpoints as retention and recovery policies stabilize. |
| CLI capability lifecycle | Covered | grant, grant with expiry, recent, revoke | Capability purge after retention defaults are decided. |
| CLI audit feed | Covered | audit recent, audit verify | Audit purge after retention policy defaults are decided. |
| Peer authentication and token lifecycle | Covered | auth rejection, revoke, CLI self-revoke rejection, restart revoke, token rotation | Forced self-revoke recovery only with isolated temp-home fixtures. |
| Peer listing and status filters | Covered | list, live-only, revoked-only | Ambiguous-prefix coverage after machine-readable output exists. |
| A2A mailbox and restart durability | Covered | duplex, admission gate, CLI repair, restart replay | Stale-lease guard failure coverage after machine-readable status output stabilizes. |
| MCP subprocess transport | Covered | stdio initialize/list/call | Third-party fixture once selection is stable. |
| Runtime and reference agent subprocess | Covered | research subprocess, malformed stdout rejection, daemon dispatch to research agent | Daemon-level dispatch failure assertions after operator-facing failure receipts are formalized. |
| Linux gVisor runtime dispatch | External service | `live_gvisor.rs` | Linux host with `runsc` and a minimal `/bin/sh` rootfs. |
| Budget enforcement | Covered | daemon rejection when budget exhausts | Budget resume after pause/resume policy lands. |
| Local model and full acceptance path | External service | Ollama and full acceptance tests | Model availability probes before more model coverage. |

## Rules

- Live tests must remain opt-in with `#[ignore]`.
- Live tests must explain required external services in the ignore reason or module doc.
- Credentialed, paid, or rate-limited APIs require human approval before adding tests.
- Improving the live-test ratio is not enough; the riskiest uncovered surface should drive the next test.
