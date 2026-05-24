#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume unexpected_response slug literal line-ref
// drift guard. docs/ipc-and-http-gateway.md line 621 cites a
// main.rs line for the inline emission path that classifies a
// "daemon returned an unexpected response variant" error with
// the typed slug `"unexpected_response"`. The cite is the
// single-line `"unexpected_response",` argument to the
// intents_resume_error_json helper call at the third position
// (the `code` field).
//
// Selector form: the trimmed line `"unexpected_response",` is
// unique in main.rs (single occurrence at the inline emission
// path). The classifier function returns the same string at
// line 4641 but uses `return "unexpected_response";` syntax —
// not the bare arg-form selector — so collision is avoided
// without test-fn scoping.
//
// docsRegex anchoring: line 621 carries TWO inline-emission
// slug cites in the same sentence — `daemon_error` (sibling
// validator) and `unexpected_response` (target of this
// validator). The regex anchors on the unique trailing phrase
// "`\"unexpected_response\"` (when the daemon returns an
// unexpected response variant, `main.rs:N`)" so first-match
// capture targets only the unexpected_response cite, per the
// IPC docs pin collision anchor feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const selector = '"unexpected_response",';

const docsRegex =
  /`"unexpected_response"` \(when the daemon returns an unexpected response variant, `main\.rs:(\d+)`\)/;
const docsLabel =
  "intents_resume unexpected_response inline-emission slug citation";
const docsTemplate =
  "`\"unexpected_response\"` (when the daemon returns an unexpected response variant, `main.rs:N`)";

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
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === selector) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` but found ${candidates.length}; remediation: confirm the unexpected_response slug is emitted from a single inline call site, not duplicated or removed`,
    );
  } else {
    selectorLine = candidates[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the unexpected_response inline-emission line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the unexpected_response slug literal lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-intents-resume-unexpected-response-slug-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-unexpected-response-slug-line-refs: ok (slug literal main.rs:${selectorLine})`,
);
