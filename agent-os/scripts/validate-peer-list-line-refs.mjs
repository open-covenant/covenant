#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Peer-list envelope line-ref drift guard. docs/ipc-and-http-gateway.md
// cites five name-anchored main.rs line refs for the peer_list envelope,
// which has a non-standard test surface compared to the other CLI
// envelopes: there is no `peer_list_json_renders_stable_shape` test —
// the top-level pins test does double duty, and three additional
// behavioral tests pin filter and match-count contracts. The five line
// refs are spread across three sentences:
//
//   - Source-of-truth sentence (single helper line ref):
//     "The envelope source-of-truth lives at `peer_list_json` in
//     `agent-os/crates/covenant/src/main.rs:NNN`."
//   - Schema/behavioral sentence (four test line refs, with the pins
//     test cited by parenthetical description instead of its fn name):
//     "Schema and behavioral tests live at `main.rs:NNN`
//     (key set + per-key typing), `main.rs:NNN`
//     (`peer_list_json_echoes_prefix_and_match_count`), `main.rs:NNN`
//     (`peer_list_json_omits_prefix_when_inactive`), and `main.rs:NNN`
//     (`peer_list_json_reports_zero_match_count_for_empty_response`)."
//   - Pins-anchor sentence (re-cites the pins test by name): "Top-level
//     keys are pinned to exactly these seven by the test at
//     `agent-os/crates/covenant/src/main.rs:NNN`
//     (`peer_list_json_pins_top_level_schema`)".
//
// All five line numbers shift whenever main.rs grows above the cited
// declarations, and the docs do not auto-update.
//
// This validator derives the line numbers from main.rs at run time by
// matching the helper fn (strict `^fn` regex, no indentation) and the
// four test fns (indented `fn` inside a `mod tests` block). Every derived
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

const helperFnName = "peer_list_json";
const pinsTestFnName = "peer_list_json_pins_top_level_schema";
const echoesTestFnName = "peer_list_json_echoes_prefix_and_match_count";
const omitsTestFnName = "peer_list_json_omits_prefix_when_inactive";
const reportsZeroTestFnName =
  "peer_list_json_reports_zero_match_count_for_empty_response";

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
let pinsLine = null;
let echoesLine = null;
let omitsLine = null;
let reportsZeroLine = null;
const helperMatches = [];
const pinsMatches = [];
const echoesMatches = [];
const omitsMatches = [];
const reportsZeroMatches = [];

if (emitters) {
  const lines = emitters.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const helperMatch = lines[index].match(/^fn\s+(\w+)\s*\(/);
    if (helperMatch && helperMatch[1] === helperFnName) {
      helperMatches.push(index + 1);
    }
    const testMatch = lines[index].match(/^\s+fn\s+(\w+)\s*\(/);
    if (testMatch) {
      if (testMatch[1] === pinsTestFnName) {
        pinsMatches.push(index + 1);
      }
      if (testMatch[1] === echoesTestFnName) {
        echoesMatches.push(index + 1);
      }
      if (testMatch[1] === omitsTestFnName) {
        omitsMatches.push(index + 1);
      }
      if (testMatch[1] === reportsZeroTestFnName) {
        reportsZeroMatches.push(index + 1);
      }
    }
  }

  if (helperMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 top-level "fn ${helperFnName}" but found ${helperMatches.length}; remediation: confirm the peer_list envelope emitter is a single top-level helper, not renamed or duplicated`,
    );
  } else {
    helperLine = helperMatches[0];
  }
  if (pinsMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${pinsTestFnName}" but found ${pinsMatches.length}; remediation: confirm the peer_list top-level-schema pinning test still exists inside the tests module`,
    );
  } else {
    pinsLine = pinsMatches[0];
  }
  if (echoesMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${echoesTestFnName}" but found ${echoesMatches.length}; remediation: confirm the peer_list echoes-prefix-and-match-count test still exists inside the tests module`,
    );
  } else {
    echoesLine = echoesMatches[0];
  }
  if (omitsMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${omitsTestFnName}" but found ${omitsMatches.length}; remediation: confirm the peer_list omits-prefix-when-inactive test still exists inside the tests module`,
    );
  } else {
    omitsLine = omitsMatches[0];
  }
  if (reportsZeroMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${reportsZeroTestFnName}" but found ${reportsZeroMatches.length}; remediation: confirm the peer_list reports-zero-match-count-for-empty-response test still exists inside the tests module`,
    );
  } else {
    reportsZeroLine = reportsZeroMatches[0];
  }
}

