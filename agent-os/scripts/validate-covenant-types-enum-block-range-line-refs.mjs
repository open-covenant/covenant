#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-types enum block range line-ref drift guard.
// docs/ipc-and-http-gateway.md line 547 cites the MemoryRepairMode
// enum block in agent-os/crates/covenant-types/src/lib.rs as a range
// citation: `per MemoryRepairMode's #[serde(rename_all = "snake_case")]
// at covenant-types/src/lib.rs:196-201`. The range spans the contiguous
// `#[...]` attribute block (line 196 #[derive], line 197 #[serde]),
// through the `pub enum MemoryRepairMode {` opener (line 198) and the
// variants, ending at the matching closing brace (line 201).
//
// The existing covenant-types validators do not cover this enum-block
// convention: the struct validator only covers `pub struct` lines, the
// AgentId Serialize impl validator covers a single impl line, the
// annotation validator covers single annotation lines, and the
// field-attribute-range validator covers ranges that end at a `pub
// <field>:` declaration rather than a closing brace.
//
// This validator finds the `pub enum MemoryRepairMode` line, walks
// backwards while the previous line starts with `#[` to find the
// range start, then walks forward from the enum opener with
// brace-balance scan until depth returns to 0 to find the range end.
// The expected `#[serde(rename_all = "snake_case")]` annotation must
// exist somewhere in the attribute block so a rename_all variant
// change surfaces.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-types/src/lib.rs";

const target = {
  name: "MemoryRepairMode",
  expectedSerdeAttribute: '#[serde(rename_all = "snake_case")]',
  docsRegex:
    /per `MemoryRepairMode`'s `#\[serde\(rename_all = "snake_case"\)\]` at `covenant-types\/src\/lib\.rs:(\d+)-(\d+)`/,
  docsLabel: "MemoryRepairMode enum block range citation",
  docsTemplate:
    "per `MemoryRepairMode`'s `#[serde(rename_all = \"snake_case\")]` at `covenant-types/src/lib.rs:N-M`",
};

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

let startLine = null;
let endLine = null;
if (source) {
  const lines = source.split("\n");
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
  } else {
    const enumLine = openers[0];
    startLine = enumLine;
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
    } else {
      const attributeBlock = lines.slice(startLine - 1, enumLine - 1).map((line) => line.trim());
      if (!attributeBlock.includes(target.expectedSerdeAttribute)) {
        fail(
          `${sourcePath}: the attribute block for "pub enum ${target.name}" (lines ${startLine}-${enumLine - 1}) does not contain the expected \`${target.expectedSerdeAttribute}\`; remediation: confirm the rename_all variant matches what the docs cite, or update both source and docs together`,
        );
      }
      let depth = 0;
      let opened = false;
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
      }
    }
  }
}

if (docs) {
  const match = docs.match(target.docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the ${target.name} enum block range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${target.docsLabel} cites covenant-types/src/lib.rs:${citedStart}-${citedEnd} but the ${target.name} enum block spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-types-enum-block-range-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-types-enum-block-range-line-refs: ok (${target.name} block lib.rs:${startLine}-${endLine})`,
);
