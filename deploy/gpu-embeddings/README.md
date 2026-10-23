# Embeddings pilot diagnostics

`run.sh` installs pinned dependencies and runs `embed.py` on the rented GPU.
The scripts emit flushed phase markers to stderr for dependency installation,
input/model downloads, CUDA session initialization, batches, and output writing.
Markers contain phase names and numeric progress, not the input URL or text.
Keep both `embeddings.npy` and `report.json` in the GPU command's declared
`outputs`; output checks only cover paths declared in that command.

After publishing a script change, pin its exact commit in the dispatch payload.
Deploying the gateway does not update an older payload's pinned download URLs.
This change does not authorize a new dispatch or increase any budget.

## Incident: hatcher-gpu-embeddings-2026-10-22-01

The live gateway record was inspected on 2026-10-23. Job
`886ad9ac-b267-4d09-a39d-fe8484f77fad` timed out with an unknown exit code and
zero returned outputs. Its terminal status was incorrectly `completed` because
the gateway treated the model's final response as success regardless of the GPU
command result. Cleanup was confirmed by `vast:instance:57327870:destroyed`.

The final receipt records 263 runtime seconds and 125 provisioning seconds.
These are provider lifecycle intervals. The 378.75-second run-tool wall time
also includes readiness, staging, execution, and collection. None establishes
125 seconds of usable inference time. The retained setup logs are insufficient
to determine which setup or inference step exhausted the window; the pip root
warning is not evidence of the cause.

The original accounting remains unchanged: GPU $0.028898, model $0.03485535,
E2B $0.01206975, total $0.0758231; GPU refund 0.004065 USDC and provisioning
0.013735 USDC covered by Covenant. Model/E2B entries use recorded usage and the
configured tariff, not authoritative provider invoices. Historical run state
and financial ledgers must not be rewritten or the dispatch replayed.

The fix durably records GPU command outcomes before execution, retains bounded
log tails and wall-clock phase timings, and marks timeouts, unknown/nonzero
exits, and missing/oversized declared outputs as failure. Cleanup and accounting
still finish before the terminal failed result is sent. Hermes and the daemon
preserve that failed result's receipt text for connector delivery.

The 300-second booking still reserves 45 seconds for collection/cleanup, and
the dispatch deadline may shorten it further. Dependency installation and model
downloads consume this command window. Phase timings diagnose that consumption;
they are not a hard provider billing cutoff. A future retry requires separate
approval and a fresh dispatch ID, with the newly pinned scripts.
