#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Chain CLI envelope field/emitter symmetry guard. The three operator-keypair
// chain verbs (register-agent, stake, buy-credits) each emit a confirmed and a
// timeout JSON envelope at well-known emitter functions in
// agent-os/crates/covenant/src/main.rs, and the per-verb field tables are
// pinned in docs/ipc-and-http-gateway.md under "## Chain Transaction
// Envelopes". validate-cli-envelope-docs.mjs already checks that the top-level
// `kind` discriminator appears in both surfaces; this validator extends that
// guarantee to every per-field JSON key.
//
// Docs structure the section as:
//   - common-field bullets shared by every variant (kind/verb/signature/...),
//   - per-verb "adds" bullets (`verb: "<name>"` — adds `<field>` ...),
//   - a mention of `timeout_ms` in the timeout-variant description.
// The validator checks common fields against the whole section, verb-specific
// fields against the matching per-verb bullet only (so a rename inside one
// verb's bullet fails even when the same name still appears elsewhere), and
// every emitter function body against its full field set as quoted JSON keys.
//
// Maintenance contract: this script is the single source of truth for the
// chain envelope field contract. When a chain verb's emitter gains or renames
// a field, update the contract below, the emitter function body, and the
// docs section in one slice — the validator fails until both surfaces agree
// with the contract.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const emittersPath = "agent-os/crates/covenant/src/main.rs";
const docsSectionStart = "## Chain Transaction Envelopes";
const docsSectionEnd = "## CLI Read Envelopes";

const commonFields = ["kind", "verb", "signature", "rpc_url", "cluster", "status"];
const timeoutField = "timeout_ms";

const verbFields = {
  "register-agent": ["agent_key"],
  "stake": ["agent_key", "amount", "lock_until"],
  "buy-credits": ["owner", "amount_covnt"],
};

const emitters = [
  { fn: "register_agent_confirmed_json", kind: "covenant.chain.tx.v1", verb: "register-agent", timeout: false },
  { fn: "register_agent_timeout_json", kind: "covenant.chain.tx.timeout.v1", verb: "register-agent", timeout: true },
  { fn: "stake_confirmed_json", kind: "covenant.chain.tx.v1", verb: "stake", timeout: false },
  { fn: "stake_timeout_json", kind: "covenant.chain.tx.timeout.v1", verb: "stake", timeout: true },
  { fn: "buy_credits_confirmed_json", kind: "covenant.chain.tx.v1", verb: "buy-credits", timeout: false },
  { fn: "buy_credits_timeout_json", kind: "covenant.chain.tx.timeout.v1", verb: "buy-credits", timeout: true },
];

const errors = [];
const fail = (message) => errors.push(message);

let docs;
try {
  docs = read(docsPath);
} catch (error) {
  fail(`cannot read ${docsPath}: ${error.message}`);
}

let emitterSource;
try {
  emitterSource = read(emittersPath);
} catch (error) {
  fail(`cannot read ${emittersPath}: ${error.message}`);
}

let docsSection = "";
if (docs) {
  const startIndex = docs.indexOf(docsSectionStart);
  if (startIndex < 0) {
    fail(
      `${docsPath} is missing the "${docsSectionStart}" section; remediation: restore the section that pins the chain CLI envelope field tables`,
    );
  } else {
    const endIndex = docs.indexOf(docsSectionEnd, startIndex + docsSectionStart.length);
    if (endIndex < 0) {
      fail(
        `${docsPath} is missing the "${docsSectionEnd}" boundary after "${docsSectionStart}"; remediation: keep the two headings in order so the chain envelope section has a defined slice`,
      );
    } else {
      docsSection = docs.slice(startIndex, endIndex);
    }
  }
}

function extractVerbBullet(verb) {
  if (!docsSection) {
    return null;
  }
  const anchor = `\`verb: "${verb}"\``;
  const anchorIndex = docsSection.indexOf(anchor);
  if (anchorIndex < 0) {
    fail(
      `${docsPath} (## Chain Transaction Envelopes) is missing the per-verb bullet anchored at ${anchor}; remediation: keep one bullet per chain verb in the form \`verb: "<name>"\` — adds \`<field>\` ...`,
    );
    return null;
  }
  const tail = docsSection.slice(anchorIndex);
  const nextVerbIndex = tail.indexOf("\n- `verb:", 1);
  if (nextVerbIndex < 0) {
    return tail;
  }
  return tail.slice(0, nextVerbIndex);
}

