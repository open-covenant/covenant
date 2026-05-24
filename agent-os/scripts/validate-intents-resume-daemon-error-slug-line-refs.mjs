#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume daemon_error slug literal line-ref drift
// guard. docs/ipc-and-http-gateway.md line 621 cites a main.rs
// line for the inline emission path that classifies a "daemon
// returned Response::Error" error with the typed slug
// `"daemon_error"`. The cite is the single-line
// `"daemon_error",` argument to the intents_resume_error_json
// helper call at the third position (the `code` field).
//
// Selector form: the trimmed line `"daemon_error",` appears 3
// times in main.rs — once at the inline emission path (target
// of this cite) and twice inside the `mod tests` block as test
// fixture values. The validator restricts the search to lines
// BEFORE the `mod tests {` boundary so collision with the
// test-fixture occurrences cannot contaminate the result.
//
// docsRegex anchoring: line 621 carries TWO inline-emission
// slug cites in the same sentence — `daemon_error` (target of
// this validator) and `unexpected_response` (sibling pinned
// separately). The regex anchors on the unique trailing phrase
// "`\"daemon_error\"` (when the daemon returns
// `Response::Error`, `main.rs:N`)" so first-match capture
// targets only the daemon_error cite, per the IPC docs pin
// collision anchor feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const selector = '"daemon_error",';
const testsBoundary = "mod tests {";

const docsRegex =
  /`"daemon_error"` \(when the daemon returns `Response::Error`, `main\.rs:(\d+)`\)/;
const docsLabel =
  "intents_resume daemon_error inline-emission slug citation";
const docsTemplate =
  "`\"daemon_error\"` (when the daemon returns `Response::Error`, `main.rs:N`)";

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
  } else {
    const candidates = [];
    for (let index = 0; index < modTestsIndex; index += 1) {
      if (lines[index].trim() === selector) {
        candidates.push(index + 1);
      }
    }
    if (candidates.length !== 1) {
      fail(
        `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` before \`${testsBoundary}\` (line ${modTestsIndex + 1}) but found ${candidates.length}; remediation: confirm the daemon_error slug is emitted from a single inline call site outside the test module`,
      );
    } else {
      selectorLine = candidates[0];
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the daemon_error inline-emission line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the daemon_error slug literal lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-intents-resume-daemon-error-slug-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-daemon-error-slug-line-refs: ok (slug literal main.rs:${selectorLine})`,
);
