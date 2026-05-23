#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// capability_grant envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites two single main.rs lines inside
// capability_grant_json_pins_top_level_schema:
//
//   - line 263 cites the `scope` (object or null) type pin.
//   - line 264 cites the `expires_at` (u64 or null) type pin.
//
// The existing validate-capability-grant-line-refs.mjs covers the
// helper fn, renders test, and pins test declaration lines, but not
// the inner type-level selector lines. Both cites were stale by ~220
// lines before this validator landed (scope was 5783 in docs while the
// real selector was at 6005; expires_at was 5787 while the real
// selector was at 6009).
//
// The validator scopes each lookup to the brace-balanced
// `capability_grant_json_pins_top_level_schema` fn body so a same-named
// selector inside a different envelope's pins test cannot contaminate
// the result. Each cite is a single-line convention.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "capability_grant_json_pins_top_level_schema";

const targets = [
  {
    field: "scope",
    selector: 'value["scope"].is_object() || value["scope"].is_null(),',
    docsRegex:
      /- `scope` \(object or null\): the structured scope object echoed from the request, or `null` when `--scope` was omitted\. Pinned at the type level by the schema test \(`main\.rs:(\d+)`\) — JSON consumers must never receive a string blob here, so a scope value of `"\{\\"version\\":1\}"` would be a contract break\./,
    docsLabel: "capability_grant.scope type-level pin citation",
    docsTemplate:
      "Pinned at the type level by the schema test (`main.rs:N`) — JSON consumers must never receive a string blob here",
  },
  {
    field: "expires_at",
    selector:
      'value["expires_at"].is_u64() || value["expires_at"].is_null(),',
    docsRegex:
      /- `expires_at` \(u64 or null\): the Unix-epoch millisecond expiry echoed from `--expires-at`, or `null` when the flag was omitted\. Pinned at the type level by the schema test \(`main\.rs:(\d+)`\) — JSON consumers must never receive a string here, so a value of `"1700000000000"` would be a contract break\./,
    docsLabel: "capability_grant.expires_at type-level pin citation",
    docsTemplate:
      "Pinned at the type level by the schema test (`main.rs:N`) — JSON consumers must never receive a string here",
  },
];

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the capability_grant pins-test still exists and is not renamed or duplicated`,
    );
  } else {
    const testStart = testMatches[0];
    const testEnd = scanBraceBalance(lines, testStart);
    if (testEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "fn ${testFnName}" starting at line ${testStart}; remediation: confirm the test fn body is brace-balanced`,
      );
    } else {
      for (const target of targets) {
        const selectorMatches = [];
        for (let index = testStart; index < testEnd; index += 1) {
          if (lines[index].trim() === target.selector) {
            selectorMatches.push(index + 1);
          }
        }
        if (selectorMatches.length !== 1) {
          fail(
            `${sourcePath}: expected exactly 1 occurrence of \`${target.selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the ${target.field} type-level assertion is present exactly once in this test`,
          );
          continue;
        }
        target.selectorLine = selectorMatches[0];
      }
    }
  }
}

if (docs) {
  for (const target of targets) {
    const match = docs.match(target.docsRegex);
    if (!match) {
      fail(
        `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the ${target.field} type-level pin line ref`,
      );
      continue;
    }
    if (target.selectorLine !== undefined) {
      const cited = parseInt(match[1], 10);
      if (cited !== target.selectorLine) {
        fail(
          `${docsPath}: the ${target.docsLabel} cites main.rs:${cited} but the capability_grant.${target.field} selector is at line ${target.selectorLine}; remediation: update the citation to :${target.selectorLine}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-capability-grant-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-capability-grant-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.selectorLine}`).join(", ")})`,
);
