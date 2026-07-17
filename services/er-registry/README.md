# er-registry

The monitor behind Covenant's verified-ER registry. On a schedule it asks the
Magic Router which MagicBlock ephemeral rollups exist, DCAP-verifies the TDX
enclave of every ER that serves quotes, and keeps a Solana Attestation Service
attestation fresh for each — keyed to the validator identity the router already
returns, so an agent checks any ER in one account read.

Attestations expire (72h by default) and are renewed while they still have 48h
left. A verification failure never force-closes an attestation; the TTL bounds
how long a no-longer-verifiable enclave can stay marked verified, and if the
monitor stops running every attestation lapses and resolvers fail closed to
unverified. Consumers resolve through
[`@covenant-org/verified-er`](../../packages/verified-er) or any SAS client.

## Run

```
npm install
ER_MONITOR_KEY_FILE=<keypair.json> RPC_URL=<mainnet-rpc> node monitor.mjs
```

The signer must be an authorized signer of the Covenant credential
(`covenant` / `er-verified` v1, authority
`AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb`). The credential authority adds
and rotates signers; it never runs in this service.

| Env | Default | |
|---|---|---|
| `ER_MONITOR_KEY` / `ER_MONITOR_KEY_FILE` | — | signer keypair (json array / path) |
| `RPC_URL` | public mainnet | Solana RPC |
| `ROUTER` | `https://router.magicblock.app` | Magic Router |
| `TTL_HOURS` | 72 | attestation lifetime |
| `RENEW_BEFORE_HOURS` | 48 | renew when less than this remains |

Exit code is non-zero when any ER failed to verify or refresh, so a scheduler
alerts on partial failures.
