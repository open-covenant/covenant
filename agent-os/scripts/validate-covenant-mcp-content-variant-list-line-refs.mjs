#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-mcp Content documented variant list drift guard.
// docs/ipc-and-http-gateway.md line 159 cites the two Content variants
// — `{type: "text", text: <string>}` for textual output and
// `{type: "json", value: <JSON>}` for structured output — and
// explicitly says "v0 ships text and json variants only". The enum
// line and `#[serde(tag = "type", rename_all = "camelCase")]`
// annotation are already pinned by
// validate-covenant-mcp-content-enum-annotation-line-refs.mjs, but the
// two documented (Rust variant, slug) pairs are not cross-checked
// against the brace-balanced Content enum body in
// covenant-mcp/src/lib.rs. A rename of a variant or addition of a new
// content type the docs miss would silently invalidate the documented
// exhaustive set.
//
// This validator asserts:
//   1. Each of the two (Rust variant, slug) pairs has exactly one
//      variant declaration at the top level of the brace-balanced
//      Content enum body.
//   2. The variant count at the enum-body top level equals exactly
//      two, so a new variant added without docs update is caught.
//   3. The docs prose contains each documented `{type: "<slug>"`
//      shape header and the v0 exhaustive statement so the slug list
//      cannot be quietly paraphrased.
//
// Convention: Content has struct-payload variants (`Text { text }`,
// `Json { value }`), so variant detection tracks brace depth relative
// to the enum body and only counts a TitleCase identifier as a
// variant when it appears at depth 0 of the body.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-mcp/src/lib.rs";

const expectedVariants = [
  { rust: "Text", slug: "text" },
  { rust: "Json", slug: "json" },
];

const docsExhaustiveStatement = {
  regex: /v0 ships text and json variants only/,
  label: "Content v0 exhaustive-variants statement",
  template: "v0 ships text and json variants only",
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
    if (/^pub\s+enum\s+Content\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub enum Content" at top level but found ${openers.length}; remediation: confirm the Content enum exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    enumStart = openers[0];
    enumEnd = scanBraceBalance(lines, enumStart);
    if (enumEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub enum Content" starting at line ${enumStart}; remediation: confirm the enum body is brace-balanced`,
      );
    } else {
      const variantRegex = /^\s*([A-Z]\w*)(?=\s|[,({=]|$)/;
      const declaredVariants = [];
      let depth = 0;
      for (let index = enumStart; index < enumEnd - 1; index += 1) {
        const text = lines[index];
        const trimmed = text.trim();
        if (depth === 0 && !trimmed.startsWith("//") && !trimmed.startsWith("#[")) {
          const match = text.match(variantRegex);
          if (match) {
            declaredVariants.push({ name: match[1], line: index + 1 });
          }
        }
        for (const char of text) {
          if (char === "{") depth += 1;
          else if (char === "}") depth -= 1;
        }
      }
      for (const variant of expectedVariants) {
        const matches = declaredVariants.filter((entry) => entry.name === variant.rust);
        if (matches.length !== 1) {
          fail(
            `${sourcePath}: expected exactly 1 "${variant.rust}" variant at the top level of the Content enum body (lines ${enumStart}-${enumEnd}) but found ${matches.length}; remediation: the docs cite the camelCase slug "${variant.slug}" as a documented variant; confirm the Rust variant ${variant.rust} exists in Content, or update both docs and source together`,
          );
        }
      }
      if (declaredVariants.length !== expectedVariants.length) {
        const declaredNames = declaredVariants.map((entry) => entry.name).join(", ");
        fail(
          `${sourcePath}: expected exactly ${expectedVariants.length} top-level variants inside the Content enum body (lines ${enumStart}-${enumEnd}) but found ${declaredVariants.length} (${declaredNames}); remediation: the docs cite an exhaustive two-variant list — text and json; update the docs to include any added variant or remove unused declarations`,
        );
      }
    }
  }
}

if (docs) {
  if (!docsExhaustiveStatement.regex.test(docs)) {
    fail(
      `${docsPath}: missing the ${docsExhaustiveStatement.label} ("${docsExhaustiveStatement.template}"); remediation: restore the docs sentence that records the Content v0 exhaustive variant list`,
    );
  }
  for (const variant of expectedVariants) {
    const slugRegex = new RegExp(`\\{type: "${variant.slug}"`);
    if (!slugRegex.test(docs)) {
      fail(
        `${docsPath}: missing the Content documented bullet shape \`{type: "${variant.slug}", ...}\`; remediation: restore the shape literal that records the type-discriminator wire form for ${variant.rust}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-mcp-content-variant-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const variantSummary = expectedVariants.map((variant) => `${variant.rust}=${variant.slug}`).join(", ");
console.log(
  `validate-covenant-mcp-content-variant-list-line-refs: ok (Content lib.rs:${enumStart}-${enumEnd}, variants pinned: ${variantSummary})`,
);
