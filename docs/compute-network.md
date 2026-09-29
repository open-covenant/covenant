# Covenant compute network

The compute network lets an agent buy inference and batch compute, or
rent a whole GPU by the second, as a native, budget-capped Covenant
capability, and lets anyone with spare hardware sell it for USDC. A
coordinator matches signed job envelopes against registered operator
nodes, escrows the price per job, verifies the operator's signed work
receipt, and settles — release to the operator on a verified receipt,
refund to the buyer otherwise. Money is real buyer revenue in SOL/USDC
terms (amounts are micro-USDC throughout); there is no network token
and no emission schedule.

This page is the map. Each crate's README is the operating manual for
its side of the wire.

## Crates

| Crate | Role |
| --- | --- |
| [`covenant-compute-protocol`](../agent-os/crates/covenant-compute-protocol/README.md) | Wire types and signing: job envelopes, capability profiles, work receipts, escrow-hold attestations. No I/O, no environment, no Solana dependency. |
| [`covenant-compute-coordinator`](../agent-os/crates/covenant-compute-coordinator/README.md) | The matching/escrow/settlement service and its binary: registers operators, holds funds per job, verifies receipts, pushes payouts, keeps every money book in a replayable journal. |
| [`covenant-compute-node`](../agent-os/crates/covenant-compute-node/README.md) | The operator binary: registers a declared capability profile, long-polls for offers, admits fail-closed, executes against a model backend, signs receipts, tracks earnings. Deliberately Solana-free — it never holds funds or signs a transaction. |
| [`covenant-compute-buyer`](../agent-os/crates/covenant-compute-buyer/README.md) | The demand-side client crate, plus the `covenant-compute-mcp` and `covenant-compute-openai` server binaries built on it. |
| [`covenant-compute-control`](../agent-os/crates/covenant-compute-control/README.md) | The beta GPU-workspace API: a bearer-token `/v1` surface that browses an app catalog and live GPU offers, resolves a request into a priced plan, and launches a bounded lease against the coordinator's engine. |

## Buying compute

Four surfaces, one behavior. All four sign the same envelope, pay the
same way, and return the same verified receipt; the tool names below
are shared by the daemon capability and the MCP server, defined once in
`covenant-compute-buyer`.

1. **Native daemon capability.** A `covenantd` agent calls `compute.*`
   tools like any other capability-gated tool: scoped by grants,
   debited from the agent's budget, written to the hash-chained audit
   log. Enable with `COVENANT_COMPUTE_ENABLED=1` and
   `COVENANT_COMPUTE_COORDINATOR_URL` on the daemon.
2. **MCP server.** `covenant-compute-mcp` (stdio) plugs the same tool
   set into any MCP client. Configure with
   `COVENANT_COMPUTE_COORDINATOR_URL`; cap spend with
   `COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC` (per call) and
   `COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC` (per server run; it resets on
   restart, so the funded balance is the lasting limit).
3. **OpenAI-compatible endpoint.** `covenant-compute-openai` serves
   OpenAI's `POST /v1/chat/completions`, the legacy `POST /v1/completions`,
   and `POST /v1/embeddings`, so any OpenAI client (the SDKs, LangChain,
   `curl`) buys network inference or embeddings by pointing its base URL at
   it, with no code change. `GET /v1/models` lists what is servable now;
   `stream: true` returns server-sent chunks, and `n` returns several
   completions in one request, each run and paid as its own job on the
   network. Under the same spend caps as the MCP server.
4. **Rust crate.** `covenant-compute-buyer` for programmatic
   dispatch — sign, submit, poll, verify, one call.

