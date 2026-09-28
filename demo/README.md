# Covenant Compute, end to end, in under a minute

```sh
./demo/run-demo.sh --dry-run
```

That replays three runs from 2026-09-05 and then asks Solana mainnet and Solana
devnet to confirm them, so it works on any machine with `bash`, `python3` and
`curl`. It takes about 45 seconds and spends nothing.

## What the run shows

**A real GPU, paid for in USDC on Solana mainnet.** A buyer signs a lease at
100 micro-USDC per second over a 600 second window. A broker that owns no
hardware rents one machine on a live market, attaches the buyer's own SSH key
and hands back the address. The buyer holds the box and closes the lease. The
operator is paid for the seconds the session actually ran, and the payment
carries a memo binding it to the operator's signed work receipt.

In the recorded run that was an L40S, reachable 24 seconds after the lease was
accepted, held for 44638 ms. The operator was paid 0.004464 USDC and 0.045536
was swept back. A lease closed early costs what it ran for.

The 0.060000 hold behind that lease is a line in the coordinator's own ledger.
Custody held 0.050000 USDC on chain, less than the hold, so a session that ran
the full window could not have settled from it. The hold is bookkeeping, not
collateral. Closing that gap is what the second leg is for.

**The same arithmetic as an onchain meter.** `open_lease` and `delegate_lease`
land on Solana. The account then moves into a MagicBlock Ephemeral Rollup,
where the meter ticks once per second, each tick folding a receipt hash into a
provenance chain. Ticks are free to the renter and clear in tens of
milliseconds. `undelegate_lease` commits the final state back to Solana and
`settle_lease` pays the operator and refunds the rest.

The replay recomputes that hash chain from all 60 recorded tick receipts and
compares it against the root the program committed. If they ever stopped
matching, the script would say so and exit non-zero.

**Only the coordinator the renter named can move the meter.** The renter names
one coordinator key when the lease opens. That key is written into the meter
account and it is the only key allowed to tick, undelegate or settle. An
unauthorized tick is refused by the program, on the L1 and inside the rollup
alike.

The regression that proves it is
`programs/compute-lease/client/attack-unauthorized-tick.mjs`. It opens a lease,
funds it, and then attacks its own meter eight ways with preflight off, so
every rejection is a landed transaction anyone can look up:

| Code | What it means |
|---|---|
| 3005 | no coordinator account was supplied at all |
| 2001 | the meter's pinned coordinator is not this signer |
| 3010 | the real coordinator was named but did not sign |

Eight of eight were refused. The attacker asked for the full 600000 ms window
and then for `u64::MAX`; the lease then settled for the five seconds it
honestly ran, paying the operator 500 micro. The dry run replays all eight and
re-fetches one of them from devnet live.

What that does not cover: the refusals bind every key except the coordinator
itself, which can still meter any value up to the window ceiling. The ceiling
and the vault balance are the only limits on it.

## The boundary, stated plainly

The mainnet leg and the devnet leg are two artifacts joined by one signed work
receipt. They are not the same lease.

The mainnet settlement is an SPL transfer with a memo, signed by the
coordinator. It does not call the lease program and it does not touch the
rollup. The onchain meter runs on devnet, and its escrow token is a throwaway
6 decimal mint, because devnet USDC is not freely mintable. The program treats
any 6 decimal mint identically, so the arithmetic is the arithmetic that would
run against USDC.

The mainnet machine was rented and held for 44.6 seconds. No job ran on it, so
the payment is for access time priced by a clock, and nothing in it is tied to
computation. The GPU is not attested: "L40S, 46068 MiB VRAM" is the market's
own listing echoed back.

The buyer's deposit is still the coordinator's custodial ledger on the mainnet
leg. Money out is onchain. Money in is not yet.

Ticks inside the rollup are free to the renter, not free. `delegate`, the
commit and `settle` are ordinary Solana transactions, and the rollup validator
paid 33800 lamports on L1 to commit the meter back.

The rate used here is a demo rate. It is not a price.

## Running it live

```sh
./demo/run-demo.sh                 # run each leg live if this machine can, replay it otherwise
./demo/run-demo.sh --meter-only    # just the onchain meter and its refusals
./demo/run-demo.sh --live          # insist on live; fail loudly rather than replay
```

The refusals are always a replay. They are landed transactions from a
regression run, and re-sending them would mean opening a fresh lease to attack.

The onchain meter leg needs `node`, the client dependencies in
`programs/compute-lease/client`, and a funded devnet keypair at
`scratchpad/compute-mainnet/devnet-deployer.json`. The devnet faucet is
usually rate limited, which is why the script wants a local funder. Without
those the leg replays and says why.

The GPU leg rents a real machine with real credit, so it never runs unless you
pass `--spend` and set `COVENANT_VAST_API_KEY`. For a mainnet settlement it
also wants `COVENANT_COMPUTE_PAYOUT_SIGNER`,
`COVENANT_COMPUTE_FUNDING_KEYPAIR`, `COVENANT_COMPUTE_RPC_URL` and
`COVENANT_COMPUTE_PAYOUT_ADDRESS`. Without those four the rental and the meter
still run and the payout is recorded rather than pushed. The script says which
of the two it is doing before it starts.

Other options: `--ticks N`, `--no-attack`, `--fast` (no pauses, for CI),
`--offline` (skip the closing check against public RPC), `--no-color`,
`--help`.

## Files

| | |
|---|---|
| `run-demo.sh` | the whole demo, one script |
| `recorded-run.json` | the 2026-09-05 artifacts: transaction signatures, addresses, the eight refusals, and all 60 tick receipts. Public onchain data, no key material |

## Verify it without trusting any of this

The mainnet payout:

```sh
curl -s -X POST https://api.mainnet-beta.solana.com -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getTransaction","params":["vb2aRkWGhZeFY2ri5fhxaJTJUNqTMYwzDksf12GisBcqpSt9TMZBmRNubXo8KaQv51dw6DGhidFm2Dyb9h8ChKi",{"encoding":"jsonParsed","maxSupportedTransactionVersion":0}]}'
```

A refusal, which landed on devnet and failed with `Custom: 2001`:

```sh
curl -s -X POST https://api.devnet.solana.com -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getTransaction","params":["yDkvbni6xhRXCM6m3w7M4KXohEzhK3CZUUwYhvgfEJcw6iM9tjLrNrXYJdLUjPh6LPXRdkh9hVo2cBBxuBtBF3M",{"encoding":"json","maxSupportedTransactionVersion":0}]}'
```

- settlement: https://explorer.solana.com/tx/vb2aRkWGhZeFY2ri5fhxaJTJUNqTMYwzDksf12GisBcqpSt9TMZBmRNubXo8KaQv51dw6DGhidFm2Dyb9h8ChKi
- meter account on devnet, carrying the provenance root: https://explorer.solana.com/address/GBerDYJVjY3czQdRYv1ikcBmykY3sgj4vporgW6ZMS5y?cluster=devnet
- lease account on devnet: https://explorer.solana.com/address/5Uh4DrNda9C6iUGFSDoV78EJxEpS23mrENyBPZA257mp?cluster=devnet
- lease program on devnet: `CLSeVNrRi4TpXsXAkAuLh58kGCCAd1w1bj2CcEhTEESd`

Rollup signatures resolve on `https://devnet-eu.magicblock.app`, not on the L1
explorer.