function extractEmitterBody(fnName) {
  if (!emitterSource) {
    return null;
  }
  const signature = `fn ${fnName}(`;
  const sigIndex = emitterSource.indexOf(signature);
  if (sigIndex < 0) {
    fail(
      `${emittersPath} does not define "fn ${fnName}"; remediation: either the emitter was renamed and the contract is stale, or a verb was dropped — reconcile the emitters array`,
    );
    return null;
  }
  const openBrace = emitterSource.indexOf("{", sigIndex);
  if (openBrace < 0) {
    fail(
      `${emittersPath} "fn ${fnName}" body opening brace not found; remediation: confirm the emitter still uses a brace-delimited body`,
    );
    return null;
  }
  let depth = 0;
  for (let cursor = openBrace; cursor < emitterSource.length; cursor += 1) {
    const ch = emitterSource[cursor];
    if (ch === "{") {
      depth += 1;
    } else if (ch === "}") {
      depth -= 1;
      if (depth === 0) {
        return emitterSource.slice(openBrace, cursor + 1);
      }
    }
  }
  fail(
    `${emittersPath} "fn ${fnName}" body closing brace not found; remediation: confirm the emitter still uses a brace-delimited body`,
  );
  return null;
}

let pinnedLiterals = 0;

if (docsSection) {
  for (const field of commonFields) {
    pinnedLiterals += 1;
    if (!docsSection.includes(`\`${field}\``)) {
      fail(
        `${docsPath} (## Chain Transaction Envelopes) does not mention common field \`${field}\`; remediation: add the field to the shared bullet list or remove it from commonFields if the emitter dropped it`,
      );
    }
  }
  pinnedLiterals += 1;
  if (!docsSection.includes(`\`${timeoutField}\``)) {
    fail(
      `${docsPath} (## Chain Transaction Envelopes) does not mention the timeout-variant field \`${timeoutField}\`; remediation: add the bullet or remove it from the contract if the timeout emitter dropped it`,
    );
  }
}

for (const [verb, fields] of Object.entries(verbFields)) {
  const bullet = extractVerbBullet(verb);
  for (const field of fields) {
    pinnedLiterals += 1;
    if (bullet !== null && !bullet.includes(`\`${field}\``)) {
      fail(
        `${docsPath} per-verb bullet for verb "${verb}" does not mention field \`${field}\`; remediation: add the field to the verb's "adds" bullet or remove it from verbFields if the emitter dropped it`,
      );
    }
  }
}

for (const entry of emitters) {
  const body = extractEmitterBody(entry.fn);
  if (body === null) {
    continue;
  }
  const expected = [...commonFields, ...(verbFields[entry.verb] ?? [])];
  if (entry.timeout) {
    expected.push(timeoutField);
  }
  for (const field of expected) {
    pinnedLiterals += 1;
    if (!body.includes(`"${field}":`)) {
      fail(
        `${emittersPath} fn ${entry.fn} (verb ${entry.verb}, kind ${entry.kind}) does not emit JSON key "${field}"; remediation: reconcile the emitter with the contract — either the emitter was renamed and the docs are stale, or the field was dropped`,
      );
    }
  }
  if (!body.includes(`"${entry.kind}"`)) {
    fail(
      `${emittersPath} fn ${entry.fn} does not emit kind "${entry.kind}"; remediation: confirm the emitter still carries the expected discriminator`,
    );
  }
  if (!body.includes(`"${entry.verb}"`)) {
    fail(
      `${emittersPath} fn ${entry.fn} does not emit verb "${entry.verb}"; remediation: confirm the emitter still carries the expected verb literal`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-chain-cli-envelope-fields: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-chain-cli-envelope-fields: ok (${pinnedLiterals} field literals pinned across docs and emitters)`,
);
