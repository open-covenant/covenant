#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Peers_rotate envelope line-ref drift guard. docs/ipc-and-http-gateway.md
// cites three name-anchored main.rs line refs for the peers_rotate
// envelope across two anchor sentences (hybrid pattern):
//
//   - Pins-anchor sentence (count-phrase form): "Top-level keys are pinned
//     to exactly these two by the test at
//     `agent-os/crates/covenant/src/main.rs:NNN`
//     (`peers_rotate_json_pins_top_level_schema`)."
//   - Combined source-of-truth sentence with both test fn names inline:
//     "The envelope source-of-truth lives at `peers_rotate_json` in
//     `agent-os/crates/covenant/src/main.rs:NNN`. The shape-pinning tests at
//     `main.rs:NNN` (`peers_rotate_json_renders_stable_shape`) and
//     `main.rs:NNN` (`peers_rotate_json_pins_top_level_schema`) cover..."
//
// The pins line ref appears in both sentences; the validator confirms both
// citations. All three line numbers shift when main.rs grows above the
// cited declarations.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const emittersPath = "agent-os/crates/covenant/src/main.rs";

const helperFnName = "peers_rotate_json";
const rendersTestFnName = "peers_rotate_json_renders_stable_shape";
const pinsTestFnName = "peers_rotate_json_pins_top_level_schema";

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
      `${emittersPath}: expected exactly 1 top-level "fn ${helperFnName}" but found ${helperMatches.length}; remediation: confirm the peers_rotate envelope emitter is a single top-level helper, not renamed or duplicated`,
    );
  } else {
    helperLine = helperMatches[0];
  }
  if (rendersMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${rendersTestFnName}" but found ${rendersMatches.length}; remediation: confirm the peers_rotate renders-stable-shape test still exists inside the tests module`,
    );
  } else {
    rendersLine = rendersMatches[0];
  }
  if (pinsMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${pinsTestFnName}" but found ${pinsMatches.length}; remediation: confirm the peers_rotate top-level-schema pinning test still exists inside the tests module`,
    );
  } else {
    pinsLine = pinsMatches[0];
  }
}

let sourceOfTruthSentence = null;
if (docs) {
  const match = docs.match(
    /The envelope source-of-truth lives at `peers_rotate_json` in `agent-os\/crates\/covenant\/src\/main\.rs:\d+`\. The shape-pinning tests at `main\.rs:\d+` \(`peers_rotate_json_renders_stable_shape`\) and `main\.rs:\d+` \(`peers_rotate_json_pins_top_level_schema`\)/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the peers_rotate source-of-truth sentence ("The envelope source-of-truth lives at \`peers_rotate_json\` in \`agent-os/crates/covenant/src/main.rs:NNN\`. The shape-pinning tests at \`main.rs:NNN\` (\`peers_rotate_json_renders_stable_shape\`) and \`main.rs:NNN\` (\`peers_rotate_json_pins_top_level_schema\`) ..."); remediation: restore the sentence so all three peers_rotate line refs are cited`,
    );
  } else {
    sourceOfTruthSentence = match[0];
  }
}

let pinsAnchorSentence = null;
if (docs) {
  const match = docs.match(
    /Top-level keys are pinned to exactly these two by the test at `agent-os\/crates\/covenant\/src\/main\.rs:\d+` \(`peers_rotate_json_pins_top_level_schema`\)/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the peers_rotate pins-anchor sentence ("Top-level keys are pinned to exactly these two by the test at \`agent-os/crates/covenant/src/main.rs:NNN\` (\`peers_rotate_json_pins_top_level_schema\`)"); remediation: restore the sentence that records the pins-test line ref`,
    );
  } else {
    pinsAnchorSentence = match[0];
  }
}

if (sourceOfTruthSentence && helperLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${helperLine}`)) {
    fail(
      `${docsPath}: the peers_rotate source-of-truth sentence does not cite main.rs:${helperLine} (fn ${helperFnName}); remediation: update the sentence to include this helper line number`,
    );
  }
}
if (sourceOfTruthSentence && rendersLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${rendersLine}`)) {
    fail(
      `${docsPath}: the peers_rotate source-of-truth sentence does not cite main.rs:${rendersLine} (fn ${rendersTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}
if (sourceOfTruthSentence && pinsLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${pinsLine}`)) {
    fail(
      `${docsPath}: the peers_rotate source-of-truth sentence does not cite main.rs:${pinsLine} (fn ${pinsTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}

if (pinsAnchorSentence && pinsLine !== null) {
  if (!pinsAnchorSentence.includes(`main.rs:${pinsLine}`)) {
    fail(
      `${docsPath}: the peers_rotate pins-anchor sentence does not cite main.rs:${pinsLine} (fn ${pinsTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-peers-rotate-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-peers-rotate-line-refs: ok (helper main.rs:${helperLine}, renders main.rs:${rendersLine}, pins main.rs:${pinsLine})`,
);
