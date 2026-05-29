import { Connection, PublicKey } from "@solana/web3.js";
import bs58 from "bs58";
import {
  readBool,
  readI64LE,
  readU128LE,
  readU16LE,
  readU32LE,
  readU64LE,
} from "./anchor";
import { configPda, rewardVaultPda } from "./pdas";
import { STAKE_PROGRAM_ID } from "./env";

export interface ConfigState {
  authority: PublicKey;
  pauseAuthority: PublicKey;
  paused: boolean;
  covntMint: PublicKey;
  minLockAmount: bigint;
  maxActiveLocks: number;
  activeLockCount: number;
  accSolPerWeight: bigint;
  totalWeight: bigint;
  pendingSolLamports: bigint;
  lastAccrualTs: bigint;
  cumulativeSolDistributed: bigint;
  cumulativeBuylockCvnt: bigint;
  initializedTs: bigint;
}

export function decodeConfig(data: Uint8Array): ConfigState {
  let off = 8;
  const authority = new PublicKey(data.slice(off, off + 32));
  off += 32;
  const pauseAuthority = new PublicKey(data.slice(off, off + 32));
  off += 32;
  const paused = readBool(data, off);
  off += 1;
  const covntMint = new PublicKey(data.slice(off, off + 32));
  off += 32;
  const minLockAmount = readU64LE(data, off);
  off += 8;
  const maxActiveLocks = readU32LE(data, off);
  off += 4;
  const activeLockCount = readU32LE(data, off);
  off += 4;
  const accSolPerWeight = readU128LE(data, off);
  off += 16;
  const totalWeight = readU128LE(data, off);
  off += 16;
  const pendingSolLamports = readU64LE(data, off);
  off += 8;
  const lastAccrualTs = readI64LE(data, off);
  off += 8;
  const cumulativeSolDistributed = readU64LE(data, off);
  off += 8;
  const cumulativeBuylockCvnt = readU64LE(data, off);
  off += 8;
  const initializedTs = readI64LE(data, off);

  return {
    authority,
    pauseAuthority,
    paused,
    covntMint,
    minLockAmount,
    maxActiveLocks,
    activeLockCount,
    accSolPerWeight,
    totalWeight,
    pendingSolLamports,
    lastAccrualTs,
    cumulativeSolDistributed,
    cumulativeBuylockCvnt,
    initializedTs,
  };
}

export interface StakePositionState {
  pubkey: PublicKey;
  owner: PublicKey;
  nonce: bigint;
  amount: bigint;
  weight: bigint;
  multiplierBps: number;
  lockStart: bigint;
  lockEnd: bigint;
  rewardDebt: bigint;
  unclaimedLamports: bigint;
  createdAt: bigint;
}

export function decodeStakePosition(
  pubkey: PublicKey,
  data: Uint8Array,
): StakePositionState {
  let off = 8;
  const owner = new PublicKey(data.slice(off, off + 32));
  off += 32;
  const nonce = readU64LE(data, off);
  off += 8;
  const amount = readU64LE(data, off);
  off += 8;
  const weight = readU128LE(data, off);
  off += 16;
  const multiplierBps = readU16LE(data, off);
  off += 2;
  const lockStart = readI64LE(data, off);
  off += 8;
  const lockEnd = readI64LE(data, off);
  off += 8;
  const rewardDebt = readU128LE(data, off);
  off += 16;
  const unclaimedLamports = readU64LE(data, off);
  off += 8;
  const createdAt = readI64LE(data, off);

  return {
    pubkey,
    owner,
    nonce,
    amount,
    weight,
    multiplierBps,
    lockStart,
    lockEnd,
    rewardDebt,
    unclaimedLamports,
    createdAt,
  };
}

export async function fetchConfig(
  connection: Connection,
): Promise<ConfigState | null> {
  const acc = await connection.getAccountInfo(configPda(), "confirmed");
  if (!acc) return null;
  return decodeConfig(acc.data);
}

export async function fetchRewardVaultLamports(
  connection: Connection,
): Promise<bigint> {
  const balance = await connection.getBalance(rewardVaultPda(), "confirmed");
  return BigInt(balance);
}

export async function fetchOwnerPositions(
  connection: Connection,
  owner: PublicKey,
): Promise<StakePositionState[]> {
  const positionDataLen = 8 + 32 + 8 + 8 + 16 + 2 + 8 + 8 + 16 + 8 + 8 + 1;
  const accounts = await connection.getProgramAccounts(STAKE_PROGRAM_ID, {
    commitment: "confirmed",
    filters: [
      { dataSize: positionDataLen },
      { memcmp: { offset: 8, bytes: owner.toBase58() } },
    ],
  });
  return accounts.map((a) =>
    decodeStakePosition(a.pubkey, new Uint8Array(a.account.data)),
  );
}

export function computePendingLamports(
  pos: StakePositionState,
  accSolPerWeight: bigint,
): bigint {
  const ACC_SCALE = 1_000_000_000_000n;
  const grossPending = (pos.weight * accSolPerWeight) / ACC_SCALE;
  if (grossPending < pos.rewardDebt) return pos.unclaimedLamports;
  return grossPending - pos.rewardDebt + pos.unclaimedLamports;
}

export async function fetchTokenAccountAmount(
  connection: Connection,
  pubkey: PublicKey,
): Promise<bigint | null> {
  const acc = await connection.getAccountInfo(pubkey, "confirmed");
  if (!acc) return null;
  return readU64LE(new Uint8Array(acc.data), 64);
}

export interface OwnedTokenAccount {
  pubkey: PublicKey;
  amount: bigint;
}

export async function fetchOwnerTokenAccountsForMint(
  connection: Connection,
  owner: PublicKey,
  mint: PublicKey,
  tokenProgramId: PublicKey,
): Promise<OwnedTokenAccount[]> {
  const resp = await connection.getParsedTokenAccountsByOwner(
    owner,
    { programId: tokenProgramId },
    "confirmed",
  );
  const mintStr = mint.toBase58();
  return resp.value
    .map((acc) => {
      const info = (
        acc.account.data as {
          parsed: { info: { mint: string; tokenAmount: { amount: string } } };
        }
      ).parsed.info;
      return {
        pubkey: acc.pubkey,
        mint: info.mint,
        amount: BigInt(info.tokenAmount.amount),
      };
    })
    .filter((a) => a.mint === mintStr)
    .map(({ pubkey, amount }) => ({ pubkey, amount }));
}

export function pickStakeSource(accounts: OwnedTokenAccount[]): OwnedTokenAccount | null {
  if (accounts.length === 0) return null;
  return accounts.reduce((best, cur) => (cur.amount > best.amount ? cur : best));
}

export function sumBalances(accounts: OwnedTokenAccount[]): bigint {
  return accounts.reduce((s, a) => s + a.amount, 0n);
}

export function ownerPositionFilter(owner: PublicKey) {
  return {
    memcmp: { offset: 8, bytes: bs58.encode(owner.toBuffer()) },
  };
}
