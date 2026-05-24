#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// peer_revoke --json exit-code coupling line-ref drift guard.
// docs/ipc-and-http-gateway.md line 345 (the peer_revoke
// `**Exit-code coupling**` sentence) carries three main.rs cites in one
// sentence describing the documented exit-code contract:
//   1. the `peer_revoke_is_failure` classifier fn (range 4682-4689);
//   2. its application on the `--json` path (range 3807-3809);
//   3. the regression test `peer_revoke_json_exit_classification_matches_human_cli`
//      (single line 7610) that pins the classifier mapping.
//
// No prior validator bound any of these: the sibling exit-code-coupling
// validators (validate-ignore-report-exit-code-line-refs.mjs,
// validate-intents-resume-exit-code-coupling-line-refs.mjs) guard the
// ignore-report and intents-resume verbs only. Those siblings share the
// `**Exit-code coupling**` lead, so each docsRegex below leads on a
// peer_revoke-unique phrase (`peer_revoke_is_failure`, the exact
// `` `--json` path (`main.rs: `` parenthetical, and the test name) to
// keep first-match capture from drifting onto a sibling sentence, per
// the IPC docs pin collision-anchor lesson.
//
// The three cites are NOT in ascending source order — the classifier fn
// declaration (4682) sits below its `--json` call site (3807) and the
// test (7610) sits below both — so each cite is bound independently by
// its own unique anchor with no relative-order check (an ascending-order
// assertion like the intents-resume sibling's would false-fail here).
//
// Closer detection differs per range: cite 1 is a top-level fn whose
// closer is a column-0 `}`; cite 2 is a nested CLI-arm `if` block whose
// closer is a `}` at the opener's own indentation, with the enclosed
// `std::process::exit(1);` asserted as a structural sanity check.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const classifierOpener =
  "fn peer_revoke_is_failure(outcome: &RevokeOutcome) -> bool {";
const jsonPathOpener = "if peer_revoke_is_failure(&outcome) {";
const exitSelector = "std::process::exit(1);";
const testOpener =
  "fn peer_revoke_json_exit_classification_matches_human_cli() {";
const closerWindowLines = 40;

const classifierRegex =
  /the `peer_revoke_is_failure` classifier at `agent-os\/crates\/covenant\/src\/main\.rs:(\d+)-(\d+)`/;
const jsonPathRegex = /including in the `--json` path \(`main\.rs:(\d+)-(\d+)`\)/;
const testRegex =
  /pinned by the test at `main\.rs:(\d+)` \(`peer_revoke_json_exit_classification_matches_human_cli`\)/;

const errors = [];
const fail = (message) => errors.push(message);

let docs;
let source;
try {
  docs = read(docsPath);
} catch (error) {
  fail(`cannot read ${docsPath}: ${error.message}`);
}
try {
  source = read(sourcePath);
} catch (error) {
  fail(`cannot read ${sourcePath}: ${error.message}`);
}

let sourceLines = [];
if (source) {
  sourceLines = source.split("\n");
}

function uniqueLine(anchor, label) {
  const candidates = [];
  for (let index = 0; index < sourceLines.length; index += 1) {
    if (sourceLines[index].trim() === anchor) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${anchor}\` (${label}) but found ${candidates.length}; remediation: confirm the ${label} anchor is present exactly once`,
    );
    return null;
  }
  return candidates[0];
}

function deriveTopLevelFnRange(anchor, label) {
  const openerLine = uniqueLine(anchor, label);
  if (openerLine === null) {
    return null;
  }
  for (let offset = 1; offset <= closerWindowLines; offset += 1) {
    const targetIndex = openerLine - 1 + offset;
    if (targetIndex >= sourceLines.length) {
      break;
    }
    if (sourceLines[targetIndex] === "}") {
      return [openerLine, targetIndex + 1];
    }
  }
  fail(
    `${sourcePath}:${openerLine}: no column-0 \`}\` within ${closerWindowLines} lines after the ${label} opener; remediation: confirm ${label} is a top-level fn closed by a column-0 brace`,
  );
  return null;
}

function deriveJsonPathRange(anchor, label) {
  const openerLine = uniqueLine(anchor, label);
  if (openerLine === null) {
    return null;
  }
  const indent = sourceLines[openerLine - 1].match(/^\s*/)[0];
  const closer = `${indent}}`;
  let sawExit = false;
  for (let offset = 1; offset <= closerWindowLines; offset += 1) {
    const targetIndex = openerLine - 1 + offset;
    if (targetIndex >= sourceLines.length) {
      break;
    }
    if (sourceLines[targetIndex].trim() === exitSelector) {
      sawExit = true;
    }
    if (sourceLines[targetIndex] === closer) {
      if (!sawExit) {
        fail(
          `${sourcePath}:${openerLine}: the ${label} block closes at :${targetIndex + 1} without an enclosed \`${exitSelector}\`; remediation: confirm the --json failure path still exits(1) inside the peer_revoke_is_failure guard`,
        );
        return null;
      }
      return [openerLine, targetIndex + 1];
    }
  }
  fail(
    `${sourcePath}:${openerLine}: no \`}\` at the opener indentation within ${closerWindowLines} lines after the ${label} opener; remediation: confirm the --json guard block is closed at its own indentation`,
  );
  return null;
}

const classifierRange = deriveTopLevelFnRange(
  classifierOpener,
  "peer_revoke_is_failure classifier fn",
);
const jsonPathRange = deriveJsonPathRange(
  jsonPathOpener,
  "peer_revoke --json exit guard",
);
const testLine = uniqueLine(testOpener, "peer_revoke exit-classification test");

function checkRangeCite(regex, range, label) {
  if (!docs) {
    return;
  }
  const match = docs.match(regex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${label} range citation; remediation: restore the \`main.rs:N-M\` cite in the peer_revoke Exit-code coupling sentence`,
    );
    return;
  }
  if (range === null) {
    return;
  }
  const cited = [parseInt(match[1], 10), parseInt(match[2], 10)];
  if (cited[0] !== range[0] || cited[1] !== range[1]) {
    fail(
      `${docsPath}: the ${label} cite is main.rs:${cited[0]}-${cited[1]} but the source range is :${range[0]}-${range[1]}; remediation: update the citation to :${range[0]}-${range[1]}`,
    );
  }
}

checkRangeCite(classifierRegex, classifierRange, "classifier fn");
checkRangeCite(jsonPathRegex, jsonPathRange, "--json exit path");

if (docs) {
  const match = docs.match(testRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the exit-classification test citation; remediation: restore the \`main.rs:N\` cite naming peer_revoke_json_exit_classification_matches_human_cli`,
    );
  } else if (testLine !== null) {
    const cited = parseInt(match[1], 10);
    if (cited !== testLine) {
      fail(
        `${docsPath}: the exit-classification test cite is main.rs:${cited} but the test fn lives at :${testLine}; remediation: update the citation to :${testLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-peer-revoke-exit-code-classifier-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-peer-revoke-exit-code-classifier-line-refs: ok (classifier main.rs:${classifierRange[0]}-${classifierRange[1]}, --json path main.rs:${jsonPathRange[0]}-${jsonPathRange[1]}, test main.rs:${testLine})`,
);
