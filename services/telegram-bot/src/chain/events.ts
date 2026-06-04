// Decode the stake program's `PositionCreated` Anchor event out of a
// transaction's program logs.
//
// Anchor `emit!(PositionCreated{..})` lowers to `sol_log_data`, which the
// validator renders as a single `Program data: <base64>` log line. The
// base64 payload is `discriminator(8) || borsh(event)`, where the event
// discriminator is `sha256("event:PositionCreated")[..8]`.
//
// On-chain layout (`agent-os/programs/stake/src/lib.rs`):
//   owner: Pubkey(32) · nonce: u64 · amount: u64 · weight: u128 ·
//   multiplier_bps: u16 · lock_end: i64

import { createHash } from "node:crypto";
import { PublicKey } from "@solana/web3.js";

export interface PositionCreatedEvent {
  owner: PublicKey;
  nonce: bigint;
  /** Staked principal in base units (6 decimals). */
  amount: bigint;
  weight: bigint;
  multiplierBps: number;
  /** Unix timestamp (seconds) when the lock unlocks. */
  lockEnd: bigint;
}

const PROGRAM_DATA_PREFIX = "Program data: ";

// 8 + owner(32) + nonce(8) + amount(8) + weight(16) + multiplier_bps(2) + lock_end(8)
const POSITION_CREATED_LEN = 8 + 32 + 8 + 8 + 16 + 2 + 8;

export function eventDiscriminator(name: string): Buffer {
  return createHash("sha256")
    .update(`event:${name}`)
    .digest()
    .subarray(0, 8);
}

export const POSITION_CREATED_DISCRIMINATOR =
  eventDiscriminator("PositionCreated");

function readU128LE(buf: Buffer, offset: number): bigint {
  const lo = buf.readBigUInt64LE(offset);
  const hi = buf.readBigUInt64LE(offset + 8);
  return (hi << 64n) | lo;
}

/** Decode one base64 `Program data` payload; null unless it's a PositionCreated. */
export function decodePositionCreated(
  buf: Buffer,
): PositionCreatedEvent | null {
  if (buf.length < POSITION_CREATED_LEN) return null;
  if (!buf.subarray(0, 8).equals(POSITION_CREATED_DISCRIMINATOR)) return null;
  let off = 8;
  const owner = new PublicKey(buf.subarray(off, off + 32));
  off += 32;
  const nonce = buf.readBigUInt64LE(off);
  off += 8;
  const amount = buf.readBigUInt64LE(off);
  off += 8;
  const weight = readU128LE(buf, off);
  off += 16;
  const multiplierBps = buf.readUInt16LE(off);
  off += 2;
  const lockEnd = buf.readBigInt64LE(off);
  return { owner, nonce, amount, weight, multiplierBps, lockEnd };
}

/** Scan a transaction's log lines for every PositionCreated event in it. */
export function extractPositionCreatedEvents(
  logs: readonly string[] | null | undefined,
): PositionCreatedEvent[] {
  if (!logs) return [];
  const out: PositionCreatedEvent[] = [];
  for (const line of logs) {
    if (!line.startsWith(PROGRAM_DATA_PREFIX)) continue;
    const b64 = line.slice(PROGRAM_DATA_PREFIX.length).trim();
    const buf = Buffer.from(b64, "base64");
    const event = decodePositionCreated(buf);
    if (event) out.push(event);
  }
  return out;
}

/**
 * Inverse of {@link decodePositionCreated} — serialize an event to the same
 * `Program data:` log line the validator would emit. Used by tests and the
 * `/stakepreview` sample so the wire layout is exercised end to end.
 */
export function encodePositionCreatedLogLine(
  event: PositionCreatedEvent,
): string {
  const buf = Buffer.alloc(POSITION_CREATED_LEN);
  POSITION_CREATED_DISCRIMINATOR.copy(buf, 0);
  let off = 8;
  event.owner.toBuffer().copy(buf, off);
  off += 32;
  buf.writeBigUInt64LE(event.nonce, off);
  off += 8;
  buf.writeBigUInt64LE(event.amount, off);
  off += 8;
  const MASK64 = (1n << 64n) - 1n;
  buf.writeBigUInt64LE(event.weight & MASK64, off);
  buf.writeBigUInt64LE((event.weight >> 64n) & MASK64, off + 8);
  off += 16;
  buf.writeUInt16LE(event.multiplierBps, off);
  off += 2;
  buf.writeBigInt64LE(event.lockEnd, off);
  return `${PROGRAM_DATA_PREFIX}${buf.toString("base64")}`;
}
