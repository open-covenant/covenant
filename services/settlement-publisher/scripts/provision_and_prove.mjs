// Mainnet Phase-1 proof: provision the operator credit account and exercise
// the full metering loop against the ALREADY-DEPLOYED settlement program
// (no program upgrade): open_credit_account -> buy_credits -> consume_credits
// -> anchor_receipt_batch. Every tx is simulated before it is sent. Amounts
// are tiny. Idempotent: skips steps already done.
//
// Usage: node scripts/provision_and_prove.mjs [--send]
//   (default is dry-run: simulate only; pass --send to broadcast)
import {
  Connection, PublicKey, Keypair, SystemProgram, Transaction, TransactionInstruction,
} from "@solana/web3.js";
import { TOKEN_2022_PROGRAM_ID } from "@solana/spl-token";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";

const SEND = process.argv.includes("--send");
const RPC = process.env.COVENANT_SOLANA_RPC_URL || "https://api.mainnet-beta.solana.com";
const PROGRAM_ID = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
const MINT = new PublicKey("2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump");
const OWNER_COVNT = new PublicKey("59DS2Vsfcp55KKFupXak8bFhn8AmHMAyxZH8uvmp4UPf");

const BUY_AMOUNT_BASE_UNITS = 1000n; // 0.001 $CVNT (mint has 6 decimals)
const CONSUME_CREDITS = 1000n;

const conn = new Connection(RPC, "confirmed");
const owner = Keypair.fromSecretKey(
  Uint8Array.from(JSON.parse(readFileSync(`${homedir()}/.config/solana/id.json`, "utf8")))
);

const disc = (name) => createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
const u64le = (v) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(v)); return b; };
const pda = (seeds) => PublicKey.findProgramAddressSync(seeds, PROGRAM_ID)[0];

const CONFIG = pda([Buffer.from("config")]);
const CREDITS = pda([Buffer.from("credits"), owner.publicKey.toBuffer()]);
const TREASURY = new PublicKey("8zBseLENQbY8gDQS8X1YoY7ijfxVUfcjvnS5dgjNrqXQ");

const meta = (pubkey, isSigner, isWritable) => ({ pubkey, isSigner, isWritable });

async function exists(pk) { return (await conn.getAccountInfo(pk)) !== null; }

// Bundle the whole metering loop into ONE atomic transaction: they execute in
// order in simulation (so dependent steps see prior state), and atomicity means
// a failure anywhere reverts everything — no partial $CVNT spend.
async function runAtomic(ixs) {
  const tx = new Transaction();
  for (const { ix } of ixs) tx.add(ix);
  tx.feePayer = owner.publicKey;
  tx.recentBlockhash = (await conn.getLatestBlockhash()).blockhash;
  tx.sign(owner);
  const sim = await conn.simulateTransaction(tx);
  if (sim.value.err) {
    console.log("  SIMULATE FAILED:", JSON.stringify(sim.value.err));
    console.log("   logs:\n   " + (sim.value.logs || []).join("\n   "));
    throw new Error("atomic simulation failed");
  }
  console.log(`  simulate OK — steps: ${ixs.map((x) => x.label).join(", ")} (cu=${sim.value.unitsConsumed})`);
  if (!SEND) return null;
  const sig = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: false });
  await conn.confirmTransaction(sig, "confirmed");
  console.log(`  SENT  https://solscan.io/tx/${sig}`);
  return sig;
}

(async () => {
  console.log(`mode=${SEND ? "SEND" : "DRY-RUN"} rpc=${RPC}`);
  console.log(`owner=${owner.publicKey} credits_pda=${CREDITS}`);
  const sol = await conn.getBalance(owner.publicKey);
  console.log(`owner SOL=${sol / 1e9}`);

  const steps = [];

  // 1. open_credit_account (only if not already provisioned)
  if (await exists(CREDITS)) {
    console.log("  open_credit_account already done — skip");
  } else {
    steps.push({ label: "open_credit_account", ix: new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(CREDITS, false, true),
        meta(owner.publicKey, true, true),
        meta(SystemProgram.programId, false, false),
      ],
      data: disc("open_credit_account"),
    })});
  }

  // 2. buy_credits(amount_covnt)
  steps.push({ label: "buy_credits", ix: new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(CONFIG, false, false),
      meta(CREDITS, false, true),
      meta(owner.publicKey, true, true),
      meta(OWNER_COVNT, false, true),
      meta(TREASURY, false, true),
      meta(MINT, false, false),
      meta(TOKEN_2022_PROGRAM_ID, false, false),
    ],
    data: Buffer.concat([disc("buy_credits"), u64le(BUY_AMOUNT_BASE_UNITS)]),
  })});

  // 3. consume_credits(amount, receipt_hash) — CURRENT program signature
  const receiptHash = createHash("sha256").update("covenant-mainnet-proof:t0").digest();
  steps.push({ label: "consume_credits", ix: new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      meta(CONFIG, false, false),
      meta(CREDITS, false, true),
      meta(owner.publicKey, true, false),
    ],
    data: Buffer.concat([disc("consume_credits"), u64le(CONSUME_CREDITS), receiptHash]),
  })});

  // 4. anchor_receipt_batch — single-receipt batch over receiptHash, using the
  //    same derivation as covenant-settlement (merkle_root = leaf for n=1,
  //    batch_id = sha256("covenant-receipts:" + merkle_root)).
  const merkleRoot = receiptHash;
  const batchId = createHash("sha256")
    .update(Buffer.concat([Buffer.from("covenant-receipts:"), merkleRoot])).digest();
  const BATCH = pda([Buffer.from("receipt_batch"), batchId]);
  const args = Buffer.concat([batchId, merkleRoot, Buffer.from(Uint32Array.of(1).buffer)]);
  if (await exists(BATCH)) {
    console.log("  anchor_receipt_batch already done for this batch_id — skip");
  } else {
    steps.push({ label: "anchor_receipt_batch", ix: new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        meta(CONFIG, false, false),
        meta(BATCH, false, true),
        meta(owner.publicKey, true, true),
        meta(SystemProgram.programId, false, false),
      ],
      data: Buffer.concat([disc("anchor_receipt_batch"), args]),
    })});
  }

  if (steps.length) await runAtomic(steps);
  console.log(SEND ? "DONE (broadcast)" : "DONE (dry-run; re-run with --send to broadcast)");
})().catch((e) => { console.error("FAILED:", e.message); process.exit(1); });
