// Separately keyed verifier for a supplied Covenant run log.
//
// It reads a run's audit log, recomputes the tamper-evident root from the raw
// event lines, runs a configured event-lineage heuristic, and signs the root
// with a key distinct from the daemon's. Separate keys improve attribution but
// do not make this an independent source of truth. The scan does not establish
// semantic correctness, log completeness, runtime mediation, or W009/W011
// enforcement. It writes the artifacts consumed by the /verify page.
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
import {
  buildSkillManifest,
  buildVerifierStatement,
  recomputeRoot,
  scanRefutations,
  verifierMessage,
} from "./verify-lib.mjs";

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

const lines = readFileSync(join(home, "audit", "events.jsonl"), "utf8")
  .split("\n")
  .filter((l) => l.length > 0);

// Recompute the chain root from the raw lines, exactly as the daemon does.
const root = recomputeRoot(lines);
const events = lines.map((l) => JSON.parse(l));

// Skill-run manifest, pulled from the run's own events.
const manifest = buildSkillManifest(events);

// Event-lineage heuristic over the supplied log: flag a signed skill action
// ordered after an untrusted-input event for the same issuer unless a context
// reset appears between them. This is not a semantic or causal proof.
const { verdict, refutations } = scanRefutations(events);

// Verifier identity: a separately stored ed25519 key. Key separation allows a
// reader to attribute this signature to a different key; it is not evidence of
// organizational, input, model, or runtime independence.
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

const verifierStatement = buildVerifierStatement(
  root,
  lines.length,
  verdict,
  refutations,
  verifierPubkey,
);
const message = verifierMessage(verifierStatement);
const sig = edSign(null, message, privKey).toString("base64url");

const write = (p, content) => {
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, content);
};
write(
  join(repo, "attestations", `${sha}.json`),
  `${JSON.stringify({ audit_root_hex: root, event_count: lines.length, steps: lines, verifier_statement: verifierStatement }, null, 2)}\n`,
);
write(join(repo, "attestations", `${sha}.verifier.sig`), `${sig}\n`);
write(join(repo, "landing", "public", "witness", "verifier-pubkey.txt"), `${verifierPubkey}\n`);
write(join(repo, "landing", "public", "witness", "skill", `${sha}.json`), `${JSON.stringify(manifest, null, 2)}\n`);

console.log(JSON.stringify({ sha, root, event_count: lines.length, verdict, verifier_pubkey: verifierPubkey }));
