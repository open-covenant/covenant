#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-a2a A2AAutoRetrySkipReason documented variant list drift
// guard.
// docs/ipc-and-http-gateway.md line 511 explicitly states
// "`A2AAutoRetrySkipReason` enumerates exactly these nine snake_case
// slugs (per `covenant-a2a/src/lib.rs:240-252`)" followed by a bulleted
// list pinning each documented slug.
//
// The :240-252 enum block range and the
// `#[serde(rename_all = "snake_case")]` attribute are already pinned by
// validate-covenant-a2a-enum-block-range-line-refs.mjs, but the nine
// documented slugs are not cross-checked against (a) Rust variants
// inside the brace-balanced enum body and (b) the matching arms in the
// adjacent `as_str()` impl that returns the wire literal. A rename of a
// variant, divergence between rename_all and as_str, or addition of an
// undocumented variant would silently invalidate the documented
// exhaustive slug list.
//
// This validator asserts:
//   1. Each of the nine (Rust variant, slug) pairs has exactly one
//      variant declaration inside the brace-balanced
//      A2AAutoRetrySkipReason enum body.
//   2. Each (Rust variant, slug) pair has exactly one matching arm
//      `Self::<Variant> => "<slug>",` inside the brace-balanced
//      `impl A2AAutoRetrySkipReason { fn as_str(...) {...} }` body.
//   3. The variant count inside the enum body equals exactly nine, so
//      a new variant added without docs update is caught.
//   4. The docs prose contains the nine documented slug literals in
//      backticks so the list cannot be quietly paraphrased.
//
// Convention: the enum and impl bodies are located by brace-balance
// from their respective openers, so variant-name collisions across
// different enums do not contaminate the lookup.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-a2a/src/lib.rs";

const expectedVariants = [
  { rust: "Disabled", slug: "disabled" },
  { rust: "NotInFlight", slug: "not_in_flight" },
  { rust: "MissingLease", slug: "missing_lease" },
  { rust: "LeaseTooYoung", slug: "lease_too_young" },
  { rust: "MissingIdempotency", slug: "missing_idempotency" },
  { rust: "UnsafeDuplicateSafety", slug: "unsafe_duplicate_safety" },
  { rust: "MaxAttemptsReached", slug: "max_attempts_reached" },
  { rust: "LimitReached", slug: "limit_reached" },
  { rust: "CapabilityScopeMismatch", slug: "capability_scope_mismatch" },
];

const docsHeader = {
  regex:
    /`A2AAutoRetrySkipReason` enumerates exactly these nine snake_case slugs \(per `covenant-a2a\/src\/lib\.rs:\d+-\d+`\)/,
  label: "A2AAutoRetrySkipReason exhaustive-slug header",
  template:
    "`A2AAutoRetrySkipReason` enumerates exactly these nine snake_case slugs (per `covenant-a2a/src/lib.rs:N-M`)",
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
let implStart = null;
let implEnd = null;
if (source) {
  const lines = source.split("\n");
  const enumOpeners = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^pub\s+enum\s+A2AAutoRetrySkipReason\b/.test(lines[index])) {
      enumOpeners.push(index + 1);
    }
  }
  if (enumOpeners.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub enum A2AAutoRetrySkipReason" at top level but found ${enumOpeners.length}; remediation: confirm the A2AAutoRetrySkipReason enum exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    enumStart = enumOpeners[0];
    enumEnd = scanBraceBalance(lines, enumStart);
    if (enumEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub enum A2AAutoRetrySkipReason" starting at line ${enumStart}; remediation: confirm the enum body is brace-balanced`,
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
            `${sourcePath}: expected exactly 1 "${variant.rust}" variant inside the A2AAutoRetrySkipReason enum body (lines ${enumStart}-${enumEnd}) but found ${matches.length}; remediation: the docs cite the snake_case slug "${variant.slug}" as a documented variant; confirm the Rust variant ${variant.rust} exists in A2AAutoRetrySkipReason, or update both docs and source together`,
          );
        }
      }
      if (declaredVariants.length !== expectedVariants.length) {
        const declaredNames = declaredVariants.map((entry) => entry.name).join(", ");
        fail(
          `${sourcePath}: expected exactly ${expectedVariants.length} variants inside the A2AAutoRetrySkipReason enum body (lines ${enumStart}-${enumEnd}) but found ${declaredVariants.length} (${declaredNames}); remediation: the docs cite an exhaustive nine-slug list; update the docs to include any added variant or remove unused declarations`,
        );
      }
    }
  }

  const implOpeners = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^impl\s+A2AAutoRetrySkipReason\b/.test(lines[index])) {
      implOpeners.push(index + 1);
    }
  }
  if (implOpeners.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "impl A2AAutoRetrySkipReason" at top level but found ${implOpeners.length}; remediation: confirm the as_str() impl exists at top level for the enum`,
    );
  } else {
    implStart = implOpeners[0];
    implEnd = scanBraceBalance(lines, implStart);
    if (implEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "impl A2AAutoRetrySkipReason" starting at line ${implStart}; remediation: confirm the impl body is brace-balanced`,
      );
    } else {
      for (const variant of expectedVariants) {
        const armRegex = new RegExp(`^\\s*Self::${variant.rust}\\s*=>\\s*"${variant.slug}"\\s*,?\\s*$`);
        const matches = [];
        for (let index = implStart; index < implEnd; index += 1) {
          if (armRegex.test(lines[index])) {
            matches.push(index + 1);
          }
        }
        if (matches.length !== 1) {
          fail(
            `${sourcePath}: expected exactly 1 "Self::${variant.rust} => \"${variant.slug}\"," arm inside the impl A2AAutoRetrySkipReason body (lines ${implStart}-${implEnd}) but found ${matches.length}; remediation: confirm the as_str() arm for ${variant.rust} returns the documented snake_case slug, or update both docs and source together`,
          );
        }
      }
    }
  }
}

if (docs) {
  if (!docsHeader.regex.test(docs)) {
    fail(
      `${docsPath}: missing the ${docsHeader.label} ("${docsHeader.template}"); remediation: restore the docs sentence that introduces the exhaustive nine-slug list with the line-range citation`,
    );
  }
  for (const variant of expectedVariants) {
    const slugRegex = new RegExp(`\`"${variant.slug}"\``);
    if (!slugRegex.test(docs)) {
      fail(
        `${docsPath}: missing the A2AAutoRetrySkipReason documented slug \`"${variant.slug}"\` in the exhaustive list; remediation: restore the bullet that records the snake_case wire form for ${variant.rust}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-a2a-auto-retry-skip-reason-variant-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const variantSummary = expectedVariants.map((variant) => `${variant.rust}=${variant.slug}`).join(", ");
console.log(
  `validate-covenant-a2a-auto-retry-skip-reason-variant-list-line-refs: ok (enum lib.rs:${enumStart}-${enumEnd}, impl lib.rs:${implStart}-${implEnd}, variants pinned: ${variantSummary})`,
);
