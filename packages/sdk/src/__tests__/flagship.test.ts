import { Connection, Keypair, PublicKey, SystemProgram, Transaction } from '@solana/web3.js';
import { describe, expect, it } from 'vitest';
import { bool, concat, hash32, i64, pubkey, u64 } from '../solana/borsh.js';
import { CovenantClient } from '../solana/client.js';
import { decodeAgent, decodeConfig, decodeTask } from '../solana/decode.js';
import {
  deriveAgentPda,
  deriveAssociatedTokenAddress,
  deriveConfigPda,
  deriveCreditsPda,
  deriveStakePositionPda,
  deriveTaskPda,
} from '../solana/pda.js';
import { keypairSigner, walletAdapterSigner } from '../solana/signer.js';

const AGENT_KEY = '11'.repeat(32);
const TASK_ID = '22'.repeat(32);
const OWNER = 'So11111111111111111111111111111111111111112';
const u8 = (n: number) => Uint8Array.of(n);

describe('pda derivation', () => {
  // Known-good values recomputed from the on-chain program id + IDL seeds. A
  // change here means a seed regressed, which would send funds to a dead address.
  it('matches the program addresses byte-for-byte', () => {
    expect(deriveConfigPda().address.toBase58()).toBe('BGGx99dV5LU2GpKCmhXqT1mi1yNr8EuMuMd5BAG7Lcvi');
    expect(deriveConfigPda().bump).toBe(255);
    expect(deriveAgentPda(AGENT_KEY).address.toBase58()).toBe('7zgovS6Dfw2w6bchge81vVXnArCgCQstn5x1gNpcQjbo');
    expect(deriveCreditsPda(OWNER).address.toBase58()).toBe('43M4nZ5KU9xrPf6LNTDP8rvMYQvtF1pARaXEE1TYgsjF');
    expect(deriveTaskPda(TASK_ID).address.toBase58()).toBe('EJ6gpcxSuYnZwT7TMnva5u5h54Szs1ixefjUW6UcGg95');
    expect(deriveStakePositionPda(AGENT_KEY, OWNER).address.toBase58()).toBe(
      '7oYXp8eJfUKT1Cz8KgNhHxBE1EsjHYNE63zFWpWMXkTd',
    );
  });

  it('is deterministic and off-curve', () => {
    const a = deriveConfigPda().address;
    const b = deriveConfigPda().address;
    expect(a.equals(b)).toBe(true);
    expect(PublicKey.isOnCurve(a.toBytes())).toBe(false);
  });

  it('rejects a malformed agent key', () => {
    expect(() => deriveAgentPda('not-hex')).toThrow();
  });
});

describe('account decode round-trips against the encoders', () => {
  it('decodes an Agent', () => {
    const operator = Keypair.generate().publicKey.toBase58();
    const data = concat([
      Uint8Array.from([47, 166, 112, 147, 155, 197, 86, 7]),
      hash32(AGENT_KEY),
      pubkey(operator),
      hash32('aa'.repeat(32)),
      hash32('bb'.repeat(32)),
      u64('2500000000'),
      u64('914'),
      bool(true),
      u8(254),
    ]);
    const agent = decodeAgent(data);
    expect(agent.agentKey).toBe(AGENT_KEY);
    expect(agent.operator).toBe(operator);
    expect(agent.stake).toBe(2500000000n);
    expect(agent.reputation).toBe(914n);
    expect(agent.active).toBe(true);
    expect(agent.bump).toBe(254);
  });

  it('decodes a Config', () => {
    const [authority, slash, mint, treasury] = Array.from({ length: 4 }, () =>
      Keypair.generate().publicKey.toBase58(),
    );
    const data = concat([
      Uint8Array.from([155, 12, 170, 224, 30, 250, 204, 130]),
      pubkey(authority!),
      pubkey(slash!),
      pubkey(mint!),
      pubkey(treasury!),
      u64('1000'),
      bool(false),
      u8(255),
      u64('86400'),
    ]);
    const config = decodeConfig(data);
    expect(config.authority).toBe(authority);
    expect(config.covntMint).toBe(mint);
    expect(config.creditsPerCovnt).toBe(1000n);
    expect(config.paused).toBe(false);
    expect(config.minStakeLock).toBe(86400n);
  });

  it('decodes a Task including a negative-safe i64 deadline', () => {
    const client = Keypair.generate().publicKey.toBase58();
    const provider = Keypair.generate().publicKey.toBase58();
    const data = concat([
      Uint8Array.from([79, 34, 229, 55, 88, 90, 55, 84]),
      hash32(TASK_ID),
      pubkey(client),
      hash32(AGENT_KEY),
      pubkey(provider),
      u64('125000000'),
      hash32('cc'.repeat(32)),
      hash32('dd'.repeat(32)),
      hash32('ee'.repeat(32)),
      i64('1780000000'),
      u8(1),
      u8(253),
    ]);
    const task = decodeTask(data);
    expect(task.taskId).toBe(TASK_ID);
    expect(task.client).toBe(client);
    expect(task.provider).toBe(provider);
    expect(task.amountCovnt).toBe(125000000n);
    expect(task.deadline).toBe(1780000000n);
    expect(task.status).toBe(1);
  });

  it('rejects the wrong discriminator', () => {
    const wrong = concat([Uint8Array.from([0, 0, 0, 0, 0, 0, 0, 0]), hash32(AGENT_KEY)]);
    expect(() => decodeAgent(wrong)).toThrow(/discriminator/);
  });
});

