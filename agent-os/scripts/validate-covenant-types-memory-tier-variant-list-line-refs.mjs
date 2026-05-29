#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-types MemoryTier documented variant list drift guard.
// docs/ipc-and-http-gateway.md line 413 cites the MemoryTier wire form
// as `exactly one of `"working"`, `"episodic"`, or `"longterm"` (one
// word, per `MemoryTier`'s `#[serde(rename_all = "lowercase")]` at
// `covenant-types/src/lib.rs:23`...)`. The :23 annotation line is
// already pinned by
// validate-covenant-types-enum-serde-rename-annotation-line-refs.mjs,
// but the three documented lowercase slugs are not cross-checked
// against the corresponding Rust variants inside the brace-balanced
// enum body. A rename of a variant or addition of a new tier the docs
// miss would silently invalidate the documented exhaustive slug list
// while the annotation line still matches.
//
// This validator asserts:
//   1. Each of the three (Rust variant, slug) pairs has exactly one
//      variant declaration inside the brace-balanced MemoryTier enum
//      body in covenant-types/src/lib.rs.
//   2. The variant count inside the enum body equals exactly three, so
//      a new variant added without docs update is caught.
//   3. The docs prose contains the documented exhaustive-slug
//      citation verbatim (regex match) so the slug list cannot be
//      quietly paraphrased into a different shape.
//
// Convention: the enum body is located by brace-balance from the
// `pub enum MemoryTier {` opener; doc comments (`///`) and per-variant
// `#[serde(rename = "...")]` attribute lines inside the body are
// skipped so they do not collide with the variant lookup.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-types/src/lib.rs";

const expectedVariants = [
  { rust: "Working", slug: "working" },
  { rust: "Episodic", slug: "episodic" },
  { rust: "LongTerm", slug: "longterm" },
];

const docsCitation = {
  regex:
    /exactly one of `"working"`, `"episodic"`, or `"longterm"` \(one word, per `MemoryTier`'s `#\[serde\(rename_all = "lowercase"\)\]` at `covenant-types\/src\/lib\.rs:\d+`/,
  label: "MemoryTier documented exhaustive-slug citation",
  template:
    "exactly one of `\"working\"`, `\"episodic\"`, or `\"longterm\"` (one word, per `MemoryTier`'s `#[serde(rename_all = \"lowercase\")]` at `covenant-types/src/lib.rs:NN`",
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
    if (/^pub\s+enum\s+MemoryTier\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub enum MemoryTier" at top level but found ${openers.length}; remediation: confirm the MemoryTier enum exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    enumStart = openers[0];
    enumEnd = scanBraceBalance(lines, enumStart);
    if (enumEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub enum MemoryTier" starting at line ${enumStart}; remediation: confirm the enum body is brace-balanced`,
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
            `${sourcePath}: expected exactly 1 "${variant.rust}" variant inside the MemoryTier enum body (lines ${enumStart}-${enumEnd}) but found ${matches.length}; remediation: the docs cite the lowercase slug "${variant.slug}" as a documented variant; confirm the Rust variant ${variant.rust} exists in MemoryTier, or update both docs and source together`,
          );
        }
      }
      if (declaredVariants.length !== expectedVariants.length) {
        const declaredNames = declaredVariants.map((entry) => entry.name).join(", ");
        fail(
          `${sourcePath}: expected exactly ${expectedVariants.length} variants inside the MemoryTier enum body (lines ${enumStart}-${enumEnd}) but found ${declaredVariants.length} (${declaredNames}); remediation: the docs cite an exhaustive lowercase slug list "working", "episodic", "longterm"; update the docs to include any added variant or remove unused declarations`,
        );
      }
    }
  }
}

if (docs) {
  if (!docsCitation.regex.test(docs)) {
    fail(
      `${docsPath}: missing the ${docsCitation.label} ("${docsCitation.template}"); remediation: restore the docs sentence that records all three slugs in the documented order — "working", "episodic", "longterm"`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-types-memory-tier-variant-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const variantSummary = expectedVariants.map((variant) => `${variant.rust}=${variant.slug}`).join(", ");
console.log(
  `validate-covenant-types-memory-tier-variant-list-line-refs: ok (MemoryTier lib.rs:${enumStart}-${enumEnd}, variants pinned: ${variantSummary})`,
);
