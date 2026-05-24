#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume_error_code fn-range line-ref drift guard.
// docs/ipc-and-http-gateway.md line 621 cites a main.rs range
// for the `intents_resume_error_code` helper fn — the slug
// classifier that maps daemon error messages to the documented
// typed slugs (no_budget_exhausted_row, missing_intent_id,
// invalid_intent_id, conflicting_flags, plus catch-all "error").
// The cite range is the top-level fn declaration line through
// its matching closing brace.
//
// Selector form: `fn intents_resume_error_code(` is unique at
// the top level (`^fn` regex with no indentation). The closer
// derives via brace-balancing forward from the opener until
// depth returns to zero. Refactor-safe: arg or body reordering
// does not break the closer detection as long as braces stay
// balanced.
//
// docsRegex anchoring: line 621 carries the fn-range cite plus
// two trailing single-line cites in the same bullet's daemon_error
// (`main.rs:3954`) and unexpected_response (`main.rs:3969`)
// parentheticals, and one trailing test-fn cite at the end
// (`main.rs:5291`). The regex anchors on the unique prefix
// "The recognised slugs from `intents_resume_error_code` at
// `main.rs:N-M`" so first-match capture targets only the
// fn-range cite, per the IPC docs pin collision anchor feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const fnName = "intents_resume_error_code";

const docsRegex =
  /The recognised slugs from `intents_resume_error_code` at `main\.rs:(\d+)-(\d+)`/;
const docsLabel =
  "intents_resume_error_code fn-range citation";
const docsTemplate =
  "The recognised slugs from `intents_resume_error_code` at `main.rs:N-M`";

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
    const match = lines[index].match(/^fn\s+(\w+)\s*\(/);
    if (match && match[1] === fnName) {
      openerMatches.push(index + 1);
    }
  }
  if (openerMatches.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 top-level "fn ${fnName}" but found ${openerMatches.length}; remediation: confirm the slug classifier is a single top-level helper, not renamed or duplicated`,
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
        `${sourcePath}: could not locate the closing brace for fn ${fnName} opened at line ${openerLine}; remediation: confirm the fn body has balanced braces`,
      );
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the slug classifier fn range`,
    );
  } else if (openerLine !== null && closerLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== openerLine || citedEnd !== closerLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but fn ${fnName} lives at :${openerLine}-${closerLine}; remediation: update the citation to :${openerLine}-${closerLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-intents-resume-error-code-fn-range-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-error-code-fn-range-line-refs: ok (fn ${fnName} main.rs:${openerLine}-${closerLine})`,
);
