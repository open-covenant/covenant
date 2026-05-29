#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intent_result Request::SubmitIntent.prefer_stream line-ref drift
// guard. docs/ipc-and-http-gateway.md line 243 cites a main.rs
// line for the `prefer_stream: prefer_stream.then_some(true),`
// field assignment inside the `&Request::SubmitIntent { ... }`
// literal. The cite anchors the documented `--stream` flag
// behaviour ("sets Request::SubmitIntent.prefer_stream = Some(true)
// (main.rs:N)") to the source-of-truth so a refactor that shifts
// the intent verb body is caught at the docs-validator level —
// same drift class as the eight preceding integrated slices.
//
// Selector form: the single-line statement
// `prefer_stream: prefer_stream.then_some(true),` is NOT unique
// (3 occurrences in main.rs: SubmitIntent at 2026, RecentMemory
// at 2104, RecentAudit at 3227). Disambiguation: require an
// `&Request::SubmitIntent {` opener exactly 2 lines above (with
// a `text: request_text,` field between opener and target).
// Only the SubmitIntent literal uses this pattern, making the
// triple-line shape (opener, text field, prefer_stream field) a
// unique anchor.
//
// docsRegex anchoring: line 243 contains MULTIPLE `main.rs:N`
// cites in the source-of-truth sentence — helper at :4320,
// renders-test at :5743, pins-test at :5761, CLI verb wiring
// range :2000-2074, leading-position parser range :2013-2022,
// and the prefer_stream cite (target of this validator). The
// regex anchors on the unique phrase `sets
// \`Request::SubmitIntent.prefer_stream = Some(true)\`
// (\`main.rs:N\`)` to capture only the prefer_stream cite. Per
// the IPC docs pin collision anchor feedback, the regex extends
// past the unique full-path `Request::SubmitIntent.prefer_stream`
// reference to prevent first-match contamination from the
// adjacent cites.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const opener = "&Request::SubmitIntent {";
const middle = "text: request_text,";
const selector = "prefer_stream: prefer_stream.then_some(true),";

const docsRegex =
  /sets `Request::SubmitIntent\.prefer_stream = Some\(true\)` \(`main\.rs:(\d+)`\)/;
const docsLabel =
  "intent_result SubmitIntent.prefer_stream citation";
const docsTemplate =
  "sets `Request::SubmitIntent.prefer_stream = Some(true)` (`main.rs:N`)";

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

let selectorLine = null;
if (source) {
  const lines = source.split("\n");
  const candidates = [];
  for (let index = 0; index < lines.length - 2; index += 1) {
    if (
      lines[index].trim() === opener &&
      lines[index + 1].trim() === middle &&
      lines[index + 2].trim() === selector
    ) {
      candidates.push(index + 3);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of the triple-line pattern (\`${opener}\`, \`${middle}\`, \`${selector}\`) but found ${candidates.length}; remediation: confirm the SubmitIntent literal in the intent CLI verb still uses this exact three-line shape, not refactored or duplicated`,
    );
  } else {
    selectorLine = candidates[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the SubmitIntent.prefer_stream assignment line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the prefer_stream assignment lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-intent-result-prefer-stream-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intent-result-prefer-stream-line-refs: ok (prefer_stream main.rs:${selectorLine})`,
);
