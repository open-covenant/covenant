#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume CLI verb wiring range line-ref drift guard.
// docs/ipc-and-http-gateway.md line 628 cites a main.rs range
// for the `"intents" => {` match arm in the CLI dispatch table —
// the structural anchor that documents where the
// `covenant intents resume` verb runs. The cite range is the
// opener line (the `"intents" => {` statement) through the
// matching closing brace at the same indentation depth (the
// arm-body terminator before the next `"ignore" => {` arm).
//
// Selector form: the opener `"intents" => {` (at the leading
// indentation matching other CLI verb arms) is unique in
// main.rs as a verb-arm opener. The closer is derived by
// brace-balancing forward from the opener until the depth
// returns to zero. Brace counting is **string-aware** because
// the arm body contains format-string placeholders like
// `bail!("unexpected response: {other:?}")` that a naive
// counter would miscount, throwing off the closer detection.
// The counter respects `"..."` string literals and `\"` escapes;
// raw strings (`r"..."`) and char literals are not used in this
// arm, so they are not handled.
//
// docsRegex anchoring: multiple envelope blocks in the doc
// start with "The CLI verb is wired at `main.rs:N-M`", so the
// prefix alone is not unique. The regex anchors on the
// intents_resume-specific continuation "; without `--json`, the
// success branch prints the result" — distinct from the
// ignore_report "matched case prints" wording at line 587
// (already pinned) per the IPC docs pin collision anchor
// feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const opener = '"intents" => {';

const docsRegex =
  /The CLI verb is wired at `main\.rs:(\d+)-(\d+)`; without `--json`, the success branch prints the result/;
const docsLabel =
  "intents_resume CLI verb wiring range citation";
const docsTemplate =
  "The CLI verb is wired at `main.rs:N-M`; without `--json`, the success branch prints the result";

function countBracesStringAware(line) {
  let inString = false;
  let escape = false;
  let opens = 0;
  let closes = 0;
  for (let index = 0; index < line.length; index += 1) {
    const ch = line[index];
    if (escape) {
      escape = false;
      continue;
    }
    if (ch === "\\") {
      escape = true;
      continue;
    }
    if (ch === '"') {
      inString = !inString;
      continue;
    }
    if (inString) continue;
    if (ch === "{") opens += 1;
    else if (ch === "}") closes += 1;
  }
  return { opens, closes };
}

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
      `${sourcePath}: expected exactly 1 occurrence of \`${opener}\` (verb-arm opener) but found ${openerMatches.length}; remediation: confirm the intents CLI verb is wired through exactly one match arm with this exact opener`,
    );
  } else {
    openerLine = openerMatches[0];
    let depth = 0;
    for (let index = openerLine - 1; index < lines.length; index += 1) {
      const { opens, closes } = countBracesStringAware(lines[index]);
      depth += opens;
      if (closes > 0) {
        for (let c = 0; c < closes; c += 1) {
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
        `${sourcePath}: could not locate the closing brace for the \`${opener}\` arm opened at line ${openerLine}; remediation: confirm the arm body has balanced braces (string-aware count)`,
      );
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the intents CLI verb wiring range`,
    );
  } else if (openerLine !== null && closerLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== openerLine || citedEnd !== closerLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the \`"intents" => { ... }\` arm lives at :${openerLine}-${closerLine}; remediation: update the citation to :${openerLine}-${closerLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-intents-resume-cli-wiring-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-cli-wiring-line-refs: ok (CLI verb arm main.rs:${openerLine}-${closerLine})`,
);
