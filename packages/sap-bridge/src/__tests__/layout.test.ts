// Conformance tests for the SAP instruction layouts we build against
// SDK 0.17.0 (the canonical program contract). These are offline: they
// build the instructions and assert the account ordering without
// hitting the network, so a future SDK/program drift fails here loudly
// rather than only at on-chain simulation.

import { describe, it, expect } from 'vitest';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const sdk = require('@oobe-protocol-labs/synapse-sap-sdk');
const web3 = require('@solana/web3.js') as typeof import('@solana/web3.js');
const anchor = require('@coral-xyz/anchor') as typeof import('@coral-xyz/anchor');

const PROGRAM_ID = 'SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ';

function fixedClientAndWallet() {
  const kp = web3.Keypair.generate();
  const wallet = { publicKey: kp.publicKey, signTransaction: async (t: unknown) => t, signAllTransactions: async (t: unknown) => t };
  // No network calls are made while merely building an instruction.
  const client = sdk.createSapClient('https://api.devnet.solana.com', wallet);
  return { kp, client };
}

describe('register_agent layout (SDK 0.17.0)', () => {
  it('builds exactly 5 accounts in the expected order', async () => {
    const { kp, client } = fixedClientAndWallet();
    const w = kp.publicKey;
    const [agent] = sdk.Pdas.getAgentPDA(w);
    const [agentStats] = sdk.Pdas.getAgentStatsPDA(w);
    const [globalRegistry] = sdk.Pdas.getGlobalPDA();

    const ix = await client.agent.registerAgent({
      signer: kp,
      wallet: w,
      agent,
      agentStats,
      globalRegistry,
      name: 'covenant-demo',
      description: '',
      capabilities: [],
      pricing: [],
      protocols: ['a2a'],
      agentId: null,
      agentUri: null,
      x402Endpoint: null,
    });

    const keys = ix.keys.map((k: { pubkey: { toBase58(): string } }) => k.pubkey.toBase58());
    expect(ix.programId.toBase58()).toBe(PROGRAM_ID);
    expect(keys).toEqual([
      w.toBase58(),
      agent.toBase58(),
      agentStats.toBase58(),
      globalRegistry.toBase58(),
      web3.SystemProgram.programId.toBase58(),
    ]);
    // The wallet is the sole signer and is writable (pays rent).
    expect(ix.keys[0].isSigner).toBe(true);
    expect(ix.keys[0].isWritable).toBe(true);
  });
});

describe('create_attestation layout (SDK 0.17.0)', () => {
  it('derives the attestation PDA from ["sap_attest", agent, attester] and orders accounts correctly', async () => {
    const { kp, client } = fixedClientAndWallet();
    const w = kp.publicKey;
    const [agent] = sdk.Pdas.getAgentPDA(w);
    const [globalRegistry] = sdk.Pdas.getGlobalPDA();
    const [attestation] = web3.PublicKey.findProgramAddressSync(
      [Buffer.from('sap_attest'), agent.toBuffer(), w.toBuffer()],
      new web3.PublicKey(PROGRAM_ID),
    );

    const ix = await client.attestation.createAttestation({
      signer: kp,
      attester: w,
      agent,
      attestation,
      globalRegistry,
      attestationType: 'covenant.audit-root',
      metadataHash: new Array(32).fill(0),
      expiresAt: new anchor.BN(0),
    });

    const keys = ix.keys.map((k: { pubkey: { toBase58(): string } }) => k.pubkey.toBase58());
    expect(ix.programId.toBase58()).toBe(PROGRAM_ID);
    // attester, agent, attestation, global_registry, system_program
    expect(keys).toEqual([
      w.toBase58(),
      agent.toBase58(),
      attestation.toBase58(),
      globalRegistry.toBase58(),
      web3.SystemProgram.programId.toBase58(),
    ]);
  });
});
