#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const emittersPath = "agent-os/crates/covenant/src/main.rs";

const heading = "## Chain Transaction Envelopes";
const kinds = ["covenant.chain.tx.v1", "covenant.chain.tx.timeout.v1"];

const errors = [];
const fail = (message) => errors.push(message);

let docs;
try {
  docs = read(docsPath);
} catch (error) {
  fail(`cannot read ${docsPath}: ${error.message}`);
}

let emitters;
try {
  emitters = read(emittersPath);
} catch (error) {
  fail(`cannot read ${emittersPath}: ${error.message}`);
}

if (docs && !docs.includes(heading)) {
  fail(
    `${docsPath} is missing the "${heading}" section; remediation: restore the Chain Transaction Envelopes section that pins both chain.tx kinds`,
  );
}

if (docs) {
  for (const kind of kinds) {
    if (!docs.includes(kind)) {
      fail(
        `${docsPath} does not mention kind "${kind}"; remediation: the docs section must list every kind emitted by the chain CLI verbs`,
      );
    }
  }
}

if (emitters) {
  for (const kind of kinds) {
    if (!emitters.includes(kind)) {
      fail(
        `${emittersPath} does not emit kind "${kind}"; remediation: either the emitter was renamed and the docs are stale, or a verb was dropped — reconcile both sides`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-chain-tx-envelope-docs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-chain-tx-envelope-docs: ok (${kinds.length} kinds pinned across docs and emitters)`,
);
