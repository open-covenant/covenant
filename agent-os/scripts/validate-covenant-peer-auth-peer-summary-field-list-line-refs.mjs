#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-peer-auth PeerSummary documented field list drift guard.
// docs/ipc-and-http-gateway.md documents the inner PeerSummary
// shape immediately after the peer_list envelope (lines ~300-305)
// with a bulleted list pinning four documented field names —
// `agent_id`, `token_prefix`, `registered_at`, `revoked_at` — and
// cites the struct at
// `agent-os/crates/covenant-peer-auth/src/lib.rs:140`.
// The :140 struct line is already pinned by
// validate-covenant-peer-auth-struct-line-refs.mjs, but the four
// documented field names are not bound to source: a rename or
// removal of any field would silently invalidate the docs prose.
// The field list is part of the public peer-roster contract — JSON
// consumers route on these field names; `token_prefix` in particular
// is the same value `peers revoke <token-prefix>` accepts and the
// `revoked_at` field is the only signal of tombstoning.
//
// This validator asserts:
//   1. Each of the four documented field names exists as a
//      `pub <field>:` declaration inside the brace-balanced
//      PeerSummary struct body in covenant-peer-auth/src/lib.rs.
//   2. The docs prose contains each documented field name as a
//      bullet header (`- \`<field>\` (`type`)`) so the list cannot
//      be quietly paraphrased.
//
// Convention: the struct body is located by brace-balance from the
// `pub struct PeerSummary {` opener, so field-name collisions across
// different structs do not contaminate the lookup.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-peer-auth/src/lib.rs";

const expectedFields = [
  "agent_id",
  "token_prefix",
  "registered_at",
  "revoked_at",
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
    if (/^pub\s+struct\s+PeerSummary\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub struct PeerSummary" at top level but found ${openers.length}; remediation: confirm the PeerSummary struct exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    structStart = openers[0];
    structEnd = scanBraceBalance(lines, structStart);
    if (structEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub struct PeerSummary" starting at line ${structStart}; remediation: confirm the struct body is brace-balanced`,
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
            `${sourcePath}: expected exactly 1 "pub ${field}:" inside the PeerSummary struct (lines ${structStart}-${structEnd}) but found ${matches.length}; remediation: the docs cite \`${field}\` as a documented PeerSummary field; confirm the field exists in source or update both docs and source together`,
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
        `${docsPath}: missing the PeerSummary documented bullet header for field \`${field}\`; remediation: restore the docs bullet that records the type and contract for ${field}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-peer-auth-peer-summary-field-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-peer-auth-peer-summary-field-list-line-refs: ok (PeerSummary lib.rs:${structStart}-${structEnd}, fields pinned: ${expectedFields.join(", ")})`,
);
