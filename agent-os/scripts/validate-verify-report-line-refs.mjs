#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Verify-report envelope line-ref drift guard. docs/ipc-and-http-gateway.md
// cites three name-anchored main.rs line refs for the verify_report envelope,
// one of which is a range:
//
//   - The `verify_report_json` helper fn declaration line, cited in the
//     source-of-truth sentence ("The envelope source-of-truth lives at
//     `verify_report_json` in `agent-os/crates/covenant/src/main.rs:NNN`").
//   - The `verify_report_json_pins_top_level_schema` test fn declaration
//     line, cited in two places: in the source-of-truth sentence as the
//     start of the test-body range ("The shape-pinning test at
//     `main.rs:NNN-MMM` covers both the populated and empty cases ...")
//     and in the pins-anchor sentence ("Top-level keys are pinned to
//     exactly these five by the test at
//     `agent-os/crates/covenant/src/main.rs:NNN`
//     (`verify_report_json_pins_top_level_schema`)").
//   - The closing-brace line of the pins test fn body, cited as the end
//     of the test-body range in the source-of-truth sentence. The
//     verify_report envelope has no separate `renders_stable_shape` line
//     ref in docs: the single pins-top-level-schema test contains the
//     populated and empty `assert_shape` cases, so the docs reference
//     the whole test body via a range rather than two test fns.
//
// All three line numbers shift whenever main.rs grows above or inside
// the pins test fn body, and the docs do not auto-update.
//
// This validator derives the line numbers from main.rs at run time by
// matching the helper fn (strict `^fn` regex, no indentation), the pins
// test fn declaration (indented `fn` inside a `mod tests` block), and
// the matching closing brace at the test fn's indentation level. Every
// derived line number must appear in its expected anchor sentence;
// the range citation must use both the start and end lines. Line
// numbers are never hardcoded.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const emittersPath = "agent-os/crates/covenant/src/main.rs";

const helperFnName = "verify_report_json";
const pinsTestFnName = "verify_report_json_pins_top_level_schema";

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
let pinsEndLine = null;
const helperMatches = [];
const pinsMatches = [];

if (emitters) {
  const lines = emitters.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const helperMatch = lines[index].match(/^fn\s+(\w+)\s*\(/);
    if (helperMatch && helperMatch[1] === helperFnName) {
      helperMatches.push(index + 1);
    }
    const testMatch = lines[index].match(/^(\s+)fn\s+(\w+)\s*\(/);
    if (testMatch && testMatch[2] === pinsTestFnName) {
      pinsMatches.push({ line: index + 1, indent: testMatch[1] });
    }
  }

  if (helperMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 top-level "fn ${helperFnName}" but found ${helperMatches.length}; remediation: confirm the verify_report envelope emitter is a single top-level helper, not renamed or duplicated`,
    );
  } else {
    helperLine = helperMatches[0];
  }
  if (pinsMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${pinsTestFnName}" but found ${pinsMatches.length}; remediation: confirm the verify_report top-level-schema pinning test still exists inside the tests module`,
    );
  } else {
    pinsLine = pinsMatches[0].line;
    const indent = pinsMatches[0].indent;
    const closing = new RegExp(`^${indent}}\\s*$`);
    for (let index = pinsLine; index < lines.length; index += 1) {
      if (closing.test(lines[index])) {
        pinsEndLine = index + 1;
        break;
      }
    }
    if (pinsEndLine === null) {
      fail(
        `${emittersPath}: could not find the matching closing brace for "fn ${pinsTestFnName}" (started at line ${pinsLine}); remediation: confirm the test body is well-formed and the closing brace shares the fn declaration's indentation`,
      );
    }
  }
}

let sourceOfTruthSentence = null;
if (docs) {
  const match = docs.match(
    /The envelope source-of-truth lives at `verify_report_json` in `agent-os\/crates\/covenant\/src\/main\.rs:\d+`\. The shape-pinning test at `main\.rs:\d+-\d+` covers both the populated and empty cases/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the verify_report source-of-truth sentence ("The envelope source-of-truth lives at \`verify_report_json\` in \`agent-os/crates/covenant/src/main.rs:NNN\`. The shape-pinning test at \`main.rs:NNN-MMM\` covers both the populated and empty cases ..."); remediation: restore the sentence so the helper line ref and the pins-test body range are cited`,
    );
  } else {
    sourceOfTruthSentence = match[0];
  }
}

let pinsAnchorSentence = null;
if (docs) {
  const match = docs.match(
    /Top-level keys are pinned to exactly these five by the test at `agent-os\/crates\/covenant\/src\/main\.rs:\d+` \(`verify_report_json_pins_top_level_schema`\)/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the verify_report pins-anchor sentence ("Top-level keys are pinned to exactly these five by the test at \`agent-os/crates/covenant/src/main.rs:NNN\` (\`verify_report_json_pins_top_level_schema\`)"); remediation: restore the sentence that records the pins-test line ref`,
    );
  } else {
    pinsAnchorSentence = match[0];
  }
}

if (sourceOfTruthSentence && helperLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${helperLine}`)) {
    fail(
      `${docsPath}: the verify_report source-of-truth sentence does not cite main.rs:${helperLine} (fn ${helperFnName}); remediation: update the sentence to include this helper line number`,
    );
  }
}
if (sourceOfTruthSentence && pinsLine !== null && pinsEndLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${pinsLine}-${pinsEndLine}`)) {
    fail(
      `${docsPath}: the verify_report source-of-truth sentence does not cite main.rs:${pinsLine}-${pinsEndLine} (fn ${pinsTestFnName} body range); remediation: update the sentence to include this test-body range`,
    );
  }
}

if (pinsAnchorSentence && pinsLine !== null) {
  if (!pinsAnchorSentence.includes(`main.rs:${pinsLine}`)) {
    fail(
      `${docsPath}: the verify_report pins-anchor sentence does not cite main.rs:${pinsLine} (fn ${pinsTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-verify-report-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-verify-report-line-refs: ok (helper main.rs:${helperLine}, pins main.rs:${pinsLine}, pins-end main.rs:${pinsEndLine})`,
);
