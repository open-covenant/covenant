#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-a2a enum block range line-ref drift guard.
// docs/ipc-and-http-gateway.md cites three enum block ranges in
// agent-os/crates/covenant-a2a/src/lib.rs as `:N-M` range citations:
//
//   - A2ATaskStatus           at :40-46  (docs line 467)
//   - A2ATaskQueueState       at :124-129 (docs line 440)
//   - A2AAutoRetrySkipReason  at :240-252 (docs line 511)
//
// Each range spans the contiguous `#[...]` attribute block (typically
// `#[derive(...)]` then `#[serde(rename_all = "snake_case")]`), through
// the `pub enum <Name> {` opener, the variants, and the matching
// closing brace.
//
// The existing covenant-a2a validators do not cover this convention:
// the struct validator only matches `pub struct` lines, and the
// field-attribute-range validator's ranges terminate at a `pub
// <field>:` declaration rather than a closing brace. This validator
// follows the convention already established by
// validate-covenant-types-enum-block-range-line-refs.mjs, generalised
// to three targets.
//
// For each target, find the `pub enum <Name>` line, walk backwards
// while the previous line starts with `#[` to find the range start,
// then walk forward from the enum opener with brace-balance scan until
// depth returns to 0 to find the range end. The expected `#[serde]`
// annotation must exist somewhere in the attribute block so a
// rename_all change surfaces rather than silently re-anchoring on
// different code.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-a2a/src/lib.rs";

const targets = [
  {
    name: "A2ATaskStatus",
    expectedSerdeAttribute: '#[serde(rename_all = "snake_case")]',
    docsRegex:
      /\(snake_case per `covenant-a2a\/src\/lib\.rs:(\d+)-(\d+)`\)/,
    docsLabel: "A2ATaskStatus enum block range citation",
    docsTemplate:
      "(snake_case per `covenant-a2a/src/lib.rs:N-M`)",
    startLine: null,
    endLine: null,
  },
  {
    name: "A2ATaskQueueState",
    expectedSerdeAttribute: '#[serde(rename_all = "snake_case")]',
    docsRegex:
      /per `A2ATaskQueueState`'s `#\[serde\(rename_all = "snake_case"\)\]` at `covenant-a2a\/src\/lib\.rs:(\d+)-(\d+)`/,
    docsLabel: "A2ATaskQueueState enum block range citation",
    docsTemplate:
      "per `A2ATaskQueueState`'s `#[serde(rename_all = \"snake_case\")]` at `covenant-a2a/src/lib.rs:N-M`",
    startLine: null,
    endLine: null,
  },
  {
    name: "A2AAutoRetrySkipReason",
    expectedSerdeAttribute: '#[serde(rename_all = "snake_case")]',
    docsRegex:
      /`A2AAutoRetrySkipReason` enumerates exactly these nine snake_case slugs \(per `covenant-a2a\/src\/lib\.rs:(\d+)-(\d+)`\)/,
    docsLabel: "A2AAutoRetrySkipReason enum block range citation",
    docsTemplate:
      "`A2AAutoRetrySkipReason` enumerates exactly these nine snake_case slugs (per `covenant-a2a/src/lib.rs:N-M`)",
    startLine: null,
    endLine: null,
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

if (source) {
  const lines = source.split("\n");
  for (const target of targets) {
    const openers = [];
    for (let index = 0; index < lines.length; index += 1) {
      if (new RegExp(`^pub\\s+enum\\s+${target.name}\\b`).test(lines[index])) {
        openers.push(index + 1);
      }
    }
    if (openers.length !== 1) {
      fail(
        `${sourcePath}: expected exactly 1 "pub enum ${target.name}" at top level but found ${openers.length}; remediation: confirm the ${target.name} enum exists at top level and is not renamed, moved, or duplicated`,
      );
      continue;
    }
    const enumLine = openers[0];
    let startLine = enumLine;
    for (let index = enumLine - 2; index >= 0; index -= 1) {
      const trimmed = lines[index].trim();
      if (trimmed.startsWith("#[")) {
        startLine = index + 1;
      } else {
        break;
      }
    }
    if (startLine === enumLine) {
      fail(
        `${sourcePath}:${enumLine}: expected one or more "#[...]" attribute lines immediately above "pub enum ${target.name}", but the preceding line is not an attribute; remediation: restore the attribute block above the enum declaration`,
      );
      continue;
    }
    const attributeBlock = lines.slice(startLine - 1, enumLine - 1).map((line) => line.trim());
    if (!attributeBlock.includes(target.expectedSerdeAttribute)) {
      fail(
        `${sourcePath}: the attribute block for "pub enum ${target.name}" (lines ${startLine}-${enumLine - 1}) does not contain the expected \`${target.expectedSerdeAttribute}\`; remediation: confirm the rename_all variant matches what the docs cite, or update both source and docs together`,
      );
      continue;
    }
    let depth = 0;
    let opened = false;
    let endLine = null;
    for (let index = enumLine - 1; index < lines.length; index += 1) {
      for (const char of lines[index]) {
        if (char === "{") {
          depth += 1;
          opened = true;
        } else if (char === "}") {
          depth -= 1;
        }
      }
      if (opened && depth === 0) {
        endLine = index + 1;
        break;
      }
    }
    if (endLine === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub enum ${target.name}" starting at line ${enumLine}; remediation: confirm the enum body is brace-balanced`,
      );
      continue;
    }
    target.startLine = startLine;
    target.endLine = endLine;
  }
}

if (docs) {
  for (const target of targets) {
    const match = docs.match(target.docsRegex);
    if (!match) {
      fail(
        `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the ${target.name} enum block range`,
      );
      continue;
    }
    if (target.startLine !== null && target.endLine !== null) {
      const citedStart = parseInt(match[1], 10);
      const citedEnd = parseInt(match[2], 10);
      if (citedStart !== target.startLine || citedEnd !== target.endLine) {
        fail(
          `${docsPath}: the ${target.docsLabel} cites covenant-a2a/src/lib.rs:${citedStart}-${citedEnd} but the ${target.name} enum block spans :${target.startLine}-${target.endLine}; remediation: update the citation to :${target.startLine}-${target.endLine}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-a2a-enum-block-range-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const summary = targets
  .map((target) => `${target.name} :${target.startLine}-${target.endLine}`)
  .join(", ");
console.log(
  `validate-covenant-a2a-enum-block-range-line-refs: ok (${summary})`,
);
