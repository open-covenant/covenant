#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-audit AuditIntegrityReport documented field list drift
// guard.
// docs/ipc-and-http-gateway.md documents the inner
// AuditIntegrityReport shape after the audit_integrity envelope
// (lines ~386-392) with a bulleted list pinning five documented
// field names — `events`, `anchors`, `valid`, `root_hash_hex`,
// `failures` — and cites the struct at
// `agent-os/crates/covenant-audit/src/lib.rs:61`.
// The :61 struct line is already pinned by
// validate-covenant-audit-struct-line-refs.mjs, but the five
// documented field names are not bound to source. A rename or
// removal of any field would silently invalidate the docs prose.
// The field list is part of the public audit-integrity contract —
// JSON consumers route on these field names; `valid` is the primary
// success signal and `root_hash_hex` is the chain-root attestation
// JSON consumers compare against the audit-root receipt.
//
// This validator asserts:
//   1. Each of the five documented field names exists as a
//      `pub <field>:` declaration inside the brace-balanced
//      AuditIntegrityReport struct body in covenant-audit/src/lib.rs.
//   2. The docs prose contains each documented field name as a
//      bullet header (`- \`<field>\` (`type`)`) so the list cannot
//      be quietly paraphrased.
//
// Convention: the struct body is located by brace-balance from the
// `pub struct AuditIntegrityReport {` opener, so field-name
// collisions across different structs do not contaminate the lookup.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-audit/src/lib.rs";

const expectedFields = [
  "events",
  "anchors",
  "valid",
  "root_hash_hex",
  "failures",
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

let structStart = null;
let structEnd = null;
if (source) {
  const lines = source.split("\n");
  const openers = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^pub\s+struct\s+AuditIntegrityReport\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub struct AuditIntegrityReport" at top level but found ${openers.length}; remediation: confirm the AuditIntegrityReport struct exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    structStart = openers[0];
    structEnd = scanBraceBalance(lines, structStart);
    if (structEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub struct AuditIntegrityReport" starting at line ${structStart}; remediation: confirm the struct body is brace-balanced`,
      );
    } else {
      for (const field of expectedFields) {
        const fieldRegex = new RegExp(`^\\s*pub\\s+${field}\\s*:`);
        const matches = [];
        for (let index = structStart; index < structEnd; index += 1) {
          if (fieldRegex.test(lines[index])) {
            matches.push(index + 1);
          }
        }
        if (matches.length !== 1) {
          fail(
            `${sourcePath}: expected exactly 1 "pub ${field}:" inside the AuditIntegrityReport struct (lines ${structStart}-${structEnd}) but found ${matches.length}; remediation: the docs cite \`${field}\` as a documented AuditIntegrityReport field; confirm the field exists in source or update both docs and source together`,
          );
        }
      }
    }
  }
}

if (docs) {
  for (const field of expectedFields) {
    const fieldRegex = new RegExp(`^- \`${field}\` \\(`, "m");
    if (!fieldRegex.test(docs)) {
      fail(
        `${docsPath}: missing the AuditIntegrityReport documented bullet header for field \`${field}\`; remediation: restore the docs bullet that records the type and contract for ${field}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-audit-integrity-report-field-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-audit-integrity-report-field-list-line-refs: ok (AuditIntegrityReport lib.rs:${structStart}-${structEnd}, fields pinned: ${expectedFields.join(", ")})`,
);
