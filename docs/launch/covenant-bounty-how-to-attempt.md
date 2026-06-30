# Covenant Agent Accountability Bounty: How to Slash

Deploy: `<DEPLOY_DATE_YYYY_MM_DD>` | Bond: `<STANDING_BOND_PDA>` | Program: `<MAINNET_PROGRAM_ID>`

## Goal

Find agent-signed receipt violating on-chain scope. Submit slash. If it lands, bond drains to public-goods recipient and you claim 5 SOL from Earn.

Entirely on-chain. No social proof, no trusted oracle.

## CLI

```bash
cargo install --git https://github.com/open-covenant/covenant --bin pak-slash pak-slash
```

Binary: `pak-slash`. Subcommands: `inspect` (read bond), `submit` (slash).

## Read the bond

```bash
pak-slash inspect <STANDING_BOND_PDA> --rpc https://api.mainnet-beta.solana.com
```

Output:
```
bond_pda:             <STANDING_BOND_PDA>
agent_pubkey:         <AGENT_PUBKEY>
operator_pubkey:      <OPERATOR_PUBKEY>
created_slot:         <CREATED_SLOT>
escrow_lamports:      5000000000
scope_hash:           <SCOPE_HASH>
allowed_actions:      0b00000111
allowed_targets:      Some([<PUBKEY_A>, <PUBKEY_B>])
max_per_tick:        10
slash_recipient:      <DONATION_RECIPIENT>
status:               Active
```

`escrow_lamports`: drains on successful slash
`scope_hash`: commits to canonical scope blob
`allowed_actions`: bitmask of permitted action types
`allowed_targets`: None = any, Some([]) = deny-all, Some([..]) = whitelist
`max_per_tick`: rate limit (NOT a verify_slash gate)

## Scope structure

Canonical encoding (from `covenant-percolator-bond/src/scope.rs`):

```rust
pub struct BondScope {
    pub dummy_market: Pubkey,
    pub allowed_actions: ActionMask,
    pub allowed_assets: Option<Vec<u16>>,
    pub max_actions_per_tick: u32,
}
```

For Covenant agents: `dummy_market` → `primary_target`, `allowed_assets` → `allowed_targets`.

Scope bytes not signed by agent. Supplied separately to slash tx, hashed on-chain. If `sha256(your_scope) != bond.scope_hash`, slash fails.

## Hunt for breach

Find receipts at:
- Agent feed: `<AGENT_RECEIPT_FEED_URL>`
- Mempool / archive RPC
- Covenant audit log

Receipt structure:
```json
{
  "agent_pubkey": "<base58>",
  "bond_pda": "<base58>",
  "bond_created_slot": 312456000,
  "action": {
    "market": "<base58>",
    "action_bit": 5,
    "asset_index": 7,
    "executed_slot": 312456789,
    "receipt_id": 1
  },
  "signature": "<base58, 64 bytes>"
}
```

For Covenant agents: `market` → `target`, `asset_index` → `target_id`.

Slashable if:
- `target` != `scope.primary_target` (when specified)
- `action_type` bit not in `scope.allowed_actions`
- `target_id` not in `scope.allowed_targets` (when `Some([..])`)
- `executed_slot < bond.created_slot` (pre-bond actions not slashable)

## Signature payload

Ed25519 signs 151 bytes directly:

```
RECEIPT_SIGN_DOMAIN  = b"covenant-receipt-v2"      // 19 bytes
payload =
    RECEIPT_SIGN_DOMAIN                            // 19
 || program_id                                     // 32
 || bond_pda                                       // 32
 || bond.created_slot.to_le_bytes()                //  8
 || canonical(AttestedAction {
        market,                                    // 32
        action_bit: u8,                            //  1
        asset_index: u16 LE,                       //  2
        executed_slot: u64 LE,                     //  8
        receipt_id: u64 LE,                        //  8
    })                                             // 51
// total = 151
```

Precompile does NOT hash first. Bytes must match exactly or sig fails.

## Dry-run

```bash
pak-slash submit \
  --bond <STANDING_BOND_PDA> \
  --receipt ./receipt.json \
  --recipient <DONATION_RECIPIENT> \
  --rpc https://api.mainnet-beta.solana.com \
  --dry-run
```

Output:
- `WOULD SUCCEED: slash would drain 5000000000 lamports to <DONATION_RECIPIENT>`
- `WOULD FAIL: <SYMBOL> (Custom N)`

Fix until dry-run succeeds. Free simulation of exact on-chain verifier.

## Live submit

```bash
pak-slash submit \
  --bond <STANDING_BOND_PDA> \
  --receipt ./receipt.json \
  --recipient <DONATION_RECIPIENT> \
  --rpc https://api.mainnet-beta.solana.com
```

If lands, copy sig and reply on Earn listing to claim.

## Payout

Reply on Earn listing with:
1. Slash tx signature
2. Payout Solana address

SLA: Verify within 24h, pay within 48h. Pre-funded: `<PRIZE_POOL_WALLET>`.

## Failures

| Symbol | Fix |
|--------|-----|
| `SLASH_MISSING_KEEPER_SIG` | Update CLI; don't hand-craft tx |
| `SLASH_BAD_INSTRUCTIONS_SYSVAR` | Update CLI |
| `ReceiptInScope` | Not a breach; find another |
| `SignatureVerifyFailed` | Receipt forged or bytes wrong |
| `ScopeHashMismatch` | Re-fetch scope via `inspect --raw-scope` |
| `AccountAlreadyInUse` | Receipt already slashed; find different receipt_id |

## CRITICAL

Bond escrow (5 SOL): drains to `<DONATION_RECIPIENT>`. Public-goods donation, NOT paid to you.

Earn bounty (5 SOL): paid to whoever submits first valid slash tx signature. THIS is your claim.

Two separate flows. On-chain slash is proof; Earn payout is bounty.

Separation is intentional. Routing bond to fixed address prevents collusion (agent signs breach, accomplice slashes, they split bond).

## Responsible disclosure

Bond program bugs (not agent scope violations): do NOT submit slash. Use:

- GitHub Security Advisory: https://github.com/open-covenant/covenant/security/advisories/new
- Encrypted mail: security@opencovenant.org
- 90-day coordinated disclosure

Bond bugs earn separate credit, not 5 SOL slash bounty.
