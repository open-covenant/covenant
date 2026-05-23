#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// capability_list envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 248 cites a single main.rs line for
// the capability_list.limit type-level pin:
//
//   - `limit` (u64): ... Pinned at the type level by the schema test
//     (`main.rs:NNNN`) — JSON consumers must never receive a string here.
//
// The existing validate-capability-list-line-refs.mjs covers the helper
// fn, renders test, and pins test declaration lines, but not the inner
// type-level selector line. The capability_list cite already drifted
// ~222 lines from its prior documented value before this validator
// landed (the line was 5698 in docs while the real selector was at
// 5920), so forward protection is the explicit goal.
//
// The validator scopes its lookup to the brace-balanced
// `capability_list_json_pins_top_level_schema` fn body so a same-named
// selector inside a different envelope's pins test (receipt_list,
// peer_list, etc.) cannot contaminate the result. The cite is a
// single-line convention (the line containing the
// `value["limit"].is_u64(),` selector), not a range.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "capability_list_json_pins_top_level_schema";
const selector = 'value["limit"].is_u64(),';

const docsRegex =
  /- `limit` \(u64\): the request limit echoed back from `-n`\/`--limit` \(default `10`, see `main\.rs:\d+`\)\. Pinned at the type level by the schema test \(`main\.rs:(\d+)`\) — JSON consumers must never receive a string here\./;
const docsLabel = "capability_list.limit type-level pin citation";
const docsTemplate =
  "Pinned at the type level by the schema test (`main.rs:N`) — JSON consumers must never receive a string here.";

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

function scanBraceBalance(lines, openerLine) {
  let depth = 0;
  let opened = false;
  for (let index = openerLine - 1; index < lines.length; index += 1) {
    for (const char of lines[index]) {
      if (char === "{") {
        depth += 1;
        opened = true;
      } else if (char === "}") {
        depth -= 1;
      }
    }
    if (opened && depth === 0) {
      return index + 1;
    }
  }
  return null;
}

let selectorLine = null;
if (source) {
  const lines = source.split("\n");
  const testOpenerRegex = new RegExp(`^\\s+fn\\s+${testFnName}\\s*\\(`);
  const testMatches = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (testOpenerRegex.test(lines[index])) {
      testMatches.push(index + 1);
    }
  }
  if (testMatches.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the capability_list pins-test still exists and is not renamed or duplicated`,
    );
  } else {
    const testStart = testMatches[0];
    const testEnd = scanBraceBalance(lines, testStart);
    if (testEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "fn ${testFnName}" starting at line ${testStart}; remediation: confirm the test fn body is brace-balanced`,
      );
    } else {
      const selectorMatches = [];
      for (let index = testStart; index < testEnd; index += 1) {
        if (lines[index].trim() === selector) {
          selectorMatches.push(index + 1);
        }
      }
      if (selectorMatches.length !== 1) {
        fail(
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the limit type-level assertion is present exactly once in this test`,
        );
      } else {
        selectorLine = selectorMatches[0];
      }
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the capability_list.limit type-level pin line ref`,
    );
  } else if (selectorLine !== null) {
    const cited = parseInt(match[1], 10);
    if (cited !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${cited} but the capability_list.limit selector is at line ${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-capability-list-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-capability-list-type-level-pin-line-refs: ok (capability_list.limit selector main.rs:${selectorLine})`,
);
