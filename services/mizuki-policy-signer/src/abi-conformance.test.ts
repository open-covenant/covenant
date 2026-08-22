import { readFileSync } from 'node:fs';
import {
  Keypair,
  PublicKey,
  SYSVAR_CLOCK_PUBKEY,
  SYSVAR_RENT_PUBKEY,
  SystemProgram,
} from '@solana/web3.js';
import { describe, expect, it } from 'vitest';
import {
  buildEscrowInstruction,
  encodeEscrowBindData,
  encodeEscrowFundData,
  encodeEscrowResolutionData,
} from './chain.js';
import type { ChainOperation } from './domain.js';

type AbiAccount = {
  name: string;
  signer: boolean;
  writable: boolean;
  address?: string;
};

type AbiInstruction = {
  name: 'fund' | 'bind' | 'release' | 'refund';
  discriminator: number;
  dataLength: number;
  accounts: AbiAccount[];
};

type Abi = {
  instructions: AbiInstruction[];
  state: { length: number };
  vault: { length: number };
  guard: { length: number };
  timeBoundary: Record<string, string>;
  goldenVectors: Record<string, { dataHex: string }>;
};

const abi = JSON.parse(
  readFileSync(
    new URL('../../../programs/mizuki-escrow/abi/mizuki-escrow-v1.json', import.meta.url),
    'utf8',
  ),
) as Abi;

describe('shared escrow ABI conformance', () => {
  it('matches every program-owned golden instruction vector byte for byte', () => {
    expect(
      encodeEscrowFundData({
        bountyDigest: '11'.repeat(32),
        amountLamports: String(0x0102_0304_0506_0708n),
        expiresAtUnixSeconds: '1700000000',
        acceptanceHash: 'aa'.repeat(32),
        stateBump: 255,
        vaultBump: 254,
        guardBump: 253,
      }).toString('hex'),
    ).toBe(abi.goldenVectors.fund.dataHex);
    expect(
      encodeEscrowBindData({
        bountyDigest: '11'.repeat(32),
        claimantWallet: new PublicKey(Buffer.alloc(32, 0x22)).toBase58(),
        claimExpiresAtUnixSeconds: '1700003600',
        bindingEvidence: 'bb'.repeat(32),
      }).toString('hex'),
    ).toBe(abi.goldenVectors.bind.dataHex);
    expect(
      encodeEscrowResolutionData({
        kind: 'escrow_release',
        bountyDigest: '11'.repeat(32),
        resolutionEvidence: 'cc'.repeat(32),
      }).toString('hex'),
    ).toBe(abi.goldenVectors.release.dataHex);
    expect(
      encodeEscrowResolutionData({
        kind: 'escrow_refund',
        bountyDigest: '11'.repeat(32),
        resolutionEvidence: 'dd'.repeat(32),
      }).toString('hex'),
    ).toBe(abi.goldenVectors.refund.dataHex);
  });

  it('matches ordered account flags, data lengths, PDA identities, and time split', () => {
    const authority = Keypair.generate().publicKey;
    const claimant = Keypair.generate().publicKey;
    const program = Keypair.generate().publicKey;
    const operations: Record<
      AbiInstruction['name'],
      Exclude<ChainOperation, { kind: 'refund' }>
    > = {
      fund: {
        kind: 'escrow_reserve',
        intentId: 'fund',
        bountyDigest: '11'.repeat(32),
        amountLamports: '42',
        expiresAtUnixSeconds: '1700000000',
        acceptanceHash: 'aa'.repeat(32),
      },
      bind: {
        kind: 'escrow_bind',
        intentId: 'bind',
        bountyDigest: '11'.repeat(32),
        claimantWallet: claimant.toBase58(),
        claimExpiresAtUnixSeconds: '1700003600',
        bindingEvidence: 'bb'.repeat(32),
      },
      release: {
        kind: 'escrow_release',
        intentId: 'release',
        bountyDigest: '11'.repeat(32),
        claimantWallet: claimant.toBase58(),
        resolutionEvidence: 'cc'.repeat(32),
      },
      refund: {
        kind: 'escrow_refund',
        intentId: 'refund',
        bountyDigest: '11'.repeat(32),
        resolutionEvidence: 'dd'.repeat(32),
      },
    };

    for (const contract of abi.instructions) {
      const built = buildEscrowInstruction(program, authority, operations[contract.name]);
      expect(built.instruction.data).toHaveLength(contract.dataLength);
      expect(built.instruction.data[0]).toBe(contract.discriminator);
      expect(
        built.instruction.keys.map((account) => ({
          signer: account.isSigner,
          writable: account.isWritable,
        })),
      ).toEqual(
        contract.accounts.map((account) => ({
          signer: account.signer,
          writable: account.writable,
        })),
      );
      expect(built.instruction.keys.map((account) => account.pubkey.toBase58())).toEqual(
        contract.accounts.map((account) =>
          accountAddress(account, authority, claimant, built.derived),
        ),
      );
    }

    expect(abi).toMatchObject({
      state: { length: 236 },
      vault: { length: 40 },
      guard: { length: 108 },
      timeBoundary: {
        release: 'clock.unixTimestamp < claimExpiresAt',
        refundBound: 'clock.unixTimestamp >= claimExpiresAt',
        refundUnbound: 'clock.unixTimestamp >= offerExpiresAt',
      },
    });
  });
});

function accountAddress(
  account: AbiAccount,
  authority: PublicKey,
  claimant: PublicKey,
  derived: Record<string, string>,
): string {
  const addresses: Record<string, string | undefined> = {
    authority: authority.toBase58(),
    claimant: claimant.toBase58(),
    state: derived.escrowAddress,
    vault: derived.vaultAddress,
    guard: derived.guardAddress,
    systemProgram: SystemProgram.programId.toBase58(),
    clock: SYSVAR_CLOCK_PUBKEY.toBase58(),
    rent: SYSVAR_RENT_PUBKEY.toBase58(),
  };
  const address = account.address ?? addresses[account.name];
  if (!address) throw new Error(`Unknown ABI account ${account.name}`);
  return address;
}
