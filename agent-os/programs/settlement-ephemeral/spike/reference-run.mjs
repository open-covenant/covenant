// Covenant reference deployment: our own demo agent, real work, metered on
// mainnet through the verified MagicBlock ER, with the on-chain provenance root
// proven to equal the hash-chain of that real work.
//
// For each intent the demo agent answers for real (Haiku 4.5, its exact sandbox
// system prompt). Each answer is metered by consume_credits in the ER (gasless),
// which folds sha256(prev_root || sha256(work)) into the credit account's
// provenance_root and commits it to L1 on undelegate. We then recompute the fold
// off chain from the work items and assert it matches the on-chain root, so the
// record is provably the agent's real output, not an arbitrary hash.
//
//   ER=https://mainnet-tee.magicblock.app VALIDATOR=MTEW... node reference-run.mjs
//   ER=https://mainnet.magicblock.app     VALIDATOR=MAS1... node reference-run.mjs   # open fallback
import fs from "node:fs";
import os from "node:os";
import crypto from "node:crypto";
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, TransactionInstruction, sendAndConfirmTransaction } from "@solana/web3.js";
import {
  DELEGATION_PROGRAM_ID, MAGIC_PROGRAM_ID, MAGIC_CONTEXT_ID,
  delegateBufferPdaFromDelegatedAccountAndOwnerProgram,
  delegationRecordPdaFromDelegatedAccount,
  delegationMetadataPdaFromDelegatedAccount,
} from "@magicblock-labs/ephemeral-rollups-sdk";
import { getAuthToken } from "@magicblock-labs/ephemeral-rollups-sdk/privacy";

const PROG = new PublicKey("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
const ER_URL = process.env.ER || "https://mainnet-tee.magicblock.app";
const VALIDATOR = new PublicKey(process.env.VALIDATOR || "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo");
const MODEL = "claude-haiku-4-5";
const IS_TEE = /tee\./.test(ER_URL);

const SYSTEM_PROMPT =
  "You are the demo agent on Covenant's public sandbox. Reply briefly, 2 to 4 sentences. " +
  "You're running as a sandboxed agent inside a Covenant daemon that signs and logs every step " +
  "(capability check, dispatch, your reply). When it fits the user's question, mention that they " +
  "can open the activity log to see this exchange signed. You have no internet, no file access, " +
  "and no memory of prior turns on this sandbox. If asked what you can do: say you're a Haiku-backed " +
  "demo for showing how Covenant routes, signs, and audits tasks, and that the real value shows up " +
  "when the user runs Covenant locally and plugs in their own agents.";

const INTENTS = [
  "What is Covenant and what does this sandbox show?",
  "Can you read my files or reach the internet from here?",
  "How would I verify this exchange actually happened?",
  "What changes if I run Covenant locally with my own agent?",
  "In one sentence, what did you just prove?",
];

const ANTHROPIC_KEY = (() => {
  if (process.env.ANTHROPIC_API_KEY) return process.env.ANTHROPIC_API_KEY;
  for (const p of [`${os.homedir()}/Projects/covenant/.env`]) {
    try {
      const m = fs.readFileSync(p, "utf8").match(/^ANTHROPIC_API_KEY=(.+)$/m);
      if (m) return m[1].trim().replace(/^["']|["']$/g, "");
    } catch {}
  }
  return null;
})();

const owner = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(`${os.homedir()}/.config/solana/solana-id.json`, "utf8"))));
const l1 = new Connection(fs.readFileSync("/tmp/cov-deploy-rpc", "utf8").trim(), "confirmed");
const pda = (s) => PublicKey.findProgramAddressSync(s, PROG)[0];
const credits = pda([Buffer.from("credits"), owner.publicKey.toBuffer()]);
const config = pda([Buffer.from("config")]);
const disc = (n) => crypto.createHash("sha256").update(`global:${n}`).digest().subarray(0, 8);
const sha256 = (...b) => crypto.createHash("sha256").update(Buffer.concat(b.map(Buffer.from))).digest();
const m = (pubkey, s, w) => ({ pubkey, isSigner: s, isWritable: w });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const send = (conn, ix, opts = {}) => sendAndConfirmTransaction(conn, new Transaction().add(ix), [owner], { commitment: "confirmed", ...opts });
const provRoot = async (conn) => { const ai = await conn.getAccountInfo(credits); return ai && ai.data.length >= 81 ? Buffer.from(ai.data.subarray(49, 81)) : null; };
const canon = (o) => JSON.stringify(Object.keys(o).sort().reduce((a, k) => ((a[k] = o[k]), a), {}));

async function agentWork(intent) {
  if (!ANTHROPIC_KEY) return "[no ANTHROPIC_API_KEY: agent offline fallback] Covenant routes, signs, and audits each task; run it locally to plug in your own agent.";
  const r = await fetch("https://api.anthropic.com/v1/messages", {
    method: "POST",
    headers: { "content-type": "application/json", "x-api-key": ANTHROPIC_KEY, "anthropic-version": "2023-06-01" },
    body: JSON.stringify({ model: MODEL, max_tokens: 200, system: SYSTEM_PROMPT, messages: [{ role: "user", content: intent }] }),
  });
  if (!r.ok) throw new Error(`anthropic ${r.status}: ${(await r.text()).slice(0, 120)}`);
  return (await r.json()).content.map((c) => c.text).join("").trim();
}

