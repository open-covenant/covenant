// Recover DrawYGmd to HOME: commit_and_undelegate on the open mainnet ER, owner-signed
// by the treasury key (the only ER fee-payer the rollup accepts, and an owner-gated
// undelegate on DrawYGmd - permitted). The task key cannot be the ER fee-payer: the
// rollup rejects any writable account not delegated to it (InvalidWritableAccount).
import fs from "node:fs";
import os from "node:os";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import {
  PROGRAM, ER_URL, l1, creditsPda, readCredits, foldReceipts, send, ixUndelegateCredits, connOpts,
} from "../settlement.mjs";

const MAINNET_GENESIS = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const OWNER_B58 = "8xbXHAhiVe2BrYDq4qpTA5SSYJG9XNjNN6jcrudhTKCM";
const CREDITS_B58 = "DrawYGmdbQ7sULxzzczUqyZT2nmP8SZeYPuJzy6TNksj";
const START_ROOT = "2769ee46c8c7dc49e38737c8a3c6d0f57a48553d9b2af08d0bc82cf80ce88933";
const EXPECT_BAL = 998972n;

const owner = Keypair.fromSecretKey(new Uint8Array(JSON.parse(
  fs.readFileSync(`${os.homedir()}/.config/solana/id.json`, "utf8"))));
if (owner.publicKey.toBase58() !== OWNER_B58) { console.error("id.json != 8xbXHA"); process.exit(1); }

const credits = creditsPda(owner.publicKey);
const conn = l1();
const er = new Connection(ER_URL, connOpts);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const genesis = await conn.getGenesisHash();
if (genesis !== MAINNET_GENESIS) { console.error(`genesis ${genesis} != mainnet`); process.exit(1); }
if (credits.toBase58() !== CREDITS_B58) { console.error("PDA != DrawYGmd"); process.exit(1); }
const ai = await conn.getAccountInfo(credits);
if (!ai) { console.error("credit account not found on L1"); process.exit(1); }
const onchainOwner = new PublicKey(ai.data.subarray(8, 40)).toBase58();
if (onchainOwner !== OWNER_B58) { console.error(`owner ${onchainOwner} != 8xbXHA`); process.exit(1); }
const delegated = !ai.owner.equals(PROGRAM);
console.log(`genesis mainnet OK, DrawYGmd owner 8xbXHA OK, delegated=${delegated}`);
if (!delegated) { console.log("already HOME - nothing to undelegate"); }

let undelSig = null;
if (delegated) {
  undelSig = await send(er, ixUndelegateCredits(owner.publicKey), [owner], { skipPreflight: true });
  console.log(`undelegate ${undelSig} (owner id.json, ER-gasless)`);
}

let finalState = null;
for (let i = 0; i < 60; i++) {
  await sleep(1000);
  finalState = await readCredits(conn, owner.publicKey);
  if (finalState && !finalState.delegated) { console.log(`L1 home ~${i + 1}s`); break; }
}
if (!finalState || finalState.delegated) { console.error("STILL DELEGATED after undelegate"); process.exit(2); }

const finalRoot = finalState.provenanceRoot.toString("hex");
const finalBal = finalState.balance;

const HASHES_FILE = process.env.HASHES_FILE;
let refold = null;
if (HASHES_FILE && fs.existsSync(HASHES_FILE)) {
  const hashes = fs.readFileSync(HASHES_FILE, "utf8").split("\n").map((s) => s.trim()).filter(Boolean);
  refold = foldReceipts(Buffer.from(START_ROOT, "hex"), hashes.map((h) => Buffer.from(h, "hex"))).toString("hex");
} else {
  console.log("HASHES_FILE not set or missing - skipping off-chain refold verification");
}

console.log("\n================ RECOVER / FINAL ================");
console.log(`undelegate     ${undelSig}`);
console.log(`start root     ${START_ROOT}`);
console.log(`final root L1  ${finalRoot}`);
if (refold !== null) {
  console.log(`offchain fold  ${refold}`);
  console.log(`root match     ${finalRoot === refold ? "MATCH ✓" : "MISMATCH ✗"}`);
}
console.log(`balance        998992 -> ${finalBal}  (expected ${EXPECT_BAL})  ${finalBal === EXPECT_BAL ? "OK ✓" : "MISMATCH ✗"}`);
console.log(`left HOME       ${!finalState.delegated ? "yes ✓" : "NO ✗"}`);

const out = process.env.RESULT_FILE;
if (out) {
  const prev = fs.existsSync(out) ? JSON.parse(fs.readFileSync(out, "utf8")) : {};
  fs.writeFileSync(out, JSON.stringify({ ...prev, undelegateSig: undelSig, finalRoot, finalBalance: finalBal.toString(),
    offchainRefold: refold, rootMatch: refold === null ? null : finalRoot === refold, balanceMatch: finalBal === EXPECT_BAL, leftHome: !finalState.delegated }, null, 2));
}
if ((refold !== null && finalRoot !== refold) || finalBal !== EXPECT_BAL || finalState.delegated) process.exit(3);
