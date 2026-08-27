// Turn the wallet-agnostic instruction descriptors from `instructions.ts` and
// `stake.ts` into real, signable `TransactionInstruction`s. The encoding is
// driven entirely by the vendored program IDLs: each instruction's 8-byte
// discriminator and argument layout come from the IDL, so this cannot drift
// from what the on-chain program accepts without the IDL changing too.
import { PublicKey, TransactionInstruction } from '@solana/web3.js';
import type { PreparedSolanaBundle, PreparedSolanaInstruction } from './instructions.js';
import { computeSettlementIdl } from './idl/compute-settlement.js';
import { settlementIdl } from './idl/settlement.js';
import { stakeIdl } from './idl/stake.js';
import * as borsh from './borsh.js';

type IdlType =
  | 'bool'
  | 'u8'
  | 'u16'
  | 'u32'
  | 'u64'
  | 'i64'
  | 'pubkey'
  | { array: [IdlType, number] }
  | { option: IdlType }
  | { defined: string | { name: string } };

interface IdlField {
  name: string;
  type: IdlType;
}
interface IdlInstruction {
  name: string;
  discriminator: number[];
  args: IdlField[];
  accounts: { name: string }[];
}
interface Idl {
  address: string;
  instructions: IdlInstruction[];
  types?: { name: string; type: { fields: IdlField[] } }[];
}

const IDLS: Idl[] = [
  computeSettlementIdl as unknown as Idl,
  settlementIdl as unknown as Idl,
  stakeIdl as unknown as Idl,
];

// The human-facing descriptors name a few fields differently from the on-chain
// IDL (and carry the odd display-only field the program does not accept). Keyed
// by instruction name -> { idlFieldName: descriptorKey }.
const FIELD_ALIASES: Record<string, Record<string, string>> = {
  stake: { amount: 'amount_covnt' },
};

function definedName(type: IdlType): string | null {
  if (typeof type === 'object' && 'defined' in type) {
    return typeof type.defined === 'string' ? type.defined : type.defined.name;
  }
  return null;
}

// descriptors supply struct fields flat, so flatten the IDL args to match
function leafFields(idl: Idl, fields: IdlField[]): IdlField[] {
  const out: IdlField[] = [];
  for (const field of fields) {
    const struct = definedName(field.type);
    if (struct) {
      const def = idl.types?.find((t) => t.name === struct);
      if (!def) throw new Error(`IDL type not found: ${struct}`);
      out.push(...leafFields(idl, def.type.fields));
    } else {
      out.push(field);
    }
  }
  return out;
}

function encode(idl: Idl, type: IdlType, value: unknown): borsh.Bytes {
  if (typeof type === 'string') {
    switch (type) {
      case 'bool':
        return borsh.bool(value as boolean);
      case 'u16':
        return borsh.u16(value as never);
      case 'u32':
        return borsh.u32(value as never);
      case 'u64':
        return borsh.u64(value as never);
      case 'i64':
        return borsh.i64(value as never);
      case 'pubkey':
        return borsh.pubkey(value as string);
      default:
        throw new Error(`unsupported IDL scalar type: ${type}`);
    }
  }
  if ('array' in type) {
    const [inner, len] = type.array;
    if (inner === 'u8' && len === 32) return borsh.hash32(value as string);
    throw new Error(`unsupported array type: ${JSON.stringify(type)}`);
  }
  if ('option' in type) {
    return borsh.option(value, (inner) => encode(idl, type.option, inner));
  }
  const struct = definedName(type);
  if (struct) {
    const def = idl.types?.find((t) => t.name === struct);
    if (!def) throw new Error(`IDL type not found: ${struct}`);
    const obj = value as Record<string, unknown>;
    return borsh.concat(def.type.fields.map((f) => encode(idl, f.type, obj[f.name])));
  }
  throw new Error(`unsupported IDL type: ${JSON.stringify(type)}`);
}

function findInstruction(name: string, programId: string): { idl: Idl; instruction: IdlInstruction } {
  const matches = IDLS.flatMap((idl) =>
    idl.instructions.filter((ix) => ix.name === name).map((instruction) => ({ idl, instruction })),
  );
  if (matches.length === 0) throw new Error(`no covenant IDL instruction named "${name}"`);
  const exact = matches.find((m) => m.idl.address === programId);
  if (exact) return exact;
  if (matches.length > 1) {
    throw new Error(`instruction "${name}" exists in multiple programs; program id ${programId} matches none`);
  }
  return matches[0]!;
}

/** Throws if the descriptor's accounts or fields do not match the program IDL. */
export function toTransactionInstruction(prepared: PreparedSolanaInstruction): TransactionInstruction {
  const { idl, instruction } = findInstruction(prepared.instruction, prepared.programId);
  const expected = instruction.accounts.map((account) => account.name);
  const got = prepared.accounts.map((account) => account.name);
  if (got.length !== expected.length || got.some((name, i) => name !== expected[i])) {
    throw new Error(
      `account mismatch for "${instruction.name}": expected [${expected.join(', ')}], got [${got.join(', ')}]`,
    );
  }
  const aliases = FIELD_ALIASES[instruction.name] ?? {};
  const args = leafFields(idl, instruction.args).map((field) => {
    const key = aliases[field.name] ?? field.name;
    if (!(key in prepared.data)) {
      throw new Error(`descriptor for "${instruction.name}" is missing field "${key}"`);
    }
    return encode(idl, field.type, prepared.data[key]);
  });
  return new TransactionInstruction({
    programId: new PublicKey(prepared.programId),
    keys: prepared.accounts.map((account) => ({
      pubkey: new PublicKey(account.address),
      isSigner: account.signer,
      isWritable: account.writable,
    })),
    data: Buffer.from(borsh.concat([Uint8Array.from(instruction.discriminator), ...args])),
  });
}

export function toTransactionInstructions(bundle: PreparedSolanaBundle): TransactionInstruction[] {
  return bundle.instructions.map(toTransactionInstruction);
}