| Tool | What it does |
| --- | --- |
| `compute.capacity` | Read the live supply directory before buying: (kind, model) rows, operator counts, ask ranges, the floors in force. Free, no identities. |
| `compute.infer` | Buy one inference call: model, chat messages or prompt, optional images for a vision model, generation knobs; pays, waits, returns output plus the verified receipt. |
| `compute.embed` | Buy an embedding: one `text`, or a `texts` batch bought as a single paid job (one vector per input, in order). Returns the vectors plus the verified receipt. The retrieval and semantic-memory primitive; name the model so every vector in a store stays comparable. |
| `compute.run` | Buy one batch job (a command an operator's executor runs to completion). |
| `compute.transcribe` | Buy one speech-to-text transcription: an audio clip in, the text out, optionally with per-segment timestamps for captions. Pays, waits, returns the transcript plus the verified receipt. |
| `compute.speak` | Buy one text-to-speech synthesis: text in, an audio clip out. Pays, waits, saves the clip and returns its path plus the verified receipt. |
| `compute.stream_start` / `compute.stream_poll` | The streaming pair: start a paid inference stream, then cursor-poll chunks as the operator relays them. |
| `compute.receipts` | This buyer's purchase history, signed-read from the coordinator. |
| `compute.output` | Re-read a past job's output and its locally re-verified receipt: recover an answer bought once, without paying again. |
| `compute.verify` | The one-shot money trail for a job: receipt signature, escrow state, payout transaction where one exists. |
| `compute.deposit` / `compute.balance` / `compute.withdraw` / `compute.withdrawals` | Fund once by memo-tagged USDC transfer, watch the balance, take unspent funds back out, and read the withdrawal history to confirm each transfer landed. |
| `compute.dispute` | Contest a settled job inside the dispute window; a landed dispute counts against the operator's reputation. |
| `compute.cancel` | Withdraw a job no operator has accepted yet: full refund now instead of waiting out the deadline, and the job's idempotency key is freed to buy again. Never a clawback — accepted work settles by result or deadline. |

`compute.infer`, `compute.embed`, `compute.transcribe`, `compute.speak`,
and `compute.run` take an `idempotency_key`: a crashed buyer that retries
the same key replays the recorded purchase instead of paying twice.

The same tools take a `dry_run` flag. Set it to resolve the price,
routing, and deadline a real call would use and return them without
dispatching a job or spending anything, so an agent can weigh cost and
feasibility before it buys. A preview refuses an ask no operator can serve
instead of falling back to the price ceiling, and it reserves no purchase,
so it takes no `idempotency_key`. `compute.stream_start` opens a live feed
and has nothing to preview.

A buyer can require a minimum operator reputation with
`min_reputation_bps` (basis points, `8000` = 80%): the coordinator routes
the job only to operators rated at or above it, and a job that clears no
operator refunds rather than settle on a lesser one. It rides the signed
envelope, so the coordinator cannot lower it. The CLI takes
`--min-reputation-bps`; the daemon and MCP tools take a `min_reputation_bps`
argument; the OpenAI endpoint reads an `x-covenant-min-reputation-bps`
header. A default-priced floored buy is priced against only the operators
that clear the floor, so the offer it makes is one they would accept.

An inference buy can offer the model `tools` (function definitions) and a
`tool_choice`. The model may then answer with tool calls instead of prose:
each call names a function and carries JSON arguments, and the buyer runs
the functions and feeds the results back as `tool` messages to continue.
The tool definitions ride the signed envelope and the calls ride the
attested output, so the operator's receipt commits to what the model asked
to call, not only to what it wrote. Tools are available on `compute.infer`,
the daemon capability, and the OpenAI endpoint (OpenAI's `tools` /
`tool_choice` fields, and `tool_calls` in the reply). A job that offers
tools is served whole rather than token by token, so a streamed request
receives the calls in its closing frame. Whether a model calls tools
depends on the model an operator serves; one that ignores the offer simply
answers in prose.

An inference buy can also set a `response_format` to constrain the reply's
shape: JSON-object mode for any valid JSON, or a named JSON schema for
structured output. It rides the signed envelope alongside the sampling
knobs, so the constraint is part of the paid, attested input, and the
network maps it to the backend's own control (an Ollama node's `format`, an
OpenAI-compatible node's `response_format`). It is available on
`compute.infer`, the daemon capability, the CLI's `--response-format`, and
the OpenAI endpoint's `response_format` field. How closely the output
conforms depends on the model an operator serves.

An inference buy can also ask for `logprobs`: the log probability of each
generated token, with an optional number of most-likely alternatives per
token. The probabilities ride the signed receipt's attested output, so the
confidence figures a buyer receives are the ones the operator committed to,
not a separate unverifiable claim. They are available on `compute.infer`,
the CLI's `--logprobs`, and the OpenAI endpoint's `logprobs` and
`top_logprobs` fields, returned in OpenAI's own `choices[].logprobs` shape.

An inference buy can attach `images` to a message for an operator serving a
vision model: base64 bytes the model reads to describe a picture, pull text
out of a screenshot, or answer questions about a chart. The image rides
inside the signed envelope, so the picture a buyer paid to have looked at is
part of the attested input. It is available on `compute.infer`, the daemon
capability, the CLI's `--image`, and the OpenAI endpoint's `image_url`
content parts. Only inline base64 data URIs are served: the network relays
the bytes a buyer sends and does not reach out to fetch a remote address, so
a remote image URL is refused up front. What a model makes of an image
depends on the vision model an operator serves.

Speech-to-text is a job of its own. A transcription buy sends an audio clip
to an operator running a whisper backend and gets the text back, paid and
receipted like any other job. The audio rides base64 inside the signed
envelope, so the clip a buyer paid to have transcribed is part of the
attested input. Ask for timestamps and every segment returns with its start
and end time, the form a caption track or an alignment pass reads in. It is
available on `compute.transcribe`, the daemon capability, the CLI's
`transcribe --audio` (add `--timestamps`), and the OpenAI endpoint's
`POST /v1/audio/transcriptions`. That endpoint's `response_format` is `json`
(`{"text": …}` plus the verified receipt) or `text` for the bare transcript,
and `verbose_json`, `srt`, or `vtt` for the timestamped transcript as
segments or ready-to-use subtitles.

Text-to-speech runs the other way. A speech buy sends a line of text to an
operator running a synthesis backend and gets an audio clip back, paid and
receipted like any other job. Name a voice and a speaking rate, or take the
operator's defaults. It is available on `compute.speak`, the daemon
capability, and the CLI's `speak`, which save the clip to a file and return
its path, size, and format with the verified receipt, keeping the audio bytes
out of an agent's context. The OpenAI endpoint's `POST /v1/audio/speech` returns the audio as
the response body with the verified receipt in an `x-covenant-receipt`
header, right where an OpenAI client expects the bytes. The network produces
WAV and AIFF; a request for a container no operator can make is refused
before it costs anything.

## Renting a whole GPU

Some work needs a whole machine and a shell: an interactive session, a
training run, a tool that expects a CUDA device of its own. A lease rents
one GPU for a bounded window and returns an SSH address to reach it. The
window's ceiling, the per-second rate times the duration, is escrowed up
front; the session is billed for each second it runs, and the unused
remainder is refunded when the lease closes. A lease no operator accepts,
or one whose machine never comes up, is refunded in full.

The `covenant-compute` CLI rents from the command line: `lease open
--minutes <n> --rate <micro-usdc> --ssh-key <path>` places the lease,
waits for the machine to come up, and prints its address; `lease view
<job-id>` shows the endpoint, the elapsed time, and the running cost;
`lease close <job-id>` ends the session and settles the meter to the
seconds served. The `--gpu-class`, `--min-vram-gb`, and
`--min-reputation-bps` constraints a per-job buy takes apply to a lease
too, and the `covenant-compute-buyer` crate exposes the same flow to Rust
callers.

`covenant-compute-control` is the beta GPU-workspace API over the same
engine: a bearer-token `/v1` surface that browses an app catalog and the
live GPU market, resolves a request into a priced plan, and opens the
lease for a caller who never addresses the coordinator directly. Either
way a lease is a signed, escrow-settled job: the operator's receipt
meters the charge, and closing the lease returns whatever the session did
not spend.

## One job, end to end

1. The buyer signs a job envelope (price, deadline, capability
   requirement, input) and posts it to the coordinator.
2. The coordinator escrows the price from the buyer's balance and
   signs an escrow-hold attestation. Duplicate submissions of the same
   job id echo the existing state instead of double-holding.
3. The matcher filters operators by capability and standing — alive,
   not offline, above the deployment's score and bond floors and any
   reputation floor the buyer set on the job — then picks the cheapest
   ask; ties break on reputation.
4. The chosen node receives the offer on its long poll, re-verifies
   the buyer's signature and the escrow attestation itself (never the
   coordinator's word), checks the job against its own declared
   profile and capacity, and only then accepts. Every check is
   fail-closed.
5. The node executes with the job's remaining absolute deadline as its
   budget, meters the work (tokens, wall time), and signs a work
   receipt binding the job hash, the result hash, and its own audit
   chain root.
6. The coordinator verifies the receipt and settles: release to the
   operator on `Ok`, refund to the buyer otherwise. A job past its
   deadline refunds no matter which layer notices first — admission,
   settlement, or the background sweep apply the same predicate.
   Every refund pins its reason on the job record, and both parties'
   history reads serve it — a buyer sees whether the market timed out
   or they cancelled; an operator sees which refunds fault its
   standing and which were the buyer walking away.
7. Payout is pushed to the operator's registered address; the buyer
   can audit the whole trail afterward with `compute.verify`, and the
   operator with `earnings verify` against their own RPC endpoint.

## Selling compute

```bash
covenant-compute-node setup     # wizard: coordinator, payout address, backend probe
covenant-compute-node           # register + serve (or: service install)
covenant-compute-node status    # standing, floors, the market you compete in, why-not
covenant-compute-node earnings  # credited jobs; `earnings verify` re-checks paid rows on-chain
covenant-compute-node bond      # stake posture, claim/unbond, the exact memo to post
```

Executors behind one trait: `ollama` and `openai-compat` (vLLM,
llama.cpp, LM Studio, hosted endpoints) serve real token-metered
inference, or embeddings from a backend that hosts an embedding model
(`setup --job-kinds embedding`); `container` runs batch jobs inside a
locked-down OCI container; `subprocess` is the trusted-local fallback. A
model-serving node discovers its model list from the backend at boot so
the declared profile is honest by construction, and registration is
gated on a benchmark the node must actually pass — an embedding claim
proven with a real vector, an inference claim with a live completion.

## Trust model

- Every wire message is ed25519-signed over a domain-separated payload,
  and the exact signed bytes travel with the signature — verifiers
  never re-serialize and hope.
- Nobody extends trust transitively: the node re-verifies the buyer's
  envelope and the coordinator's escrow attestation; the buyer verifies
  the operator's receipt against the operator's own key.
- Declared capability is checked, not believed: benchmark on register,
  canary probes with known answers, and redundancy sampling that
  re-buys a released job from other operators and faults the minority.
- Reputation is derived from the audit trail (releases, faults,
  canaries, disputes, redundancy verdicts) and gates matching via a
  score floor; deployments can additionally require a posted bond,
  slashed only on coordinator-proven faults.
- A deployment can require each node to hold CVNT staked on chain for
  its identity. The stake stays locked in a program-controlled vault,
  counts only while its lock outlasts a lease and the dispute window,
  and the protocol's slash authority can send it to the treasury. The
  public deployment requires 1,000,000 CVNT per node; each operator's
  reputation endpoint reports the live minimum as `stake_required`.
- Public reads are anonymous aggregates: `/metrics`,
  `/federation/capacity`, `/federation/fees`, `/federation/subsidy`
  expose counts, asks, and money totals — never operator identities.
- The operator node never touches a chain: payouts are
  coordinator-initiated transfers to the registered payout address,
  and the payout signer runs isolated from the serving process. An
  address that doesn't decode to a real key is refused at node boot
  and again at registration, so the match book never carries an
  operator the coordinator can't pay.

## Money model

- Amounts are micro-USDC (`1_000_000` = 1 USDC). Payout rails are
  SOL/USDC only; nothing in the network mints, promises, or pays a
  token.
- Buyers prefund by USDC transfer with a claim memo
  (`/federation/deposit-info` names the account and memo shape),
  spend from that balance, and can withdraw the remainder.
- Escrow settlement is mechanical, never discretionary: release
  requires a verified receipt, refund requires a passed deadline or an
  explicit rejection — there is no unilateral button on either side.
- The coordinator's fee (`COVENANT_COMPUTE_FEE_BPS`, disclosed at
  `/federation/fees`) comes out of the operator's side and funds
  partner revenue shares; nodes refuse to register with a coordinator
  whose disclosed fee exceeds their own ceiling.