describe('signers', () => {
  it('keypairSigner signs a transaction', async () => {
    const kp = Keypair.generate();
    const signer = keypairSigner(kp);
    expect(signer.publicKey.equals(kp.publicKey)).toBe(true);
    const tx = new Transaction().add(
      SystemProgram.transfer({ fromPubkey: kp.publicKey, toPubkey: kp.publicKey, lamports: 1 }),
    );
    tx.feePayer = kp.publicKey;
    tx.recentBlockhash = Keypair.generate().publicKey.toBase58();
    const signed = await signer.signTransaction(tx);
    expect(signed.signatures[0]?.signature).not.toBeNull();
    expect(signed.verifySignatures()).toBe(true);
  });

  it('walletAdapterSigner wraps a connected adapter and rejects a disconnected one', () => {
    const kp = Keypair.generate();
    const signer = walletAdapterSigner({
      publicKey: kp.publicKey,
      async signTransaction<T>(tx: T): Promise<T> {
        return tx;
      },
    });
    expect(signer.publicKey.equals(kp.publicKey)).toBe(true);
    expect(() => walletAdapterSigner({ publicKey: null })).toThrow(/not connected/);
  });
});

describe('CovenantClient', () => {
  const blockhash = Keypair.generate().publicKey.toBase58();
  const mockConnection = {
    getAccountInfo: async () => null,
    getLatestBlockhash: async () => ({ blockhash, lastValidBlockHeight: 1000 }),
    sendRawTransaction: async () => 'S'.repeat(64),
    confirmTransaction: async () => ({ value: { err: null } }),
  } as unknown as Connection;

  it('exposes PDA accessors matching the derivers', () => {
    const client = new CovenantClient({ connection: mockConnection });
    expect(client.configPda().toBase58()).toBe(deriveConfigPda().address.toBase58());
    expect(client.agentPda(AGENT_KEY).toBase58()).toBe(deriveAgentPda(AGENT_KEY).address.toBase58());
  });

  it('registerAgent derives PDAs, signs, and sends end to end', async () => {
    const kp = Keypair.generate();
    const client = new CovenantClient({ connection: mockConnection, signer: keypairSigner(kp) });
    const signature = await client.registerAgent({
      agentKey: AGENT_KEY,
      metadataHash: 'aa'.repeat(32),
      capabilityHash: 'bb'.repeat(32),
    });
    expect(signature).toBe('S'.repeat(64));
  });

  it('a write without a signer throws', async () => {
    const client = new CovenantClient({ connection: mockConnection });
    await expect(
      client.registerAgent({ agentKey: AGENT_KEY, metadataHash: 'aa'.repeat(32), capabilityHash: 'bb'.repeat(32) }),
    ).rejects.toThrow(/requires a signer/);
  });
});

describe('hardening regressions', () => {
  it('rejects a same-discriminator account from the wrong program via trailing bytes', () => {
    const pk = () => Keypair.generate().publicKey.toBase58();
    const config = concat([
      Uint8Array.from([155, 12, 170, 224, 30, 250, 204, 130]),
      pubkey(pk()),
      pubkey(pk()),
      pubkey(pk()),
      pubkey(pk()),
      u64('1000'),
      bool(false),
      u8(255),
      u64('86400'),
    ]);
    expect(() => decodeConfig(config)).not.toThrow();
    // The stake program's Config shares this discriminator but is larger; the
    // extra bytes must make the settlement decoder throw, not silently misread.
    expect(() => decodeConfig(concat([config, new Uint8Array(51)]))).toThrow(/trailing bytes/);
  });

  it('derives the associated token account', () => {
    expect(
      deriveAssociatedTokenAddress(
        'So11111111111111111111111111111111111111112',
        '2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump',
      ).toBase58(),
    ).toBe('GmQHNrFudrehjGQFcsNVAgiQrY32EZoP5Gn3xBAU96WX');
  });

  it('walletAdapterSigner reads publicKey live across an account switch', () => {
    const a = Keypair.generate();
    const b = Keypair.generate();
    const adapter: { publicKey: PublicKey; signTransaction<T>(tx: T): Promise<T> } = {
      publicKey: a.publicKey,
      async signTransaction<T>(tx: T): Promise<T> {
        return tx;
      },
    };
    const signer = walletAdapterSigner(adapter);
    expect(signer.publicKey.equals(a.publicKey)).toBe(true);
    adapter.publicKey = b.publicKey;
    expect(signer.publicKey.equals(b.publicKey)).toBe(true);
  });

  it('rejects a number amount before touching the network', async () => {
    const client = new CovenantClient({
      connection: {
        getLatestBlockhash: async () => ({
          blockhash: Keypair.generate().publicKey.toBase58(),
          lastValidBlockHeight: 1000,
        }),
        sendRawTransaction: async () => 'x',
        confirmTransaction: async () => ({ value: { err: null } }),
      } as unknown as Connection,
      signer: keypairSigner(Keypair.generate()),
      covntMint: 'So11111111111111111111111111111111111111112',
    });
    await expect(
      client.buyCredits({ ownerCovntAccount: OWNER, treasury: OWNER, amountCovnt: 123 as unknown as bigint }),
    ).rejects.toThrow(/string or bigint/);
  });
});
