# covenant-compute-node

The supply side of the Covenant compute network: run this on a machine
and it earns by serving other agents' paid jobs — registered against a
coordinator, matched by capability and price, paid per verified receipt.
The network-level map lives at
[docs/compute-network.md](../../../docs/compute-network.md).

```sh
covenant-compute-node setup      # one-time: anchors, executor, pricing -> node.env
covenant-compute-node            # serve jobs in the foreground
covenant-compute-node service install   # or: as a background service, across reboots
covenant-compute-node status     # healthy and earning? standing, floors, market, why-not
covenant-compute-node earnings   # what you've earned, paid vs outstanding
covenant-compute-node earnings verify   # prove each paid row on-chain
covenant-compute-node bond       # this node's stake: posted, slashed, unbonding
```

`setup` walks a new operator from nothing to configured: it mints (or
keeps) the node identity, collects and validates the three trust
anchors — coordinator URL, the coordinator's pinned pubkey, your payout
address — probes the model backend, and writes `node.env` in the node
home. Every boot reads that file; real environment variables win over
it. The node refuses to boot with any anchor missing rather than guess.

`setup` also probes for a GPU: an NVIDIA card (`nvidia-smi`), or Apple
Silicon on macOS (advertised at a conservative share of unified memory).
A detected GPU is reported and pinned into `node.env`, so the node
advertises for GPU jobs from the first boot; a machine without one
advertises CPU-only. When no hardware is pinned, boot re-detects, so a
GPU added later is picked up without re-running `setup`.

The node home (`$COVENANT_COMPUTE_NODE_HOME`, default
`~/.covenant-compute-node`) is the operator's whole persistent state:
`identity.json` (earnings and reputation accrue against this key —
never regenerated), the hash-chained audit log whose root is embedded
in every signed receipt, `earnings.jsonl`, `outbox.jsonl` (results the
coordinator has not yet acknowledged — a coordinator restart at the
wrong moment cannot cost the operator a finished job; the serve loop
re-pushes them until one answer lands, and earnings credit only on a
released verdict), `accepted.jsonl` (jobs this node accepted and has
not finished — a node restart mid-job re-runs each one whose deadline
still allows at the next `serve`, instead of leaving it to the refund
sweep and eating the fault; a job whose execution killed the node
twice is dropped as poison), and `node.env`.

## Executors

The executor decides what a job physically runs as. Both inference
executors honor the buyer's signed sampling knobs (temperature, top_p,
max_tokens, seed, stop) when the job carries them; anything unset stays
the backend's default. Serve a vision model and image jobs work too: a
job's images reach Ollama as its native message-level `images` and an
OpenAI-compatible backend as `image_url` content parts.

- **`ollama`** — inference against a local Ollama server
  (`COVENANT_COMPUTE_OLLAMA_URL`); models advertised are models served.
  `COVENANT_COMPUTE_OLLAMA_KEEP_ALIVE` sets how long the model stays
  resident between jobs (`5m`, `1h`, `-1` to keep it loaded, `0` to
  unload at once); leave it unset for Ollama's default, or raise it on a
  node serving one model steadily so a sparse-traffic reload can't blow
  a job deadline. Streams: a job that signed the `stream` flag gets its
  tokens relayed to the coordinator as they generate. Executors that
  can't stream serve the same job one-shot — the flag is advisory, and
  settlement rides the signed receipt either way.
- **`openai-compat`** — inference against anything speaking the OpenAI
  chat-completions API: vLLM, llama.cpp server, LM Studio, a hosted
  endpoint, or Ollama's own `/v1` shim (`COVENANT_COMPUTE_OPENAI_URL`,
  plus `COVENANT_COMPUTE_OPENAI_API_KEY` if the endpoint requires
  one). Same posture as `ollama`: models advertised are the backend's
  own `/models` list, streaming is served over SSE, and token metering
  comes from the backend's `usage` report.
