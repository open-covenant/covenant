// MAINNET inference-fold cutover on cov9UDyp, credit account DrawYGmd (owner 8xbXHA).
//
// Folds N real inference receipt hashes into DrawYGmd's on-chain provenance_root via
// gasless consume_credits on an OPEN mainnet MagicBlock ER, then commits + undelegates
// so the new root lands on L1 and the account is left HOME.
//
// Bounded, one-shot. The treasury key (id.json = 8xbXHA) signs ONLY delegate / consume
// / undelegate on DrawYGmd. The pre-funded task key is the L1 tx fee-payer so the
// treasury's SOL is not a blocker. Genesis + account + owner are re-asserted before
// every signing step; any failure after delegate attempts undelegate to avoid
// stranding the account delegated.
import fs from "node:fs";
import os from "node:os";
import {
  Connection, Keypair, PublicKey, Transaction, sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  PROGRAM, VALIDATOR, ER_URL, l1, creditsPda, readCredits, foldReceipts, connOpts,
  ixDelegateCredits, ixConsumeCredits, ixCommitCreditsPermissionless, ixUndelegateCredits,
} from "../settlement.mjs";

const MAINNET_GENESIS = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const OWNER_B58 = "8xbXHAhiVe2BrYDq4qpTA5SSYJG9XNjNN6jcrudhTKCM";
const CREDITS_B58 = "DrawYGmdbQ7sULxzzczUqyZT2nmP8SZeYPuJzy6TNksj";
const TASK_B58 = "13vGvvgTz3EZZ6rgz4BsCV3BSfBz4V8LvBDjhV6GN5xQ";
const START_ROOT = "2769ee46c8c7dc49e38737c8a3c6d0f57a48553d9b2af08d0bc82cf80ce88933";
const START_BAL = 998992n;
const AMOUNT = 1n;

const HASHES_FILE = process.env.HASHES_FILE;
const RESULT_FILE = process.env.RESULT_FILE || new URL("./fold-mainnet-result.json", import.meta.url).pathname;

const owner = Keypair.fromSecretKey(new Uint8Array(JSON.parse(
  fs.readFileSync(`${os.homedir()}/.config/solana/id.json`, "utf8"))));
const task = Keypair.fromSecretKey(new Uint8Array(JSON.parse(
  fs.readFileSync(`${os.homedir()}/.config/covenant/mainnet-inference-fold-signer.json`, "utf8"))));

const credits = creditsPda(owner.publicKey);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const die = (m) => { console.error(`ABORT: ${m}`); process.exit(1); };

// Never send a signature until genesis + target account + on-chain owner all check out.
async function assertL1(conn, phase) {
  const genesis = await conn.getGenesisHash();
  if (genesis !== MAINNET_GENESIS) die(`[${phase}] genesis ${genesis} != mainnet`);
  if (credits.toBase58() !== CREDITS_B58) die(`[${phase}] target ${credits.toBase58()} != DrawYGmd`);
  const ai = await conn.getAccountInfo(credits);
  if (!ai) die(`[${phase}] DrawYGmd missing on L1`);
  const onchainOwner = new PublicKey(ai.data.subarray(8, 40)).toBase58();
  if (onchainOwner !== OWNER_B58) die(`[${phase}] CreditAccount.owner ${onchainOwner} != 8xbXHA`);
  return ai;
}

function sendTx(conn, ix, signers, feePayer, opts = {}) {
  const tx = new Transaction().add(ix);
  tx.feePayer = feePayer;
  return sendAndConfirmTransaction(conn, tx, signers, { commitment: "confirmed", ...opts });
}

async function tryUndelegate(er, reason) {
  console.error(`RECOVER (${reason}): undelegating so DrawYGmd is not left delegated`);
  try {
    const sig = await sendTx(er, ixUndelegateCredits(owner.publicKey), [owner], owner.publicKey, { skipPreflight: true });
    console.error(`  undelegate sent ${sig}`);
    return sig;
  } catch (e) {
    console.error(`  undelegate during recovery FAILED: ${e.message}`);
    return null;
  }
}

