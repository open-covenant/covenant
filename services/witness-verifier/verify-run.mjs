// Independent, separately-keyed verifier for a Covenant run.
//
// It reads a run's audit log, recomputes the tamper-evident root from the raw
// event lines, runs the W011 refutation scan, and signs the root with a key
// distinct from the daemon's. It writes the four artifacts the /verify page
// checks. Run it as its own process, not inside the daemon.
//
//   node verify-run.mjs --home <COVENANT_HOME> --sha <commit> --repo <repo-root>
//
// Anyone can reproduce the root with no special tooling: for each line of
// audit/events.jsonl, event_hash = sha256(line), chain = sha256(prev + "\n" +
// event_hash); the final chain value is the root. This mirrors the daemon's own
// chain (covenant-audit), so an honest daemon and this verifier land on the
// same root. Then check the signature against the published verifier pubkey.

import {
  createPrivateKey,
  createPublicKey,
  generateKeyPairSync,
  sign as edSign,
} from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { recomputeRoot, scanRefutations } from "./verify-lib.mjs";

const argv = process.argv.slice(2);
const arg = (name) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : undefined;
};
const home = arg("home");
const sha = arg("sha");
const repo = arg("repo");
if (!home || !sha || !repo) {
  console.error("usage: verify-run.mjs --home <COVENANT_HOME> --sha <commit> --repo <repo-root>");
  process.exit(2);
}

const DOMAIN = "covenant.witness.v1";

const lines = readFileSync(join(home, "audit", "events.jsonl"), "utf8")
  .split("\n")
  .filter((l) => l.length > 0);

// Recompute the chain root from the raw lines, exactly as the daemon does.
const root = recomputeRoot(lines);
const events = lines.map((l) => JSON.parse(l));

// Skill-run manifest, pulled from the run's own events.
const installed = events.find((e) => e.kind?.type === "skill_installed");
const injected = events.find((e) => e.kind?.type === "skill_context_injected");
const txSigned = events.find((e) => e.kind?.type === "skill_tx_signed");
const name = injected?.kind.skill_name || installed?.kind.name || "covenant";
const digestHex = injected?.kind.skill_digest_hex || installed?.kind.digest_hex || "";
const manifest = {
  skill: { name, digest: digestHex ? `sha256:${digestHex}` : "" },
  capabilities: [`skill.use.${name}`],
  tx: txSigned ? { sig: txSigned.kind.signature_b58, cluster: "devnet", slot: null } : null,
};

// W011 refutation: a signed skill action that causally follows an untrusted
// on-chain read, with no skill-context reset between them, is refutable.
const { verdict, refutations } = scanRefutations(events);

// Verifier identity: an ed25519 key the daemon does not hold. Stored as a PEM
// private key; the raw public key is published (base64url) for anyone to check.
const keyPath = join(home, "identity-verifier", "ed25519.pem");
let privKey;
if (existsSync(keyPath)) {
  privKey = createPrivateKey(readFileSync(keyPath, "utf8"));
} else {
  const { privateKey } = generateKeyPairSync("ed25519");
  mkdirSync(dirname(keyPath), { recursive: true });
  writeFileSync(keyPath, privateKey.export({ type: "pkcs8", format: "pem" }), { mode: 0o600 });
  privKey = privateKey;
}
const verifierPubkey = createPublicKey(privKey).export({ format: "jwk" }).x; // raw 32 bytes, base64url

const message = Buffer.from(`${DOMAIN}\n${root}`, "utf8");
const sig = edSign(null, message, privKey).toString("base64url");

const write = (p, content) => {
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, content);
};
write(
  join(repo, "attestations", `${sha}.json`),
  `${JSON.stringify({ audit_root_hex: root, event_count: lines.length, steps: lines, verdict, refutations, verifier_pubkey: verifierPubkey, domain: DOMAIN }, null, 2)}\n`,
);
write(join(repo, "attestations", `${sha}.verifier.sig`), `${sig}\n`);
write(join(repo, "landing", "public", "witness", "verifier-pubkey.txt"), `${verifierPubkey}\n`);
write(join(repo, "landing", "public", "witness", "skill", `${sha}.json`), `${JSON.stringify(manifest, null, 2)}\n`);

console.log(JSON.stringify({ sha, root, event_count: lines.length, verdict, verifier_pubkey: verifierPubkey }));
