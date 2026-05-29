#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// A2A-status envelope line-ref drift guard. docs/ipc-and-http-gateway.md
// cites four name-anchored main.rs line refs for the a2a_status envelope
// (an extra behavioral test compared to the standard three-line-ref shape,
// because the envelope pins an always-emitted-as-null contract on its
// filter fields via a dedicated `_omits_deadline_filter_when_inactive`
// test in addition to the usual renders + pins pair):
//
//   - The `a2a_status_json` helper fn declaration line, cited in the
//     source-of-truth sentence ("The envelope source-of-truth lives at
//     `a2a_status_json` in `agent-os/crates/covenant/src/main.rs:NNN`").
//   - The `a2a_status_json_renders_stable_shape` unit test line, cited in
//     the source-of-truth sentence ("Three unit tests at `main.rs:NNN`
//     (`a2a_status_json_renders_stable_shape`), ...").
//   - The `a2a_status_json_omits_deadline_filter_when_inactive` unit test
//     line, cited in the source-of-truth sentence ("..., `main.rs:NNN`
//     (`a2a_status_json_omits_deadline_filter_when_inactive`, which pins
//     the always-emitted-as-null contract on the filter fields), and
//     ...").
//   - The `a2a_status_json_pins_top_level_schema` unit test line, cited
//     both in the source-of-truth sentence ("..., and `main.rs:NNN`
//     cover ...") and in the pins-anchor sentence ("Top-level keys are
//     pinned to exactly these seven by the test at
//     `agent-os/crates/covenant/src/main.rs:NNN`
//     (`a2a_status_json_pins_top_level_schema`)").
//
// All four line numbers shift whenever main.rs grows above the cited
// declarations, and the docs do not auto-update.
//
// This validator derives the line numbers from main.rs at run time by
// matching the helper fn (strict `^fn` regex, no indentation) and the
// three test fns (indented `fn` inside a `mod tests` block). Every derived
// line number must appear in its expected anchor sentence. Line numbers
// are never hardcoded; the validator self-corrects to wherever the
// declarations currently live.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const emittersPath = "agent-os/crates/covenant/src/main.rs";

const helperFnName = "a2a_status_json";
const rendersTestFnName = "a2a_status_json_renders_stable_shape";
const omitsTestFnName = "a2a_status_json_omits_deadline_filter_when_inactive";
const pinsTestFnName = "a2a_status_json_pins_top_level_schema";

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
let omitsLine = null;
let pinsLine = null;
const helperMatches = [];
const rendersMatches = [];
const omitsMatches = [];
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
    if (testMatch && testMatch[1] === omitsTestFnName) {
      omitsMatches.push(index + 1);
    }
    if (testMatch && testMatch[1] === pinsTestFnName) {
      pinsMatches.push(index + 1);
    }
  }

  if (helperMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 top-level "fn ${helperFnName}" but found ${helperMatches.length}; remediation: confirm the a2a_status envelope emitter is a single top-level helper, not renamed or duplicated`,
    );
  } else {
    helperLine = helperMatches[0];
  }
  if (rendersMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${rendersTestFnName}" but found ${rendersMatches.length}; remediation: confirm the a2a_status renders-stable-shape test still exists inside the tests module`,
    );
  } else {
    rendersLine = rendersMatches[0];
  }
  if (omitsMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${omitsTestFnName}" but found ${omitsMatches.length}; remediation: confirm the a2a_status omits-deadline-filter-when-inactive test still exists inside the tests module`,
    );
  } else {
    omitsLine = omitsMatches[0];
  }
  if (pinsMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${pinsTestFnName}" but found ${pinsMatches.length}; remediation: confirm the a2a_status top-level-schema pinning test still exists inside the tests module`,
    );
  } else {
    pinsLine = pinsMatches[0];
  }
}

let sourceOfTruthSentence = null;
if (docs) {
  const match = docs.match(
    /The envelope source-of-truth lives at `a2a_status_json` in `agent-os\/crates\/covenant\/src\/main\.rs:\d+`\. Three unit tests at `main\.rs:\d+` \(`a2a_status_json_renders_stable_shape`\), `main\.rs:\d+` \(`a2a_status_json_omits_deadline_filter_when_inactive`[^)]*\), and `main\.rs:\d+` cover/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the a2a_status source-of-truth sentence ("The envelope source-of-truth lives at \`a2a_status_json\` in \`agent-os/crates/covenant/src/main.rs:NNN\`. Three unit tests at \`main.rs:NNN\` (\`a2a_status_json_renders_stable_shape\`), \`main.rs:NNN\` (\`a2a_status_json_omits_deadline_filter_when_inactive\`...), and \`main.rs:NNN\` cover ..."); remediation: restore the sentence so all four a2a_status line refs are cited`,
    );
  } else {
    sourceOfTruthSentence = match[0];
  }
}

let pinsAnchorSentence = null;
if (docs) {
  const match = docs.match(
    /Top-level keys are pinned to exactly these seven by the test at `agent-os\/crates\/covenant\/src\/main\.rs:\d+` \(`a2a_status_json_pins_top_level_schema`\)/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the a2a_status pins-anchor sentence ("Top-level keys are pinned to exactly these seven by the test at \`agent-os/crates/covenant/src/main.rs:NNN\` (\`a2a_status_json_pins_top_level_schema\`)"); remediation: restore the sentence that records the pins-test line ref`,
    );
  } else {
    pinsAnchorSentence = match[0];
  }
}

if (sourceOfTruthSentence && helperLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${helperLine}`)) {
    fail(
      `${docsPath}: the a2a_status source-of-truth sentence does not cite main.rs:${helperLine} (fn ${helperFnName}); remediation: update the sentence to include this helper line number`,
    );
  }
}
if (sourceOfTruthSentence && rendersLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${rendersLine}`)) {
    fail(
      `${docsPath}: the a2a_status source-of-truth sentence does not cite main.rs:${rendersLine} (fn ${rendersTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}
if (sourceOfTruthSentence && omitsLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${omitsLine}`)) {
    fail(
      `${docsPath}: the a2a_status source-of-truth sentence does not cite main.rs:${omitsLine} (fn ${omitsTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}
if (sourceOfTruthSentence && pinsLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${pinsLine}`)) {
    fail(
      `${docsPath}: the a2a_status source-of-truth sentence does not cite main.rs:${pinsLine} (fn ${pinsTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}

if (pinsAnchorSentence && pinsLine !== null) {
  if (!pinsAnchorSentence.includes(`main.rs:${pinsLine}`)) {
    fail(
      `${docsPath}: the a2a_status pins-anchor sentence does not cite main.rs:${pinsLine} (fn ${pinsTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-a2a-status-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-a2a-status-line-refs: ok (helper main.rs:${helperLine}, renders main.rs:${rendersLine}, omits main.rs:${omitsLine}, pins main.rs:${pinsLine})`,
);
