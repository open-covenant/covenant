import { PublicKey, SystemProgram } from '@solana/web3.js';
import { describe, expect, it } from 'vitest';
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  associatedTokenAddress,
  createAssociatedTokenAccountIdempotentInstruction,
  createTransferCheckedInstruction,
} from './token.js';

const authority = new PublicKey('9C6hybhQ6Aycep9jaUnP6uL9ZYvDjUp1aSkFWPUFJtpj');
const owner = new PublicKey('54B23SrtQ4rv8tRs1PzACs4WZJY2PzyMXnZzeu1Zo5FE');
const mint = new PublicKey('14BGX7Knx1pJrMnQGdmxDykxFEzkMghYVqsBSTzppbFX');
const source = new PublicKey('EmioZbJyUpBKBLRTZUXgRU1g1uSGpSNbAtYANSAGUrSP');
const destination = new PublicKey('Ef8L3YSH8GMqA2txyC49kXz4SwhUWSQ3EANyDcGNV9cV');

describe('token instruction conformance', () => {
  it('matches the canonical associated-account derivation and idempotent instruction', () => {
    expect(associatedTokenAddress(mint, authority).equals(source)).toBe(true);
    expect(associatedTokenAddress(mint, owner).equals(destination)).toBe(true);

    const instruction = createAssociatedTokenAccountIdempotentInstruction(
      authority,
      destination,
      owner,
      mint,
    );
    expect(instruction.programId.equals(ASSOCIATED_TOKEN_PROGRAM_ID)).toBe(true);
    expect(instruction.data.toString('hex')).toBe('01');
    expect(
      instruction.keys.map((key) => [key.pubkey.toBase58(), key.isSigner, key.isWritable]),
    ).toEqual([
      [authority.toBase58(), true, true],
      [destination.toBase58(), false, true],
      [owner.toBase58(), false, false],
      [mint.toBase58(), false, false],
      [SystemProgram.programId.toBase58(), false, false],
      [TOKEN_PROGRAM_ID.toBase58(), false, false],
    ]);
  });

  it('matches the canonical checked-transfer instruction', () => {
    const instruction = createTransferCheckedInstruction(
      source,
      mint,
      destination,
      authority,
      123_456_789n,
      6,
    );
    expect(instruction.programId.equals(TOKEN_PROGRAM_ID)).toBe(true);
    expect(instruction.data.toString('hex')).toBe('0c15cd5b070000000006');
    expect(
      instruction.keys.map((key) => [key.pubkey.toBase58(), key.isSigner, key.isWritable]),
    ).toEqual([
      [source.toBase58(), false, true],
      [mint.toBase58(), false, false],
      [destination.toBase58(), false, true],
      [authority.toBase58(), true, false],
    ]);
  });

  it('rejects values outside the token instruction widths', () => {
    expect(() =>
      createTransferCheckedInstruction(source, mint, destination, authority, -1n, 6),
    ).toThrow('Token amount exceeds u64');
    expect(() =>
      createTransferCheckedInstruction(source, mint, destination, authority, 1n << 64n, 6),
    ).toThrow('Token amount exceeds u64');
    expect(() =>
      createTransferCheckedInstruction(source, mint, destination, authority, 1n, 256),
    ).toThrow('Token decimals exceed u8');
  });

  it('derives a refund account for a program-owned payer', () => {
    const [programOwned] = PublicKey.findProgramAddressSync(
      [Buffer.from('payer')],
      SystemProgram.programId,
    );
    expect(() => associatedTokenAddress(mint, programOwned)).not.toThrow();
  });
});
