# Hatcher GPU embedding test

The first Hatcher test that does real work on the rented GPU. A Hatcher agent
asks for one text file to be embedded; the coding gateway books one GPU, runs
[`gpu-embeddings/run.sh`](gpu-embeddings/run.sh) on it, returns the embeddings
and a report as download links, cancels the GPU, and ends its result with a
receipt of what was charged. Hatcher checks the output on its own CPU with
[`gpu-embeddings/verify.py`](gpu-embeddings/verify.py).

The limits are the ones already enforced for the smoke test: one 300-second
booking capped at $0.20 of GPU time, and $0.50 for the whole dispatch. Covenant
pays for GPU start-up time and provider overruns.

## October 23 timeout correction

The Hatcher dispatch below timed out with no returned outputs, despite the
original terminal status incorrectly reporting success. Cleanup and the
$0.0758231 receipt are preserved. See [incident and diagnostics](gpu-embeddings/README.md)
for the verified timing distinction and the limits of the retained setup logs.

Release `00d5f26b032bcb5c3cdce20bb474f696462b45ff` changes the gateway and daemon
so a failed GPU command remains failed while its final receipt is delivered.
Deploy both with no work in flight, then verify the live revisions, readiness,
unchanged dispatch pin and stored state. Connector production code is unchanged.
The new phase-logging scripts are available at that same revision; a future
approved payload must pin it. The payload below documents the original attempt
and must not be resent. No replacement dispatch is armed by this correction.

## Initial rollout

All three services stay on branch `codex/hatcher-gpu-smoke`. The daemon is
unchanged. Deploy the gateway first, then the connector, with no dispatch in
flight (connector `GET /healthz` reports `in_flight: 0`).

Gateway (`covenant-hatcher-gateway`), new or changed environment:

```dotenv
COMPUTE_ARTIFACT_BASE_URL=https://covenant-hatcher-connector.onrender.com/artifacts
CODER_E2B_TARIFF_REF=https://raw.githubusercontent.com/open-covenant/covenant/a27898e172b939726752cbaaef4202304d21e55a/deploy/e2b-tariff-2026-10-22.json#sha256=78ef61d321bd07f3d3d1cf34c937f49cb857d5b1d6c1ff77b08a05b55572fa96
```

Outputs are stored under `/data/artifacts` on the existing disk and expire
after seven days. The refreshed tariff evidence is valid until
2026-10-29T17:22:42Z; after that `/readyz` fails `tariff` and runs are refused
until the evidence is renewed.

Connector (`covenant-hatcher-connector`):

```dotenv
HATCHER_ARTIFACT_ORIGIN=http://covenant-hatcher-gateway:10000
```

Verify after each deploy: the live commit in Render, gateway `/readyz` with
`dependencies.tariff.ok` and `dependencies.compute.ok`, connector `/healthz`
with the socket open, and `GET /artifacts/<32 characters>/x` on the connector
returning 404 (the route exists and found nothing).

## Arming the dispatch

The gateway accepts exactly one run key. For dispatch
`hatcher-gpu-embeddings-2026-10-22-01` from agent `cmp2dohkb00jtlsvsq1m5hriq`
it is:

```dotenv
CODER_ALLOWED_RUN_KEY=49dff9cc91a88665d9d29a4618cba5ddf8a484960f24a8f84c1e8835a76631a4
```

Set it, redeploy, and confirm `/readyz` before telling Hatcher to send. A
retry of the same dispatch returns the same result; a new attempt needs a new
dispatch ID and a new key.

## The dispatch

Hatcher replaces `<INPUT_URL>` with a public `https` link to a UTF-8 text file
of 1 to 10,000 lines, at most 4 MB. Each line becomes one row; a trailing
newline does not add a row.

