// Settlement primitives shared by the CLI and the HTTP bridge.
//
// Everything here targets the already-proven Covenant settlement program on devnet
// (cov9UDyp). The instruction encodings and account orders match the #[delegate] /
// #[commit] macro expansions verbatim (see the settlement-ephemeral spikes): anchor
// global discriminator = sha256("global:<ix>")[:8], receipt folded on-chain as
// provenance_root = sha256(provenance_root || receipt_hash).

import fs from "node:fs";
import crypto from "node:crypto";
import {
  Connection, Keypair, PublicKey, SystemProgram,
  Transaction, TransactionInstruction, sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  DELEGATION_PROGRAM_ID, MAGIC_PROGRAM_ID, MAGIC_CONTEXT_ID,
  delegateBufferPdaFromDelegatedAccountAndOwnerProgram,
  delegationRecordPdaFromDelegatedAccount,
  delegationMetadataPdaFromDelegatedAccount,
} from "@magicblock-labs/ephemeral-rollups-sdk";

const env = (k, d) => process.env[k] ?? d;

export const PROGRAM = new PublicKey(env("PROGRAM_ID", "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y"));
export const L1_RPC = env("SOLANA_RPC", "https://api.devnet.solana.com");
export const ER_URL = env("ER_URL", "https://devnet-eu.magicblock.app");
export const VALIDATOR = new PublicKey(env("VALIDATOR", "MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e"));
const TOKEN_PROGRAM = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const ATA_PROGRAM = new PublicKey("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

const disc = (name) => crypto.createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
const sha256 = (...parts) => crypto.createHash("sha256").update(Buffer.concat(parts.map(Buffer.from))).digest();
const meta = (pubkey, isSigner, isWritable) => ({ pubkey, isSigner, isWritable });
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const pda = (seeds) => PublicKey.findProgramAddressSync(seeds, PROGRAM)[0];
export const configPda = () => pda([Buffer.from("config")]);
export const creditsPda = (owner) => pda([Buffer.from("credits"), owner.toBuffer()]);
export const ataFor = (owner, mint) =>
  PublicKey.findProgramAddressSync([owner.toBuffer(), TOKEN_PROGRAM.toBuffer(), mint.toBuffer()], ATA_PROGRAM)[0];

export function loadKeypair(path) {
  return Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(path, "utf8"))));
}

// Devnet RPCs occasionally drop a connection mid-run; retry transient fetch/5xx
// failures a few times so a single blip never aborts a long soak.
async function retryingFetch(url, opts) {
  let last;
  for (let i = 0; i < 5; i++) {
    try {
      const res = await fetch(url, { ...opts, signal: AbortSignal.timeout(15000) });
      if (res.status === 429 || res.status >= 500) throw new Error(`http ${res.status}`);
      return res;
    } catch (e) {
      last = e;
      await sleep(300 * (i + 1));
    }
  }
  throw last;
}

export const connOpts = { commitment: "confirmed", fetch: retryingFetch };

export function l1() {
  return new Connection(L1_RPC, connOpts);
}

// Devnet TEE validators are self-serve (zero MagicBlock dependency); the open EU/AS
// validators need no token. Mint one only when the endpoint is a TEE host.
export async function erConnection(owner) {
  if (!/tee\./.test(ER_URL)) return new Connection(ER_URL, connOpts);
  const { getAuthToken } = await import("@magicblock-labs/ephemeral-rollups-sdk/privacy");
  const token = await getAuthToken(ER_URL, owner.publicKey, async (msg) =>
    new Uint8Array(crypto.sign(null, Buffer.from(msg), crypto.createPrivateKey({
      key: Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), Buffer.from(owner.secretKey.slice(0, 32))]),
      format: "der", type: "pkcs8",
    }))));
  return new Connection(`${ER_URL}?token=${token}`, "confirmed");
}

export async function readConfig(conn) {
  const ai = await conn.getAccountInfo(configPda());
  if (!ai) throw new Error("config account missing on this cluster");
  const d = ai.data;
  return {
    authority: new PublicKey(d.subarray(8, 40)),
    covntMint: new PublicKey(d.subarray(72, 104)),
    treasury: new PublicKey(d.subarray(104, 136)),
    creditsPerCovnt: Number(d.readBigUInt64LE(136)),
    paused: d[144] === 1,
  };
}

// CreditAccount layout: disc(8) owner(32) balance(u64@40) bump(u8@48) provenance_root([32]@49).
export async function readCredits(conn, owner) {
  const ai = await conn.getAccountInfo(creditsPda(owner));
  if (!ai) return null;
  return {
    delegated: !ai.owner.equals(PROGRAM),
    balance: ai.data.length >= 48 ? ai.data.readBigUInt64LE(40) : null,
    provenanceRoot: ai.data.length >= 81 ? Buffer.from(ai.data.subarray(49, 81)) : null,
  };
}

