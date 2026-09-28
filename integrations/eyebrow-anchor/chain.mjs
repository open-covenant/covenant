// Chain-side helpers: where the settlement program lives, how to address a
// receipt batch, and how to read one back. No key ever passes through here
// except the signer the caller loaded for itself.

import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from "@solana/web3.js";

/** Covenant settlement program. Same id on both clusters. */
export const PROGRAM_ID = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");

export const CLUSTERS = Object.freeze({
  mainnet: {
    key: "mainnet",
    rpcUrl: "https://api.mainnet-beta.solana.com",
    explorerQuery: "",
  },
  devnet: {
    key: "devnet",
    rpcUrl: "https://api.devnet.solana.com",
    explorerQuery: "?cluster=devnet",
  },
});

/** The account the program opens is 8 discriminator bytes + ReceiptBatch::INIT_SPACE. */
export const RECEIPT_BATCH_LEN = 8 + 109;

const anchorDiscriminator = (namespace, name) =>
  createHash("sha256").update(`${namespace}:${name}`).digest().subarray(0, 8);

export const ANCHOR_RECEIPT_BATCH_IX = anchorDiscriminator("global", "anchor_receipt_batch");
export const RECEIPT_BATCH_ACCOUNT = anchorDiscriminator("account", "ReceiptBatch");

export const configPda = () =>
  PublicKey.findProgramAddressSync([Buffer.from("config")], PROGRAM_ID)[0];

export const batchPda = (batchId) =>
  PublicKey.findProgramAddressSync([Buffer.from("receipt_batch"), Buffer.from(batchId)], PROGRAM_ID)[0];

export const explorerTx = (signature, cluster) =>
  `https://explorer.solana.com/tx/${signature}${cluster.explorerQuery}`;

export const explorerAddress = (address, cluster) =>
  `https://explorer.solana.com/address/${address}${cluster.explorerQuery}`;

/**
 * Whether the program is deployed and callable right now.
 *
 * A closed upgradeable program keeps its 36-byte program account and loses its
 * program data account, so `executable` alone is not the answer.
 */
export async function programStatus(connection) {
  const program = await connection.getAccountInfo(PROGRAM_ID);
  if (!program) return { live: false, reason: "no account at the program id" };
  if (!program.executable) return { live: false, reason: "the program id is not executable" };
  if (program.data.length < 36) return { live: true, programDataAddress: null };
  const programDataAddress = new PublicKey(program.data.subarray(4, 36));
  const programData = await connection.getAccountInfo(programDataAddress);
  if (!programData) {
    return {
      live: false,
      reason: "the program was closed; its program data account no longer exists",
      programDataAddress: programDataAddress.toBase58(),
    };
  }
  return { live: true, programDataAddress: programDataAddress.toBase58() };
}

/** The Config account's authority: the only key `anchor_receipt_batch` accepts. */
export async function readConfigAuthority(connection) {
  const address = configPda();
  const account = await connection.getAccountInfo(address);
  if (!account) return { address: address.toBase58(), authority: null, paused: null };
  const data = account.data;
  return {
    address: address.toBase58(),
    authority: new PublicKey(data.subarray(8, 40)).toBase58(),
    // Config: 8 discriminator + 4 pubkeys + credits_per_covnt(u64) puts `paused` at 144.
    paused: data.length > 144 ? data[144] === 1 : null,
  };
}

/** Decode a ReceiptBatch account from its raw bytes. */
export function decodeReceiptBatch(data) {
  if (data.length < RECEIPT_BATCH_LEN) {
    throw new Error(`account is ${data.length} bytes, expected at least ${RECEIPT_BATCH_LEN}`);
  }
  if (!data.subarray(0, 8).equals(RECEIPT_BATCH_ACCOUNT)) {
    throw new Error("account discriminator is not ReceiptBatch");
  }
  return {
    batchId: Buffer.from(data.subarray(8, 40)),
    authority: new PublicKey(data.subarray(40, 72)).toBase58(),
    merkleRoot: Buffer.from(data.subarray(72, 104)),
    receiptCount: data.readUInt32LE(104),
    createdAt: Number(data.readBigInt64LE(108)),
    bump: data[116],
  };
}

/** Read the ReceiptBatch at a batch id, or null if nothing is anchored there. */
export async function readReceiptBatch(connection, batchId) {
  const address = batchPda(batchId);
  const account = await connection.getAccountInfo(address);
  if (!account) return { address, account: null };
  if (!account.owner.equals(PROGRAM_ID)) {
    throw new Error(`${address.toBase58()} is not owned by the settlement program`);
  }
  return { address, account: decodeReceiptBatch(account.data) };
}

/** The `anchor_receipt_batch` instruction. Accounts in the order the program declares them. */
export function anchorReceiptBatchIx({ authority, batchId, merkleRoot, receiptCount }) {
  const count = Buffer.alloc(4);
  count.writeUInt32LE(receiptCount);
  return new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: configPda(), isSigner: false, isWritable: false },
      { pubkey: batchPda(batchId), isSigner: false, isWritable: true },
      { pubkey: authority, isSigner: true, isWritable: true },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    ],
    data: Buffer.concat([
      ANCHOR_RECEIPT_BATCH_IX,
      Buffer.from(batchId),
      Buffer.from(merkleRoot),
      count,
    ]),
  });
}

/**
 * Load a signer from a Solana CLI keypair file.
 *
 * The bytes are read, turned into a Keypair, and dropped. Callers print the
 * public key and nothing else.
 */
export function loadSigner(path) {
  const raw = JSON.parse(readFileSync(path, "utf8"));
  if (!Array.isArray(raw)) throw new Error(`${path} is not a Solana CLI keypair file`);
  return Keypair.fromSecretKey(Uint8Array.from(raw));
}

export { Connection, PublicKey };
