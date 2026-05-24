#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume --json exit-code coupling line-ref drift guard.
// docs/ipc-and-http-gateway.md line 624 carries three main.rs
// cites in one sentence describing the `--json` exit(1) call
// sites for the three error branches of `covenant intents resume`.
// Each cite must point at an actual `std::process::exit(1);` call
// in the CLI arm; main.rs has 14 exit(1) call sites file-wide
// (verified by grep -nF), so the validator scopes by anchoring on
// the unique args/literal that precedes each target exit(1) and
// walks forward a bounded window to the call.
//
// Three branches:
//   1. missing/conflicting/invalid intent_id (no daemon round-trip
//      reached): the call args trim to
//      `mode, None, code, &message` — the only `None`-second-arg
//      call to intents_resume_error_json in the inline-emission
//      region. exit(1) follows within 3 lines.
//   2. Response::Error daemon_error branch: the slug literal
//      `"daemon_error",` appears 3 times in main.rs (once inline
//      + twice in test bodies); the validator scopes to lines
//      before the `mod tests {` boundary as the sibling
//      validate-intents-resume-daemon-error-slug-line-refs.mjs
//      does, then walks forward 4 lines to exit(1).
//   3. Other-variant unexpected_response branch: the slug literal
//      `"unexpected_response",` appears once total; exit(1)
//      follows within 4 lines.
//
// Source-order ordering (3909 < 3958 < 3973 in current main.rs)
// is verified so a future docs re-ordering that decouples the
// triple from source-code order surfaces immediately.
//
// docsRegex anchoring: line 624 starts with **Exit-code coupling**
// and contains the ordered triple in one parenthetical
// `(\`main.rs:N1\`, \`main.rs:N2\`, \`main.rs:N3\`)` after the
// `exits \`1\` on every error branch` phrase. The regex captures
// all three numbers in order from this single sentence.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testsBoundary = "mod tests {";
const exitSelector = "std::process::exit(1);";
const exitWindowLines = 6;

const anchors = [
  {
    label: "missing_intent_id branch",
    selector: "mode, None, code, &message",
    scopeBeforeTests: false,
  },
  {
    label: "daemon_error branch",
    selector: '"daemon_error",',
    scopeBeforeTests: true,
  },
  {
    label: "unexpected_response branch",
    selector: '"unexpected_response",',
    scopeBeforeTests: false,
  },
];

const docsRegex =
  /exits `1` on every error branch \(`main\.rs:(\d+)`, `main\.rs:(\d+)`, `main\.rs:(\d+)`\)/;
const docsLabel = "intents_resume --json exit-code coupling triple citation";
const docsTemplate =
  "exits `1` on every error branch (`main.rs:N1`, `main.rs:N2`, `main.rs:N3`)";

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

const derivedExitLines = [];
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
  for (const anchor of anchors) {
    const upperBound = anchor.scopeBeforeTests && modTestsIndex >= 0
      ? modTestsIndex
      : lines.length;
    const candidates = [];
    for (let index = 0; index < upperBound; index += 1) {
      if (lines[index].trim() === anchor.selector) {
        candidates.push(index + 1);
      }
    }
    if (candidates.length !== 1) {
      const scopeNote = anchor.scopeBeforeTests
        ? ` before \`${testsBoundary}\` (line ${modTestsIndex + 1})`
        : "";
      fail(
        `${sourcePath}: expected exactly 1 occurrence of \`${anchor.selector}\` (${anchor.label})${scopeNote} but found ${candidates.length}; remediation: confirm the ${anchor.label} anchor is present exactly once on the inline emission path`,
      );
      derivedExitLines.push(null);
      continue;
    }
    const anchorLine = candidates[0];
    let exitLine = null;
    for (let offset = 1; offset <= exitWindowLines; offset += 1) {
      const targetLine = anchorLine + offset;
      if (targetLine > upperBound) {
        break;
      }
      if (lines[targetLine - 1].trim() === exitSelector) {
        exitLine = targetLine;
        break;
      }
    }
    if (exitLine === null) {
      fail(
        `${sourcePath}:${anchorLine}: no \`${exitSelector}\` within ${exitWindowLines} lines after the ${anchor.label} anchor; remediation: confirm the --json exit path still calls std::process::exit(1) within a short window of the intents_resume_error_json call`,
      );
      derivedExitLines.push(null);
      continue;
    }
    derivedExitLines.push(exitLine);
  }
  const validLines = derivedExitLines.filter((line) => line !== null);
  if (validLines.length === derivedExitLines.length) {
    for (let index = 1; index < validLines.length; index += 1) {
      if (validLines[index] <= validLines[index - 1]) {
        fail(
          `${sourcePath}: exit(1) call sites must be in ascending source order but got [${validLines.join(", ")}]; remediation: the docs sentence and source-code branch order must agree`,
        );
        break;
      }
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the three --json exit(1) call sites`,
    );
  } else if (
    derivedExitLines.length === anchors.length &&
    derivedExitLines.every((line) => line !== null)
  ) {
    const cited = [
      parseInt(match[1], 10),
      parseInt(match[2], 10),
      parseInt(match[3], 10),
    ];
    for (let index = 0; index < cited.length; index += 1) {
      if (cited[index] !== derivedExitLines[index]) {
        fail(
          `${docsPath}: the ${docsLabel} cite #${index + 1} (${anchors[index].label}) is main.rs:${cited[index]} but the exit(1) call lives at :${derivedExitLines[index]}; remediation: update the citation to :${derivedExitLines[index]}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-intents-resume-exit-code-coupling-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-exit-code-coupling-line-refs: ok (missing_intent_id main.rs:${derivedExitLines[0]}, daemon_error main.rs:${derivedExitLines[1]}, unexpected_response main.rs:${derivedExitLines[2]})`,
);
