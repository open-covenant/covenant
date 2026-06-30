// covenant-tee — verify a MagicBlock Private ER's TDX enclave (Intel DCAP via the
// Phala QVL) and relay the result into a signed Covenant attestation.
//
// Optionally binds an agent and its on-chain provenance root into the quote
// challenge, so the attestation proves "this agent's verifiable record came from
// this genuine, attested enclave" — the link between Covenant's accountability
// layer (provenance + slashable bonds) and confidential execution on a PER.
//
//   attest({ teeUrl, signer, agent?, provenanceRoot?, now? }) -> attestation
//   verifyAttestation(attestation) -> { ok, reason }
//   signerFromKeypairFile(path) -> signer
//
// CLI self-test (agent-bound, round-trip):  node covenant-tee.mjs [teeUrl]
import fs from "node:fs";
import os from "node:os";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { Keypair, PublicKey } from "@solana/web3.js";

const here = dirname(fileURLToPath(import.meta.url));
const WASM = join(here, "node_modules/@phala/dcap-qvl-web/dcap-qvl-web_bg.wasm");
const PCCS = "https://pccs.phala.network/tdx/certification/v4";
const ACCEPTABLE = new Set(["UpToDate"]);
const BIND_DOMAIN = "covenant.tee.bind.v1";

const canon = (o) =>
  Array.isArray(o)
    ? "[" + o.map(canon).join(",") + "]"
    : o && typeof o === "object"
      ? "{" + Object.keys(o).sort().map((k) => JSON.stringify(k) + ":" + canon(o[k])).join(",") + "}"
      : JSON.stringify(o);

const edSign = (secret32, msg) =>
  crypto.sign(null, Buffer.from(msg), crypto.createPrivateKey({
    key: Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), Buffer.from(secret32)]),
    format: "der", type: "pkcs8",
  }));
const edVerify = (pub32, msg, sigB64) =>
  crypto.verify(null, Buffer.from(msg), crypto.createPublicKey({
    key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), Buffer.from(pub32)]),
    format: "der", type: "spki",
  }), Buffer.from(sigB64, "base64"));

export function signerFromKeypairFile(path) {
  const kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(path, "utf8"))));
  return { pubkey: kp.publicKey.toBase58(), sign: (msg) => edSign(kp.secretKey.slice(0, 32), msg) };
}

// 64-byte quote challenge. With an agent + provenance root, the first 32 bytes
// commit to sha256(domain || agent || provenance_root) and the last 32 are a
// fresh nonce (anti-replay). Unbound: 64 random bytes.
function buildChallenge(agentBytes, provBytes) {
  const nonce = crypto.randomBytes(32);
  if (!agentBytes || !provBytes) return { challenge: Buffer.concat([crypto.randomBytes(32), nonce]), nonce };
  const binding = crypto.createHash("sha256").update(BIND_DOMAIN).update(Buffer.from(agentBytes)).update(provBytes).digest();
  return { challenge: Buffer.concat([binding, nonce]), nonce };
}

async function dcapVerify(rawQuote) {
  const { default: init, js_get_collateral, js_verify } = await import("@phala/dcap-qvl-web");
  await init(fs.readFileSync(WASM)); // load wasm from local bytes; the -web build's HTTP fetch fails headless
  const collateral = await js_get_collateral(PCCS, rawQuote);
  const result = js_verify(rawQuote, collateral, BigInt(Math.floor(Date.now() / 1000)));
  const report = result.report.TD10 || result.report.TD15 || Object.values(result.report)[0];
  return { status: result.status, advisory_ids: result.advisory_ids, report };
}