- Every escrow hold is tagged with its funding source (`organic` |
  `bootstrap`); subsidized deployments run under a hard subsidy-ratio
  ceiling. Bootstrap money is a bounded bridge, never a faucet.
- Conservation is scrapable: `/metrics` publishes every book's totals
  so deposits, escrow states, payouts, fees, and withdrawals can be
  cross-checked from the outside at any time.

## Running a deployment

The coordinator binary boots from environment knobs (persistent
identity and a replayable money journal under
`COVENANT_COMPUTE_COORDINATOR_HOME`); a restart reconciles every
in-flight hold from the journal before serving. The open-internet
posture is `COVENANT_COMPUTE_REQUIRE_PREFUNDED=1` plus the volumetric
backstops (`COVENANT_COMPUTE_MAX_OPERATORS`,
`COVENANT_COMPUTE_MAX_INFLIGHT_PER_BUYER`); the full knob tables live
in the coordinator README. Every public route answers garbage with a
4xx and keeps serving — the hostile-wire posture is a tested law of
the route table, as is the MCP server's envelope handling on the
demand side.

Version skew is first-class: every client stamps the wire protocol
version on its requests and every coordinator reply names its own, so
when a breaking wire change ships, a deployment raises
`COVENANT_COMPUTE_MIN_PROTOCOL` and stale clients are refused with
both numbers and the word "upgrade" — never a parse error. `/health`
and `/metrics` stay open to versionless probes.
