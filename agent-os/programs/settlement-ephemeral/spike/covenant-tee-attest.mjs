// Covenant TEE attestation: pull a TDX quote from a MagicBlock Private ER,
// verify it with Intel DCAP (Phala QVL), and relay the verified result into a
// signed Covenant attestation binding "this ER ran in a genuine attested
// enclave". Self-serve on devnet-tee. Then verify the attestation round-trip.
import fs from "node:fs";
import os from "node:os";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { Keypair, Connection } from "@solana/web3.js";
const here = dirname(fileURLToPath(import.meta.url));

const TEE = process.env.TEE || "https://devnet-tee.magicblock.app";
const ACCEPTABLE = new Set(["UpToDate"]);

// --- Covenant attester identity (ed25519 Solana key) ---
const keyPath = [process.env.COVENANT_ATTESTER_KEY, `${os.homedir()}/.config/solana/covenant-agent.json`, `${os.homedir()}/.config/solana/id.json`].find((p) => p && fs.existsSync(p));
const kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(keyPath, "utf8"))));
const signEd = (msg) => crypto.sign(null, Buffer.from(msg), crypto.createPrivateKey({ key: Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), Buffer.from(kp.secretKey.slice(0, 32))]), format: "der", type: "pkcs8" }));
const verifyEd = (msg, sig, pub) => crypto.verify(null, Buffer.from(msg), crypto.createPublicKey({ key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), Buffer.from(pub)]), format: "der", type: "spki" }), sig);
const canon = (o) => Array.isArray(o) ? "[" + o.map(canon).join(",") + "]" : (o && typeof o === "object") ? "{" + Object.keys(o).sort().map((k) => JSON.stringify(k) + ":" + canon(o[k])).join(",") + "}" : JSON.stringify(o);

// 1. fresh challenge + quote + validator identity
const challenge = crypto.randomBytes(64);
const qr = await fetch(`${TEE}/quote?challenge=${encodeURIComponent(challenge.toString("base64"))}`).then((r) => r.json());
if (!("quote" in qr)) throw new Error("no quote: " + JSON.stringify(qr).slice(0, 200));
const rawQuote = Uint8Array.from(Buffer.from(qr.quote, "base64"));
const idr = await fetch(TEE, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getIdentity" }) }).then((r) => r.json());
const validator = idr.result?.identity ?? null;

// 2. DCAP verify (wasm from local bytes -> headless-safe)
const { default: init, js_get_collateral, js_verify } = await import("@phala/dcap-qvl-web");
await init(fs.readFileSync(join(here, "node_modules/@phala/dcap-qvl-web/dcap-qvl-web_bg.wasm")));
const collateral = await js_get_collateral("https://pccs.phala.network/tdx/certification/v4", rawQuote);
const result = js_verify(rawQuote, collateral, BigInt(Math.floor(Date.now() / 1000)));
const report = result.report.TD10 || result.report.TD15 || Object.values(result.report)[0];

// 3. bind: the verified quote must echo our challenge (anti-replay) and be UpToDate
if (report.report_data !== challenge.toString("hex")) throw new Error("report_data != challenge (stale/replayed quote)");
if (!ACCEPTABLE.has(result.status)) throw new Error(`unacceptable TCB status: ${result.status}`);

// 4. relay into a signed Covenant attestation
const body = {
  kind: "covenant.tee.attestation.v1",
  er: { rpc_url: TEE, validator, tee: "intel-tdx" },
  enclave: { status: result.status, advisory_ids: result.advisory_ids, mr_td: report.mr_td, rt_mr0: report.rt_mr0, rt_mr1: report.rt_mr1, rt_mr2: report.rt_mr2 },
  challenge: challenge.toString("base64"),
  verified_at: Number(process.env.NOW_TS || Math.floor(Date.now() / 1000)),
};
const sig = signEd(canon(body));
const attestation = { ...body, attester: kp.publicKey.toBase58(), sig_alg: "ed25519", signature: sig.toString("base64") };

// 5. independent round-trip verify of the attestation
const okSig = verifyEd(canon(body), sig, kp.publicKey.toBytes());
console.log(JSON.stringify(attestation, null, 2));
console.log(`\nDCAP status   : ${result.status}`);
console.log(`anti-replay   : report_data == challenge  (${report.report_data.slice(0,16)}…)`);
console.log(`attester      : ${attestation.attester}`);
console.log(`signature ok  : ${okSig}`);
console.log(okSig ? "\nCOVENANT TEE ATTESTATION VERIFIED ✓" : "\nSIGNATURE FAILED ✗");
