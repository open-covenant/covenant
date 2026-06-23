#!/usr/bin/env node
// Covenant SNS signer sidecar.
//
// covenantd spawns this per write, pipes a SignerRequest JSON on stdin, and
// reads a SignerResponse JSON on stdout. The parent-domain keypair lives only
// here (COVENANT_SNS_KEYPAIR), never in the daemon. Builds writes via the
// Bonfida SDK, simulates first, then submits.
//
// stdin:  {"kind":"register_subdomain","parent":"opencovenant.sol","subdomain":"foundation","owner":"<base58>"}
//         {"kind":"set_record","domain":"opencovenant.sol","record":"url","value":"https://opencovenant.org"}
// stdout: {"signature":"<sig>","domain":"foundation.opencovenant.sol","cluster":"mainnet-beta"}
//
// env: COVENANT_SNS_RPC_URL (required), COVENANT_SNS_KEYPAIR (required path),
//      COVENANT_SNS_SUBDOMAIN_SPACE (optional, default 1000)

import { Connection, Keypair, PublicKey, Transaction } from "@solana/web3.js";
import {
  createSubdomain,
  transferSubdomain,
  createRecordV2Instruction,
  updateRecordV2Instruction,
  getRecordV2Key,
  Record,
} from "@bonfida/spl-name-service";
import { readFileSync } from "node:fs";

const RPC = process.env.COVENANT_SNS_RPC_URL;
const KEYPATH = process.env.COVENANT_SNS_KEYPAIR;
const SPACE = Number(process.env.COVENANT_SNS_SUBDOMAIN_SPACE || 1000);

const die = (msg) => {
  process.stderr.write(`covenant-sns-signer: ${msg}\n`);
  process.exit(1);
};

const cluster = () => (RPC && /devnet/i.test(RPC) ? "devnet" : "mainnet-beta");

async function readStdin() {
  const chunks = [];
  for await (const c of process.stdin) chunks.push(c);
  return Buffer.concat(chunks).toString("utf8").trim();
}

function loadSigner() {
  if (!KEYPATH) die("COVENANT_SNS_KEYPAIR is not set");
  try {
    return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(KEYPATH, "utf8"))));
  } catch (e) {
    return die(`cannot load keypair at ${KEYPATH}: ${e.message}`);
  }
}

// Simulate, then submit. All diagnostics go to stderr so stdout stays pure JSON.
async function submit(conn, signer, ixs) {
  const { blockhash, lastValidBlockHeight } = await conn.getLatestBlockhash("confirmed");
  const tx = new Transaction({ feePayer: signer.publicKey, blockhash, lastValidBlockHeight }).add(...ixs);
  const sim = await conn.simulateTransaction(tx);
  if (sim.value.err) {
    die(`simulation failed: ${JSON.stringify(sim.value.err)}\n${(sim.value.logs || []).join("\n")}`);
  }
  tx.sign(signer);
  const sig = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: false, maxRetries: 5 });
  await conn.confirmTransaction({ signature: sig, blockhash, lastValidBlockHeight }, "confirmed");
  process.stderr.write(`submitted ${sig}\n`);
  return sig;
}

// Issue <subdomain>.<parent> and bind it to `owner`. The Bonfida helper only
// creates a subdomain owned by the signer (its reverse step assumes the new
// owner is the signing parent owner), so when a different owner is requested we
// create it under the parent key, then transfer.
async function registerSubdomain(conn, signer, req) {
  const { parent, subdomain, owner } = req;
  if (!parent || !subdomain || !owner) die("register_subdomain needs parent, subdomain, owner");
  const bareParent = String(parent).replace(/\.sol$/i, "");
  const full = `${subdomain}.${bareParent}`;
  const target = new PublicKey(owner);

  const created = await createSubdomain(conn, full, signer.publicKey, SPACE, signer.publicKey);
  let sig = await submit(conn, signer, Array.isArray(created) ? created : [created]);

  if (!target.equals(signer.publicKey)) {
    const transferIx = await transferSubdomain(conn, full, target);
    sig = await submit(conn, signer, [transferIx]);
  }
  return { signature: sig, domain: `${full}.sol`, cluster: cluster() };
}

// Write a typed record on a domain the signer owns (the parent, or a subdomain
// before it is transferred). Create the record account if absent, else update.
async function setRecord(conn, signer, req) {
  const { domain, record, value } = req;
  if (!domain || !record || value == null) die("set_record needs domain, record, value");
  const valid = new Set(Object.values(Record));
  if (!valid.has(record)) die(`unsupported record type: ${record}`);
  const bare = String(domain).replace(/\.sol$/i, "");
  const owner = signer.publicKey;

  const key = getRecordV2Key(bare, record);
  const existing = await conn.getAccountInfo(key);
  const ix = existing?.data
    ? updateRecordV2Instruction(bare, record, value, owner, owner)
    : createRecordV2Instruction(bare, record, value, owner, owner);
  const sig = await submit(conn, signer, [ix]);
  return { signature: sig, domain: `${bare}.sol`, cluster: cluster() };
}

async function main() {
  if (!RPC) die("COVENANT_SNS_RPC_URL is not set");
  const raw = await readStdin();
  if (!raw) die("empty request on stdin");
  let req;
  try {
    req = JSON.parse(raw);
  } catch (e) {
    return die(`invalid request JSON: ${e.message}`);
  }
  const conn = new Connection(RPC, "confirmed");
  const signer = loadSigner();

  let res;
  switch (req.kind) {
    case "register_subdomain":
      res = await registerSubdomain(conn, signer, req);
      break;
    case "set_record":
      res = await setRecord(conn, signer, req);
      break;
    default:
      return die(`unknown request kind: ${req.kind}`);
  }
  process.stdout.write(JSON.stringify(res));
}

main().catch((e) => die(e.stack || e.message || String(e)));
