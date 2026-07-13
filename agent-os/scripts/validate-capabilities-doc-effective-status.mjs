#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// docs/capabilities.md documents the capability_usage `effective` field as a
// closed set (live, expired, revoked, exhausted) that supervisors consume as
// the daemon's authorization verdict, sourced from the CapabilityEffectiveStatus
// enum in covenant-ipc/src/lib.rs. Nothing binds the doc to the enum: a fifth
// variant would ship undocumented against a doc claiming a closed set, and a
// dropped #[serde(rename_all = "snake_case")] would silently change the wire
// slugs while the doc kept the old ones. This guard extracts the enum variants
// and their serde snake_case rename from the code (never hard-coded) and asserts
// the doc's "one of ..." closed set matches exactly. Reads only committed files;
// fails loud on any empty extraction.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const DOC_PATH = "docs/capabilities.md";
const CODE_PATH = "agent-os/crates/covenant-ipc/src/lib.rs";
const ENUM_NAME = "CapabilityEffectiveStatus";

function sliceBraces(source, needle) {
  const start = source.indexOf(needle);
  if (start < 0) return null;
  const open = source.indexOf("{", start);
  if (open < 0) return null;
  let depth = 0;
  for (let i = open; i < source.length; i++) {
    if (source[i] === "{") depth++;
    else if (source[i] === "}") {
      depth--;
      if (depth === 0) return source.slice(open, i + 1);
    }
  }
  return null;
}

function snakeCase(name) {
  return name
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1_$2")
    .toLowerCase();
}

function extractCode(source) {
  const errors = [];
  if (!source.includes(`enum ${ENUM_NAME}`)) {
    return { slugs: [], hasSnakeCase: false, errors: [`could not locate enum ${ENUM_NAME} in ${CODE_PATH}; the effective-status type moved out from under this guard`] };
  }

  const lines = source.split("\n");
  const enumLine = lines.findIndex((l) => l.includes(`enum ${ENUM_NAME}`));
  const attrBlock = [];
  for (let i = enumLine - 1; i >= 0; i--) {
    const t = lines[i].trim();
    if (t.startsWith("#")) attrBlock.push(lines[i]);
    else if (t === "") continue;
    else break;
  }
  const hasSnakeCase = /rename_all\s*=\s*"snake_case"/.test(attrBlock.join("\n"));
  if (!hasSnakeCase) {
    errors.push(`enum ${ENUM_NAME} must retain #[serde(rename_all = "snake_case")] or the wire slugs drift from the doc set`);
  }

  const body = sliceBraces(source, `enum ${ENUM_NAME}`);
  const variants = body == null ? [] : [...body.matchAll(/^\s+([A-Z][A-Za-z0-9]*)\s*,\s*$/gm)].map((m) => m[1]);
  if (variants.length === 0) {
    errors.push(`found no variants in enum ${ENUM_NAME} in ${CODE_PATH}; the parser drifted out from under this guard`);
  }
  const slugs = variants.map(snakeCase);
  return { slugs, hasSnakeCase, errors };
}

function extractDoc(doc) {
  if (doc == null) return { slugs: [], errors: [`${DOC_PATH} must stay present`] };
  const errors = [];
  const effIdx = doc.indexOf("`effective`");
  if (effIdx < 0) {
    errors.push(`${DOC_PATH} must document the \`effective\` field whose closed set this guard binds`);
    return { slugs: [], errors };
  }
  const after = doc.slice(effIdx, effIdx + 600);
  const clause = after.match(/one of\s+([^.\n—]*?)(?:—|\.)/);
  if (clause == null) {
    errors.push(`${DOC_PATH} must state the effective-status closed set as "one of \`live\`, ... " near the \`effective\` field`);
    return { slugs: [], errors };
  }
  const slugs = [...clause[1].matchAll(/`([a-z0-9_]+)`/g)].map((m) => m[1]);
  if (slugs.length === 0) {
    errors.push(`${DOC_PATH} must list the effective-status slugs in backticks inside the "one of ..." clause`);
  }
  return { slugs, errors };
}

