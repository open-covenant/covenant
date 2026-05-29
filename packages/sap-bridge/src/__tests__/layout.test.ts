// Conformance tests for the SAP instruction layouts we build against
// SDK 0.18.0 (the canonical program contract). These are offline: they
// build the instructions and assert the account ordering without
// hitting the network, so a future SDK/program drift fails here loudly
// rather than only at on-chain simulation.
//
// 0.18.0 introduced a native treasury — fee-collecting instructions
// (currently: register_agent, close_agent, settle escrow, featured
// listings) get the `TREASURY_WALLET` appended as a remaining account.
// Attestations are free, so create_attestation stays at 5 accounts.

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

describe('register_agent layout (SDK 0.18.0)', () => {
  it('builds the 5 named accounts in order, then appends the treasury', async () => {
    const { kp, client } = fixedClientAndWallet();
    const w = kp.publicKey;
    const [agent] = sdk.Pdas.getAgentPDA(w);
    // The deployed program enforces seeds = ["sap_stats", agent]. SDK
    // 0.18.0's getAgentStatsPDA(wallet) still seeds from the wallet —
    // so we derive it directly to pin the correct on-chain layout (and
    // flag if the SDK helper drifts back into alignment in a future
    // release).
    const [agentStats] = web3.PublicKey.findProgramAddressSync(
      [Buffer.from('sap_stats'), agent.toBuffer()],
      new web3.PublicKey(PROGRAM_ID),
    );
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
      sdk.TREASURY_WALLET.toBase58(),
    ]);
    // The wallet is the sole signer and is writable (pays rent + fee).
    expect(ix.keys[0].isSigner).toBe(true);
    expect(ix.keys[0].isWritable).toBe(true);
    // Treasury is appended as a remaining account: writable (it receives
    // lamports), but never a signer.
    expect(ix.keys[5].isWritable).toBe(true);
    expect(ix.keys[5].isSigner).toBe(false);
  });
});

// publishAuditRoot anchors roots via the ledger module (init → write →
// seal), not create_attestation — the program rejects self-attestation
// (SelfAttestationNotAllowed), and a daemon anchoring its own root is by
// definition self-signed. These build against the raw Anchor program
// (the ledger flow has no high-level SDK wrapper) and pin the account
// orders the deployed program enforces, so any SDK/program drift fails
// here rather than only at on-chain simulation.
describe('ledger flow layout (SDK 0.18.0)', () => {
  const PID = new web3.PublicKey(PROGRAM_ID);
  // Fixed 32-byte session_hash keeps the derivation deterministic and
  // offline; the real bridge derives it as sha256(ledgerLabel).
  const SESSION_HASH = Array.from({ length: 32 }, (_, i) => i);

  function ledgerLineage(w: import('@solana/web3.js').PublicKey) {
    const [agent] = sdk.Pdas.getAgentPDA(w);
    const [vault] = web3.PublicKey.findProgramAddressSync(
      [Buffer.from('sap_vault'), agent.toBuffer()],
      PID,
    );
    const [session] = web3.PublicKey.findProgramAddressSync(
      [Buffer.from('sap_session'), vault.toBuffer(), Buffer.from(SESSION_HASH)],
      PID,
    );
    const [ledger] = web3.PublicKey.findProgramAddressSync(
      [Buffer.from('sap_ledger'), session.toBuffer()],
      PID,
    );
    return { agent, vault, session, ledger };
  }

  it('init_ledger orders wallet, agent, vault, session, ledger, system_program', async () => {
    const { kp, client } = fixedClientAndWallet();
    const w = kp.publicKey;
    const { agent, vault, session, ledger } = ledgerLineage(w);
    const ix = await client.program.methods
      .initLedger()
      .accountsPartial({ wallet: w, agent, vault, session, ledger, systemProgram: web3.SystemProgram.programId })
      .instruction();
    const keys = ix.keys.map((k: { pubkey: { toBase58(): string } }) => k.pubkey.toBase58());
    expect(ix.programId.toBase58()).toBe(PROGRAM_ID);
    expect(keys).toEqual([
      w.toBase58(),
      agent.toBase58(),
      vault.toBase58(),
      session.toBase58(),
      ledger.toBase58(),
      web3.SystemProgram.programId.toBase58(),
    ]);
    // wallet is the sole signer and writable (pays rent).
    expect(ix.keys[0].isSigner).toBe(true);
    expect(ix.keys[0].isWritable).toBe(true);
  });

  it('write_ledger orders wallet, session, vault, agent, ledger', async () => {
    const { kp, client } = fixedClientAndWallet();
    const w = kp.publicKey;
    const { agent, vault, session, ledger } = ledgerLineage(w);
    const data = Buffer.from('{}', 'utf8');
    const ix = await client.program.methods
      .writeLedger(data, new Array(32).fill(0))
      .accountsPartial({ wallet: w, session, vault, agent, ledger })
      .instruction();
    const keys = ix.keys.map((k: { pubkey: { toBase58(): string } }) => k.pubkey.toBase58());
    expect(ix.programId.toBase58()).toBe(PROGRAM_ID);
    expect(keys).toEqual([
      w.toBase58(),
      session.toBase58(),
      vault.toBase58(),
      agent.toBase58(),
      ledger.toBase58(),
    ]);
    // wallet signs but write_ledger does not make it writable.
    expect(ix.keys[0].isSigner).toBe(true);
    // only the ledger account is mutated.
    const ledgerKey = ix.keys.find((k: { pubkey: { toBase58(): string } }) => k.pubkey.toBase58() === ledger.toBase58());
    expect(ledgerKey.isWritable).toBe(true);
  });
});
