#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-ipc field #[serde] attribute pair range line-ref drift guard.
// docs/ipc-and-http-gateway.md line 132 cites the VerifyDrift.id
// attribute pair range in agent-os/crates/covenant-ipc/src/lib.rs:
//
//   `Serialized via #[serde(default, skip_serializing_if =
//    "Option::is_none")] at covenant-ipc/src/lib.rs:35-36`
//
// The range spans the contiguous `#[serde(...)]` attribute line
// (line 35) through the immediately-following `pub id: Option<String>`
// declaration (line 36).
//
// Convention: mirrors validate-covenant-types-field-attribute-range-line-refs.mjs
// and validate-covenant-a2a-field-attribute-range-line-refs.mjs. The
// `id` field is struct-scoped via brace-balanced lookup so a future
// `pub id:` added to a different struct in covenant-ipc/src/lib.rs
// cannot collide with this pin.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-ipc/src/lib.rs";

const targets = [
  {
    structName: "VerifyDrift",
    fieldName: "id",
    expectedTopAttribute:
      '#[serde(default, skip_serializing_if = "Option::is_none")]',
    docsRegex:
      /Serialized via `#\[serde\(default, skip_serializing_if = "Option::is_none"\)\]` at `covenant-ipc\/src\/lib\.rs:(\d+)-(\d+)`/,
    docsLabel: "VerifyDrift.id #[serde] range citation",
    docsTemplate:
      "Serialized via `#[serde(default, skip_serializing_if = \"Option::is_none\")]` at `covenant-ipc/src/lib.rs:N-M`",
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
  for (const target of targets) {
    const structOpenerRegex = new RegExp(`^pub\\s+struct\\s+${target.structName}\\b`);
    const structOpeners = [];
    for (let index = 0; index < lines.length; index += 1) {
      if (structOpenerRegex.test(lines[index])) {
        structOpeners.push(index + 1);
      }
    }
    if (structOpeners.length !== 1) {
      fail(
        `${sourcePath}: expected exactly 1 "pub struct ${target.structName}" at top level but found ${structOpeners.length}; remediation: confirm the ${target.structName} struct exists at top level`,
      );
      continue;
    }
    const structStart = structOpeners[0];
    const structEnd = scanBraceBalance(lines, structStart);
    if (structEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub struct ${target.structName}" starting at line ${structStart}; remediation: confirm the struct body is brace-balanced`,
      );
      continue;
    }
    const fieldDeclRegex = new RegExp(`^\\s*pub\\s+${target.fieldName}\\s*:`);
    const fieldMatches = [];
    for (let index = structStart; index < structEnd; index += 1) {
      if (fieldDeclRegex.test(lines[index])) {
        fieldMatches.push(index + 1);
      }
    }
    if (fieldMatches.length !== 1) {
      fail(
        `${sourcePath}: expected exactly 1 "pub ${target.fieldName}:" inside the ${target.structName} struct (lines ${structStart}-${structEnd}) but found ${fieldMatches.length}; remediation: confirm the field exists and is not renamed or duplicated`,
      );
      continue;
    }
    const fieldLine = fieldMatches[0];
    let attrStart = fieldLine;
    for (let index = fieldLine - 2; index >= 0; index -= 1) {
      const trimmed = lines[index].trim();
      if (trimmed.startsWith("#[")) {
        attrStart = index + 1;
      } else {
        break;
      }
    }
    if (attrStart === fieldLine) {
      fail(
        `${sourcePath}:${fieldLine}: expected one or more "#[...]" attribute lines immediately above "pub ${target.fieldName}:" in ${target.structName}, but the preceding line is not an attribute; remediation: restore the attribute block above the field declaration`,
      );
      continue;
    }
    const topAttribute = lines[attrStart - 1].trim();
    if (topAttribute !== target.expectedTopAttribute) {
      fail(
        `${sourcePath}:${attrStart}: expected the first attribute above ${target.structName}.${target.fieldName} to be exactly \`${target.expectedTopAttribute}\` but found \`${topAttribute}\`; remediation: restore the literal annotation, or update both source and docs together if the wire format changed`,
      );
      continue;
    }
    target.startLine = attrStart;
    target.endLine = fieldLine;
  }
}

if (docs) {
  for (const target of targets) {
    const match = docs.match(target.docsRegex);
    if (!match) {
      fail(
        `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the ${target.structName}.${target.fieldName} attribute pair range`,
      );
      continue;
    }
    if (target.startLine !== null && target.endLine !== null) {
      const citedStart = parseInt(match[1], 10);
      const citedEnd = parseInt(match[2], 10);
      if (citedStart !== target.startLine || citedEnd !== target.endLine) {
        fail(
          `${docsPath}: the ${target.docsLabel} cites covenant-ipc/src/lib.rs:${citedStart}-${citedEnd} but ${target.structName}.${target.fieldName} spans :${target.startLine}-${target.endLine}; remediation: update the citation to :${target.startLine}-${target.endLine}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-ipc-field-attribute-range-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const summary = targets
  .map((target) => `${target.structName}.${target.fieldName} lib.rs:${target.startLine}-${target.endLine}`)
  .join(", ");
console.log(
  `validate-covenant-ipc-field-attribute-range-line-refs: ok (${summary})`,
);
