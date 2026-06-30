# @covenant/said-bridge

TypeScript worker that drives [SAID Protocol](https://saidprotocol.com)'s on-chain instructions for a Covenant daemon. The Rust crate `covenant-said-bridge` spawns this as a subprocess for `register-agent`, `get-verified`, `submit-anchor`, and `validate-work`. The Rust crate handles REST and xchain reads in-process.

The worker is also usable from a shell for smoke testing.

## Calling convention

```
node dist/worker.js <command>
```

Commands and how they read input:

| Command          | Payload                                                                  | Source       |
| ---------------- | ------------------------------------------------------------------------ | ------------ |
| `status`         | none                                                                     | argv only    |
| `register-agent` | `{ "metadataUri": "https://..." }`                                       | stdin (JSON) |
| `get-verified`   | none, or `{}`                                                            | stdin (JSON) |
| `submit-anchor`  | `{ "anchorIndex", "startSeq", "endSeq", "merkleRootHex" }`               | stdin (JSON) |
| `validate-work`  | `{ "agent", "taskHashHex", "passed", "evidenceUri" }`                    | stdin (JSON) |

The worker writes exactly one envelope on stdout and exits.

```
{ "ok": true,  "data":  { ... } }
{ "ok": false, "error": "<message>", "name": "<ErrorName>" }
```

`submit-anchor` and `validate-work` currently return `BridgeUnsupportedError` until said-sdk publishes those instructions.

## Configuration

Every variable is read from the inherited environment. All paid gates default off.

| Variable                                       | Purpose                                                         | Default                                |
| ---------------------------------------------- | --------------------------------------------------------------- | -------------------------------------- |
| `COVENANT_SAID_ENABLED`                        | Master toggle. Worker refuses every paid verb when off.         | off                                    |
| `COVENANT_SOLANA_CLUSTER`                      | `mainnet` or `devnet`. Selects which program + RPC the worker resolves. | `devnet` |
| `COVENANT_SAID_KEYPAIR`                        | Path to a Solana CLI JSON keypair file (64-byte array).         | unset                                  |
| `COVENANT_SAID_MAINNET_RPC_URL`                | RPC endpoint when cluster=mainnet. Falls back to global `COVENANT_SAID_RPC_URL`. | `https://api.mainnet-beta.solana.com` |
| `COVENANT_SAID_DEVNET_RPC_URL`                 | Same, for devnet.                                               | `https://api.devnet.solana.com`        |
| `COVENANT_SAID_API_BASE_URL`                   | SAID REST base URL. Rejected if not http(s).                    | `https://api.saidprotocol.com`         |
| `COVENANT_SAID_ALLOW_PAID_REGISTER`            | Open `register-agent` paid gate.                                | off                                    |
| `COVENANT_SAID_ALLOW_PAID_VERIFY`              | Open `get-verified` paid gate (costs 0.01 SOL).                 | off                                    |
| `COVENANT_SAID_ALLOW_PAID_ANCHOR`              | Open `submit-anchor` paid gate.                                 | off                                    |
| `COVENANT_SAID_ALLOW_PAID_VALIDATE_WORK`       | Open `validate-work` paid gate.                                 | off                                    |
| `COVENANT_SAID_STDIN_TIMEOUT_MS`               | How long the worker waits for stdin before erroring.            | `10000`                                |
| `COVENANT_SAID_RPC_TIMEOUT_MS`                 | How long each said-sdk RPC call races against.                  | `30000`                                |

## Examples

Status, no signer required:

```
node dist/worker.js status
```

Status against mainnet:

```
COVENANT_SAID_ENABLED=1 \
COVENANT_SOLANA_CLUSTER=mainnet \
COVENANT_SAID_KEYPAIR=$HOME/.config/solana/covenant-agent.json \
node dist/worker.js status
```

Register an agent on mainnet (real SOL):

```
echo '{"metadataUri":"https://opencovenant.org/.well-known/said-agent.json"}' | \
  COVENANT_SAID_ENABLED=1 \
  COVENANT_SOLANA_CLUSTER=mainnet \
  COVENANT_SAID_ALLOW_PAID_REGISTER=1 \
  COVENANT_SAID_KEYPAIR=$HOME/.config/solana/covenant-agent.json \
  node dist/worker.js register-agent
```

Verify (costs 0.01 SOL):

```
COVENANT_SAID_ENABLED=1 \
COVENANT_SOLANA_CLUSTER=mainnet \
COVENANT_SAID_ALLOW_PAID_VERIFY=1 \
COVENANT_SAID_KEYPAIR=$HOME/.config/solana/covenant-agent.json \
node dist/worker.js get-verified
```

## Daemon use

`covenantd` spawns the worker by name from `PATH` by default. Set `COVENANT_SAID_WORKER_CMD` to point at a local build (`node /abs/path/dist/worker.js`). The Rust caller forwards the inherited env and a JSON payload on stdin; the worker writes one envelope on stdout and exits.

## CLI

The `covenant` CLI exposes the daemon's SAID verbs:

```
covenant said status
covenant said lookup --wallet AdChc…
covenant said free-tier --address AdChc…
covenant said inbox --chain solana --address AdChc…
covenant said register --metadata-uri https://…
covenant said verify
covenant said anchor --start 0 --end 4 --root <64-hex>
covenant said anchor-status
```

These talk to a running `covenantd` over its UNIX socket. Start the daemon first.

## Two-key design

The operator key signs Covenant settlement (`~/.config/solana/id.json`). The SAID owner key (`COVENANT_SAID_KEYPAIR`) signs SAID instructions. They can rotate independently. The wallet that signs a `register-agent` becomes the agent's identity on SAID.