- **`whisper`** — speech-to-text against a local whisper.cpp install.
  `COVENANT_COMPUTE_WHISPER_MODEL` names the ggml model file (e.g.
  `ggml-base.en.bin`); `COVENANT_COMPUTE_WHISPER_BIN` overrides the
  `whisper-cli` path when it isn't on `PATH`. `setup --executor whisper`
  writes both, asking for the model file (or taking `--whisper-model`).
  Serves `transcription` jobs:
  a buyer's audio is written to a scratch file and run through the CLI
  under the same deadline and process-group controls as `subprocess`, and
  the transcript comes back as the job's output. The audio is data, never
  a command, so this runs the fixed CLI rather than a stranger's shell.
- **`say`** — text-to-speech against a local synthesizer, the mirror of
  `whisper`. Defaults to macOS's `say`; `COVENANT_COMPUTE_SAY_BIN` points
  at a compatible engine on other systems, and `COVENANT_COMPUTE_SAY_VOICE`
  sets the voice used when a job names none. `setup --executor say`
  configures it; on macOS that is the whole setup, since `say` ships with
  the OS. Serves `speech_synthesis`
  jobs: a buyer's text is written to a scratch file and voiced through the
  synthesizer under the same deadline and process-group controls as
  `subprocess`, and the audio (WAV or AIFF) comes back as the job's output.
  The text is data and the voice is validated to a bare name, so this runs
  the fixed synthesizer rather than a stranger's shell.
- **`container`** — batch commands inside a container
  (`COVENANT_COMPUTE_NODE_CONTAINER_IMAGE`, docker or podman, no
  network by default, memory/cpu/pids caps) — the right default for
  strangers' work.
- **`subprocess`** — batch commands directly on the host with env
  scrubbing, a scratch cwd, output caps and deadline kill, but no
  filesystem or network wall. The binary warns at boot when this
  executor is exposed to `batch_job` work.
- **`broker`** — rents a real GPU per lease from a cloud market instead
  of owning hardware. Serves `lease_session` jobs: when a buyer opens a
  lease, the broker takes the cheapest admissible machine, boots it
  carrying the buyer's SSH key, hands back the address, and destroys it
  when the buyer closes the lease or the window ends. Needs a funded
  market account (`COVENANT_VAST_API_KEY`) and a digest-pinned image
  (`COVENANT_COMPUTE_BROKER_IMAGE`); the account bounds its own spend.
  Provisioning and teardown are the operator's cost, never the buyer's:
  billing runs from the coordinator's accept, not from boot.
