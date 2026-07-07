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

`CovenantClient` is the high-level path. It derives every PDA, fills the signer as the operator,
owner, or client account, resolves the `$CVNT` mint from the on-chain config, and runs each call
build, sign, send, confirm. Reads decode on-chain state and need no signer.

```typescript
import { Connection, Keypair } from '@solana/web3.js';
import { CovenantClient, keypairSigner, hash32FromText } from '@covenant-org/sdk';

const client = new CovenantClient({
  connection: new Connection('https://api.mainnet-beta.solana.com', 'confirmed'),
  signer: keypairSigner(operatorKeypair),
});

// read decoded on-chain state (no signer required)
const config = await client.getConfig();
const agent = await client.getAgent(hash32FromText('my-agent'));

// register: derives the config + agent PDAs, signs, sends, confirms, returns the signature
const signature = await client.registerAgent({
  agentKey: hash32FromText('my-agent'),
  metadataHash: hash32FromText('https://example.com/agents/my-agent.json'),
  capabilityHash: hash32FromText('research,settlement'),
});
```

In the browser, hand the client a wallet-adapter wallet instead of a keypair:

```typescript
import { walletAdapterSigner } from '@covenant-org/sdk';
const client = new CovenantClient({ connection, signer: walletAdapterSigner(wallet) });
```

## Deriving addresses

Each PDA has a `derive*Pda` helper that bakes in the program id and returns `{ address: PublicKey, bump }`:

```typescript
import { deriveConfigPda, deriveAgentPda, deriveAssociatedTokenAddress } from '@covenant-org/sdk';

const { address: config } = deriveConfigPda();
const { address: agent } = deriveAgentPda(hash32FromText('my-agent'));
const ata = deriveAssociatedTokenAddress(owner, covntMint); // token account for owner + mint
```

The client derives these internally; you only reach for them with the low-level builders, whose
account fields are base58 strings, so unwrap a PDA with `.address.toBase58()`.

## Low-level instruction builders

Each `prepare*Instruction` returns a wallet-agnostic descriptor; `toTransactionInstructions` turns
it into signable `TransactionInstruction`s (an 8-byte Anchor discriminator plus Borsh-encoded args
from the on-chain IDLs).

```typescript
import { Connection, Keypair, Transaction, sendAndConfirmTransaction } from '@solana/web3.js';
import {
  deriveAgentPda,
  deriveConfigPda,
  hash32FromText,
  prepareRegisterAgentInstruction,
  resolveSolanaNetwork,
  toTransactionInstructions,
} from '@covenant-org/sdk';

const operator = Keypair.generate();
const connection = new Connection(resolveSolanaNetwork().rpcUrl, 'confirmed');
const agentKey = hash32FromText('my-agent');

const bundle = prepareRegisterAgentInstruction({
  configAccount: deriveConfigPda().address.toBase58(),
  agentAccount: deriveAgentPda(agentKey).address.toBase58(),
  operator: operator.publicKey.toBase58(),
  agentKey,
  metadataHash: hash32FromText('https://example.com/agents/my-agent.json'),
  capabilityHash: hash32FromText('research,settlement'),
});

const tx = new Transaction().add(...toTransactionInstructions(bundle));
await sendAndConfirmTransaction(connection, tx, [operator]);
```

`fetchAgent`, `fetchConfig`, `fetchTask`, and the `decode*` functions read and decode state
directly, without a client.

## Token accounts and the CVNT mint

`stake`, `buy_credits`, `create_task`, `release_task`, `claim_task`, and `refund_task` move `$CVNT`, so each needs the mint and
the owner's token account. Derive the token account with `deriveAssociatedTokenAddress`, and read
the mint from the on-chain config (or set the `COVNT_MINT` env var, since
`resolveSolanaNetwork().covntMint` is `null` until you do):

```typescript
import {
  deriveAgentPda,
  deriveAssociatedTokenAddress,
  deriveConfigPda,
  deriveStakePositionPda,
  fetchConfig,
  hash32FromText,
  prepareStakeInstruction,
  toTransactionInstructions,
} from '@covenant-org/sdk';

const owner = operator.publicKey;
const agentKey = hash32FromText('my-agent');
const { covntMint } = (await fetchConfig(connection, deriveConfigPda().address))!;

const bundle = prepareStakeInstruction({
  configAccount: deriveConfigPda().address.toBase58(),
  agentAccount: deriveAgentPda(agentKey).address.toBase58(),
  positionAccount: deriveStakePositionPda(agentKey, owner).address.toBase58(),
  owner: owner.toBase58(),
  ownerCovntAccount: deriveAssociatedTokenAddress(owner, covntMint).toBase58(),
  stakeVault: STAKE_VAULT,
  covntMint,
  amountCovnt: '1000000000',
  lockUntil: String(Math.floor(Date.now() / 1000) + 30 * 86400),
});
const [ix] = toTransactionInstructions(bundle);
```

`stakeVault` is the program's `$CVNT` vault, a deployment address you configure once. `CovenantClient`
takes the same token accounts if you prefer the high-level path.

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
| Client | `CovenantClient` |
| Signers | `keypairSigner`, `walletAdapterSigner` |
| PDA derivation | `deriveConfigPda`, `deriveAgentPda`, `deriveTaskPda`, `deriveCreditsPda`, `deriveStakePositionPda`, `deriveReceiptBatchPda`, `deriveAssociatedTokenAddress`, the stake-program `derive*Pda`, `SETTLEMENT_PROGRAM_ID`, `STAKE_PROGRAM_ID` |
| Account reads | `fetchConfig`, `fetchAgent`, `fetchTask`, `fetchCreditAccount`, `fetchStakePosition`, `fetchReceiptBatch`, and the matching `decode*` |
| Settlement instructions | `prepareRegisterAgentInstruction`, `prepareStakeInstruction`, `prepareBuyCreditsInstruction`, `prepareCreateTaskInstruction`, `prepareSubmitTaskInstruction`, `prepareReleaseTaskInstruction`, `prepareClaimTaskInstruction`, `prepareRefundTaskInstruction`, `prepareAnchorReceiptBatchInstruction` |
| Stake-program instructions | `prepareStakeInitializeInstruction`, `prepareStakeCreatePositionInstruction`, `prepareStakeIncreaseAmountInstruction`, `prepareStakeClaimInstruction`, `prepareStakeClosePositionInstruction`, plus the fee-router, pause, and authority admin builders |
| Transactions and serialization | `buildTransaction`, `sendAndConfirmSignedTransaction`, `toTransactionInstruction`, `toTransactionInstructions` |
| Addresses and hashing | `isSolanaAddress`, `assertSolanaAddress`, `assertHash32`, `hash32FromText`, `toBase58`, `toPublicKey`, `ACCOUNT_SEEDS` |
| Network | `resolveSolanaNetwork`, `solanaExplorerHref` |
| Tasks | `TASK_STATUS_VALUES` (`TaskStatus`, `DiscoveryEventRecord`, and `DiscoveryStats` are TypeScript types only) |

Everything imports from the package root. Instruction data is encoded from the program IDLs
(settlement `cov9UDyp...`, stake `CstkpU2q...`), so the wire bytes cannot drift from what the
on-chain program accepts.

## Stability

`0.2.1`, alpha. `compatibility/exports.v1.json` and `compatibility/instructions.v1.json` pin the
exported surface and each instruction's account order and data keys against accidental drift.
Semver support windows are not yet guaranteed, so pin an exact version.

## License

Apache-2.0.