let sourceOfTruthSentence = null;
if (docs) {
  const match = docs.match(
    /The envelope source-of-truth lives at `peer_list_json` in `agent-os\/crates\/covenant\/src\/main\.rs:\d+`\./,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the peer_list source-of-truth sentence ("The envelope source-of-truth lives at \`peer_list_json\` in \`agent-os/crates/covenant/src/main.rs:NNN\`."); remediation: restore the sentence that records the helper line ref`,
    );
  } else {
    sourceOfTruthSentence = match[0];
  }
}

let schemaBehavioralSentence = null;
if (docs) {
  const match = docs.match(
    /Schema and behavioral tests live at `main\.rs:\d+` \(key set \+ per-key typing\), `main\.rs:\d+` \(`peer_list_json_echoes_prefix_and_match_count`\), `main\.rs:\d+` \(`peer_list_json_omits_prefix_when_inactive`\), and `main\.rs:\d+` \(`peer_list_json_reports_zero_match_count_for_empty_response`\)\./,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the peer_list schema/behavioral sentence ("Schema and behavioral tests live at \`main.rs:NNN\` (key set + per-key typing), \`main.rs:NNN\` (\`peer_list_json_echoes_prefix_and_match_count\`), \`main.rs:NNN\` (\`peer_list_json_omits_prefix_when_inactive\`), and \`main.rs:NNN\` (\`peer_list_json_reports_zero_match_count_for_empty_response\`)."); remediation: restore the sentence so all four peer_list test line refs are cited`,
    );
  } else {
    schemaBehavioralSentence = match[0];
  }
}

let pinsAnchorSentence = null;
if (docs) {
  const match = docs.match(
    /Top-level keys are pinned to exactly these seven by the test at `agent-os\/crates\/covenant\/src\/main\.rs:\d+` \(`peer_list_json_pins_top_level_schema`\)/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the peer_list pins-anchor sentence ("Top-level keys are pinned to exactly these seven by the test at \`agent-os/crates/covenant/src/main.rs:NNN\` (\`peer_list_json_pins_top_level_schema\`)"); remediation: restore the sentence that records the pins-test line ref`,
    );
  } else {
    pinsAnchorSentence = match[0];
  }
}

if (sourceOfTruthSentence && helperLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${helperLine}`)) {
    fail(
      `${docsPath}: the peer_list source-of-truth sentence does not cite main.rs:${helperLine} (fn ${helperFnName}); remediation: update the sentence to include this helper line number`,
    );
  }
}

if (schemaBehavioralSentence) {
  if (pinsLine !== null && !schemaBehavioralSentence.includes(`main.rs:${pinsLine}`)) {
    fail(
      `${docsPath}: the peer_list schema/behavioral sentence does not cite main.rs:${pinsLine} (fn ${pinsTestFnName}, "key set + per-key typing" parenthetical); remediation: update the sentence to include this test line number`,
    );
  }
  if (echoesLine !== null && !schemaBehavioralSentence.includes(`main.rs:${echoesLine}`)) {
    fail(
      `${docsPath}: the peer_list schema/behavioral sentence does not cite main.rs:${echoesLine} (fn ${echoesTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
  if (omitsLine !== null && !schemaBehavioralSentence.includes(`main.rs:${omitsLine}`)) {
    fail(
      `${docsPath}: the peer_list schema/behavioral sentence does not cite main.rs:${omitsLine} (fn ${omitsTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
  if (reportsZeroLine !== null && !schemaBehavioralSentence.includes(`main.rs:${reportsZeroLine}`)) {
    fail(
      `${docsPath}: the peer_list schema/behavioral sentence does not cite main.rs:${reportsZeroLine} (fn ${reportsZeroTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}

if (pinsAnchorSentence && pinsLine !== null) {
  if (!pinsAnchorSentence.includes(`main.rs:${pinsLine}`)) {
    fail(
      `${docsPath}: the peer_list pins-anchor sentence does not cite main.rs:${pinsLine} (fn ${pinsTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-peer-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-peer-list-line-refs: ok (helper main.rs:${helperLine}, pins main.rs:${pinsLine}, echoes main.rs:${echoesLine}, omits main.rs:${omitsLine}, reports-zero main.rs:${reportsZeroLine})`,
);
