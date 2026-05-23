#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-audit AuditKind enum annotation line-ref drift guard.
// docs/ipc-and-http-gateway.md line 363 cites the AuditKind tagged-enum
// "defined at `covenant-audit/src/lib.rs:71` onwards) with a `type`
// discriminator (e.g., `"capability_granted"`, `"intent_dispatched"`,
// `"hermes_tool_invoked"`)". The :71 enum-line reference is already
// pinned by validate-covenant-audit-struct-line-refs.mjs, but the
// `#[serde(tag = "type", rename_all = "snake_case")]` annotation that
// drives the discriminator name and variant casing is not bound to
// source: a change to the discriminator key or the variant casing
// would silently invalidate the docs prose and the three example
// variant slugs.
//
// This validator asserts:
//   1. The immediately-preceding line above `pub enum AuditKind {` in
//      covenant-audit/src/lib.rs is exactly the expected annotation
//      literal.
//   2. The docs prose contains the literal discriminator name
//      `with a `type` discriminator` (backtick-quoted) next to the
//      AuditKind citation.
//   3. The three example variant slugs documented in the docs prose
//      (`"capability_granted"`, `"intent_dispatched"`,
//      `"hermes_tool_invoked"`) correspond to actual Rust variants
//      (`CapabilityGranted`, `IntentDispatched`, `HermesToolInvoked`)
//      declared inside the brace-balanced AuditKind enum body — so a
//      rename or removal of any of these variants forces docs and
//      source to be reconciled together.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-audit/src/lib.rs";

const expectedAnnotation = '#[serde(tag = "type", rename_all = "snake_case")]';
const expectedVariants = [
  { rust: "CapabilityGranted", slug: "capability_granted" },
  { rust: "IntentDispatched", slug: "intent_dispatched" },
  { rust: "HermesToolInvoked", slug: "hermes_tool_invoked" },
];

const docsCitations = {
  discriminator: {
    regex:
      /tagged-enum `AuditKind` \(defined at `covenant-audit\/src\/lib\.rs:\d+` onwards\) with a `type` discriminator/,
    label: "AuditKind 'with a `type` discriminator' citation",
    template:
      "tagged-enum `AuditKind` (defined at `covenant-audit/src/lib.rs:NN` onwards) with a `type` discriminator",
  },
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

let annotationLine = null;
if (source) {
  const lines = source.split("\n");
  const enumOpeners = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^pub\s+enum\s+AuditKind\b/.test(lines[index])) {
      enumOpeners.push(index + 1);
    }
  }
  if (enumOpeners.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub enum AuditKind" at top level but found ${enumOpeners.length}; remediation: confirm the AuditKind enum exists at top level`,
    );
  } else {
    const enumStart = enumOpeners[0];
    if (enumStart < 2) {
      fail(
        `${sourcePath}: "pub enum AuditKind" is at line ${enumStart}; expected the #[serde] annotation on the preceding line, but the enum is at the top of the file`,
      );
    } else {
      const candidate = enumStart - 1;
      const text = lines[candidate - 1];
      if (text !== expectedAnnotation) {
        fail(
          `${sourcePath}:${candidate}: expected exactly \`${expectedAnnotation}\` immediately above "pub enum AuditKind" at line ${enumStart}, but found \`${text}\`; remediation: restore the annotation, or update docs and source together if the wire format changed`,
        );
      } else {
        annotationLine = candidate;
      }
    }
    const enumEnd = scanBraceBalance(lines, enumStart);
    if (enumEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub enum AuditKind" starting at line ${enumStart}; remediation: confirm the enum body is brace-balanced`,
      );
    } else {
      for (const variant of expectedVariants) {
        const variantRegex = new RegExp(`^\\s*${variant.rust}\\s*[\\{(]`);
        const matches = [];
        for (let index = enumStart; index < enumEnd; index += 1) {
          if (variantRegex.test(lines[index])) {
            matches.push(index + 1);
          }
        }
        if (matches.length !== 1) {
          fail(
            `${sourcePath}: expected exactly 1 "${variant.rust}" variant inside the AuditKind enum body (lines ${enumStart}-${enumEnd}) but found ${matches.length}; remediation: the docs cite the snake_case slug "${variant.slug}" as an example variant; confirm the Rust variant ${variant.rust} exists in AuditKind, or update both docs and source together`,
          );
        }
      }
    }
  }
}

if (docs) {
  if (!docsCitations.discriminator.regex.test(docs)) {
    fail(
      `${docsPath}: missing the ${docsCitations.discriminator.label} ("${docsCitations.discriminator.template}"); remediation: restore the docs sentence that records the AuditKind discriminator name verbatim`,
    );
  }
  for (const variant of expectedVariants) {
    const slugRegex = new RegExp(`\`"${variant.slug}"\``);
    if (!slugRegex.test(docs)) {
      fail(
        `${docsPath}: missing the AuditKind example variant slug \`"${variant.slug}"\` in the discriminator citation; remediation: restore the slug in the example list — it pins the snake_case wire form for ${variant.rust}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-audit-kind-annotation-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const variantSummary = expectedVariants.map((variant) => variant.rust).join(", ");
console.log(
  `validate-covenant-audit-kind-annotation-line-refs: ok (AuditKind annotation lib.rs:${annotationLine}, variants pinned: ${variantSummary})`,
);