async function erConnection() {
  if (!IS_TEE) return new Connection(ER_URL, "confirmed");
  const token = await getAuthToken(ER_URL, owner.publicKey, async (msg) => new Uint8Array(crypto.sign(null, Buffer.from(msg),
    crypto.createPrivateKey({ key: Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), Buffer.from(owner.secretKey.slice(0, 32))]), format: "der", type: "pkcs8" }))));
  console.log("minted TEE auth token (self-serve)");
  return new Connection(`${ER_URL}?token=${token}`, "confirmed");
}

console.log(`operator  ${owner.publicKey.toBase58()}`);
console.log(`credits   ${credits.toBase58()} (our demo agent's metered account)`);
console.log(`ER        ${ER_URL}  validator ${VALIDATOR.toBase58()}`);
console.log(`model     ${MODEL}  |  work items ${INTENTS.length}  |  key ${ANTHROPIC_KEY ? "present" : "absent (fallback)"}\n`);

const rootBefore = await provRoot(l1);
if (!rootBefore) { console.log("credit account missing or not migrated"); process.exit(1); }
console.log(`provenance_root before: ${rootBefore.toString("hex")}\n`);

console.log("agent doing real work:");
const work = [];
for (const intent of INTENTS) {
  const reply = await agentWork(intent);
  const item = canon({ model: MODEL, intent, reply });
  const receipt = sha256(Buffer.from(item));
  work.push({ intent, reply, receipt });
  console.log(`  Q: ${intent}\n  A: ${reply.replace(/\s+/g, " ").slice(0, 100)}${reply.length > 100 ? "..." : ""}\n     receipt ${receipt.toString("hex").slice(0, 16)}...`);
}

const er = await erConnection();

{
  const ix = new TransactionInstruction({
    programId: PROG,
    keys: [
      m(owner.publicKey, true, true),
      m(delegateBufferPdaFromDelegatedAccountAndOwnerProgram(credits, PROG), false, true),
      m(delegationRecordPdaFromDelegatedAccount(credits), false, true),
      m(delegationMetadataPdaFromDelegatedAccount(credits), false, true),
      m(credits, false, true), m(PROG, false, false),
      m(DELEGATION_PROGRAM_ID, false, false), m(SystemProgram.programId, false, false),
      m(VALIDATOR, false, false),
    ],
    data: disc("delegate_credits"),
  });
  console.log(`\ndelegated ${await send(l1, ix)}`);
}

let ready = false;
for (let i = 0; i < 30; i++) {
  await sleep(1000);
  const ai = await er.getAccountInfo(credits).catch(() => null);
  if (ai) { ready = true; console.log(`ER ready ~${i + 1}s\n`); break; }
}
if (!ready) { console.log("ER never picked up; try the open validator (MAS1)"); process.exit(1); }

const meterSigs = [];
console.log("metering each answer in the ER (gasless):");
for (const w of work) {
  const data = Buffer.alloc(48);
  disc("consume_credits").copy(data, 0);
  data.writeBigUInt64LE(1n, 8);
  w.receipt.copy(data, 16);
  const ix = new TransactionInstruction({ programId: PROG, keys: [m(config, false, false), m(credits, false, true), m(owner.publicKey, true, false)], data });
  const sig = await send(er, ix, { skipPreflight: true });
  meterSigs.push(sig);
  console.log(`  metered ${w.receipt.toString("hex").slice(0, 16)}...  ${sig.slice(0, 16)}...`);
}
console.log(`\nER provenance_root: ${(await provRoot(er))?.toString("hex")}`);

const undelSig = await send(er, new TransactionInstruction({ programId: PROG, keys: [m(owner.publicKey, true, true), m(credits, false, true), m(MAGIC_PROGRAM_ID, false, false), m(MAGIC_CONTEXT_ID, false, true)], data: disc("undelegate_credits") }));
console.log(`undelegated ${undelSig} (commits provenance to L1)`);

let rootAfter = null;
const expected = work.reduce((root, w) => sha256(root, w.receipt), rootBefore);
for (let i = 0; i < 30; i++) { await sleep(1000); rootAfter = await provRoot(l1); if (rootAfter && rootAfter.equals(expected)) break; }

const matched = rootAfter && rootAfter.equals(expected);
console.log("\n================ REFERENCE RUN ================");
console.log(`agent            our Covenant demo agent (${MODEL})`);
console.log(`work items       ${work.length} real answers, metered gaslessly in the ER`);
console.log(`ER / validator   ${ER_URL} / ${VALIDATOR.toBase58()}`);
console.log(`root before      ${rootBefore.toString("hex")}`);
console.log(`root on L1 after ${rootAfter?.toString("hex")}`);
console.log(`recomputed root  ${expected.toString("hex")}`);
console.log(`provenance check ${matched ? "MATCH — on-chain root IS the hash-chain of the agent's real work ✓" : "MISMATCH ✗"}`);
console.log(`undelegate sig   ${undelSig}`);

fs.writeFileSync(new URL("./reference-run-result.json", import.meta.url), JSON.stringify({
  ts: new Date().toISOString(), program: PROG.toBase58(), operator: owner.publicKey.toBase58(),
  credits: credits.toBase58(), er: ER_URL, validator: VALIDATOR.toBase58(), model: MODEL,
  rootBefore: rootBefore.toString("hex"), rootAfterL1: rootAfter?.toString("hex"),
  recomputed: expected.toString("hex"), provenanceVerified: matched,
  work: work.map((w) => ({ intent: w.intent, reply: w.reply, receipt: w.receipt.toString("hex") })),
  sigs: { meter: meterSigs, undelegate: undelSig },
}, null, 2));
console.log(`\nwrote reference-run-result.json  (${matched ? "VERIFIED" : "CHECK"})`);