```json
{
  "v": 1,
  "type": "dispatch",
  "dispatch_id": "hatcher-gpu-embeddings-2026-10-22-01",
  "agent_id": "cmp2dohkb00jtlsvsq1m5hriq",
  "intent": {
    "text": "Run the Covenant GPU embedding test exactly as written. Do not write files or install anything in this sandbox.\n1. Call gpu_workspace with action launch and duration_secs 300.\n2. Call gpu_workspace with action run, the job_id from step 1, outputs [\"embeddings.npy\", \"report.json\"], and this command unchanged:\npython3 -c \"import sys, urllib.request as r; [r.urlretrieve(sys.argv[1] + n, n) for n in ('run.sh', 'embed.py')]\" https://raw.githubusercontent.com/open-covenant/covenant/e47c7ad75a41d60f6c7d6fbdaf15fdc0bccc1072/deploy/gpu-embeddings/ && bash run.sh <INPUT_URL>\nIf run reports that the GPU is still starting, call run again with the same arguments.\n3. Call gpu_workspace with action cancel for that job_id as soon as run returns, whatever the result.\n4. Reply with the exit code, the full report.json, and stderr_tail if the exit code is not 0. Do not launch again or retry anything else.",
    "context": {
      "max_cost_usd": 0.5,
      "gpu": { "duration_secs": 300 }
    }
  },
  "grants": [
    {
      "scope": "compute.gpu_workspace",
      "constraints": { "max_usdc_micros": 200000, "duration_secs": 300, "max_launches": 1 }
    }
  ]
}
```

Use `deadline_ms = Date.now() + 600000`.

## What comes back

The corrected final result includes these bracketed blocks:

- `[gpu_workspace workload: {...}]`: command outcome and phase timings, separate
  from provider billing intervals. A timeout or missing declared output fails the dispatch.
- `[gpu_workspace outputs: [...]]`: when available, `embeddings.npy` and `report.json`, each
  with its size, SHA-256 and a download link valid for seven days.
- `[gpu_workspace final receipts: [...]]`: the GPU's settled receipt.
- `[run accounting: {...}]`: the receipt, version 2. With `final: true` the GPU
  settlement, model usage valuation and sandbox tariff calculation add up to
  `totalUsd`. This is not an authoritative consolidated provider invoice.
  `coveredByCovenant` lists GPU start-up time excluded from the dispatch charge.

`report.json` names the GPU, driver, ONNX Runtime version and execution
provider, the GPU memory in use before and with the model loaded, the input
and output hashes, and timings.

## Checking the result

```sh
pip install sentence-transformers numpy
python3 verify.py embeddings.npy report.json <INPUT_URL>
```

It confirms both files match `report.json`, that there is one row per line,
and that 20 random rows recomputed on CPU match at a cosine similarity of at
least 0.999. On 1,000 mixed lines the job's pipeline, run on CPU, matched
sentence-transformers to 0.99999976.

## Live check

On 2026-10-22 the deployed gateway code ran this job once on a real GPU, from a
one-off Render job with no model or sandbox, on the 1,413-line
`services/coding-gateway/src/server.ts` at `f42e10f00`:

- Start-up took 51 seconds. The command took 70 seconds, including installs, on
  an NVIDIA RTX A6000 (driver 570.133.20) through `CUDAExecutionProvider`, with
  803 MiB of GPU memory in use once the model was loaded.
- 1,413 rows came back; sampled rows matched sentence-transformers on CPU at a
  cosine similarity of 0.9999999 or better.
- Receipt: 101 seconds charged at 11,347 micro-USDC, 22,357 refunded, start-up
  5,730 covered by Covenant, instance destroyed.

Earlier attempts found three problems that are now handled: Jupyter refusing
connections just after Compute reported the workspace ready, Vast refusing to
create an instance on a machine that had just been released, and the control
plane refusing that machine's stale quote.

## Expected cost

About one minute of GPU start-up (Covenant pays), one to three minutes of
installs and embedding at the cheapest offer, a few model turns, and a sandbox
open for a few minutes. These are estimates: cold setup consumes the same bounded
command window and can time out. The operating limits remain $0.20 GPU and
$0.50 total; Covenant covers provider startup and cleanup overruns separately.
