#!/usr/bin/env node
// Covenant inference settlement bridge.
//
//   node cli.mjs init    — open + fund (buy_credits) + delegate the credit account
//   node cli.mjs serve   — HTTP: POST /fold, POST /commit, GET /root
//   node cli.mjs commit  — commit_credits (permissionless) + undelegate, reconcile L1
//
// Owner = a fresh devnet throwaway keypair (KEYPAIR env, default ./.keys/owner.json).
// Never the treasury. All settlement instructions here are signed by that key alone.

import http from "node:http";
import {
  PROGRAM, ER_URL, VALIDATOR, L1_RPC,
  loadKeypair, l1, erConnection, readConfig, readCredits,
  configPda, creditsPda, ataFor, send, sleep,
  ixOpenCreditAccount, ixBuyCredits, ixDelegateCredits, ixConsumeCredits,
  ixCommitCreditsPermissionless, ixUndelegateCredits,
} from "./settlement.mjs";

const env = (k, d) => process.env[k] ?? d;
const KEYPAIR = env("KEYPAIR", new URL("./.keys/owner.json", import.meta.url).pathname);
const owner = loadKeypair(KEYPAIR);
const TARGET_CREDITS = BigInt(env("TARGET_CREDITS", "50000"));

async function init() {
  const conn = l1();
  const cfg = await readConfig(conn);
  const credits = creditsPda(owner.publicKey);
  console.log(`program   ${PROGRAM.toBase58()}`);
  console.log(`owner     ${owner.publicKey.toBase58()} (throwaway)`);
  console.log(`credits   ${credits.toBase58()}`);
  console.log(`mint      ${cfg.covntMint.toBase58()}  rate ${cfg.creditsPerCovnt}/COVNT  paused ${cfg.paused}`);

  let state = await readCredits(conn, owner.publicKey);
  if (!state) {
    const sig = await send(conn, ixOpenCreditAccount(owner.publicKey), [owner]);
    console.log(`opened    ${sig}`);
    state = await readCredits(conn, owner.publicKey);
  } else {
    console.log(`credits account exists (balance ${state.balance}, delegated ${state.delegated})`);
  }
  if (state.delegated) {
    console.log("already delegated; init complete");
    return;
  }

  if (state.balance < TARGET_CREDITS) {
    const need = TARGET_CREDITS - state.balance;
    const amountCovnt = (need + BigInt(cfg.creditsPerCovnt) - 1n) / BigInt(cfg.creditsPerCovnt);
    const ata = ataFor(owner.publicKey, cfg.covntMint);
    const bal = await conn.getTokenAccountBalance(ata).then((r) => BigInt(r.value.amount)).catch(() => 0n);
    if (bal < amountCovnt) {
      throw new Error(`owner COVNT ata ${ata.toBase58()} has ${bal} base units, need ${amountCovnt}; seed it first`);
    }
    const sig = await send(conn, ixBuyCredits(owner.publicKey, cfg.covntMint, cfg.treasury, amountCovnt), [owner]);
    state = await readCredits(conn, owner.publicKey);
    console.log(`bought    ${sig}  (+${amountCovnt} COVNT -> balance ${state.balance})`);
  }

  const delSig = await send(conn, ixDelegateCredits(owner.publicKey), [owner]);
  console.log(`delegated ${delSig}  pinned validator ${VALIDATOR.toBase58()}`);

  const er = await erConnection(owner);
  for (let i = 0; i < 30; i++) {
    await sleep(1000);
    const ai = await er.getAccountInfo(credits).catch(() => null);
    if (ai) { console.log(`ER ready ~${i + 1}s  balance ${ai.data.readBigUInt64LE(40)}`); return; }
  }
  throw new Error("ER never picked up the delegated account");
}

