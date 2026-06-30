# Covenant Agent Accountability — X Thread

Deploy day placeholders. No emojis.

## Post 1

```
Covenant agent accountability is live on Solana mainnet.

5 SOL slashable bond on an autonomous agent. Every action signed, every receipt public.

Break the declared scope, drain the bond. 30-day window starts <DEPLOY_DATE_YYYY_MM_DD>.

Bond PDA: <STANDING_BOND_PDA>
Program:  <MAINNET_PROGRAM_ID>
```

## Post 2

```
Autonomous agents hold capabilities and sign transactions. Without accountability, a compromised agent can burn permissions before anyone notices.

Covenant fixes this with provenance-backed permissions.

Every agent action is signed with ed25519. Scope declared on-chain. Slashable bond backs the commitment.
```

## Post 3

```
Agent signs receipts for every action: (agent_pubkey, target, action_type, slot, receipt_id).

Anyone submits slash tx with signed receipt violating on-chain scope. Program drains bond to pre-declared public-goods address. No multisig. No grace.

Slash path is the only signal that matters.
```

## Post 4

```
Agent's declared scope pinned on-chain:

allowed_actions:   bitmask of permitted action types
allowed_targets:   whitelist of targets
max_per_tick:      rate limit

Find signed receipt where action_type/target/slot violates scope. Submit it. Verifier agrees → bond drains.
```

## Post 5

```
Audit public. 96 tests, 5 adversarial passes, 0 critical/high/medium residual.

Day-zero ceremony drained 0.1 SOL to prove mechanism moves real lamports.

Ceremony bond: <CEREMONY_BOND_PDA>
Slash tx:      <CEREMONY_SLASH_TX>

Same program path standing 5 SOL bond uses. No special case.

github.com/open-covenant/covenant
```

## Post 6

```
If you find violating receipt:

  pak-slash submit \
    --bond <STANDING_BOND_PDA> \
    --receipt ./receipt.json \
    --dry-run

Drop --dry-run to send. Recipient fixed at bond creation, so 5 SOL goes to public-goods address regardless of who submits.

Claim 5 SOL bounty from Superteam Earn by replying with slash tx signature.
```

## Post 7

```
Window: 30 days from <DEPLOY_DATE_YYYY_MM_DD>.

If slashed: bond drains to public-goods, slasher claims 5 SOL from Earn.

If unslashed at expiry: 5 SOL bond + 1 SOL bonus donate to recipient with memo "Covenant agent lived. What you sign, you stand behind."

Either way, agent's actions are provably accountable.
```

## Post 8

```
Agents moving from chatbots to long-running engineering work. They need same infrastructure guarantees as critical systems: scoped authority, signed provenance, enforceable accountability.

This bounty proves agent accountability is real, not hypothetical. Same pattern applies to any agent signing transactions or holding permissions.
```

## Post 9

```
Covenant is operating layer for autonomous agents:

- Governed dispatch with scoped capabilities
- Durable memory across sessions
- Append-only audit with signed receipts
- Settlement primitives for agent economics

Bounty demonstrates Settlement primitive: slashable bonds turning accountability into on-chain enforcement.

opencovenant.org
```

## Post 10

```
Program, audit, CLI, verifier spec:

github.com/open-covenant/covenant

Program ID:    <MAINNET_PROGRAM_ID>
Program sha256: <BOND_PROGRAM_SHA256>

Verify on-chain binary matches audit. Then decide if you trust the bond.
```

## Optional Post 11

```
Mechanic is the message: agent without slashable bond is best-effort. Agent with one is accountable to anyone holding signed receipt.

If you find receipt violating scope, bond is yours to drain. CLI takes about a minute.
```

## Posting notes

Post 1 AFTER `<CEREMONY_SLASH_TX>` confirmed. Don't post before mechanism proven live.

Replace every `<...>` placeholder. Final grep for `<` must return zero hits.

Don't tag other protocols/authors. Audience recognizing accountability primitive surfaces it.

If post exceeds 280 chars after substitution, drop sha256 first, then program ID second. Never drop bond PDA.

Quote-tweet commit hash for audit verification, not branch link. Artifact must be immutable.