export async function attest({ teeUrl, signer, agent = null, provenanceRoot = null, now = null }) {
  const agentBytes = agent ? new PublicKey(agent).toBytes() : null;
  const provBytes = provenanceRoot ? Buffer.from(provenanceRoot, "hex") : null;
  const { challenge, nonce } = buildChallenge(agentBytes, provBytes);

  const qr = await fetch(`${teeUrl}/quote?challenge=${encodeURIComponent(challenge.toString("base64"))}`).then((r) => r.json());
  if (!("quote" in qr)) throw new Error("no quote: " + JSON.stringify(qr).slice(0, 200));
  const rawQuote = Uint8Array.from(Buffer.from(qr.quote, "base64"));
  const idr = await fetch(teeUrl, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getIdentity" }),
  }).then((r) => r.json());

  const { status, advisory_ids, report } = await dcapVerify(rawQuote);
  if (report.report_data !== challenge.toString("hex")) throw new Error("report_data != challenge (stale/replayed quote)");
  if (!ACCEPTABLE.has(status)) throw new Error(`unacceptable TCB status: ${status}`);

  const body = {
    kind: "covenant.tee.attestation.v1",
    er: { rpc_url: teeUrl, validator: idr.result?.identity ?? null, tee: "intel-tdx" },
    enclave: { status, advisory_ids, mr_td: report.mr_td, rt_mr0: report.rt_mr0, rt_mr1: report.rt_mr1, rt_mr2: report.rt_mr2 },
    subject: agent ? { agent, provenance_root: provenanceRoot } : null,
    challenge: challenge.toString("base64"),
    nonce: nonce.toString("base64"),
    verified_at: now ?? Math.floor(Date.now() / 1000),
  };
  return { ...body, attester: signer.pubkey, sig_alg: "ed25519", signature: Buffer.from(signer.sign(canon(body))).toString("base64") };
}

// Verify a Covenant TEE attestation: the attester's ed25519 signature (the trust
// anchor — Covenant vouches it ran the DCAP verify), the recorded TCB status, the
// agent/provenance binding committed in the quote challenge, and the nonce.
export function verifyAttestation(att) {
  const { attester, sig_alg, signature, ...body } = att;
  if (sig_alg !== "ed25519") return { ok: false, reason: "unsupported sig_alg" };
  if (!edVerify(new PublicKey(attester).toBytes(), canon(body), signature)) return { ok: false, reason: "bad attester signature" };
  if (!ACCEPTABLE.has(att.enclave.status)) return { ok: false, reason: `TCB status ${att.enclave.status}` };
  const challenge = Buffer.from(att.challenge, "base64");
  if (att.subject) {
    const binding = crypto.createHash("sha256").update(BIND_DOMAIN)
      .update(Buffer.from(new PublicKey(att.subject.agent).toBytes()))
      .update(Buffer.from(att.subject.provenance_root, "hex")).digest();
    if (!binding.equals(challenge.subarray(0, 32))) return { ok: false, reason: "agent/provenance not committed in the quote" };
  }
  if (!Buffer.from(att.nonce, "base64").equals(challenge.subarray(32, 64))) return { ok: false, reason: "nonce mismatch" };
  return { ok: true, reason: "verified" };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const teeUrl = process.argv[2] || process.env.TEE || "https://devnet-tee.magicblock.app";
  const keyPath = [process.env.COVENANT_ATTESTER_KEY, `${os.homedir()}/.config/solana/covenant-agent.json`, `${os.homedir()}/.config/solana/id.json`].find((p) => p && fs.existsSync(p));
  const signer = signerFromKeypairFile(keyPath);
  // A real provenance_root comes from the agent's credit account; sampled here.
  const agent = signer.pubkey;
  const provenanceRoot = crypto.createHash("sha256").update("sample agent action chain").digest().toString("hex");
  const att = await attest({ teeUrl, signer, agent, provenanceRoot, now: Number(process.env.NOW_TS) || undefined });
  console.log(JSON.stringify(att, null, 2));
  const v = verifyAttestation(att);
  console.log(`\nbinding       : agent + provenance_root committed in the quote challenge`);
  console.log(`verify        : ${JSON.stringify(v)}`);
  console.log(v.ok ? "\nCOVENANT TEE ATTESTATION (agent-bound) VERIFIED ✓" : "\nFAILED ✗");
}
