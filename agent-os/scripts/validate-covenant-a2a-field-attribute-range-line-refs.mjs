#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-a2a field #[serde] attribute pair range line-ref drift guard.
// docs/ipc-and-http-gateway.md cites five attribute-pair ranges in
// agent-os/crates/covenant-a2a/src/lib.rs that span a contiguous
// `#[...]` attribute block through the immediately-following pub field
// declaration:
//
//   - A2ATaskQueueEntry.lease_id          at :135-136 (docs line 448)
//   - A2ATaskQueueEntry.attempt           at :141-142 (docs line 451)
//   - A2ATaskResult.error_message         at :392-393 (docs line 469)
//   - A2AAutoRetryReport.requeued         at :291-292 (docs line 486)
//   - A2AAutoRetrySkipped.lease_age_ms    at :275-276 (docs line 509)
//
// Field name `attempt` collides between A2ATaskQueueEntry (line 142),
// A2AAutoRetrySkipped (line 274), and A2AAutoRetryRequeued (line 283),
// so each target is struct-scoped: find `pub struct <Name> {`, walk
// forward with brace-balance scan to find the matching closing brace,
// then locate the field declaration inside that span. From the field
// line, walk backwards while the prior line is a `#[...]` attribute;
// the first attribute line is the range start.
//
// The first attribute line's literal text is pinned per target so a
// refactor that adds or removes an attribute is surfaced rather than
// letting the range silently re-anchor on different code.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-a2a/src/lib.rs";

const targets = [
  {
    structName: "A2ATaskQueueEntry",
    fieldName: "lease_id",
    expectedTopAttribute:
      '#[serde(default, skip_serializing_if = "Option::is_none")]',
    docsRegex:
      /`lease_id` \(string, omitted when null\)[^\n]*Carries `#\[serde\(default, skip_serializing_if = "Option::is_none"\)\]` at `covenant-a2a\/src\/lib\.rs:(\d+)-(\d+)`/,
    docsLabel: "A2ATaskQueueEntry.lease_id #[serde] range citation",
    docsTemplate:
      "`lease_id` (string, omitted when null) ... Carries `#[serde(default, skip_serializing_if = \"Option::is_none\")]` at `covenant-a2a/src/lib.rs:N-M`",
    startLine: null,
    endLine: null,
  },
  {
    structName: "A2ATaskQueueEntry",
    fieldName: "attempt",
    expectedTopAttribute: "#[serde(default)]",
    docsRegex:
      /`attempt` \(u32\) — delivery attempt counter \(always emitted; `0` for a fresh queue entry per `#\[serde\(default\)\]` at `covenant-a2a\/src\/lib\.rs:(\d+)-(\d+)`\)/,
    docsLabel: "A2ATaskQueueEntry.attempt #[serde(default)] range citation",
    docsTemplate:
      "`attempt` (u32) — delivery attempt counter (always emitted; `0` for a fresh queue entry per `#[serde(default)]` at `covenant-a2a/src/lib.rs:N-M`)",
    startLine: null,
    endLine: null,
  },
  {
    structName: "A2ATaskResult",
    fieldName: "error_message",
    expectedTopAttribute:
      '#[serde(default, skip_serializing_if = "Option::is_none")]',
    docsRegex:
      /`error_message` \(string, omitted when null\)[^\n]*`skip_serializing_if = "Option::is_none"` per `covenant-a2a\/src\/lib\.rs:(\d+)-(\d+)`/,
    docsLabel: "A2ATaskResult.error_message skip_serializing_if range citation",
    docsTemplate:
      "`error_message` (string, omitted when null) ... `skip_serializing_if = \"Option::is_none\"` per `covenant-a2a/src/lib.rs:N-M`",
    startLine: null,
    endLine: null,
  },
  {
    structName: "A2AAutoRetryReport",
    fieldName: "requeued",
    expectedTopAttribute: "#[serde(default)]",
    docsRegex:
      /`requeued` \(array of `A2AAutoRetryRequeued`\)[^\n]*Carries `#\[serde\(default\)\]` on the deserialization side \(`covenant-a2a\/src\/lib\.rs:(\d+)-(\d+)`\)/,
    docsLabel: "A2AAutoRetryReport.requeued #[serde(default)] range citation",
    docsTemplate:
      "`requeued` (array of `A2AAutoRetryRequeued`) ... Carries `#[serde(default)]` on the deserialization side (`covenant-a2a/src/lib.rs:N-M`)",
    startLine: null,
    endLine: null,
  },
  {
    structName: "A2AAutoRetrySkipped",
    fieldName: "lease_age_ms",
    expectedTopAttribute:
      '#[serde(default, skip_serializing_if = "Option::is_none")]',
    docsRegex:
      /`lease_age_ms` \(u64, omitted when null\)[^\n]*Carries `#\[serde\(default, skip_serializing_if = "Option::is_none"\)\]` at `covenant-a2a\/src\/lib\.rs:(\d+)-(\d+)`/,
    docsLabel: "A2AAutoRetrySkipped.lease_age_ms #[serde] range citation",
    docsTemplate:
      "`lease_age_ms` (u64, omitted when null) ... Carries `#[serde(default, skip_serializing_if = \"Option::is_none\")]` at `covenant-a2a/src/lib.rs:N-M`",
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
          `${docsPath}: the ${target.docsLabel} cites covenant-a2a/src/lib.rs:${citedStart}-${citedEnd} but the attribute range for ${target.structName}.${target.fieldName} is :${target.startLine}-${target.endLine}; remediation: update the citation to :${target.startLine}-${target.endLine}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-a2a-field-attribute-range-line-refs: failed");
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
console.log(`validate-covenant-a2a-field-attribute-range-line-refs: ok (${summary})`);
