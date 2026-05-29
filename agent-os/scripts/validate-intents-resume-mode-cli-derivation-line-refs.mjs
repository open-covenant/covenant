#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume mode CLI derivation line-ref drift guard.
// docs/ipc-and-http-gateway.md line 607 cites a main.rs line for
// the CLI arm that derives the envelope's `mode` field from the
// invocation form — `let mode = if want_latest { "latest" } else
// { "explicit" };`. The cite is a single-line statement inside
// the `covenant intents resume` CLI arm; it is the sole binding
// site for the value that both the success and error
// intents_resume_*_json helpers later receive as a `&str`.
//
// Selector form: the full single-line statement
// `let mode = if want_latest { "latest" } else { "explicit" };`
// appears exactly once in main.rs (verified by grep -nF). The
// validator asserts candidates==1 so a refactor that introduces a
// second mode-binding pattern surfaces the duplication instead of
// silently picking the first match.
//
// docsRegex anchoring: line 607 carries TWO sibling cite forms in
// the same sentence — the CLI-derivation cite (target of this
// validator at main.rs:3889) and a type-level Pinned-as cite pair
// (5375 success / 5223 error) already covered by the sibling
// validate-intents-resume-mode-type-level-pin-line-refs.mjs. The
// regex anchors on the unique phrase
// "derived from the CLI invocation form at `main.rs:N`
// (`--latest`/`latest` → `\"latest\"`," so first-match capture
// targets only the CLI-derivation cite, per the IPC docs pin
// collision anchor feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const selector =
  'let mode = if want_latest { "latest" } else { "explicit" };';

const docsRegex =
  /derived from the CLI invocation form at `main\.rs:(\d+)` \(`--latest`\/`latest` → `"latest"`,/;
const docsLabel =
  "intents_resume mode CLI derivation citation";
const docsTemplate =
  "derived from the CLI invocation form at `main.rs:N` (`--latest`/`latest` → `\"latest\"`,";

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
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` but found ${candidates.length}; remediation: confirm the intents_resume mode-binding line is present exactly once in the CLI arm`,
    );
  } else {
    selectorLine = candidates[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the intents_resume mode CLI derivation line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the mode-binding statement lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-intents-resume-mode-cli-derivation-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-mode-cli-derivation-line-refs: ok (mode binding main.rs:${selectorLine})`,
);
