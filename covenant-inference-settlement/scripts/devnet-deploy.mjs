// Devnet test deploy of the settlement program (the ephemeral build, byte-identical
// logic to mainnet cov9UDyp — only declare_id differs) under a throwaway program id,
// with a throwaway-controlled COVNT mint + config. This exists because the devnet
// cov9UDyp deployment predates the ER instructions (delegate/commit/undelegate), so
// there is no on-devnet instance to reuse. No treasury key, no mainnet, self-funded.
//
//   node scripts/devnet-deploy.mjs
//
// Writes .keys/deploy.json { program, mint, treasury } for the e2e to source.

import fs from "node:fs";
import { execFileSync } from "node:child_process";
import {
  Keypair, PublicKey, SystemProgram, Transaction, sendAndConfirmTransaction, LAMPORTS_PER_SOL,
} from "@solana/web3.js";
import {
  createMint, getOrCreateAssociatedTokenAccount, createAccount, mintTo,
} from "@solana/spl-token";
import { l1, loadKeypair, ixInitialize, configPda, send } from "../settlement.mjs";

const env = (k, d) => process.env[k] ?? d;
const here = (p) => new URL(p, import.meta.url).pathname;
const SOLANA = env("SOLANA_BIN", `${process.env.HOME}/.local/share/solana/install/active_release/bin/solana`);
const SO = env("PROGRAM_SO");
if (!SO) throw new Error("set PROGRAM_SO to the built covenant_settlement_program_er.so (settlement-ephemeral cargo build-sbf)");
const RPC = env("SOLANA_RPC", "https://api.devnet.solana.com");

const owner = loadKeypair(here("../.keys/owner.json"));
const programKp = loadKeypair(here("../.keys/program.json"));
const mintKp = loadKeypair(here("../.keys/mint.json"));
const solFaucet = loadKeypair(env("SOL_FAUCET", `${process.env.HOME}/.config/solana/vigilfi-devnet-test.json`));
const conn = l1();
const PROGRAM = programKp.publicKey;

const bal = async (pk) => (await conn.getBalance(pk)) / LAMPORTS_PER_SOL;

console.log(`program  ${PROGRAM.toBase58()}`);
console.log(`owner    ${owner.publicKey.toBase58()}  SOL ${await bal(owner.publicKey)}`);

const DEPLOY_SOL = Number(env("DEPLOY_SOL", "5"));
if ((await bal(owner.publicKey)) < DEPLOY_SOL - 0.2) {
  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(SystemProgram.transfer({
    fromPubkey: solFaucet.publicKey, toPubkey: owner.publicKey, lamports: Math.round(DEPLOY_SOL * LAMPORTS_PER_SOL),
  })), [solFaucet], { commitment: "confirmed" });
  console.log(`funded ${DEPLOY_SOL} SOL from ${solFaucet.publicKey.toBase58()}  ${sig}`);
}

const deployed = await conn.getAccountInfo(PROGRAM);
if (!deployed) {
  console.log("deploying program (this sends many txs; ~30-60s)...");
  execFileSync(SOLANA, [
    "program", "deploy", SO,
    "--program-id", here("../.keys/program.json"),
    "--keypair", here("../.keys/owner.json"),
    "--upgrade-authority", here("../.keys/owner.json"),
    "--url", RPC,
    "--commitment", "confirmed",
  ], { stdio: "inherit" });
} else {
  console.log("program already deployed");
}

// Test COVNT mint (throwaway is the mint authority — never the treasury).
let mint;
if (!(await conn.getAccountInfo(mintKp.publicKey))) {
  mint = await createMint(conn, owner, owner.publicKey, null, 6, mintKp);
  console.log(`mint created ${mint.toBase58()} (decimals 6, authority owner)`);
} else {
  mint = mintKp.publicKey;
  console.log(`mint exists ${mint.toBase58()}`);
}

const ownerAta = await getOrCreateAssociatedTokenAccount(conn, owner, mint, owner.publicKey);
if (ownerAta.amount < 1_000_000n) {
  await mintTo(conn, owner, mint, ownerAta.address, owner, 1_000_000n);
  console.log(`minted 1_000_000 COVNT base units -> owner ata ${ownerAta.address.toBase58()}`);
}

// Dedicated treasury token account (owned by owner, distinct from the ATA).
const treasuryPath = here("../.keys/treasury.json");
let treasuryKp;
if (fs.existsSync(treasuryPath)) treasuryKp = loadKeypair(treasuryPath);
else { treasuryKp = Keypair.generate(); fs.writeFileSync(treasuryPath, JSON.stringify([...treasuryKp.secretKey])); }
let treasury = treasuryKp.publicKey;
if (!(await conn.getAccountInfo(treasury))) {
  treasury = await createAccount(conn, owner, mint, owner.publicKey, treasuryKp);
  console.log(`treasury token account ${treasury.toBase58()}`);
} else {
  console.log(`treasury exists ${treasury.toBase58()}`);
}

const config = configPda();
if (!(await conn.getAccountInfo(config))) {
  const sig = await send(conn, ixInitialize(owner.publicKey, mint, treasury, {
    slashAuthority: owner.publicKey, creditsPerCovnt: 1000, minStakeLock: 0,
  }), [owner]);
  console.log(`initialized config ${config.toBase58()}  ${sig}`);
} else {
  console.log(`config already initialized ${config.toBase58()}`);
}

fs.writeFileSync(here("../.keys/deploy.json"), JSON.stringify({
  program: PROGRAM.toBase58(), mint: mint.toBase58(), treasury: treasury.toBase58(),
  config: config.toBase58(), owner: owner.publicKey.toBase58(),
}, null, 2));
console.log(`\nwrote .keys/deploy.json  SOL left ${await bal(owner.publicKey)}`);