function evaluate({ doc, code }) {
  const errors = [];
  const fail = (message) => errors.push(message);

  const { slugs: codeSlugs, hasSnakeCase, errors: codeErrors } = extractCode(code ?? "");
  for (const codeError of codeErrors) fail(codeError);

  const { slugs: docSlugs, errors: docErrors } = extractDoc(doc);
  for (const docError of docErrors) fail(docError);

  if (codeSlugs.length === 0 || docSlugs.length === 0) {
    return errors;
  }

  const codeSet = new Set(codeSlugs);
  const docSet = new Set(docSlugs);
  for (const slug of codeSlugs) {
    if (!docSet.has(slug)) fail(`CapabilityEffectiveStatus has variant "${slug}" but docs/capabilities.md omits it from the effective-status closed set`);
  }
  for (const slug of docSlugs) {
    if (!codeSet.has(slug)) fail(`docs/capabilities.md lists effective status "${slug}" but CapabilityEffectiveStatus has no such variant; the published closed set names a status the daemon never produces`);
  }

  return errors;
}

function goodInputs() {
  return {
    code: [
      "/// dominates `Exhausted`.",
      "#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]",
      '#[serde(rename_all = "snake_case")]',
      `pub enum ${ENUM_NAME} {`,
      "    /// Authorizes now.",
      "    Live,",
      "    /// Past its expiry.",
      "    Expired,",
      "    /// Revoked.",
      "    Revoked,",
      "    /// Budget fully spent.",
      "    Exhausted,",
      "}",
    ].join("\n"),
    doc: [
      "`effective` is the daemon's own verdict on whether the grant would authorize an action right now — one of `live`, `expired`, `revoked`, or `exhausted` — computed with the daemon clock.",
    ].join("\n"),
  };
}

function runSelfTest() {
  const failures = [];

  if (evaluate(goodInputs()).length > 0) {
    failures.push(`good fixture should pass but reported: ${evaluate(goodInputs()).join("; ")}`);
  }

  const badCases = [
    ["doc removed", (i) => { i.doc = null; }],
    ["doc omits a status", (i) => { i.doc = i.doc.replace("`expired`, ", ""); }],
    ["doc adds a phantom status", (i) => { i.doc = i.doc.replace("`exhausted`", "`exhausted`, or `pending`"); }],
    ["doc renames a status slug", (i) => { i.doc = i.doc.replace("`exhausted`", "`spent`"); }],
    ["doc missing the one-of clause", (i) => { i.doc = i.doc.replace("one of `live`, `expired`, `revoked`, or `exhausted`", "an effective verdict"); }],
    ["doc missing the effective anchor", (i) => { i.doc = i.doc.replace("`effective`", "the-status"); }],
    ["code adds a fifth variant the doc lacks", (i) => { i.code = i.code.replace("    Exhausted,", "    Exhausted,\n    /// Pending reconciliation.\n    Pending,"); }],
    ["code renames a variant", (i) => { i.code = i.code.replace("    Exhausted,", "    Spent,"); }],
    ["code enum emptied (parser drift)", (i) => { i.code = i.code.replace(/    [A-Z][A-Za-z0-9]*,/g, ""); }],
    ["code missing the enum", (i) => { i.code = i.code.replace(`enum ${ENUM_NAME}`, "enum Renamed"); }],
    ["code drops the serde rename_all", (i) => { i.code = i.code.replace('#[serde(rename_all = "snake_case")]\n', ""); }],
    ["code flips serde to camelCase", (i) => { i.code = i.code.replace('rename_all = "snake_case"', 'rename_all = "camelCase"'); }],
  ];

  for (const [label, mutate] of badCases) {
    const input = goodInputs();
    mutate(input);
    if (evaluate(input).length === 0) {
      failures.push(`bad fixture "${label}" should have been rejected but passed`);
    }
  }

  return failures;
}

function readText(relativePath) {
  try {
    return readFileSync(join(repoRoot, relativePath), "utf8");
  } catch {
    return null;
  }
}

const args = new Set(process.argv.slice(2));
for (const arg of args) {
  if (!["--self-test", "--help", "-h"].includes(arg)) {
    console.error("usage: validate-capabilities-doc-effective-status [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-capabilities-doc-effective-status [--self-test]\n\nBinds docs/capabilities.md's effective-status closed set to the CapabilityEffectiveStatus enum in covenant-ipc.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-capabilities-doc-effective-status: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-capabilities-doc-effective-status: self-test ok");
  process.exit(0);
}

const errors = evaluate({ doc: readText(DOC_PATH), code: readText(CODE_PATH) });
if (errors.length > 0) {
  console.error("validate-capabilities-doc-effective-status: capabilities.md effective-status set drifted from CapabilityEffectiveStatus");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-capabilities-doc-effective-status: ok");
