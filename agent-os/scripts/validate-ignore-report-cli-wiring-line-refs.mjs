#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// ignore_report CLI verb wiring range line-ref drift guard.
// docs/ipc-and-http-gateway.md line 587 cites a main.rs range
// for the `"ignore" => {` match arm in the CLI dispatch table —
// the structural anchor that documents where the unsuffixed
// `covenant ignore check` verb runs. The cite range is the
// opener line (the `"ignore" => {` statement) through the
// matching closing brace before the next `"settlement" => {`
// arm.
//
// Selector form: the opener `"ignore" => {` (at the leading
// indentation matching other CLI verb arms) is unique in
// main.rs as a verb-arm opener. The closer is derived by
// brace-balancing forward from the opener until the depth
// returns to the opener's level (i.e., the matching `}` that
// closes the arm body). Nested braces inside the arm are
// counted so a refactor that adds inner blocks does not break
// the closer detection.
//
// docsRegex anchoring: line 587 carries the wiring cite plus
// two follow-on print cites (matched-case and unmatched-case,
// pinned by sibling validators). Multiple envelope blocks in
// the doc start with "The CLI verb is wired at `main.rs:N-M`",
// so the prefix alone is not unique. The regex anchors on the
// full ignore_report continuation "; without `--json`, the
// matched case prints" so first-match capture is forced to this
// sentence, per the IPC docs pin collision anchor feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const opener = '"ignore" => {';

const docsRegex =
  /The CLI verb is wired at `main\.rs:(\d+)-(\d+)`; without `--json`, the matched case prints/;
const docsLabel =
  "ignore_report CLI verb wiring range citation";
const docsTemplate =
  "The CLI verb is wired at `main.rs:N-M`; without `--json`, the matched case prints";

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

let openerLine = null;
let closerLine = null;
if (source) {
  const lines = source.split("\n");
  const openerMatches = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === opener) {
      openerMatches.push(index + 1);
    }
  }
  if (openerMatches.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${opener}\` (verb-arm opener) but found ${openerMatches.length}; remediation: confirm the ignore CLI verb is wired through exactly one match arm with this exact opener`,
    );
  } else {
    openerLine = openerMatches[0];
    let depth = 0;
    for (let index = openerLine - 1; index < lines.length; index += 1) {
      const line = lines[index];
      for (let ch = 0; ch < line.length; ch += 1) {
        if (line[ch] === "{") depth += 1;
        else if (line[ch] === "}") {
          depth -= 1;
          if (depth === 0) {
            closerLine = index + 1;
            break;
          }
        }
      }
      if (closerLine !== null) break;
    }
    if (closerLine === null) {
      fail(
        `${sourcePath}: could not locate the closing brace for the \`${opener}\` arm opened at line ${openerLine}; remediation: confirm the arm body has balanced braces`,
      );
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the ignore CLI verb wiring range`,
    );
  } else if (openerLine !== null && closerLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== openerLine || citedEnd !== closerLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the \`"ignore" => { ... }\` arm lives at :${openerLine}-${closerLine}; remediation: update the citation to :${openerLine}-${closerLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-ignore-report-cli-wiring-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-ignore-report-cli-wiring-line-refs: ok (CLI verb arm main.rs:${openerLine}-${closerLine})`,
);
