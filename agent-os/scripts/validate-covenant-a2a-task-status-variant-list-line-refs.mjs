#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-a2a A2ATaskStatus documented variant list drift guard.
// docs/ipc-and-http-gateway.md line 467 cites the A2ATaskStatus slug
// space as ``A2ATaskStatus` slug, exactly one of `"ok"`, `"error"`,
// `"partial"` (snake_case per `covenant-a2a/src/lib.rs:40-46`)`. The
// :40-46 enum block range and the `#[serde(rename_all = "snake_case")]`
// attribute are already pinned by
// validate-covenant-a2a-enum-block-range-line-refs.mjs, but the three
// documented snake_case slugs are not cross-checked against the
// corresponding Rust variants inside the brace-balanced enum body. A
// rename of a variant or addition of a new variant that the docs miss
// would silently invalidate the documented exhaustive slug list while
// the line range still matches.
//
// This validator asserts:
//   1. Each of the three (Rust variant, slug) pairs has exactly one
//      variant declaration inside the brace-balanced A2ATaskStatus
//      enum body in covenant-a2a/src/lib.rs.
//   2. The variant count inside the enum body equals exactly the
//      number of documented variants (3), so a new variant added
//      without docs update is caught.
//   3. The docs prose contains the documented exhaustive-slug
//      citation verbatim (regex match) so the slug list cannot be
//      quietly paraphrased into a different shape.
//
// Convention: the enum body is located by brace-balance from the
// `pub enum A2ATaskStatus {` opener, so variant-name collisions across
// different enums do not contaminate the lookup.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-a2a/src/lib.rs";

const expectedVariants = [
  { rust: "Ok", slug: "ok" },
  { rust: "Error", slug: "error" },
  { rust: "Partial", slug: "partial" },
];

const docsCitation = {
  regex:
    /`A2ATaskStatus` slug, exactly one of `"ok"`, `"error"`, `"partial"` \(snake_case per `covenant-a2a\/src\/lib\.rs:\d+-\d+`\)/,
  label: "A2ATaskStatus documented exhaustive-slug citation",
  template:
    "`A2ATaskStatus` slug, exactly one of `\"ok\"`, `\"error\"`, `\"partial\"` (snake_case per `covenant-a2a/src/lib.rs:N-M`)",
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

let enumStart = null;
let enumEnd = null;
if (source) {
  const lines = source.split("\n");
  const openers = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^pub\s+enum\s+A2ATaskStatus\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub enum A2ATaskStatus" at top level but found ${openers.length}; remediation: confirm the A2ATaskStatus enum exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    enumStart = openers[0];
    enumEnd = scanBraceBalance(lines, enumStart);
    if (enumEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub enum A2ATaskStatus" starting at line ${enumStart}; remediation: confirm the enum body is brace-balanced`,
      );
    } else {
      const variantRegex = /^\s*([A-Z]\w*)\s*[\{(,]?\s*(?:,)?\s*$/;
      const declaredVariants = [];
      for (let index = enumStart; index < enumEnd; index += 1) {
        const text = lines[index];
        if (text.trim().startsWith("//") || text.trim().startsWith("#[")) {
          continue;
        }
        const match = text.match(variantRegex);
        if (match) {
          declaredVariants.push({ name: match[1], line: index + 1 });
        }
      }
      for (const variant of expectedVariants) {
        const matches = declaredVariants.filter((entry) => entry.name === variant.rust);
        if (matches.length !== 1) {
          fail(
            `${sourcePath}: expected exactly 1 "${variant.rust}" variant inside the A2ATaskStatus enum body (lines ${enumStart}-${enumEnd}) but found ${matches.length}; remediation: the docs cite the snake_case slug "${variant.slug}" as a documented variant; confirm the Rust variant ${variant.rust} exists in A2ATaskStatus, or update both docs and source together`,
          );
        }
      }
      if (declaredVariants.length !== expectedVariants.length) {
        const declaredNames = declaredVariants.map((entry) => entry.name).join(", ");
        fail(
          `${sourcePath}: expected exactly ${expectedVariants.length} variants inside the A2ATaskStatus enum body (lines ${enumStart}-${enumEnd}) but found ${declaredVariants.length} (${declaredNames}); remediation: the docs cite an exhaustive slug list "ok", "error", "partial"; update the docs to include any added variant or remove unused declarations`,
        );
      }
    }
  }
}

if (docs) {
  if (!docsCitation.regex.test(docs)) {
    fail(
      `${docsPath}: missing the ${docsCitation.label} ("${docsCitation.template}"); remediation: restore the docs sentence that records all three slugs in the documented order — "ok", "error", "partial"`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-a2a-task-status-variant-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const variantSummary = expectedVariants.map((variant) => `${variant.rust}=${variant.slug}`).join(", ");
console.log(
  `validate-covenant-a2a-task-status-variant-list-line-refs: ok (A2ATaskStatus lib.rs:${enumStart}-${enumEnd}, variants pinned: ${variantSummary})`,
);
