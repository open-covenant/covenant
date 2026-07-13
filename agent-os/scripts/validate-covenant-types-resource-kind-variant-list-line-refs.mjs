#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-types ResourceKind documented variant list drift guard.
// docs/ipc-and-http-gateway.md line 206 cites the ResourceKind slug
// space as ``ResourceKind` slug, exactly one of `"compute"`, `"memory"`,
// `"tool"`, `"message"`, `"registration"` (lowercase per
// `#[serde(rename_all = "lowercase")]` at `covenant-types/src/lib.rs:35`)`.
// The :35 annotation line is already pinned by
// validate-covenant-types-enum-serde-rename-annotation-line-refs.mjs,
// but the five documented lowercase slugs are not cross-checked
// against the corresponding Rust variants inside the brace-balanced
// enum body. A rename of a variant or addition of a new variant the
// docs miss would silently invalidate the documented exhaustive slug
// list while the annotation line still matches.
//
// This validator asserts:
//   1. Each of the five (Rust variant, slug) pairs has exactly one
//      variant declaration inside the brace-balanced ResourceKind
//      enum body in covenant-types/src/lib.rs.
//   2. The variant count inside the enum body equals exactly five, so
//      a new variant added without docs update is caught.
//   3. The docs prose contains the documented exhaustive-slug
//      citation verbatim (regex match) so the slug list cannot be
//      quietly paraphrased into a different shape.
//
// Convention: the enum body is located by brace-balance from the
// `pub enum ResourceKind {` opener, so variant-name collisions across
// different enums do not contaminate the lookup.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-types/src/lib.rs";

const expectedVariants = [
  { rust: "Compute", slug: "compute" },
  { rust: "Memory", slug: "memory" },
  { rust: "Tool", slug: "tool" },
  { rust: "Message", slug: "message" },
  { rust: "Registration", slug: "registration" },
];

const docsCitation = {
  regex:
    /`ResourceKind` slug, exactly one of `"compute"`, `"memory"`, `"tool"`, `"message"`, `"registration"` \(lowercase per `#\[serde\(rename_all = "lowercase"\)\]` at `covenant-types\/src\/lib\.rs:\d+`\)/,
  label: "ResourceKind documented exhaustive-slug citation",
  template:
    "`ResourceKind` slug, exactly one of `\"compute\"`, `\"memory\"`, `\"tool\"`, `\"message\"`, `\"registration\"` (lowercase per `#[serde(rename_all = \"lowercase\")]` at `covenant-types/src/lib.rs:NN`)",
};

const errors = [];
const fail = (message) => errors.push(message);

// docs/capabilities.md carries a second copy of the ResourceKind slug list under
// `chain.resource`. This block pins that copy to the variants parsed from the
// enum body (zero hard-coding): the expected slug set is the parsed enum's
// variant names lowercased (the enum is `#[serde(rename_all = "lowercase")]`),
// so a sixth variant fails both docs/ipc-and-http-gateway.md (via the count
// check below) and docs/capabilities.md (via the order check here).
const CAPABILITIES_DOC_PATH = "docs/capabilities.md";

function extractChainResourceSlugs(doc) {
  if (doc == null) return { slugs: [], errors: [`${CAPABILITIES_DOC_PATH} must stay present`] };
  const sentence = doc.match(/`resource` narrows receipt rows to\s+([^.\n]*?)\./);
  if (sentence == null) {
    return { slugs: [], errors: [`${CAPABILITIES_DOC_PATH} must keep the "\`resource\` narrows receipt rows to ..." sentence listing the ResourceKind slugs`] };
  }
  const slugs = [...sentence[1].matchAll(/`([a-z0-9_]+)`/g)].map((m) => m[1]);
  if (slugs.length === 0) {
    return { slugs: [], errors: [`${CAPABILITIES_DOC_PATH} chain.resource sentence must list the slugs in backticks`] };
  }
  return { slugs, errors: [] };
}

function compareChainResourceSlugs(docSlugs, expectedSlugs) {
  const comparisonErrors = [];
  const max = Math.max(docSlugs.length, expectedSlugs.length);
  for (let index = 0; index < max; index += 1) {
    if (docSlugs[index] !== expectedSlugs[index]) {
      const docAt = docSlugs[index] ?? "(absent)";
      const expectedAt = expectedSlugs[index] ?? "(absent)";
      comparisonErrors.push(`${CAPABILITIES_DOC_PATH} chain.resource sentence lists slug ${index + 1} as "${docAt}" but the ResourceKind enum serializes "${expectedAt}" there; the published slug list drifted in set, order, or spelling`);
      break;
    }
  }
  return comparisonErrors;
}

