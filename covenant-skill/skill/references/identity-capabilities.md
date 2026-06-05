# identity-capabilities

How the daemon binds an action to a signed authorization, and how to think about
the `skill.` and `chain.` namespaces when planning what the agent may do.

## Agent identity

Each agent has a long-lived ed25519 keypair owned by the `covenantd` daemon —
not by the agent process. The agent never reads its own private key. Asking the
user for a seed phrase, mnemonic, secret recovery phrase, or keystore file is a
hard refusal; the daemon mints, stores, and uses signing material itself.

The agent's *public* identity is exposed to the model as `$AGENT_ID` (an
ed25519 public key in base58). Use it when forming capability grant or
verification requests, never when forming signing requests.

## Capability tokens

A capability is a signed object that binds:

- a **subject** — the agent public key allowed to use it
- an **action** — a dotted namespace path (`skill.use.covenant`,
  `chain.tx.<program>.<ix>`, `memory.read`, …)
- a **scope** — a JSON object whose fields constrain the action (cluster,
  pubkey, predicate)
- an **issuer** — the operator key that signed the grant
- an **expiry** — optional `expires_at` timestamp

Tokens are persisted in the daemon's capability store; revocation is immediate
and audited.

## The `skill.` namespace

Skill use is itself capability-gated. `skill.use.<name>` controls whether the
daemon will inject the skill's content into an agent's context. Unscoped (`{}`)
is valid; the action name alone is the gate.

```json
{
  "subject": "<agent pubkey base58>",
  "action":  "skill.use.covenant",
  "scope":   { "version": 1 },
  "expires_at": "2026-06-06T00:00:00Z"
}
```

The agent never grants itself a skill. When the agent decides a skill is
required, it surfaces the request to the operator and waits.

## The `chain.tx.<program>.<ix>` predicate

On-chain action authority is keyed by **program** and **instruction**, never
by transaction hash. A grant authorizes the daemon to sign **any** transaction
whose top-level instruction matches the predicate, within the scope.

```json
{
  "subject": "<agent pubkey>",
  "action":  "chain.tx.<program-id>.<instruction-name>",
  "scope": {
    "version": 1,
    "cluster": "devnet",
    "accounts": {
      "allow": {
        "destination": "<allowed-destination-pubkey-base58>"
      }
    },
    "budget": {
      "max_lamports": 1000000
    }
  },
  "expires_at": "2026-06-06T00:00:00Z"
}
```

The scope is *signed*: the daemon refuses to sign a transaction whose accounts
or budget violate the scope, even if the broader action matches. This is W009
encoded as a check rather than a hope.

## Grant flow

```bash
covenant capability grant \
  --subject "$AGENT_ID" \
  --action  "chain.tx.<program-id>.<instruction>" \
  --scope   '{"version":1,"cluster":"devnet","budget":{"max_lamports":1000000}}' \
  --expires-at "+1h"
```

The grant emits a `CapabilityGranted` audit event and returns a token id. The
operator can revoke any time with `covenant capability revoke <id>`; revocation
takes effect immediately.

## Refusal contract

If the agent encounters an action whose required capability is missing or
expired, it must:

1. Stop the in-flight operation.
2. Emit a `SkillRefused` audit row with the missing action string.
3. Surface a structured request to the operator. Never improvise an alternative
   path that bypasses the capability check.

This contract is what lets a user trust that an autonomous Covenant agent did
nothing beyond the envelope the operator signed.
