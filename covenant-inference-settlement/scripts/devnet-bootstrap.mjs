// Devnet-only faucet: give the throwaway owner enough SOL for L1 fees/rent and a
// tiny amount of the (valueless) devnet COVNT so it can buy_credits. The throwaway
// still signs the whole settlement lifecycle itself - this only mirrors what a
// public faucet + a token drip would do, since the devnet COVNT mint authority is
// the treasury (which we never touch). Sources are non-treasury devnet keys.
//
//   SOL_FAUCET  = ~/.config/solana/vigilfi-devnet-test.json  (devnet SOL)
//   COVNT_FAUCET= ~/.config/solana/covenant-agent.json       (holds devnet COVNT)

import os from "node:os";
import { PublicKey, SystemProgram, Transaction, sendAndConfirmTransaction, LAMPORTS_PER_SOL } from "@solana/web3.js";
import {
  getOrCreateAssociatedTokenAccount, getAssociatedTokenAddress, createTransferInstruction,
} from "@solana/spl-token";
import { l1, loadKeypair, readConfig } from "../settlement.mjs";

const env = (k, d) => process.env[k] ?? d;
const home = os.homedir();
const owner = loadKeypair(env("KEYPAIR", new URL("../.keys/owner.json", import.meta.url).pathname));
const solFaucet = loadKeypair(env("SOL_FAUCET", `${home}/.config/solana/vigilfi-devnet-test.json`));
const covntFaucet = loadKeypair(env("COVNT_FAUCET", `${home}/.config/solana/covenant-agent.json`));
const SOL_TOPUP = Number(env("SOL_TOPUP", "0.3"));
const COVNT_SEED = BigInt(env("COVNT_SEED", "200")); // base units; 200 * 1000 = 200k credits

const conn = l1();
const cfg = await readConfig(conn);
const mint = cfg.covntMint;

const ownerSol = (await conn.getBalance(owner.publicKey)) / LAMPORTS_PER_SOL;
console.log(`owner ${owner.publicKey.toBase58()}  SOL ${ownerSol}`);
if (ownerSol < 0.2) {
  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(SystemProgram.transfer({
    fromPubkey: solFaucet.publicKey, toPubkey: owner.publicKey, lamports: Math.round(SOL_TOPUP * LAMPORTS_PER_SOL),
  })), [solFaucet], { commitment: "confirmed" });
  console.log(`funded ${SOL_TOPUP} SOL from ${solFaucet.publicKey.toBase58()}  ${sig}`);
}

const ownerAta = await getOrCreateAssociatedTokenAccount(conn, owner, mint, owner.publicKey);
console.log(`owner COVNT ata ${ownerAta.address.toBase58()}  balance ${ownerAta.amount}`);
if (ownerAta.amount < COVNT_SEED) {
  const from = await getAssociatedTokenAddress(mint, covntFaucet.publicKey);
  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(
    createTransferInstruction(from, ownerAta.address, covntFaucet.publicKey, COVNT_SEED),
  ), [covntFaucet], { commitment: "confirmed" });
  console.log(`seeded ${COVNT_SEED} COVNT base units from ${covntFaucet.publicKey.toBase58()}  ${sig}`);
} else {
  console.log("owner already holds enough COVNT");
}

const finalAta = await conn.getTokenAccountBalance(ownerAta.address);
console.log(`bootstrap done  SOL ${(await conn.getBalance(owner.publicKey)) / LAMPORTS_PER_SOL}  COVNT ${finalAta.value.amount}`);
