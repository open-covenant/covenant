#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-types MemoryRepairMode documented variant list drift guard.
// docs/ipc-and-http-gateway.md line 547 cites the MemoryRepairMode wire
// form as ``MemoryRepairMode` slug, exactly `"dry_run"` or `"apply"`
// (snake_case, per `MemoryRepairMode`'s `#[serde(rename_all =
// "snake_case")]` at `covenant-types/src/lib.rs:196-201`)`. The
// :196-201 enum block range plus rename_all annotation are already
// pinned by validate-covenant-types-enum-block-range-line-refs.mjs,
// but the two documented snake_case slugs are not cross-checked
// against the corresponding Rust variants inside the brace-balanced
// enum body. A rename of a variant or addition of a new repair mode
// the docs miss would silently invalidate the documented exhaustive
// slug list while the line range still matches.
//
// This validator asserts:
//   1. Each of the two (Rust variant, slug) pairs has exactly one
//      variant declaration inside the brace-balanced MemoryRepairMode
//      enum body in covenant-types/src/lib.rs.
//   2. The variant count inside the enum body equals exactly two, so
//      a new variant added without docs update is caught.
//   3. The docs prose contains the documented exhaustive-slug
//      citation verbatim (regex match) so the slug list cannot be
//      quietly paraphrased into a different shape.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-types/src/lib.rs";

const expectedVariants = [
  { rust: "DryRun", slug: "dry_run" },
  { rust: "Apply", slug: "apply" },
];

const docsCitation = {
  regex:
    /`MemoryRepairMode` slug, exactly `"dry_run"` or `"apply"` \(snake_case, per `MemoryRepairMode`'s `#\[serde\(rename_all = "snake_case"\)\]` at `covenant-types\/src\/lib\.rs:\d+-\d+`\)/,
  label: "MemoryRepairMode documented exhaustive-slug citation",
  template:
    "`MemoryRepairMode` slug, exactly `\"dry_run\"` or `\"apply\"` (snake_case, per `MemoryRepairMode`'s `#[serde(rename_all = \"snake_case\")]` at `covenant-types/src/lib.rs:N-M`)",
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
    if (/^pub\s+enum\s+MemoryRepairMode\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub enum MemoryRepairMode" at top level but found ${openers.length}; remediation: confirm the MemoryRepairMode enum exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    enumStart = openers[0];
    enumEnd = scanBraceBalance(lines, enumStart);
    if (enumEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub enum MemoryRepairMode" starting at line ${enumStart}; remediation: confirm the enum body is brace-balanced`,
      );
    } else {
      const variantRegex = /^\s*([A-Z]\w*)\s*[\{(,]?\s*(?:,)?\s*$/;
      const declaredVariants = [];
      for (let index = enumStart; index < enumEnd; index += 1) {
        const text = lines[index];
        const trimmed = text.trim();
        if (trimmed.startsWith("//") || trimmed.startsWith("#[")) {
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
            `${sourcePath}: expected exactly 1 "${variant.rust}" variant inside the MemoryRepairMode enum body (lines ${enumStart}-${enumEnd}) but found ${matches.length}; remediation: the docs cite the snake_case slug "${variant.slug}" as a documented variant; confirm the Rust variant ${variant.rust} exists in MemoryRepairMode, or update both docs and source together`,
          );
        }
      }
      if (declaredVariants.length !== expectedVariants.length) {
        const declaredNames = declaredVariants.map((entry) => entry.name).join(", ");
        fail(
          `${sourcePath}: expected exactly ${expectedVariants.length} variants inside the MemoryRepairMode enum body (lines ${enumStart}-${enumEnd}) but found ${declaredVariants.length} (${declaredNames}); remediation: the docs cite an exhaustive slug list "dry_run", "apply"; update the docs to include any added variant or remove unused declarations`,
        );
      }
    }
  }
}

if (docs) {
  if (!docsCitation.regex.test(docs)) {
    fail(
      `${docsPath}: missing the ${docsCitation.label} ("${docsCitation.template}"); remediation: restore the docs sentence that records both slugs in the documented order — "dry_run", "apply"`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-types-memory-repair-mode-variant-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const variantSummary = expectedVariants.map((variant) => `${variant.rust}=${variant.slug}`).join(", ");
console.log(
  `validate-covenant-types-memory-repair-mode-variant-list-line-refs: ok (MemoryRepairMode lib.rs:${enumStart}-${enumEnd}, variants pinned: ${variantSummary})`,
);
