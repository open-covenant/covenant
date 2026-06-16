// Devnet spike for the Covenant settlement ER build.
//
// Flow: ensure a credit balance on L1 -> delegate the credit PDA to the ER ->
// run N consume_credits through the Magic Router (they execute in the ER) ->
// commit + undelegate -> read the balance on L1 and assert exact reconciliation.
//
// NOT yet run live. It needs the ER program deployed to devnet and a funded
// payer. The delegate/commit accounts come from the program IDL (run `anchor
// build` once so the #[delegate]/#[commit] macro accounts are emitted) plus the
// MagicBlock JS SDK helpers. Verify the SDK export names against the version you
// install; they are stable but namespaced differently across minor releases.
//
// Env: PROGRAM_ID, PAYER (keypair json path), COVNT_MINT, ROUTER, L1, N, AMOUNT,
//      VALIDATOR (ER validator pubkey to pin), IDL (path to the program IDL json).

import fs from "node:fs";
import {
  Connection, Keypair, PublicKey, SystemProgram,
} from "@solana/web3.js";
import anchor from "@coral-xyz/anchor";
import * as mb from "@magicblock-labs/ephemeral-rollups-sdk";

const env = (k, d) => process.env[k] ?? (d !== undefined ? d : (() => { throw new Error(`missing env ${k}`); })());
const ROUTER = env("ROUTER", "https://devnet-router.magicblock.app");
const L1 = env("L1", "https://api.devnet.solana.com");
const N = Number(env("N", "1000"));
const AMOUNT = BigInt(env("AMOUNT", "1"));
const VALIDATOR = new PublicKey(env("VALIDATOR", "MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e"));

const programId = new PublicKey(env("PROGRAM_ID"));
const covntMint = new PublicKey(env("COVNT_MINT"));
const payer = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(env("PAYER"), "utf8"))));
const idl = JSON.parse(fs.readFileSync(env("IDL"), "utf8"));

const l1 = new Connection(L1, "confirmed");
const router = new Connection(ROUTER, "confirmed");
const wallet = new anchor.Wallet(payer);

const programOn = (conn) =>
  new anchor.Program(idl, new anchor.AnchorProvider(conn, wallet, { commitment: "confirmed" }), programId);

const pda = (seeds) => PublicKey.findProgramAddressSync(seeds, programId)[0];
const creditsPda = pda([Buffer.from("credits"), payer.publicKey.toBuffer()]);

const balance = async (conn) => {
  const acc = await programOn(conn).account.creditAccount.fetch(creditsPda);
  return BigInt(acc.balance.toString());
};

async function main() {
  const before = await balance(l1);
  console.log(`credit account ${creditsPda} starting balance on L1: ${before}`);
  if (before < AMOUNT * BigInt(N)) throw new Error(`need >= ${AMOUNT * BigInt(N)} credits; buy_credits first`);

  // 1) delegate the credit PDA on L1, pinning the EU validator.
  const p1 = programOn(l1);
  await p1.methods
    .delegateCredits()
    .accounts({
      payer: payer.publicKey,
      pda: creditsPda,
      ownerProgram: programId,
      buffer: mb.delegateBufferPdaFromDelegatedAccountAndOwnerProgram(creditsPda, programId),
      delegationRecord: mb.delegationRecordPdaFromDelegatedAccount(creditsPda),
      delegationMetadata: mb.delegationMetadataPdaFromDelegatedAccount(creditsPda),
      delegationProgram: mb.DELEGATION_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
    })
    .remainingAccounts([{ pubkey: VALIDATOR, isSigner: false, isWritable: false }])
    .rpc();
  console.log("delegated; metering in the ER...");

  // 2) N consume_credits through the router (execute in the ER, gasless).
  const pErr = programOn(router);
  const receipt = new Array(32).fill(0);
  const t0 = Date.now();
  for (let i = 0; i < N; i++) {
    await pErr.methods
      .consumeCredits(new anchor.BN(AMOUNT.toString()), receipt)
      .accounts({ config: pda([Buffer.from("config")]), credits: creditsPda, owner: payer.publicKey })
      .rpc();
  }
  const ms = Date.now() - t0;
  console.log(`${N} consume_credits in ER: ${ms} ms total, ${(ms / N).toFixed(1)} ms/op`);

  // 3) commit + undelegate.
  await pErr.methods.undelegateCredits()
    .accounts({ owner: payer.publicKey, credits: creditsPda, magicContext: mb.MAGIC_CONTEXT_ID, magicProgram: mb.MAGIC_PROGRAM_ID })
    .rpc();
  console.log("undelegated; waiting for L1 finalization...");
  await new Promise((r) => setTimeout(r, 8000));

  // 4) reconcile on L1.
  const after = await balance(l1);
  const expected = before - AMOUNT * BigInt(N);
  console.log(`L1 balance after: ${after}, expected: ${expected}`);
  if (after !== expected) throw new Error(`RECONCILE FAILED: ${after} != ${expected}`);
  console.log("RECONCILE OK — ER metering reconciled exactly to L1.");
}

main().catch((e) => { console.error(e); process.exit(1); });