export const send = (conn, ix, signers, opts = {}) =>
  sendAndConfirmTransaction(conn, new Transaction().add(ix), signers, { commitment: "confirmed", ...opts });

// initialize(InitializeArgs{ slash_authority, credits_per_covnt, min_stake_lock }).
// Accounts: config(init), authority(signer,w), covnt_mint, treasury, system_program.
export function ixInitialize(authority, covntMint, treasury, { slashAuthority, creditsPerCovnt, minStakeLock }) {
  const data = Buffer.alloc(56);
  disc("initialize").copy(data, 0);
  slashAuthority.toBuffer().copy(data, 8);
  data.writeBigUInt64LE(BigInt(creditsPerCovnt), 40);
  data.writeBigUInt64LE(BigInt(minStakeLock), 48);
  return new TransactionInstruction({
    programId: PROGRAM,
    keys: [
      meta(configPda(), false, true),
      meta(authority, true, true),
      meta(covntMint, false, false),
      meta(treasury, false, false),
      meta(SystemProgram.programId, false, false),
    ],
    data,
  });
}

export function ixOpenCreditAccount(owner) {
  return new TransactionInstruction({
    programId: PROGRAM,
    keys: [meta(creditsPda(owner), false, true), meta(owner, true, true), meta(SystemProgram.programId, false, false)],
    data: disc("open_credit_account"),
  });
}

export function ixBuyCredits(owner, mint, treasury, amountCovnt) {
  const data = Buffer.alloc(16);
  disc("buy_credits").copy(data, 0);
  data.writeBigUInt64LE(BigInt(amountCovnt), 8);
  return new TransactionInstruction({
    programId: PROGRAM,
    keys: [
      meta(configPda(), false, false),
      meta(creditsPda(owner), false, true),
      meta(owner, true, true),
      meta(ataFor(owner, mint), false, true),
      meta(treasury, false, true),
      meta(mint, false, false),
      meta(TOKEN_PROGRAM, false, false),
    ],
    data,
  });
}

export function ixDelegateCredits(owner) {
  const credits = creditsPda(owner);
  return new TransactionInstruction({
    programId: PROGRAM,
    keys: [
      meta(owner, true, true),
      meta(delegateBufferPdaFromDelegatedAccountAndOwnerProgram(credits, PROGRAM), false, true),
      meta(delegationRecordPdaFromDelegatedAccount(credits), false, true),
      meta(delegationMetadataPdaFromDelegatedAccount(credits), false, true),
      meta(credits, false, true),
      meta(PROGRAM, false, false),
      meta(DELEGATION_PROGRAM_ID, false, false),
      meta(SystemProgram.programId, false, false),
      meta(VALIDATOR, false, false),
    ],
    data: disc("delegate_credits"),
  });
}

// consume_credits(amount: u64, receipt_hash: [u8;32]); folds the receipt into
// provenance_root. Accounts: config(r), credits(w), owner(signer,r).
export function ixConsumeCredits(owner, amount, receiptHash) {
  if (receiptHash.length !== 32) throw new Error("receipt_hash must be 32 bytes");
  const data = Buffer.alloc(48);
  disc("consume_credits").copy(data, 0);
  data.writeBigUInt64LE(BigInt(amount), 8);
  receiptHash.copy(data, 16);
  return new TransactionInstruction({
    programId: PROGRAM,
    keys: [meta(configPda(), false, false), meta(creditsPda(owner), false, true), meta(owner, true, false)],
    data,
  });
}

// Permissionless checkpoint: any payer forces the delegated root onto L1.
export function ixCommitCreditsPermissionless(payer, owner) {
  return new TransactionInstruction({
    programId: PROGRAM,
    keys: [
      meta(payer, true, true),
      meta(creditsPda(owner), false, true),
      meta(MAGIC_PROGRAM_ID, false, false),
      meta(MAGIC_CONTEXT_ID, false, true),
    ],
    data: disc("commit_credits"),
  });
}

export function ixUndelegateCredits(owner) {
  return new TransactionInstruction({
    programId: PROGRAM,
    keys: [
      meta(owner, true, true),
      meta(creditsPda(owner), false, true),
      meta(MAGIC_PROGRAM_ID, false, false),
      meta(MAGIC_CONTEXT_ID, false, true),
    ],
    data: disc("undelegate_credits"),
  });
}

/// Re-fold receipts off-chain the way the program does on-chain, so a verifier can
/// prove the committed root is exactly the hash-chain of these receipts.
export function foldReceipts(rootBefore, receiptHashes) {
  return receiptHashes.reduce((root, h) => sha256(root, h), Buffer.from(rootBefore));
}

export { sha256, disc };
