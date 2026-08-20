# Closed alpha — verifiable inference network

Stands up the whole stack on one machine and drives real metered traffic through it:

```
llama.cpp (qwen3:8b)  <-  covenant-inferd serve  <-  outbound mTLS tunnel  <-  gateway  <-  client
```

The gateway runs with the payment gate armed. Payment is settled by the **dev prefunded
verifier**, not a chain: it matches a per-run allow-token and guards against replay, which
is enough to exercise the full `402 -> pay -> serve -> receipt` loop without a wallet or a
key. Real-USDC settlement is the gated cutover (`SolanaUsdcVerifier`) and is deliberately
not wired here.

This is a **closed alpha**: your own node, your own gateway, loopback only, throwaway certs.

## Run it

```sh
scripts/alpha/alpha-up.sh          # build, mint certs, boot the stack, wait healthy
scripts/alpha/alpha-soak.sh 30     # one 402 guard + 30 paid requests + the assertions
scripts/alpha/alpha-down.sh        # stop everything, keep the receipts log
```

`alpha-up.sh` is idempotent — if a node is already online at the gateway it just prints
the client base URL and dev token and exits. `alpha-soak.sh` takes an optional request
count (default 30).

## Where state lives

Everything runtime lives under `$COVENANT_ALPHA_STATE` (default
`~/.local/state/covenant-inference-alpha/`), never in the repo:

| path            | what                                                        |
|-----------------|-------------------------------------------------------------|
| `certs/`        | throwaway CA + gateway server cert + node client cert       |
| `pids/`         | one pidfile per component, used by `alpha-down.sh`          |
| `logs/`         | per-component logs (`llama`, `node-serve`, `gateway`, …)    |
| `device.json`   | the node's device identity keypair                          |
| `receipts.jsonl`| append-only receipts log (hashes + accounting only)         |
| `alpha.env`     | client base URL, dev token, rail config — sourced by soak/down |

## What the soak proves

- an unpaid request is refused with `402` and an `accepts[]` carrying the advertised price
- each paid request (unique payment `reference`, so no replay) returns a real qwen3
  completion **and** an `X-Covenant-Receipt` header
- `GET /v1/receipts` grows by exactly the number served
- the receipts log holds no prompt plaintext — a per-run canary codeword planted in a
  prompt is asserted absent

## Always-on (optional)

`org.local.covenant-inference-alpha.plist` is a **disabled** launchd template for running
the stack at login/boot. It pins a ~5 GB model resident, so only enable it on a machine
with RAM headroom. See the comment at the top of the plist for how to install it.
