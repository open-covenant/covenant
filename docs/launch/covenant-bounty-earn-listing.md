# Covenant Agent Accountability Bounty

5 SOL slashable bond on a live autonomous agent. 30-day window.

| Field | Value |
| --- | --- |
| Type | Bounty |
| Sponsor | Covenant |
| Slug | `covenant-agent-accountability-5-sol` |
| Prize | 5 SOL to first valid slasher. 1 SOL bonus to public-goods recipient on 30-day survival. |
| Deadline | `<DEPLOY_DATE_YYYY_MM_DD>` + 30 days, 00:00 UTC |
| Submission | Reply on listing with: (1) slash tx signature, (2) payout Solana address |
| Evaluation | First confirmed on-chain slash tx against `<STANDING_BOND_PDA>` invoking `<MAINNET_PROGRAM_ID>` that verifier accepts. Tiebreak: confirmed slot, then lower tx signature byte-lex. |
| Geographic | Worldwide except OFAC-sanctioned jurisdictions |
| Skills | Solana programs, ed25519, mempool analysis |

## Mechanic

Live agent on mainnet with 5 SOL bond backing its attested actions. Every action is ed25519-signed and bound to declared scope. Sign outside scope → anyone drains the bond.

Bond survives 30 days → 5 SOL + 1 SOL bonus donate to public-goods recipient.

Valid slash inside window → bond drains to recipient, slasher claims 5 SOL from this listing.

Real lamports. Ceremony tx `<CEREMONY_SLASH_TX>` drained 0.1 SOL from `<CEREMONY_BOND_PDA>` to prove the path works.

## Artifacts

| Artifact | Value |
| --- | --- |
| Bond program | `<MAINNET_PROGRAM_ID>` |
| SHA256 | `<BOND_PROGRAM_SHA256>` |
| Standing bond PDA | `<STANDING_BOND_PDA>` |
| Slash recipient | `<DONATION_RECIPIENT>` |
| Agent pubkey | `<AGENT_PUBKEY>` |
| Operator pubkey | `<OPERATOR_PUBKEY>` |
| Repo | https://github.com/open-covenant/covenant |

Verify:
```bash
solana program dump <MAINNET_PROGRAM_ID> /tmp/bond.so
sha256sum /tmp/bond.so
```

Inspect scope:
```bash
cargo install --git https://github.com/open-covenant/covenant --bin pak-slash
pak-slash inspect <STANDING_BOND_PDA> --rpc https://api.mainnet-beta.solana.com
```

## Slash conditions

Verifier runs on-chain. Slash succeeds iff:

1. Receipt violates declared scope (action not allowed, target not allowed, or slot out of range)
2. Receipt is signed by agent (ed25519 precompile, forgeries fail)
3. Receipt not used before (replay protection via PDA)
4. Bond unpaused (operator can pause, but past receipts remain slashable during 4-day withdraw delay)

Spec: `docs/pak-verifier-spec.md`. 96 tests, 5 audit passes, 0 critical/high/medium residual.

## Payout

Two flows:

1. **On-chain**: Bond drains 5 SOL to `<DONATION_RECIPIENT>`. This is a donation, not paid to you.
2. **Earn**: Claim 5 SOL from this listing by replying with slash tx signature.

We verify on-chain that tx is confirmed, invoked correct program, touched bond, succeeded, and hit deadline.

First slasher by confirmed slot wins. Tiebreak: lower tx signature byte-lex.

SLA: Verified within 24h, paid within 48h. Pre-funded wallet at `<PRIZE_POOL_WALLET>` (Solscan).

## How to attempt

```bash
git clone https://github.com/open-covenant/covenant
cd covenant/agent-os/crates/pak-slash
cargo install --path .
```

Inspect:
```bash
pak-slash inspect <STANDING_BOND_PDA> --rpc https://api.mainnet-beta.solana.com
```

Dry-run:
```bash
pak-slash submit \
  --bond <STANDING_BOND_PDA> \
  --receipt ./receipt.json \
  --recipient <DONATION_RECIPIENT> \
  --rpc https://api.mainnet-beta.solana.com \
  --dry-run
```

If `WOULD SUCCEED`, drop `--dry-run`. Copy tx sig and reply to claim.

## What you're finding

Agent publishes signed receipts for every action: agent pubkey, action type, target, executed slot, receipt ID.

Find receipt violating scope declared on-chain. Submit it. Verifier agrees → bond drains.

Receipt sources:
- Agent feed: `<AGENT_RECEIPT_FEED_URL>`
- Mempool monitoring (archive RPC)
- Covenant audit log

## Out of scope

No bounty for:
- Social engineering / phishing operator
- Key compromise outside attestation
- Bugs in downstream protocols
- Agent stopping activity (funded-absentee is deliberate; bond backs signed actions, not uptime)
- Submitting forgeries (sig check fails)

## Survival outcome

If unslashed at expiry: operator pauses, waits 4 days, withdraws to `<DONATION_RECIPIENT>`. Separate 1 SOL bonus tx with memo:

> Covenant agent lived. What you sign, you stand behind.

6 SOL total to public goods. Tx posted on-chain and linked from repo.

## Links

Program: https://solscan.io/account/<MAINNET_PROGRAM_ID>
Bond: https://solscan.io/account/<STANDING_BOND_PDA>
Ceremony: https://solscan.io/tx/<CEREMONY_SLASH_TX>
Audit: `docs/pak-bond-program-audit.md`
Verifier: `docs/pak-verifier-spec.md`

Covenant: https://opencovenant.org | $CVNT | Paper: https://doi.org/10.5281/zenodo.20134416