function runSelfTest() {
  const failures = [];
  const baseExpected = ["compute", "memory", "tool", "message", "registration"];
  const goodDoc = "- `resource` narrows receipt rows to `compute`, `memory`, `tool`, `message`, or `registration`.";

  const good = extractChainResourceSlugs(goodDoc);
  if (good.errors.length > 0 || good.slugs.join(",") !== baseExpected.join(",")) {
    failures.push(`good capabilities.md fixture mis-extracted: ${good.errors.join("; ") || good.slugs.join(",")}`);
  }
  if (compareChainResourceSlugs(good.slugs, baseExpected).length > 0) {
    failures.push("good capabilities.md slug set should match the ResourceKind variants");
  }

  const badMutations = [
    ["sentence removed", (d) => d.replace("`resource` narrows receipt rows to", "`resource` filters rows to")],
    ["omits a slug", (d) => d.replace("`tool`, ", "")],
    ["adds a phantom slug", (d) => d.replace("`registration`.", "`registration`, or `pending`.")],
    ["renames a slug", (d) => d.replace("`registration`", "`signup`")],
    ["reordered", (d) => d.replace("`compute`, `memory`", "`memory`, `compute`")],
  ];
  for (const [label, mutate] of badMutations) {
    const extracted = extractChainResourceSlugs(mutate(goodDoc));
    if (extracted.errors.length === 0 && compareChainResourceSlugs(extracted.slugs, baseExpected).length === 0) {
      failures.push(`bad capabilities.md fixture "${label}" should have been rejected but passed`);
    }
  }

  // A sixth ResourceKind variant must propagate to the capabilities.md check:
  // the doc still lists five while the enum now serializes six.
  if (compareChainResourceSlugs(good.slugs, [...baseExpected, "pending"]).length === 0) {
    failures.push("a sixth ResourceKind variant must make the capabilities.md slug-set check fail");
  }

  return failures;
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-covenant-types-resource-kind-variant-list-line-refs: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

let docs;
let capabilitiesDocs;
let source;
try {
  docs = read(docsPath);
} catch (error) {
  fail(`cannot read ${docsPath}: ${error.message}`);
}
try {
  capabilitiesDocs = read(CAPABILITIES_DOC_PATH);
} catch (error) {
  fail(`cannot read ${CAPABILITIES_DOC_PATH}: ${error.message}`);
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
let declaredVariants = [];
if (source) {
  const lines = source.split("\n");
  const openers = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^pub\s+enum\s+ResourceKind\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub enum ResourceKind" at top level but found ${openers.length}; remediation: confirm the ResourceKind enum exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    enumStart = openers[0];
    enumEnd = scanBraceBalance(lines, enumStart);
    if (enumEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub enum ResourceKind" starting at line ${enumStart}; remediation: confirm the enum body is brace-balanced`,
      );
    } else {
      const variantRegex = /^\s*([A-Z]\w*)\s*[\{(,]?\s*(?:,)?\s*$/;
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
            `${sourcePath}: expected exactly 1 "${variant.rust}" variant inside the ResourceKind enum body (lines ${enumStart}-${enumEnd}) but found ${matches.length}; remediation: the docs cite the lowercase slug "${variant.slug}" as a documented variant; confirm the Rust variant ${variant.rust} exists in ResourceKind, or update both docs and source together`,
          );
        }
      }
      if (declaredVariants.length !== expectedVariants.length) {
        const declaredNames = declaredVariants.map((entry) => entry.name).join(", ");
        fail(
          `${sourcePath}: expected exactly ${expectedVariants.length} variants inside the ResourceKind enum body (lines ${enumStart}-${enumEnd}) but found ${declaredVariants.length} (${declaredNames}); remediation: the docs cite an exhaustive lowercase slug list "compute", "memory", "tool", "message", "registration"; update the docs to include any added variant or remove unused declarations`,
        );
      }
    }
  }
}

if (docs) {
  if (!docsCitation.regex.test(docs)) {
    fail(
      `${docsPath}: missing the ${docsCitation.label} ("${docsCitation.template}"); remediation: restore the docs sentence that records all five slugs in the documented order — "compute", "memory", "tool", "message", "registration"`,
    );
  }
}

if (capabilitiesDocs != null) {
  const chainExpected =
    declaredVariants.length > 0
      ? declaredVariants.map((entry) => entry.name.toLowerCase())
      : expectedVariants.map((variant) => variant.slug);
  const { slugs: chainSlugs, errors: chainErrors } = extractChainResourceSlugs(capabilitiesDocs);
  for (const chainError of chainErrors) {
    fail(chainError);
  }
  for (const chainError of compareChainResourceSlugs(chainSlugs, chainExpected)) {
    fail(chainError);
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-types-resource-kind-variant-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const variantSummary = expectedVariants.map((variant) => `${variant.rust}=${variant.slug}`).join(", ");
console.log(
  `validate-covenant-types-resource-kind-variant-list-line-refs: ok (ResourceKind lib.rs:${enumStart}-${enumEnd}, variants pinned: ${variantSummary})`,
);
