#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// docs/ipc-and-http-gateway.md cites the A2AIdempotency struct as
// `{duplicate_safety: "unsafe"|"idempotent", key: string}`, documenting the
// duplicate-safety closed set supervisors and JSON consumers route on. The
// source of truth is the A2ADuplicateSafety enum in covenant-a2a/src/lib.rs
// (serde snake_case). The existing idempotency field-list guard hardcodes the
// two slugs in its regex; nothing derives the set from the enum. So a third
// safety variant, or a dropped #[serde(rename_all = "snake_case")], could leave
// the doc and the hardcoded regex agreeing with each other but both drifted
// from the enum. This guard extracts the enum variants and their serde
// snake_case rename from the code (never hard-coded) and asserts the doc's
// `{duplicate_safety: "..."|"..."}` citation matches exactly. Reads only
// committed files; fails loud on any empty extraction.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const DOC_PATH = "docs/ipc-and-http-gateway.md";
const CODE_PATH = "agent-os/crates/covenant-a2a/src/lib.rs";
const ENUM_NAME = "A2ADuplicateSafety";

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
    return { slugs: [], hasSnakeCase: false, errors: [`could not locate enum ${ENUM_NAME} in ${CODE_PATH}; the duplicate-safety type moved out from under this guard`] };
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
  const m = doc.match(/\{duplicate_safety:\s*("[a-z_]+"(?:\|"[a-z_]+")*)/);
  if (m == null) {
    errors.push(`${DOC_PATH} must cite the A2ADuplicateSafety closed set as \`{duplicate_safety: "unsafe"|"idempotent", ...}\` next to the A2AIdempotency struct`);
    return { slugs: [], errors };
  }
  const slugs = [...m[1].matchAll(/"([a-z_]+)"/g)].map((x) => x[1]);
  if (slugs.length === 0) {
    errors.push(`${DOC_PATH} must list the duplicate-safety slugs in double quotes inside the {duplicate_safety: ...} citation`);
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
    if (!docSet.has(slug)) fail(`A2ADuplicateSafety has variant "${slug}" but docs/ipc-and-http-gateway.md omits it from the {duplicate_safety: ...} closed set`);
  }
  for (const slug of docSlugs) {
    if (!codeSet.has(slug)) fail(`docs/ipc-and-http-gateway.md lists duplicate-safety "${slug}" but A2ADuplicateSafety has no such variant; the published set names a posture the daemon never produces`);
  }

  return errors;
}

function goodInputs() {
  return {
    code: [
      "#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]",
      '#[serde(rename_all = "snake_case")]',
      `pub enum ${ENUM_NAME} {`,
      "    /// Not safe to retry.",
      "    Unsafe,",
      "    /// Safe to retry.",
      "    Idempotent,",
      "}",
    ].join("\n"),
    doc: [
      "- `idempotency` (object, omitted when null) — optional `A2AIdempotency` `{duplicate_safety: \"unsafe\"|\"idempotent\", key: string}` (defined at `covenant-a2a/src/lib.rs:53-57`).",
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
    ["doc omits a posture", (i) => { i.doc = i.doc.replace('"unsafe"|', ""); }],
    ["doc adds a phantom posture", (i) => { i.doc = i.doc.replace('"idempotent"', '"idempotent"|"pending"'); }],
    ["doc renames a posture slug", (i) => { i.doc = i.doc.replace('"idempotent"', '"safe"'); }],
    ["doc drops the citation braces", (i) => { i.doc = i.doc.replace("{duplicate_safety:", "duplicate_safety:"); }],
    ["code adds a variant the doc lacks", (i) => { i.code = i.code.replace("    Idempotent,", "    Idempotent,\n    /// Operator judgment.\n    OperatorJudgment,"); }],
    ["code renames a variant", (i) => { i.code = i.code.replace("Idempotent", "Safe"); }],
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
    console.error("usage: validate-ipc-http-doc-duplicate-safety [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-ipc-http-doc-duplicate-safety [--self-test]\n\nBinds docs/ipc-and-http-gateway.md's duplicate-safety closed set to the A2ADuplicateSafety enum in covenant-a2a.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-ipc-http-doc-duplicate-safety: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-ipc-http-doc-duplicate-safety: self-test ok");
  process.exit(0);
}

const errors = evaluate({ doc: readText(DOC_PATH), code: readText(CODE_PATH) });
if (errors.length > 0) {
  console.error("validate-ipc-http-doc-duplicate-safety: ipc-and-http-gateway.md duplicate-safety set drifted from A2ADuplicateSafety");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-ipc-http-doc-duplicate-safety: ok");
