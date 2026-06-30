# Covenant Agent Accountability FAQ

## Is this real?

Three checks:

1. Program deployed at `<MAINNET_PROGRAM_ID>`. `.so` SHA256 `<BOND_PROGRAM_SHA256>` matches audited commit.
2. Slash mechanism proven. Day-zero ceremony tx `<CEREMONY_SLASH_TX>` drained 0.1 SOL from `<CEREMONY_BOND_PDA>`. Open on Solscan.
3. Bond escrow is PDA owned by program. Operator can't drain while bond unpaused. Slash drains to fixed public-goods recipient set at init.

## What is Covenant?

Operating layer for autonomous software agents. Provides:

- Intent: Normalized request shapes
- Runtime: Budget enforcement and sandboxing
- Memory: Durable storage across sessions
- Identity: ed25519 keys and peer registry
- Permissions: Scoped capabilities with signed delegation
- Comms: IPC, HTTP, MCP, A2A messaging
- Audit: Append-only log of signed actions
- Settlement: Slashable bonds and resource receipts

Bounty demonstrates Settlement primitive.

Web: https://opencovenant.org

## Why not multisig?

Multisig is human governance. Agents operate at machine speed. By the time multisig signs, compromised agent burned permissions.

Slashable bond is pre-commuted accountability. Bond posted before agent acts. If agent violates scope, ANYONE slashes immediately — no human approval.

## What if agent stops working?

Bond is slashable promise about signed actions, not liveness guarantee. If agent never signs receipt, bond never slashable. Operator can pause/withdraw after 4-day delay.

Deliberate. Closing this would require liveness oracles attesting agent "should have acted," converting bond to uptime escrow — different primitive.

Agent publicly commits to operating for 30-day window. Every receipt signed is permanent breach evidence.

## What stops agent acting outside scope?

Three layers:

1. Client-side: Agent runtime checks scope before signing. Refuses out-of-scope actions.
2. Signed receipts: Every receipt ed25519-signed over (agent_pubkey, target, action_type, slot). Can't forge "looks-in-scope" receipt.
3. On-chain: Slash handler re-checks scope. If receipt in-scope, tx fails. If out-of-scope and sig verifies, bond drains.

Client bugs happen. Chain is backstop.

## Can operator pause mid-window?

Yes. `set_paused(true)` starts 4-day delay before `withdraw` unlocks. During pause:

- Existing agent-signed receipts STILL valid breach evidence.
- Anyone holding scope-violating receipt from BEFORE pause can still submit slash.
- 4-day delay is public window to surface evidence.

Pausing doesn't retroactively immunize past actions.

## How is 5 SOL prize paid?

Two flows:

- **On-chain**: Slash drains 5 SOL to `slash_recipient` = `<DONATION_RECIPIENT>`. Slasher does NOT receive this.
- **Off-chain**: Superteam Earn listing pays 5 SOL to FIRST successful slasher via slash tx signature.

Separation intentional. If slash paid slasher directly, creates collusion (agent signs breach, accomplice slashes, they split bond). Routing to fixed public-goods address makes slash costly signal.

## Why no upgrade authority?

Upgrade authority retained 7 days post-deploy to patch bugs, then set to null. On `<DEPLOY_DATE_YYYY_MM_DD>` + 7 days program becomes immutable.

Deploy-day SHA256: `<BOND_PROGRAM_SHA256>`. If program patched within 7-day window, new SHA posted with rationale. After window, program can't change.

## What if I find bug in bond program?

Do NOT submit slash for bond program bug. Slash only fires on scope violations by agent. Real program bugs:

- GitHub Security Advisory: https://github.com/open-covenant/covenant/security/advisories/new
- Encrypted mail: security@opencovenant.org
- 90-day coordinated disclosure

Bond bugs earn separate credit, not 5 SOL slash bounty.

## What if operator disappears?

Three terminal states:

- **Slashed**: Valid slash executed; 5 SOL to `slash_recipient`.
- **Withdrawn**: Operator paused, waited 4 days, called `withdraw`; bond returned to operator.
- **Abandoned**: Agent called `keeper_abandon` on unfunded/drained bond; rent to operator, surplus to `slash_recipient`.

`slash_recipient` set at init, immutable. Operator disappearing doesn't change which addresses bond can drain to.

## What runs agent infrastructure?

Single worker (Render, Fly, or Hetzner — stated at deploy). SPOF for uptime. NOT SPOF for accountability:

- Past receipts remain on-chain, still valid breach evidence.
- 4-day pause/withdraw delay still applies.
- Killing worker doesn't let operator skip delay.

## Where is agent receipt feed?

`<AGENT_RECEIPT_FEED_URL>`

- NDJSON, one receipt per line, slot order
- HTTPS GET with optional `?since_slot=<u64>`
- Retention: 30-day window + 90 days post
- Rate limit: 60 req/min/IP
- Auth: none
- IPFS mirror at `/mirror.json`

## What if slash lands and Earn doesn't pay?

Pre-funded wallet: `<PRIZE_POOL_WALLET>` (Solscan in Earn listing). Commitment: verify within 24h, pay within 48h.

If no payout in 72h, escalate via:
- Public reply on Earn listing
- Solana Foundation / Superteam DAO arbitration
- On-chain memo from `<PRIZE_POOL_WALLET>` stating dispute

If `<PRIZE_POOL_WALLET>` not visible and funded at deploy, don't attempt.

## How to attempt

```bash
pak-slash submit \
  --bond <STANDING_BOND_PDA> \
  --receipt ./receipt.json \
  --rpc https://api.mainnet-beta.solana.com \
  --dry-run
```

If dry-run succeeds, drop `--dry-run`. Copy sig and reply on Earn listing.