async function commit() {
  const er = await erConnection(owner);
  const conn = l1();
  const commitSig = await send(er, ixCommitCreditsPermissionless(owner.publicKey, owner.publicKey), [owner], { skipPreflight: true });
  const undelSig = await send(er, ixUndelegateCredits(owner.publicKey), [owner], { skipPreflight: true });
  let l1State = null;
  for (let i = 0; i < 30; i++) {
    await sleep(1000);
    l1State = await readCredits(conn, owner.publicKey);
    if (l1State && !l1State.delegated) break;
  }
  const root = l1State?.provenanceRoot ? l1State.provenanceRoot.toString("hex") : null;
  return { commit_sig: commitSig, l1_sig: undelSig, provenance_root_hex: root, l1_balance: l1State?.balance?.toString() ?? null };
}

async function currentRoots() {
  const conn = l1();
  const l1State = await readCredits(conn, owner.publicKey);
  let erRoot = null;
  if (l1State?.delegated) {
    const er = await erConnection(owner);
    const erState = await readCredits(er, owner.publicKey);
    erRoot = erState?.provenanceRoot ? erState.provenanceRoot.toString("hex") : null;
  }
  return {
    er_root_hex: erRoot,
    l1_root_hex: l1State?.provenanceRoot ? l1State.provenanceRoot.toString("hex") : null,
    delegated: l1State?.delegated ?? false,
    l1_balance: l1State?.balance?.toString() ?? null,
  };
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => resolve(Buffer.concat(chunks)));
    req.on("error", reject);
  });
}

async function serve() {
  const port = Number(env("SETTLEMENT_PORT", "8799"));
  const er = await erConnection(owner);
  const credits = creditsPda(owner.publicKey);
  let position = 0;
  let queue = Promise.resolve(); // folds run one-at-a-time; the chain is order-sensitive

  const runExclusive = (task) => {
    const result = queue.then(task, task);
    queue = result.then(() => {}, () => {});
    return result;
  };

  const server = http.createServer(async (req, res) => {
    const json = (code, obj) => {
      res.writeHead(code, { "content-type": "application/json" });
      res.end(JSON.stringify(obj));
    };
    try {
      if (req.method === "POST" && req.url === "/fold") {
        const { receipt_hash_hex, amount } = JSON.parse((await readBody(req)).toString() || "{}");
        const receipt = Buffer.from(String(receipt_hash_hex), "hex");
        if (receipt.length !== 32) return json(400, { error: "receipt_hash_hex must be 32 bytes" });
        const amt = BigInt(amount ?? 1);
        const out = await runExclusive(async () => {
          const sig = await send(er, ixConsumeCredits(owner.publicKey, amt, receipt), [owner], { skipPreflight: true });
          const state = await readCredits(er, owner.publicKey);
          position += 1;
          return { er_sig: sig, provenance_root_hex: state?.provenanceRoot?.toString("hex") ?? null, position };
        });
        return json(200, out);
      }
      if (req.method === "POST" && req.url === "/commit") {
        const out = await runExclusive(commit);
        return json(200, out);
      }
      if (req.method === "GET" && req.url === "/root") {
        return json(200, await currentRoots());
      }
      if (req.method === "GET" && req.url === "/health") {
        return json(200, { status: "ok", owner: owner.publicKey.toBase58(), credits: credits.toBase58(), er: ER_URL, position });
      }
      json(404, { error: "not found" });
    } catch (error) {
      json(500, { error: String(error?.message ?? error) });
    }
  });

  await new Promise((resolve) => server.listen(port, "127.0.0.1", resolve));
  console.log(JSON.stringify({
    listening: `http://127.0.0.1:${port}`,
    owner: owner.publicKey.toBase58(), credits: credits.toBase58(),
    program: PROGRAM.toBase58(), er: ER_URL, validator: VALIDATOR.toBase58(), l1: L1_RPC,
  }));
}

const cmd = process.argv[2];
const run = { init, serve, commit: async () => console.log(JSON.stringify(await commit(), null, 2)), root: async () => console.log(JSON.stringify(await currentRoots(), null, 2)) }[cmd];
if (!run) {
  console.error("usage: cli.mjs <init|serve|commit|root>");
  process.exit(1);
}
run().catch((e) => { console.error("ERROR:", e?.message ?? e); process.exit(1); });
