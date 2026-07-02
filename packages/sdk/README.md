# @covenant-org/sdk

TypeScript SDK for the [Covenant](https://opencovenant.org) protocol on Solana. Build and sign
the on-chain instructions (agent registration, `$COVNT` staking, task escrow, credit purchase,
receipt anchoring), plus session-token verification and discovery types.

## Install

```bash
npm add @covenant-org/sdk @solana/web3.js
```

`@solana/web3.js` (v1) is a peer dependency, so bring your own version.

## Quick start

Every builder returns a wallet-agnostic descriptor. `toTransactionInstructions` turns it into
real, signable `TransactionInstruction`s (an 8-byte Anchor discriminator plus Borsh-encoded args
from the on-chain program IDLs), ready to drop into a transaction and sign with any wallet.

```typescript
import { Connection, Keypair, Transaction, sendAndConfirmTransaction } from '@solana/web3.js';
import {
  hash32FromText,
  prepareRegisterAgentInstruction,
  resolveSolanaNetwork,
  toTransactionInstructions,
} from '@covenant-org/sdk';

const operator = Keypair.generate();
const network = resolveSolanaNetwork(); // devnet by default; see Configuration
const connection = new Connection(network.rpcUrl, 'confirmed');

const bundle = prepareRegisterAgentInstruction({
  configAccount: CONFIG_PDA,
  operator: operator.publicKey.toBase58(),
  agentAccount: AGENT_PDA,
  agentKey: hash32FromText('my-agent'),
  metadataHash: hash32FromText('https://example.com/agents/my-agent.json'),
  capabilityHash: hash32FromText('research,settlement'),
});

const tx = new Transaction().add(...toTransactionInstructions(bundle));
await sendAndConfirmTransaction(connection, tx, [operator]);
```

The account addresses (the `*_PDA`s above) are derived from the protocol program; see the
protocol docs for their seeds. The builders take pre-resolved addresses and do not derive PDAs
for you.

## Token instructions need the COVNT mint

`stake`, `buy_credits`, `create_task`, and `release_task` move `$COVNT`, so each requires the
token mint as a `covntMint` field. Source it from your deployment or the `COVNT_MINT` env var;
`resolveSolanaNetwork().covntMint` is `null` until you set it.

```typescript
import { prepareStakeInstruction, toTransactionInstructions } from '@covenant-org/sdk';

const bundle = prepareStakeInstruction({
  configAccount: CONFIG_PDA,
  agentAccount: AGENT_PDA,
  positionAccount: POSITION_PDA,
  owner: operator.publicKey.toBase58(),
  ownerCovntAccount: OWNER_COVNT_ATA,
  stakeVault: STAKE_VAULT,
  covntMint: COVNT_MINT,
  amountCovnt: '1000000000',
  lockUntil: String(Math.floor(Date.now() / 1000) + 30 * 86400),
});
const [ix] = toTransactionInstructions(bundle);
```

## Configuration

`resolveSolanaNetwork(overrides?)` resolves the cluster, RPC/WS URLs, protocol program id, and
mint. Pass overrides directly, or set environment variables (a `NEXT_PUBLIC_`-prefixed form is
read for browser builds):

| Setting | Override | Env |
| --- | --- | --- |
| Cluster (`devnet` default) | `{ cluster: 'mainnet' }` | `COVENANT_SOLANA_CLUSTER` |
| RPC URL | `{ rpcUrl }` | `COVENANT_SOLANA_RPC_URL` |
| Protocol program id | `{ programId }` | `COVENANT_PROTOCOL_PROGRAM_ID` |
| COVNT mint | `{ covntMint }` | `COVNT_MINT` |

```typescript
const mainnet = resolveSolanaNetwork({ cluster: 'mainnet' });
```

## Surface

| Area | Exports |
| --- | --- |
| Settlement instructions | `prepareRegisterAgentInstruction`, `prepareStakeInstruction`, `prepareBuyCreditsInstruction`, `prepareCreateTaskInstruction`, `prepareReleaseTaskInstruction`, `prepareAnchorReceiptBatchInstruction` |
| Stake-program instructions | `prepareStakeInitializeInstruction`, `prepareStakeCreatePositionInstruction`, `prepareStakeIncreaseAmountInstruction`, `prepareStakeClaimInstruction`, `prepareStakeClosePositionInstruction`, plus the fee-router, pause, and authority admin builders |
| Serialization | `toTransactionInstruction`, `toTransactionInstructions` |
| Accounts and hashing | `isSolanaAddress`, `assertSolanaAddress`, `assertHash32`, `hash32FromText`, `ACCOUNT_SEEDS` |
| Network | `resolveSolanaNetwork`, `solanaExplorerHref` |
| Session auth | `verifySessionJwt`, `sessionSecret` |
| Discovery and tasks | `DiscoveryEventRecord`, `DiscoveryStats`, `TASK_STATUS_VALUES`, `TaskStatus` |
| Fixtures (development only) | `MOCK_AGENTS`, `MOCK_TASKS`, and the other `data/mock` helpers |

Everything imports from the package root. Instruction data is encoded from the program IDLs
(settlement `cov9UDyp...`, stake `CstkpU2q...`), so the wire bytes cannot drift from what the
on-chain program accepts.

## Stability

`0.1.0`, alpha. `compatibility/exports.v1.json` and `compatibility/instructions.v1.json` pin the
exported surface and each instruction's account order and data keys against accidental drift.
Semver support windows are not yet guaranteed, so pin an exact version.

## License

Apache-2.0.
