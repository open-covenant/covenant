#!/usr/bin/env node
// er-guard CLI. Two modes:
//
//   Covenant mode (KEYPAIRS set): watches each owner's Covenant credit PDA and
//   sends the settlement program's undelegate_credits cooperatively.
//
//   Watch mode (ACCOUNTS set): watches arbitrary delegated accounts and reports;
//   cooperative undelegation is program-specific, so bring your own via the
//   library API when your program is not Covenant.
//
//   KEYPAIRS=~/.config/solana/id.json ER=https://as.magicblock.app node cli.mjs
//   ACCOUNTS=<pubkey,pubkey> node cli.mjs
//   DRY_RUN=1 ONCE=1 node cli.mjs        # single decide-and-log pass
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, TransactionInstruction, sendAndConfirmTransaction } from "@solana/web3.js";
import { MAGIC_PROGRAM_ID, MAGIC_CONTEXT_ID } from "@magicblock-labs/ephemeral-rollups-sdk";
import { guard, edSignerFor } from "./guard.mjs";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";

const SETTLEMENT = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
const expand = (p) => p.replace(/^~/, os.homedir());
const disc = (n) => crypto.createHash("sha256").update(`global:${n}`).digest().subarray(0, 8);
const flag = (v) => !!v && v !== "0" && v !== "false";
const num = (v, d) => (Number.isFinite(+v) && v !== undefined && v !== "" ? +v : d);

const l1 = new Connection(process.env.L1 || "https://api.mainnet-beta.solana.com", "confirmed");
const erUrl = process.env.ER || "https://as.magicblock.app";
const topUpLamports = num(process.env.TOPUP_LAMPORTS, 5_000_000);

const accounts = [];
let tokenSigner = null;

if (process.env.KEYPAIRS || process.env.KEYPAIR) {
  const owners = (process.env.KEYPAIRS || process.env.KEYPAIR).split(",").map((s) => expand(s.trim())).filter(Boolean)
    .map((p) => Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(p, "utf8")))));
  tokenSigner = { publicKey: owners[0].publicKey, sign: edSignerFor(owners[0]) };
  for (const owner of owners) {
    const credits = PublicKey.findProgramAddressSync([Buffer.from("credits"), owner.publicKey.toBuffer()], SETTLEMENT)[0];
    accounts.push({
      address: credits,
      label: `credits of ${owner.publicKey.toBase58().slice(0, 8)}…`,
      isActive: (erAi) => (erAi && erAi.data.length >= 48 ? String(erAi.data.readBigUInt64LE(40)) : null),
      topUp: (l1c) => sendAndConfirmTransaction(l1c, new Transaction().add(SystemProgram.transfer({
        fromPubkey: owner.publicKey, toPubkey: credits, lamports: topUpLamports,
      })), [owner], { commitment: "confirmed" }),
      undelegate: (er) => sendAndConfirmTransaction(er, new Transaction().add(new TransactionInstruction({
        programId: SETTLEMENT,
        keys: [
          { pubkey: owner.publicKey, isSigner: true, isWritable: true },
          { pubkey: credits, isSigner: false, isWritable: true },
          { pubkey: MAGIC_PROGRAM_ID, isSigner: false, isWritable: false },
          { pubkey: MAGIC_CONTEXT_ID, isSigner: false, isWritable: true },
        ],
        data: disc("undelegate_credits"),
      })), [owner], { commitment: "confirmed" }),
    });
  }
} else if (process.env.ACCOUNTS) {
  for (const a of process.env.ACCOUNTS.split(",").map((s) => s.trim()).filter(Boolean)) {
    accounts.push({ address: new PublicKey(a) });
  }
} else {
  console.error("set KEYPAIRS (Covenant mode) or ACCOUNTS (watch mode)");
  process.exit(2);
}

await guard({
  l1, erUrl, accounts, tokenSigner,
  policy: {
    idleMs: num(process.env.IDLE_MS, 15 * 60_000),
    maxLifetimeMs: num(process.env.MAX_LIFETIME_MS, 30 * 60_000),
    stallProbes: num(process.env.STALL_PROBES, 3),
    pollMs: num(process.env.POLL_MS, 30_000),
    retryMs: num(process.env.RETRY_MS, 60_000),
    lamportFloor: num(process.env.LAMPORT_FLOOR, 5_000_000),
  },
  dryRun: flag(process.env.DRY_RUN),
  once: flag(process.env.ONCE),
});