if (owner.publicKey.toBase58() !== OWNER_B58) die(`id.json is ${owner.publicKey.toBase58()}, not 8xbXHA`);
if (task.publicKey.toBase58() !== TASK_B58) die(`task key is ${task.publicKey.toBase58()}, not 13vGvvgT`);
if (!HASHES_FILE) die("set HASHES_FILE");
const hashes = fs.readFileSync(HASHES_FILE, "utf8").split("\n").map((s) => s.trim()).filter(Boolean);
for (const h of hashes) if (!/^[0-9a-f]{64}$/.test(h)) die(`bad receipt hash: ${h}`);
const N = hashes.length;
if (N < 1) die("no receipt hashes");

const conn = l1();
const er = new Connection(ER_URL, connOpts);

console.log(`program    ${PROGRAM.toBase58()}`);
console.log(`owner      ${owner.publicKey.toBase58()} (treasury id.json)`);
console.log(`fee-payer  ${task.publicKey.toBase58()} (task key, L1 delegate only; ER ops owner-paid)`);
console.log(`credits    ${credits.toBase58()}`);
console.log(`validator  ${VALIDATOR.toBase58()} (open mainnet ER)`);
console.log(`ER         ${ER_URL}`);
console.log(`folding    N=${N} receipt hashes, amount ${AMOUNT} each\n`);

const pre = await assertL1(conn, "pre-delegate");
const preState = await readCredits(conn, owner.publicKey);
if (preState.delegated) die("DrawYGmd already delegated - expected HOME");
if (preState.balance !== START_BAL) die(`start balance ${preState.balance} != ${START_BAL}`);
if (preState.provenanceRoot.toString("hex") !== START_ROOT) die(`start root ${preState.provenanceRoot.toString("hex")} != ${START_ROOT}`);
console.log(`pre-check  HOME, balance ${preState.balance}, root ${preState.provenanceRoot.toString("hex")} OK\n`);

const result = { ts: new Date().toISOString(), program: PROGRAM.toBase58(), credits: credits.toBase58(),
  owner: owner.publicKey.toBase58(), feePayer: task.publicKey.toBase58(), validator: VALIDATOR.toBase58(),
  er: ER_URL, startRoot: START_ROOT, startBalance: START_BAL.toString(), n: N,
  consumeSigs: [], receiptHashes: hashes };

let delSig;
try {
  delSig = await sendTx(conn, ixDelegateCredits(owner.publicKey), [task, owner], task.publicKey);
} catch (e) {
  console.error(`delegate send/confirm failed: ${e.message}`);
  const st = await readCredits(conn, owner.publicKey).catch(() => null);
  if (st?.delegated) {
    await tryUndelegate(er, "delegate confirm-timeout but L1 shows delegated");
    die("delegate timed out and DrawYGmd came up delegated; undelegate attempted - verify HOME with scripts/recover-undelegate.mjs");
  }
  die(`delegate failed and L1 is not delegated (${e.message}) - safe to retry; if in doubt run scripts/recover-undelegate.mjs`);
}
result.delegateSig = delSig;
console.log(`delegated  ${delSig}`);

let ready = null;
for (let i = 0; i < 40; i++) {
  await sleep(1000);
  const ai = await er.getAccountInfo(credits).catch(() => null);
  if (ai) { ready = ai; console.log(`ER ready   ~${i + 1}s  balance ${ai.data.readBigUInt64LE(40)}`); break; }
}
if (!ready) { await tryUndelegate(er, "ER never cloned DrawYGmd in 40s"); die("ER clone timeout"); }
const erOwner = new PublicKey(ready.data.subarray(8, 40)).toBase58();
if (erOwner !== OWNER_B58) { await tryUndelegate(er, `ER account owner ${erOwner} != 8xbXHA`); die("ER owner mismatch"); }
console.log(`ER owner   ${erOwner} OK\n`);

