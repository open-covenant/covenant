#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-peer-auth RevokeOutcome documented variant list drift guard.
// docs/ipc-and-http-gateway.md line 337 introduces "The five
// `RevokeOutcome` variants the daemon may return:" followed by five
// bullets, each opening with `{type: "<slug>", ...}` and pinning the
// type-discriminator slug for that variant. The enum line and
// `#[serde(tag = "type", rename_all = "snake_case")]` annotation are
// already pinned by validate-covenant-peer-auth-revoke-outcome-enum-
// annotation-line-refs.mjs, but the five documented (Rust variant,
// slug) pairs are not cross-checked against the brace-balanced enum
// body in covenant-peer-auth/src/lib.rs.
//
// This validator asserts:
//   1. Each of the five (Rust variant, slug) pairs has exactly one
//      variant declaration inside the brace-balanced RevokeOutcome
//      enum body.
//   2. The variant count at the enum-body top level equals exactly
//      five, so a new variant added without docs update is caught.
//   3. The docs prose contains each documented `{type: "<slug>", ...}`
//      bullet header so the slug list cannot be quietly paraphrased.
//
// Convention: RevokeOutcome has payload-bearing variants (tuple
// `Revoked(PeerSummary)` and struct `Ambiguous { matches, truncated }`),
// so the variant detection tracks brace depth relative to the enum
// body and only counts a TitleCase identifier as a variant when it
// appears at depth 0 of the body. Fields inside a struct variant
// (`matches:`, `truncated:`) are lowercase, but depth gating also
// guards against a future refactor that introduces a TitleCase
// identifier inside a payload body.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-peer-auth/src/lib.rs";

const expectedVariants = [
  { rust: "Revoked", slug: "revoked" },
  { rust: "AlreadyRevoked", slug: "already_revoked" },
  { rust: "NotFound", slug: "not_found" },
  { rust: "Ambiguous", slug: "ambiguous" },
  { rust: "SelfRevokeForbidden", slug: "self_revoke_forbidden" },
];

const docsHeader = {
  regex: /The five `RevokeOutcome` variants the daemon may return:/,
  label: "RevokeOutcome exhaustive-variant header",
  template: "The five `RevokeOutcome` variants the daemon may return:",
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
    if (/^pub\s+enum\s+RevokeOutcome\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub enum RevokeOutcome" at top level but found ${openers.length}; remediation: confirm the RevokeOutcome enum exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    enumStart = openers[0];
    enumEnd = scanBraceBalance(lines, enumStart);
    if (enumEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub enum RevokeOutcome" starting at line ${enumStart}; remediation: confirm the enum body is brace-balanced`,
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
            `${sourcePath}: expected exactly 1 "${variant.rust}" variant at the top level of the RevokeOutcome enum body (lines ${enumStart}-${enumEnd}) but found ${matches.length}; remediation: the docs cite the snake_case slug "${variant.slug}" as a documented variant; confirm the Rust variant ${variant.rust} exists in RevokeOutcome, or update both docs and source together`,
          );
        }
      }
      if (declaredVariants.length !== expectedVariants.length) {
        const declaredNames = declaredVariants.map((entry) => entry.name).join(", ");
        fail(
          `${sourcePath}: expected exactly ${expectedVariants.length} top-level variants inside the RevokeOutcome enum body (lines ${enumStart}-${enumEnd}) but found ${declaredVariants.length} (${declaredNames}); remediation: the docs cite an exhaustive five-slug list; update the docs to include any added variant or remove unused declarations`,
        );
      }
    }
  }
}

if (docs) {
  if (!docsHeader.regex.test(docs)) {
    fail(
      `${docsPath}: missing the ${docsHeader.label} ("${docsHeader.template}"); remediation: restore the docs sentence that introduces the exhaustive five-variant list`,
    );
  }
  for (const variant of expectedVariants) {
    const slugRegex = new RegExp(`\\{type: "${variant.slug}"`);
    if (!slugRegex.test(docs)) {
      fail(
        `${docsPath}: missing the RevokeOutcome documented bullet header \`{type: "${variant.slug}", ...}\`; remediation: restore the bullet that records the type-discriminator wire form for ${variant.rust}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-peer-auth-revoke-outcome-variant-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const variantSummary = expectedVariants.map((variant) => `${variant.rust}=${variant.slug}`).join(", ");
console.log(
  `validate-covenant-peer-auth-revoke-outcome-variant-list-line-refs: ok (RevokeOutcome lib.rs:${enumStart}-${enumEnd}, variants pinned: ${variantSummary})`,
);
