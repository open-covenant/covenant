# Operating the x402-over-ER path (end to end)

How an operator runs the MagicBlock ER settlement path so an agent can pay per call
by metering credits in an ephemeral rollup. Every piece below is built and
live-verified on devnet; this stitches them into one runbook.

The model: an agent calls a registered `er.<slug>` tool → the daemon routes it
(`solana-er:*` network) to the ER signer sidecar → the sidecar runs
`consume_credits` in the ER → the facilitator verifies the on-chain proof → 200,
with a settlement receipt recorded. Token custody, staking, and governance stay on
L1; only the program-owned credit balance is delegated to the rollup.

## 0. Prereqs (one-time)

- Settlement program deployed (devnet: `cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y`),
  a `Config` initialized, and the agent's `CreditAccount` opened + funded
  (`buy_credits`). See the settlement program docs.
- The ER signer sidecar built: `cargo build -p covenant-x402 --features solana
  --bin covenant-x402-er-signer` (`target/release/covenant-x402-er-signer`).

## 1. Facilitator (the verifier)

Already deployed on Render: **https://covenant-x402-er-facilitator.onrender.com**
(devnet, `PRICE=1`). To run your own: `cargo run -p covenant-x402-facilitator`
with `PROGRAM` / `ER` / `PRICE` (or deploy via `deploy/Dockerfile.facilitator` +
the `render.yaml` blueprint entry). `/health` → 200, `/paid` → the x402 challenge.

## 2. Wire the daemon (covenantd)

The funding key stays in the sidecar, never the daemon. Set on the daemon:

```bash
COVENANT_X402_ENABLED=true
COVENANT_X402_SIGNER_BINARY=/path/to/covenant-x402-signer   # default SPL signer (non-ER calls)
# ER signer sidecar + its env (the sidecar holds the key):
COVENANT_X402_ER_SIGNER_BINARY=/path/to/covenant-x402-er-signer
COVENANT_X402_ER_KEYPAIR=/path/to/owner-keypair.json        # the credit-account owner
COVENANT_X402_ER_PROGRAM=cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y
COVENANT_X402_ER_RPC=https://devnet-eu.magicblock.app
# Register one or more ER providers (each becomes an er.<slug> tool):
COVENANT_X402_ER_PROVIDERS='[{"slug":"quote","endpoint":"https://covenant-x402-er-facilitator.onrender.com/paid","per_call_cap":1,"description":"A market quote."}]'
```

The daemon advertises `er.quote` in its tool list. `signer_for("solana-er:*")` routes
ER providers to the ER sidecar; everything else uses the default SPL signer.

## 3. Grant the agent the capability

```bash
covenant capabilities grant tool.call.er.quote
```

(Scope it per provider if you want least privilege.) The same budget, settlement
receipt, and audit machinery every tool/paid call uses then applies.

## 4. Delegate the credit account (the prerequisite)

ER consumes only run if the credit account is delegated to an ER validator. Do this
before agents call ER providers, and re-do it after any top-up:

```bash
cd agent-os/programs/settlement-ephemeral/spike
node er-session.mjs delegate     # delegates [b"credits", owner] to the EU validator
node er-session.mjs balance      # L1 vs ER view
```

`er-session.mjs` uses the official `@magicblock-labs/ephemeral-rollups-sdk` for the
delegation PDAs. This is the operator's delegation utility today (see "Open decision"
below for automating it).

## 5. Agent pays

The agent calls the tool (over the daemon IPC / `POST /x402/...`):

```jsonc
// CallTool
{ "name": "er.quote", "arguments": {} }
```

→ `402 → consume_credits in the ER → 200` + a `ToolResult` with the content and the
settling signature. Gasless per call; the only cost is the fixed delegate + commit +
session overhead (~0.0003 SOL), constant in the number of calls.

## 6. Top-up

Credits are consumed in the ER. To refill: `node er-session.mjs undelegate`
(commits the final balance to L1) → `buy_credits` on L1 → `er-session.mjs delegate`
again. The account is frozen on L1 while delegated, so top-ups happen between
sessions.

## Verify the live path

```bash
node er-session.mjs delegate
URL=https://covenant-x402-er-facilitator.onrender.com/paid node remote-check.mjs
node er-session.mjs undelegate
```

`remote-check.mjs` pays the deployed facilitator end to end. The daemon paths are
covered by the `#[ignore]` live tests in `covenantd`
(`pay_x402_settles_through_the_er_signer_live`,
`er_provider_tool_settles_in_the_er_live`).

## Open decision: delegation-lifecycle automation

Today delegation is a manual operator step (4/6). The production options, in order of
effort:
1. **Manual / scripted** (current): the operator delegates before a session and
   undelegates to top up. Fine for a single long-lived credit account.
2. **Keeper**: a small loop that keeps a set of credit accounts delegated and
   undelegates them on a top-up signal. The right fit if many accounts cycle.
3. **Daemon-managed**: the daemon delegates a credit account on first ER use and
   undelegates on top-up/shutdown. Most seamless, but the daemon would sign the
   delegate txs for the credit-account owner — a key-custody + policy decision.

Pick (2) or (3) based on how many accounts cycle and where the owner key should live.

## Not in scope here (gated)

Mainnet: needs the ER program security review, the settlement-readiness gates
(`docs/internal/on-chain-settlement-readiness.md`), and MagicBlock's mainnet
validator-liveness/recovery answer. Flip `ER`/`PROGRAM` to mainnet only after those.