- **`agent`**: builds and checks paid coding tasks. A build runs Claude
  Code under covguard, which holds the model key, meters the spend and
  stops the run at its cap (`COVENANT_COMPUTE_AGENT_BUDGET_USD`, lowered
  to fit the buyer's offer). The key is read from the file named by
  `COVENANT_COMPUTE_AGENT_AUTH_TOKEN_FILE` and never reaches the agent.
  With `COVENANT_COMPUTE_AGENT_BUILDER=container` the agent runs in a
  container built from the task's check image: it sees only the checkout,
  and its one way out is the proxy. A check applies another seat's patch
  to a clean checkout, runs the buyer's commands in a container with no
  network, and signs its vote with the node key. Checks run only in the
  images `COVENANT_COMPUTE_AGENT_CHECK_IMAGES` allows.
  `COVENANT_COMPUTE_AGENT_MODEL` sets the model a task gets when it names
  none. The node needs docker, git, Claude Code and covguard; container
  builds run on macOS with Colima. A seat is paid its build's metered
  spend plus the coordinator's markup when the work passes, and the check
  price for every check it completes. Seats on one task never share a
  stake owner.
- **`echo`** — a loopback for wiring tests.
- **`lease-stub`** — serves `lease_session` jobs like `broker` but rents
  no machine. It hands back a placeholder address and holds the lease
  open, so the whole lifecycle (access grant, live meter, close, metered
  settlement) can be exercised without a market account. The access grant
  says no machine is behind it, and the binary warns at boot. For smoke
  tests.

Registration is benchmarked: the node proves its declared capability
profile with a known-answer self-test before the coordinator will match
it (`COVENANT_COMPUTE_NODE_SKIP_BENCHMARK=1` opts out for loopback
rigs). A lease is the exception, whether served by `broker` or
`lease-stub`. A lease has no known answer, and the only way to
demonstrate one at boot would be to rent a machine before a buyer has
paid, so a lease's capacity is proven per rental instead.
Admission is fail-closed — a job that doesn't match the declared
profile, price or deadline is rejected before it executes.

## Outages and shutdown

The serve loop never takes work its backend cannot serve: every pass
probes the executor's backend, and a dead model server pauses intake on
the spot. The node declares itself Offline — the coordinator re-matches
whatever was queued to it immediately, faulting nobody — and re-probes
every `COVENANT_COMPUTE_NODE_BACKEND_RETRY_SECS` (default 5) until the
backend answers, then announces itself back and resumes. Finished
results still deliver during an outage; only new work waits.

Stopping is graceful by default: the first ctrl-c/SIGTERM begins a
drain — the node turns Offline so its queued offers re-route right
away, whatever is mid-execution runs to completion, submits and
credits, and the process exits clean (promptly even from an idle
long-poll). A second signal exits immediately; `accepted.jsonl`
re-serves whatever that interrupted on the next boot.

## Getting paid

Every completed job is a signed `WorkReceipt`; the coordinator releases
escrow against it and pushes the payout to your payout address. The
address is validated at `setup` and again at every boot — one that
doesn't decode to a real 32-byte key refuses to serve at all, because
the alternative is earning money a transfer can never land. The
coordinator holds the same line on the wire: a registration carrying an
unpayable address is refused, so the rule binds operators running their
own client too. The node
polls its own paid/unpaid books (`COVENANT_COMPUTE_NODE_PAYOUT_POLL_SECS`)
and reconciles them against the coordinator's signed operator feed, so
`earnings` shows exactly what settled and what is still owed. Heartbeats
(`COVENANT_COMPUTE_NODE_HEARTBEAT_SECS`, default 15s) keep the node
matchable; a stopped node stops being offered work on its own.

You don't have to take the coordinator's word for any of it: `earnings
verify` re-reads every paid row's transaction from your own RPC
endpoint (`COVENANT_COMPUTE_NODE_RPC_URL`) and requires it to carry
this node's receipt-derived memo, the exact credited amount, and your
payout address as the recipient. Any contradiction exits nonzero.
`earnings verify --job <id>` checks a single payout without re-reading
the whole ledger.

When work stops coming, `status` answers why in one read: coordinator
reachability, directory standing (registered, declared status, last
seen), the reputation counts behind this node's score, an `unpaid
rows:` summary of this node's own job books by refund reason — naming
which conclusions fault its standing and which were a buyer walking
away pre-accept (`buyer_cancelled`, never a fault) — the deployment's
score and stake floors, and the matcher's verdict — each failing gate
prints with what fixes it. All of it comes from the coordinator's
public reputation endpoint plus the local ledger, so a `matchable: yes`
that still wins nothing points at the remaining per-job variables:
price and capability fit — which the closing market section reads
directly: the coordinator's live-capacity directory filtered to what
this node serves, one row per declared (kind, model) pairing, with
this node's ask placed against the row's floor. Asks compare on raw
micro-USDC whatever their unit, exactly how the matcher prices a job,
and generic (`any`) supply competes with every named-model row of the
same kind, so the rows cross-reference both directions.

## Stake

A deployment can require operators to put money behind their answers
(its `/federation/bond-info` names the floor). Post stake by
transferring to the deployment's receiving account with the memo
`compute-bond:v1:<this node's pubkey>`, then `bond claim
<tx-signature>` — the coordinator reads the transaction itself, so the
claim carries nothing but the signature. `bond` shows the whole
picture: posted, slashed (only coordinator-proven faults — a failed
canary probe or a redundancy minority — ever slash), what's unbonding
and what came back. Exit with `bond unbond <amount-micro-usdc>
<recipient>`: the request matures over the deployment's unbonding
window, stays slashable until the refund actually leaves, and stops
this node winning organic work immediately — the floor gates on
committed stake, not posted.

A deployment can also require CVNT staked on chain for this node's
identity. The reputation endpoint reports the minimum as
`stake_required`, in CVNT base units (CVNT has 6 decimals), and whether
this node meets it as `staked`. Stake from any wallet you control with
`covenant-compute-stake`, built from
`agent-os/crates/covenant-compute-lease-signer`:

```bash
covenant-compute-stake stake <node-pubkey> 1000000 --lock-days 30 --keypair <wallet.json>
covenant-compute-stake status <node-pubkey>
covenant-compute-stake unstake <node-pubkey> --keypair <wallet.json>  # after the lock ends
```

The tokens sit in a vault the settlement program controls. Only the
staking wallet can withdraw them, and only once the lock ends. Until
then the protocol's slash authority can send them to the treasury. A
stake counts while it stays locked past the longest lease (one day)
plus the deployment's dispute window, so it stops counting shortly
before it unlocks. To keep winning work, stake again from a second
wallet before that point.

`service install` pins the current binary, the node home and PATH into
a launchd agent (macOS) or systemd user unit (Linux): crashed nodes
restart, deliberate exits stand, and `service uninstall` removes the
manager entry without touching identity or earnings.

A node left running for months has to keep its own logs in check. On
macOS the agent points `COVENANT_COMPUTE_NODE_LOG_DIR` at `<home>/logs`,
where the running log is written in dated files with the last week kept,
so it can't grow without bound; a crash or startup error still lands in
`<home>/service.log`. On Linux the unit logs to journald, which rotates
on its own. Run from a terminal instead and logs go to the console.

## Configuration reference

`setup` writes the trust anchors and the capability profile into
`node.env`; everything else has a sane default. A real environment
variable always wins over `node.env`, so a service unit or a one-off run
can override any of these without re-running `setup`. A missing trust
anchor fails the boot, named. The payout address and the marketplace-fee
ceiling are validated and fail the boot if they won't parse; any other
knob that won't parse logs a warning and falls back to its default
rather than refusing to start, so check the boot log after changing one.

**Trust anchors and home** (the three anchors are required to serve)

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_COORDINATOR_URL` | (required) | The coordinator this node registers with. |
| `COVENANT_COMPUTE_COORDINATOR_PUBKEY` | (required) | The coordinator's pinned key, used to verify every escrow-hold attestation. |
| `COVENANT_COMPUTE_PAYOUT_ADDRESS` | (required) | Where payouts land. Validated at setup and every boot; an unpayable address refuses to serve. |
| `COVENANT_COMPUTE_NODE_HOME` | `~/.covenant-compute-node` | Persistent state: identity, audit log, earnings, outbox, accepted jobs, `node.env`. |

**Capability profile** (what this node advertises to the matcher)

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_NODE_EXECUTOR` | `subprocess` | `ollama`, `openai-compat`, `whisper`, `say`, `container`, `subprocess`, `broker`, `echo`, or `lease-stub`. |
| `COVENANT_COMPUTE_NODE_HARDWARE` | `cpu` | Hardware class advertised. A GPU (NVIDIA, or Apple Silicon on macOS) is auto-detected when this is unset. |
| `COVENANT_COMPUTE_NODE_VRAM_GB` | `0` | Advertised VRAM, for jobs that require a minimum. An override of the width only: setting it does not suppress GPU detection, so a GPU box that sets just this still advertises its detected card. |
| `COVENANT_COMPUTE_NODE_MODELS` | (backend's list) | Models advertised. An inference backend reports its own served models when this is unset. |
| `COVENANT_COMPUTE_NODE_JOB_KINDS` | inference or batch | `inference_call` for the inference executors, `batch_job` otherwise. Set `embedding` to serve embeddings from an inference backend that hosts an embedding model (or pass `setup --job-kinds embedding`), and the boot benchmark proves that claim with a real embedding rather than a chat probe. Comma-separated to serve several. The boot fails if a kind isn't servable by the executor (inference and embedding need `ollama`/`openai-compat`; transcription needs `whisper`; speech synthesis needs `say`; batch needs `subprocess`/`container`; a lease session needs `broker` or `lease-stub`), so a node never sells compute it would mis-execute. |
| `COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC` | `1000` | This node's ask. |
| `COVENANT_COMPUTE_NODE_PRICE_UNIT` | `per_job` | `per_job`, `per_million_tokens`, `per_gpu_second`, or `per_lease_hour`. |
| `COVENANT_COMPUTE_NODE_MAX_FEE_BPS` | (unbounded) | Refuse to serve a coordinator whose disclosed marketplace fee exceeds this. |
| `COVENANT_COMPUTE_REFERRAL_CODE` | (none) | Signed into registration so the partner who onboarded this node earns their fee share. |

**Ollama executor**

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_OLLAMA_URL` | `http://127.0.0.1:11434` | The local Ollama server. |
| `COVENANT_COMPUTE_OLLAMA_KEEP_ALIVE` | (Ollama's default) | How long a model stays resident between jobs (`5m`, `1h`, `-1` to pin, `0` to unload at once). |
| `COVENANT_COMPUTE_OLLAMA_DEFAULT_MODEL` | (none) | Model served when a job names none. A node serving exactly one model uses it automatically; set this only when several are served. |

**OpenAI-compatible executor**

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_OPENAI_URL` | `http://127.0.0.1:8000/v1` | Any server speaking the chat-completions API (vLLM, llama.cpp, LM Studio, a hosted endpoint). |
| `COVENANT_COMPUTE_OPENAI_API_KEY` | (none) | Bearer key, if the endpoint requires one. Never logged. |
| `COVENANT_COMPUTE_OPENAI_DEFAULT_MODEL` | (none) | Model served when a job names none. A node serving exactly one model uses it automatically; set this only when several are served. |

**Whisper executor** (speech-to-text against a local whisper.cpp install)

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_WHISPER_MODEL` | (required) | Path to the ggml model file (e.g. `ggml-base.en.bin`). Required when the executor is `whisper`; the boot refuses a missing file. |
| `COVENANT_COMPUTE_WHISPER_BIN` | `whisper-cli` | The `whisper-cli` binary, resolved on `PATH` unless given an absolute path. |

The advertised model name is `whisper-1` unless `COVENANT_COMPUTE_NODE_MODELS` sets it, decoupled from the on-disk file so a buyer asks for a stable id.

**Say executor** (text-to-speech against a local synthesizer)

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_SAY_BIN` | `say` | The synthesizer binary, resolved on `PATH` unless given an absolute path. macOS ships `say`; point this at a compatible engine elsewhere. |
| `COVENANT_COMPUTE_SAY_VOICE` | (backend's default) | The voice used when a job names none. A job may still name its own. |

The advertised model name is `say-1` unless `COVENANT_COMPUTE_NODE_MODELS` sets it, decoupled from the local tool so a buyer asks for a stable id.

**Container executor** (batch commands in an isolated rootfs; the right default for strangers' work)

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_NODE_CONTAINER_IMAGE` | (required) | The image to run jobs in. Required when the executor is `container`. The node pulls it on startup when it isn't already present, so the first job runs without waiting on the download. |
| `COVENANT_COMPUTE_NODE_CONTAINER_RUNTIME` | `docker` | `docker` or `podman`. |
| `COVENANT_COMPUTE_NODE_CONTAINER_OCI_RUNTIME` | (runtime's own) | A stronger OCI runtime such as `runsc` (gVisor) or `crun`. |
| `COVENANT_COMPUTE_NODE_CONTAINER_GPUS` | (none, CPU-only) | GPUs to pass through, e.g. `all`. |
| `COVENANT_COMPUTE_NODE_CONTAINER_NETWORK` | `none` | Container network. `none` cuts egress. |
| `COVENANT_COMPUTE_NODE_CONTAINER_MEMORY` | `512m` | Memory cap. |
| `COVENANT_COMPUTE_NODE_CONTAINER_CPUS` | `1` | CPU cap. |
| `COVENANT_COMPUTE_NODE_CONTAINER_PIDS` | `256` | Process-count cap. |
| `COVENANT_COMPUTE_NODE_CONTAINER_USER` | (image's default) | The user jobs run as, e.g. `1000:1000`. |

**Broker executor** (rents a real GPU per lease from a cloud market; owns no hardware)

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_VAST_API_KEY` | (required) | The market account the broker rents against; `COVENANT_VAST_API_KEY_FILE` reads it from a file instead. Without one the executor refuses to start. Never logged. |
| `COVENANT_COMPUTE_BROKER_IMAGE` | (required) | The digest-pinned image every rented session runs (e.g. `docker.io/nvidia/cuda@sha256:...`). Pin by digest so the machine a buyer gets is the machine that was measured. |
| `COVENANT_VAST_MAX_HOURLY_MICROS` | `1000000` | The most the broker pays per hour for one machine, in micro-USDC. Bounds the operator's loss on a single lease and filters the offer book. |
| `COVENANT_VAST_GPU_MODELS` | `L40S,L40,RTX 6000Ada,RTX A6000,A40,A100 PCIE,A100 SXM4` | GPU models the broker will rent, comma-separated. |
| `COVENANT_VAST_MIN_GPU_MEMORY_MIB` | `40000` | Smallest card the broker will rent, in MiB of VRAM. |
| `COVENANT_VAST_DISK_GB` | `16` | Disk requested on each rented machine. |
| `COVENANT_VAST_MAX_INET_COST_MICROS` | `50000` | Ceiling on a machine's metered bandwidth cost before it is filtered out. |
| `COVENANT_VAST_API_URL` | `https://console.vast.ai/api/v0/` | The market API endpoint. |
| `COVENANT_COMPUTE_BROKER_READY_TIMEOUT_SECS` | `240` | How long a rented machine may take to answer before the broker gives up and the buyer is refunded. Raise it for a large image that pulls slowly. |
| `COVENANT_COMPUTE_BROKER_READY_POLL_SECS` | `5` | How often the broker asks whether a launching machine is up. |
| `COVENANT_COMPUTE_LEASE_STUB_ENDPOINT` | `stub://no-machine` | The placeholder address the `lease-stub` executor returns. No machine is behind it; the access grant says so. |

**Registration, verification and lifecycle**

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_BENCHMARK_TIMEOUT_SECS` | `120` | Per-probe timeout for the known-answer self-test run before registering. |
| `COVENANT_COMPUTE_NODE_SKIP_BENCHMARK` | off | `1` skips the self-test (loopback rigs only). |
| `COVENANT_COMPUTE_NODE_HEARTBEAT_SECS` | `15` | How often the node heartbeats to stay matchable. |
| `COVENANT_COMPUTE_NODE_BACKEND_RETRY_SECS` | `5` | How often a paused node re-probes a dead model backend before resuming. |
| `COVENANT_COMPUTE_NODE_PAYOUT_POLL_SECS` | `60` | How often `earnings` reconciles against the coordinator's signed feed. `0` disables. |
| `COVENANT_COMPUTE_NODE_RPC_URL` | (required for verify) | Your own Solana RPC, read by `earnings verify` to confirm each payout on-chain. |
| `COVENANT_COMPUTE_NODE_LOG_DIR` | (unset) | Directory for dated, week-retained logs. The macOS service sets it to `<home>/logs`; on Linux, journald handles rotation. |
