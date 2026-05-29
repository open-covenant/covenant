#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume_error.intent_id inline-range double-cite line-ref
// drift guard. docs/ipc-and-http-gateway.md line 619 carries two
// inline-range cites in one sentence describing where the error
// envelope's intent_id field switches between string-uuid and null:
//
//   * `main.rs:N1-M1` — the daemon-error JSON-emission call args
//     (Some(intent_id) second arg, "daemon_error" slug). The
//     intent_id is already resolved before the daemon round-trip
//     surfaces a Response::Error.
//   * `main.rs:N2-M2` — the missing/conflicting JSON-emission
//     block (None second arg). The intent_id could not be resolved
//     before the daemon round-trip (missing_intent_id or
//     conflicting_flags paths).
//
// Anchors:
//   * 3900-3908 block: unique line `let code =
//     intents_resume_error_code(&message);` at line 3902. Walk back
//     2 lines to `if as_json {` (range start) and forward 6 lines
//     to the closing `);` (range end). Both walk distances are
//     verified by line content so a refactor that adds/removes
//     lines inside the block surfaces a remediation message.
//   * 3951-3956 block: the daemon_error slug literal `"daemon_error",`
//     scoped to lines before `mod tests {` (3 occurrences total
//     file-wide; 1 inline + 2 in test bodies). Walk back 3 lines
//     to `serde_json::to_string(&intents_resume_error_json(`
//     (range start, the SECOND occurrence in the file) and forward
//     2 lines to `))?` (range end).
//
// docsRegex anchoring: line 619 contains THREE cites total — the
// two inline ranges (targets) and a Pinned-as cite (5224-5227,
// already covered by validate-intents-resume-type-level-pin-line-
// refs.mjs's `intent_id` target). The regex captures the two
// inline ranges via the unique prepositions "per `main.rs:N1-M1`"
// (daemon-error branch) and "paths at `main.rs:N2-M2`"
// (missing/conflicting branch), preserving docs order. The
// trailing Pinned-as cite is not captured here.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testsBoundary = "mod tests {";

const docsRegex =
  /per `main\.rs:(\d+)-(\d+)`\); \*\*null\*\* when the intent_id could not be resolved before the daemon round-trip \(e\.g\., `missing_intent_id` and `conflicting_flags` paths at `main\.rs:(\d+)-(\d+)`\)/;
const docsLabel =
  "intents_resume_error.intent_id inline-range double citation";
const docsTemplate =
  "per `main.rs:N1-M1`); **null** when the intent_id could not be resolved before the daemon round-trip (e.g., `missing_intent_id` and `conflicting_flags` paths at `main.rs:N2-M2`)";

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

function findUniqueLine(lines, selector, upperBound, label) {
  const candidates = [];
  const bound = upperBound ?? lines.length;
  for (let index = 0; index < bound; index += 1) {
    if (lines[index].trim() === selector) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` (${label}) but found ${candidates.length}; remediation: confirm the anchor is present exactly once in the expected scope`,
    );
    return null;
  }
  return candidates[0];
}

function verifyLine(lines, lineNumber, expected, label) {
  const actual = lines[lineNumber - 1];
  if (actual === undefined || actual.trim() !== expected) {
    fail(
      `${sourcePath}:${lineNumber}: expected \`${expected}\` (${label}) but found \`${(actual ?? "").trim()}\`; remediation: the inline-range convention requires this line to match the expected anchor — restore the structural shape or update this validator if the convention intentionally changed`,
    );
    return false;
  }
  return true;
}

let missingRange = null;
let daemonRange = null;
if (source) {
  const lines = source.split("\n");

  let modTestsIndex = -1;
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === testsBoundary) {
      modTestsIndex = index;
      break;
    }
  }
  if (modTestsIndex < 0) {
    fail(
      `${sourcePath}: could not locate \`${testsBoundary}\` boundary; remediation: confirm the test module declaration is present`,
    );
  }

  const codeAnchorLine = findUniqueLine(
    lines,
    "let code = intents_resume_error_code(&message);",
    null,
    "missing/conflicting branch anchor",
  );
  if (codeAnchorLine !== null) {
    const startLine = codeAnchorLine - 2;
    const endLine = codeAnchorLine + 6;
    const startOk = verifyLine(
      lines,
      startLine,
      "if as_json {",
      "missing/conflicting range start (if as_json opener)",
    );
    const endOk = verifyLine(
      lines,
      endLine,
      ");",
      "missing/conflicting range end (closing `);` after intents_resume_error_json call)",
    );
    if (startOk && endOk) {
      missingRange = [startLine, endLine];
    }
  }

  const daemonAnchorLine = findUniqueLine(
    lines,
    '"daemon_error",',
    modTestsIndex >= 0 ? modTestsIndex : null,
    `daemon_error branch anchor (before \`${testsBoundary}\` boundary)`,
  );
  if (daemonAnchorLine !== null) {
    const startLine = daemonAnchorLine - 3;
    const endLine = daemonAnchorLine + 2;
    const startOk = verifyLine(
      lines,
      startLine,
      "serde_json::to_string(&intents_resume_error_json(",
      "daemon-error range start (serde_json::to_string opener)",
    );
    const endOk = verifyLine(
      lines,
      endLine,
      "))?",
      "daemon-error range end (closing `))?` after slug literal)",
    );
    if (startOk && endOk) {
      daemonRange = [startLine, endLine];
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the two inline-range cites in the intent_id error-branch sentence`,
    );
  } else if (daemonRange !== null && missingRange !== null) {
    const citedDaemon = [parseInt(match[1], 10), parseInt(match[2], 10)];
    const citedMissing = [parseInt(match[3], 10), parseInt(match[4], 10)];
    if (citedDaemon[0] !== daemonRange[0] || citedDaemon[1] !== daemonRange[1]) {
      fail(
        `${docsPath}: the daemon-error inline-range cite is main.rs:${citedDaemon[0]}-${citedDaemon[1]} but the JSON-emission call lives at :${daemonRange[0]}-${daemonRange[1]}; remediation: update the citation to :${daemonRange[0]}-${daemonRange[1]}`,
      );
    }
    if (
      citedMissing[0] !== missingRange[0] ||
      citedMissing[1] !== missingRange[1]
    ) {
      fail(
        `${docsPath}: the missing/conflicting inline-range cite is main.rs:${citedMissing[0]}-${citedMissing[1]} but the JSON-emission block lives at :${missingRange[0]}-${missingRange[1]}; remediation: update the citation to :${missingRange[0]}-${missingRange[1]}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-intents-resume-error-intent-id-inline-ranges-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-error-intent-id-inline-ranges-line-refs: ok (daemon-error main.rs:${daemonRange[0]}-${daemonRange[1]}, missing/conflicting main.rs:${missingRange[0]}-${missingRange[1]})`,
);
