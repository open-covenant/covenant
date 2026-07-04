# @covenant-org/sdk

TypeScript SDK for the [Covenant](https://opencovenant.org) protocol on Solana. A high-level
client that derives PDAs, signs, sends, and reads decoded on-chain state, plus the lower-level
instruction builders (agent registration, `$CVNT` staking, task escrow, credit purchase, receipt
anchoring), PDA derivation, account decoding, and Node + browser (wallet-adapter) signing.

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

The account addresses above are PDAs of the protocol program. `deriveConfigPda`, `deriveAgentPda`,
`deriveTaskPda`, `deriveCreditsPda`, `deriveStakePositionPda`, and `deriveReceiptBatchPda` derive
them for you, so you rarely pass a raw PDA by hand.

## Client

For the full path — derive PDAs, sign, send, confirm, and read decoded state — use `CovenantClient`:

```typescript
import { Connection, Keypair } from '@solana/web3.js';
import { CovenantClient, keypairSigner, hash32FromText } from '@covenant-org/sdk';

const client = new CovenantClient({
  connection: new Connection('https://api.mainnet-beta.solana.com', 'confirmed'),
  signer: keypairSigner(operatorKeypair),
});

// read decoded on-chain state
const config = await client.getConfig();
const agent = await client.getAgent(hash32FromText('my-agent'));

// register: derives the config + agent PDAs, signs, sends, confirms, returns the signature
const signature = await client.registerAgent({
  agentKey: hash32FromText('my-agent'),
  metadataHash: hash32FromText('https://example.com/agents/my-agent.json'),
  capabilityHash: hash32FromText('research,settlement'),
});
```

The client fills the signer as the operator, owner, or client account and resolves the `$CVNT`
mint from the on-chain config, so you pass only the token accounts and the arguments. In the
browser, hand it a wallet-adapter wallet instead of a keypair:

```typescript
import { walletAdapterSigner } from '@covenant-org/sdk';
const client = new CovenantClient({ connection, signer: walletAdapterSigner(wallet) });
```

Reads work without a signer. `fetchAgent`, `fetchConfig`, `fetchTask`, and the `decode*` functions
are also exported directly for reading state without a client.

## Token instructions need the CVNT mint

`stake`, `buy_credits`, `create_task`, and `release_task` move `$CVNT`, so each requires the
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
| CVNT mint | `{ covntMint }` | `COVNT_MINT` |

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
| Discovery and tasks | `DiscoveryEventRecord`, `DiscoveryStats`, `TASK_STATUS_VALUES`, `TaskStatus` |

Everything imports from the package root. Instruction data is encoded from the program IDLs
(settlement `cov9UDyp...`, stake `CstkpU2q...`), so the wire bytes cannot drift from what the
on-chain program accepts.

## Stability

`0.1.0`, alpha. `compatibility/exports.v1.json` and `compatibility/instructions.v1.json` pin the
exported surface and each instruction's account order and data keys against accidental drift.
Semver support windows are not yet guaranteed, so pin an exact version.

## License

Apache-2.0.
