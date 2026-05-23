#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Chain_status envelope line-ref drift guard. docs/ipc-and-http-gateway.md
// cites three name-anchored main.rs line refs for the chain_status envelope
// in a single combined anchor sentence:
//
//   "The envelope source-of-truth lives at `chain_status_json` in
//    `agent-os/crates/covenant/src/main.rs:NNN`. Two unit tests at
//    `main.rs:NNN` (`chain_status_json_renders_stable_shape`) and
//    `main.rs:NNN` (`chain_status_json_pins_top_level_schema`) enforce..."
//
// Unlike receipt_list/memory_backfill/settlement_backfill, both test fn
// names are cited inline in backticks within the same sentence, and there
// is no separate pins-anchor sentence. All three line numbers shift
// whenever main.rs grows above the cited declarations.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const emittersPath = "agent-os/crates/covenant/src/main.rs";

const helperFnName = "chain_status_json";
const rendersTestFnName = "chain_status_json_renders_stable_shape";
const pinsTestFnName = "chain_status_json_pins_top_level_schema";

const errors = [];
const fail = (message) => errors.push(message);

let docs;
let emitters;
try {
  docs = read(docsPath);
} catch (error) {
  fail(`cannot read ${docsPath}: ${error.message}`);
}
try {
  emitters = read(emittersPath);
} catch (error) {
  fail(`cannot read ${emittersPath}: ${error.message}`);
}

let helperLine = null;
let rendersLine = null;
let pinsLine = null;
const helperMatches = [];
const rendersMatches = [];
const pinsMatches = [];

if (emitters) {
  const lines = emitters.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const helperMatch = lines[index].match(/^fn\s+(\w+)\s*\(/);
    if (helperMatch && helperMatch[1] === helperFnName) {
      helperMatches.push(index + 1);
    }
    const testMatch = lines[index].match(/^\s+fn\s+(\w+)\s*\(/);
    if (testMatch && testMatch[1] === rendersTestFnName) {
      rendersMatches.push(index + 1);
    }
    if (testMatch && testMatch[1] === pinsTestFnName) {
      pinsMatches.push(index + 1);
    }
  }

  if (helperMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 top-level "fn ${helperFnName}" but found ${helperMatches.length}; remediation: confirm the chain_status envelope emitter is a single top-level helper, not renamed or duplicated`,
    );
  } else {
    helperLine = helperMatches[0];
  }
  if (rendersMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${rendersTestFnName}" but found ${rendersMatches.length}; remediation: confirm the chain_status renders-stable-shape test still exists inside the tests module`,
    );
  } else {
    rendersLine = rendersMatches[0];
  }
  if (pinsMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${pinsTestFnName}" but found ${pinsMatches.length}; remediation: confirm the chain_status top-level-schema pinning test still exists inside the tests module`,
    );
  } else {
    pinsLine = pinsMatches[0];
  }
}

let anchorSentence = null;
if (docs) {
  const match = docs.match(
    /The envelope source-of-truth lives at `chain_status_json` in `agent-os\/crates\/covenant\/src\/main\.rs:\d+`\. Two unit tests at `main\.rs:\d+` \(`chain_status_json_renders_stable_shape`\) and `main\.rs:\d+` \(`chain_status_json_pins_top_level_schema`\)/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the chain_status anchor sentence ("The envelope source-of-truth lives at \`chain_status_json\` in \`agent-os/crates/covenant/src/main.rs:NNN\`. Two unit tests at \`main.rs:NNN\` (\`chain_status_json_renders_stable_shape\`) and \`main.rs:NNN\` (\`chain_status_json_pins_top_level_schema\`) ..."); remediation: restore the sentence so all three chain_status line refs are cited`,
    );
  } else {
    anchorSentence = match[0];
  }
}

if (anchorSentence && helperLine !== null) {
  if (!anchorSentence.includes(`main.rs:${helperLine}`)) {
    fail(
      `${docsPath}: the chain_status anchor sentence does not cite main.rs:${helperLine} (fn ${helperFnName}); remediation: update the sentence to include this helper line number`,
    );
  }
}
if (anchorSentence && rendersLine !== null) {
  if (!anchorSentence.includes(`main.rs:${rendersLine}`)) {
    fail(
      `${docsPath}: the chain_status anchor sentence does not cite main.rs:${rendersLine} (fn ${rendersTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}
if (anchorSentence && pinsLine !== null) {
  if (!anchorSentence.includes(`main.rs:${pinsLine}`)) {
    fail(
      `${docsPath}: the chain_status anchor sentence does not cite main.rs:${pinsLine} (fn ${pinsTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-chain-status-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-chain-status-line-refs: ok (helper main.rs:${helperLine}, renders main.rs:${rendersLine}, pins main.rs:${pinsLine})`,
);