let consumed = 0;
try {
  for (let i = 0; i < N; i++) {
    const receipt = Buffer.from(hashes[i], "hex");
    const sig = await sendTx(er, ixConsumeCredits(owner.publicKey, AMOUNT, receipt), [owner], owner.publicKey, { skipPreflight: true });
    result.consumeSigs.push(sig);
    consumed++;
    if (i < 3 || i === N - 1) console.log(`  consume ${i + 1}/${N}  ${hashes[i].slice(0, 12)}…  ${sig}`);
  }
} catch (e) {
  console.error(`consume stopped at ${consumed}/${N}: ${e.message}`);
  await tryUndelegate(er, "consume failure");
  result.error = `consume failed at ${consumed}: ${e.message}`;
  fs.writeFileSync(RESULT_FILE, JSON.stringify(result, null, 2));
  die("consume failure - recovered by undelegate");
}
result.consumed = consumed;
const erBal = await er.getAccountInfo(credits).then((a) => a.data.readBigUInt64LE(40)).catch(() => null);
const erRoot = await er.getAccountInfo(credits).then((a) => Buffer.from(a.data.subarray(49, 81)).toString("hex")).catch(() => null);
console.log(`\nER after ${consumed} consumes: balance ${erBal}, root ${erRoot}\n`);

await assertL1(conn, "pre-commit");
let commitSig = null;
try {
  commitSig = await sendTx(er, ixCommitCreditsPermissionless(owner.publicKey, owner.publicKey), [owner], owner.publicKey, { skipPreflight: true });
  console.log(`commit     ${commitSig} (permissionless, owner payer)`);
} catch (e) {
  console.error(`commit_credits failed (non-fatal, undelegate also commits): ${e.message}`);
}
result.commitSig = commitSig;

await assertL1(conn, "pre-undelegate");
const undelSig = await sendTx(er, ixUndelegateCredits(owner.publicKey), [owner], owner.publicKey, { skipPreflight: true });
result.undelegateSig = undelSig;
console.log(`undelegate ${undelSig} (owner id.json, owner fee-payer)\n`);

let finalState = null;
for (let i = 0; i < 40; i++) {
  await sleep(1000);
  finalState = await readCredits(conn, owner.publicKey);
  if (finalState && !finalState.delegated) { console.log(`L1 home    ~${i + 1}s`); break; }
}
if (!finalState || finalState.delegated) die("DrawYGmd still delegated after undelegate - NOT left HOME");

const finalRoot = finalState.provenanceRoot.toString("hex");
const finalBal = finalState.balance;
const refold = foldReceipts(Buffer.from(START_ROOT, "hex"), hashes.map((h) => Buffer.from(h, "hex"))).toString("hex");
const expectedBal = START_BAL - BigInt(consumed) * AMOUNT;

result.finalRoot = finalRoot;
result.finalBalance = finalBal.toString();
result.offchainRefold = refold;
result.rootMatch = finalRoot === refold;
result.balanceMatch = finalBal === expectedBal;
result.leftHome = !finalState.delegated;
fs.writeFileSync(RESULT_FILE, JSON.stringify(result, null, 2));

console.log("================ FOLD SUMMARY ================");
console.log(`consumes       ${consumed} x ${AMOUNT}`);
console.log(`start root     ${START_ROOT}`);
console.log(`final root L1  ${finalRoot}`);
console.log(`offchain fold  ${refold}`);
console.log(`root match     ${result.rootMatch ? "MATCH ✓" : "MISMATCH ✗"}`);
console.log(`balance        ${START_BAL} -> ${finalBal}  (expected ${expectedBal})  ${result.balanceMatch ? "OK ✓" : "MISMATCH ✗"}`);
console.log(`left HOME      ${result.leftHome ? "yes ✓" : "NO ✗"}`);
console.log(`delegate       ${delSig}`);
console.log(`commit         ${commitSig}`);
console.log(`undelegate     ${undelSig}`);
console.log(`result -> ${RESULT_FILE}`);
if (!result.rootMatch || !result.balanceMatch || !result.leftHome) process.exit(2);
