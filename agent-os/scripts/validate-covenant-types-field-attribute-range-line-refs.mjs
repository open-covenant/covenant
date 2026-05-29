#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-types field #[serde] attribute pair range drift guard.
// docs/ipc-and-http-gateway.md cites two attribute-pair ranges in
// agent-os/crates/covenant-types/src/lib.rs that span from a contiguous
// `#[...]` attribute block through the immediately-following `pub
// <field>:` declaration:
//
//   - SettlementReceipt.memory_record_id at lib.rs:343-344
//     (docs line 207, `#[serde(default, skip_serializing_if = "Option::is_none")]
//     at covenant-types/src/lib.rs:343-344`)
//   - MemoryRecord.parent at lib.rs:192-193
//     (docs line 428, `#[serde(default)] at covenant-types/src/lib.rs:192-193`)
//
// Field name `parent` collides between Intent (line 166) and MemoryRecord
// (line 193) so each target is struct-scoped: find `pub struct <Name> {`,
// walk forward with brace-balance scan to find the matching closing
// brace, then locate the field declaration inside that span. From the
// field line, walk backwards while the prior line is a `#[...]`
// attribute; the first attribute line is the range start.
//
// The first attribute line's literal text is also pinned per target so
// a refactor that inserts or removes an attribute is surfaced rather
// than letting the range silently re-anchor on different code.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-types/src/lib.rs";

const targets = [
  {
    structName: "SettlementReceipt",
    fieldName: "memory_record_id",
    expectedTopAttribute:
      '#[serde(default, skip_serializing_if = "Option::is_none")]',
    docsRegex:
      /Serialized via `#\[serde\(default, skip_serializing_if = "Option::is_none"\)\]` at `covenant-types\/src\/lib\.rs:(\d+)-(\d+)`/,
    docsLabel: "memory_record_id #[serde] range citation",
    docsTemplate:
      "Serialized via `#[serde(default, skip_serializing_if = \"Option::is_none\")]` at `covenant-types/src/lib.rs:N-M`",
    startLine: null,
    endLine: null,
  },
  {
    structName: "MemoryRecord",
    fieldName: "parent",
    expectedTopAttribute: "#[serde(default)]",
    docsRegex:
      /Carries `#\[serde\(default\)\]` at `covenant-types\/src\/lib\.rs:(\d+)-(\d+)` \*\*without\*\* `skip_serializing_if`/,
    docsLabel: "MemoryRecord.parent #[serde(default)] range citation",
    docsTemplate:
      "Carries `#[serde(default)]` at `covenant-types/src/lib.rs:N-M` **without** `skip_serializing_if`",
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

function findStructSpan(lines, structName) {
  const opener = new RegExp(`^pub\\s+struct\\s+${structName}\\b[\\s\\S]*\\{`);
  let startIndex = -1;
  for (let index = 0; index < lines.length; index += 1) {
    if (opener.test(lines[index])) {
      startIndex = index;
      break;
    }
  }
  if (startIndex === -1) {
    return null;
  }
  let depth = 0;
  let opened = false;
  for (let index = startIndex; index < lines.length; index += 1) {
    for (const char of lines[index]) {
      if (char === "{") {
        depth += 1;
        opened = true;
      } else if (char === "}") {
        depth -= 1;
      }
    }
    if (opened && depth === 0) {
      return { startLine: startIndex + 1, endLine: index + 1 };
    }
  }
  return null;
}

if (source) {
  const lines = source.split("\n");
  for (const target of targets) {
    const span = findStructSpan(lines, target.structName);
    if (!span) {
      fail(
        `${sourcePath}: could not find a brace-balanced "pub struct ${target.structName} { ... }" span; remediation: confirm the ${target.structName} struct exists at top level and is brace-balanced`,
      );
      continue;
    }
    const fieldRegex = new RegExp(`^\\s+pub\\s+${target.fieldName}\\s*:`);
    let fieldLine = null;
    for (let index = span.startLine; index < span.endLine; index += 1) {
      if (fieldRegex.test(lines[index])) {
        fieldLine = index + 1;
        break;
      }
    }
    if (fieldLine === null) {
      fail(
        `${sourcePath}: could not find "pub ${target.fieldName}:" inside the ${target.structName} struct (lines ${span.startLine}-${span.endLine}); remediation: confirm the field exists in the expected struct`,
      );
      continue;
    }
    let attributeStartLine = fieldLine;
    for (let index = fieldLine - 2; index >= span.startLine; index -= 1) {
      const trimmed = lines[index].trim();
      if (trimmed.startsWith("#[")) {
        attributeStartLine = index + 1;
      } else {
        break;
      }
    }
    if (attributeStartLine === fieldLine) {
      fail(
        `${sourcePath}:${fieldLine}: expected one or more "#[...]" attribute lines immediately above "pub ${target.fieldName}:" inside ${target.structName}, but the preceding line is not an attribute; remediation: restore the #[serde(...)] annotation above the field`,
      );
      continue;
    }
    const topAttributeText = lines[attributeStartLine - 1].trim();
    if (topAttributeText !== target.expectedTopAttribute) {
      fail(
        `${sourcePath}:${attributeStartLine}: expected the top attribute above "pub ${target.fieldName}:" to be exactly \`${target.expectedTopAttribute}\` but found \`${topAttributeText}\`; remediation: confirm the attribute matches what the docs cite, or update both source and docs`,
      );
      continue;
    }
    target.startLine = attributeStartLine;
    target.endLine = fieldLine;
  }
}

if (docs) {
  for (const target of targets) {
    const match = docs.match(target.docsRegex);
    if (!match) {
      fail(
        `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the ${target.structName}.${target.fieldName} attribute range`,
      );
      continue;
    }
    if (target.startLine !== null && target.endLine !== null) {
      const citedStart = parseInt(match[1], 10);
      const citedEnd = parseInt(match[2], 10);
      if (citedStart !== target.startLine || citedEnd !== target.endLine) {
        fail(
          `${docsPath}: the ${target.docsLabel} cites covenant-types/src/lib.rs:${citedStart}-${citedEnd} but the attribute range for ${target.structName}.${target.fieldName} is :${target.startLine}-${target.endLine}; remediation: update the citation to :${target.startLine}-${target.endLine}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-types-field-attribute-range-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const summary = targets
  .map(
    (t) =>
      `${t.structName}.${t.fieldName} lib.rs:${t.startLine}-${t.endLine}`,
  )
  .join(", ");
console.log(`validate-covenant-types-field-attribute-range-line-refs: ok (${summary})`);
