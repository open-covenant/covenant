# @covenant/sdk

TypeScript SDK for Covenant Protocol on Solana. The root surface prepares
Solana-native account ids, instruction descriptors, and wallet-facing payloads
for the `$COVNT` protocol program.

## Install

```bash
pnpm add @covenant/sdk
```

Not yet published to npm. Use `workspace:*` within the monorepo.

## Stability

This package is a workspace-alpha SDK surface. The root export map, `compatibility/exports.v1.json`, and `compatibility/instructions.v1.json` fixtures pin the current Solana account-order, instruction-data keys, and exported helpers against drift, but public npm publication, generated protocol binding compatibility, and semver support windows are not approved yet.

## Quick Start

```typescript
import {
  hash32FromText,
  prepareAnchorReceiptBatchInstruction,
  prepareRegisterAgentInstruction,
  resolveSolanaNetwork,
} from '@covenant/sdk';

const network = resolveSolanaNetwork();

const register = prepareRegisterAgentInstruction({
  configAccount: '11111111111111111111111111111111',
  operator: '11111111111111111111111111111111',
  agentAccount: '11111111111111111111111111111111',
  agentKey: hash32FromText('covenant.agent.alpha'),
  metadataHash: hash32FromText('https://opencovenant.org/agents/alpha.json'),
  capabilityHash: hash32FromText('research,settlement'),
});

const receiptBatch = prepareAnchorReceiptBatchInstruction({
  configAccount: '11111111111111111111111111111111',
  authority: '11111111111111111111111111111111',
  batchAccount: '11111111111111111111111111111111',
  batchId: hash32FromText('receipt-batch-1'),
  merkleRoot: hash32FromText('receipt-batch-1'),
  receiptCount: 12,
});
```

`network`, `register`, and `receiptBatch` are plain descriptors today. Wallet
adapter serialization belongs in the next SDK layer after the Anchor IDL is
generated from the Solana program.

## Modules

| Module | Description |
|--------|-------------|
| `solana/network` | Solana cluster and protocol-program configuration |
| `solana/accounts` | Solana address and hash helpers |
| `solana/instructions` | instruction-prep helpers for protocol writes |
| `auth/session` | Session token management |
| `discovery/types` | Solana protocol event payloads |
| `domain/task` | TaskStatus lifecycle enum (funded → proof_submitted → verified → released, plus disputed) |
| `data/mock` | local fixtures for apps and services |
