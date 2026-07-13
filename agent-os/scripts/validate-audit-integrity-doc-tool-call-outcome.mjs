#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// docs/audit-integrity.md documents the ToolCallCompleted `outcome` field as a
// closed set — `an outcome of ok, error_result, or failed` — that separates a
// clean tool result from a tool-reported error result and from a raised
// ToolError. The source of truth is the ToolCallOutcome enum in
// covenant-audit/src/lib.rs (serde snake_case). Nothing binds the doc to the
// enum: a fourth outcome variant would ship undocumented against a doc claiming
// a closed three-value set, and a dropped #[serde(rename_all = "snake_case")]
// would silently change the wire slug while the doc kept the old outcome. This
// guard extracts the enum variants and their serde snake_case rename from the
// code (never hard-coded) and asserts the doc's "outcome of ..." closed set
// matches exactly. Reads only committed files; fails loud on any empty
// extraction.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const DOC_PATH = "docs/audit-integrity.md";
const CODE_PATH = "agent-os/crates/covenant-audit/src/lib.rs";
const ENUM_NAME = "ToolCallOutcome";

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
    return { slugs: [], hasSnakeCase: false, errors: [`could not locate enum ${ENUM_NAME} in ${CODE_PATH}; the tool-call-outcome type moved out from under this guard`] };
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
  const anchor = doc.indexOf("an `outcome` of");
  if (anchor < 0) {
    errors.push(`${DOC_PATH} must state the tool-call-outcome closed set as "an \`outcome\` of \`ok\`, ..." near the ToolCallCompleted row`);
    return { slugs: [], errors };
  }
  const after = doc.slice(anchor);
  const clause = after.match(/of (`[a-z_]+`(?:, (?:or )?`[a-z_]+`)+)/);
  if (clause == null) {
    errors.push(`${DOC_PATH} must list the tool-call-outcome slugs as a backtick list ("of \`ok\`, \`error_result\`, or \`failed\`") right after "an \`outcome\` of"`);
    return { slugs: [], errors };
  }
  const slugs = [...clause[1].matchAll(/`([a-z_]+)`/g)].map((m) => m[1]);
  if (slugs.length === 0) {
    errors.push(`${DOC_PATH} must list at least one tool-call-outcome slug in backticks inside the "outcome of ..." clause`);
  }
  return { slugs, errors };
}

function evaluate({ doc, code }) {
  const errors = [];
  const fail = (message) => errors.push(message);

  const { slugs: codeSlugs, errors: codeErrors } = extractCode(code ?? "");
  for (const codeError of codeErrors) fail(codeError);

  const { slugs: docSlugs, errors: docErrors } = extractDoc(doc);
  for (const docError of docErrors) fail(docError);

  if (codeSlugs.length === 0 || docSlugs.length === 0) {
    return errors;
  }

  const codeSet = new Set(codeSlugs);
  const docSet = new Set(docSlugs);
  for (const slug of codeSlugs) {
    if (!docSet.has(slug)) fail(`ToolCallOutcome has variant "${slug}" but docs/audit-integrity.md omits it from the outcome closed set`);
  }
  for (const slug of docSlugs) {
    if (!codeSet.has(slug)) fail(`docs/audit-integrity.md lists outcome "${slug}" but ToolCallOutcome has no such variant; the published closed set names an outcome the daemon never produces`);
  }

  return errors;
}

function goodInputs() {
  return {
    code: [
      "/// Clean result.",
      "#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]",
      '#[serde(rename_all = "snake_case")]',
      `pub enum ${ENUM_NAME} {`,
      "    /// Tool returned is_error = false.",
      "    Ok,",
      "    /// Tool returned is_error = true.",
      "    ErrorResult,",
      "    /// Tool raised a ToolError.",
      "    Failed,",
      "}",
    ].join("\n"),
    doc: [
      "`CallTool` records `ToolCallCompleted` on both paths with an `outcome` of `ok`, `error_result`, or `failed` that separates the cases.",
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
    ["doc omits an outcome", (i) => { i.doc = i.doc.replace(", `error_result`", ""); }],
    ["doc adds a phantom outcome", (i) => { i.doc = i.doc.replace("`failed`", "`failed`, or `pending`"); }],
    ["doc renames an outcome slug", (i) => { i.doc = i.doc.replace("`failed`", "`crashed`"); }],
    ["doc missing the outcome-of clause", (i) => { i.doc = i.doc.replace("of `ok`, `error_result`, or `failed`", "of some outcome"); }],
    ["doc missing the outcome anchor", (i) => { i.doc = i.doc.replace("an `outcome` of", "the verdict"); }],
    ["code adds a variant the doc lacks", (i) => { i.code = i.code.replace("    Failed,", "    Failed,\n    /// Cancelled.\n    Cancelled,"); }],
    ["code renames a variant", (i) => { i.code = i.code.replace("Failed", "Crashed"); }],
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
    console.error("usage: validate-audit-integrity-doc-tool-call-outcome [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-audit-integrity-doc-tool-call-outcome [--self-test]\n\nBinds docs/audit-integrity.md's tool-call-outcome closed set to the ToolCallOutcome enum in covenant-audit.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-audit-integrity-doc-tool-call-outcome: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-audit-integrity-doc-tool-call-outcome: self-test ok");
  process.exit(0);
}

const errors = evaluate({ doc: readText(DOC_PATH), code: readText(CODE_PATH) });
if (errors.length > 0) {
  console.error("validate-audit-integrity-doc-tool-call-outcome: audit-integrity.md outcome set drifted from ToolCallOutcome");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-audit-integrity-doc-tool-call-outcome: ok");
