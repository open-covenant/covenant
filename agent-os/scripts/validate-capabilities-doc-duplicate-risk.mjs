#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// docs/capabilities.md documents the a2a.repair.requeue scope field
// `duplicate_risk` as a closed set: the grant posture is either `idempotent` or
// `operator-accepted`, and the daemon also accepts the wire spelling
// `operator_accepted`. The source of truth is the A2ADuplicateRisk enum in
// covenant-a2a/src/lib.rs (serde snake_case wire form) and the
// parse_duplicate_risk CLI parser in covenant/src/main.rs (which accepts the
// hyphen and underscore spellings). Nothing binds the doc to either: a third
// posture variant would ship undocumented against a doc claiming a closed set,
// a dropped #[serde(rename_all = "snake_case")] would silently change the wire
// slug while the doc kept the old spelling, and a newly accepted CLI spelling
// would ship undocumented. This guard extracts the enum variants (snake_cased)
// and the parse_duplicate_risk accepted spellings from the code (never
// hard-coded) and asserts the doc's `duplicate_risk` line lists exactly that
// union. Reads only committed files; fails loud on any empty extraction.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const DOC_PATH = "docs/capabilities.md";
const ENUM_PATH = "agent-os/crates/covenant-a2a/src/lib.rs";
const PARSER_PATH = "agent-os/crates/covenant/src/main.rs";
const ENUM_NAME = "A2ADuplicateRisk";

function sliceBraced(source, needle) {
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

function extractEnum(source) {
  const errors = [];
  if (!source.includes(`enum ${ENUM_NAME}`)) {
    return { slugs: [], hasSnakeCase: false, errors: [`could not locate enum ${ENUM_NAME} in ${ENUM_PATH}; the duplicate-risk type moved out from under this guard`] };
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
    errors.push(`enum ${ENUM_NAME} must retain #[serde(rename_all = "snake_case")] or the wire slug drifts from the doc set`);
  }

  const body = sliceBraced(source, `enum ${ENUM_NAME}`);
  const variants = body == null ? [] : [...body.matchAll(/^\s+([A-Z][A-Za-z0-9]*)\s*,\s*$/gm)].map((m) => m[1]);
  if (variants.length === 0) {
    errors.push(`found no variants in enum ${ENUM_NAME} in ${ENUM_PATH}; the parser drifted out from under this guard`);
  }
  return { slugs: variants.map(snakeCase), hasSnakeCase, errors };
}

function extractParser(source) {
  const errors = [];
  const body = sliceBraced(source, "fn parse_duplicate_risk");
  if (body == null) {
    return { spellings: [], errors: [`could not locate fn parse_duplicate_risk in ${PARSER_PATH}; the duplicate-risk CLI parser moved out from under this guard`] };
  }
  const spellings = [...body.matchAll(/"([a-z0-9_-]+)"/g)].map((m) => m[1]);
  if (spellings.length === 0) {
    errors.push(`found no accepted spellings in parse_duplicate_risk in ${PARSER_PATH}; the parser drifted out from under this guard`);
  }
  return { spellings, errors };
}

function extractDoc(doc) {
  if (doc == null) return { slugs: [], errors: [`${DOC_PATH} must stay present`] };
  const errors = [];
  const line = doc.split("\n").find((l) => l.includes("`duplicate_risk`"));
  if (line == null) {
    errors.push(`${DOC_PATH} must document the \`duplicate_risk\` scope field whose closed set this guard binds`);
    return { slugs: [], errors };
  }
  const slugs = [...line.matchAll(/`([a-z0-9_-]+)`/g)]
    .map((m) => m[1])
    .filter((s) => s !== "duplicate_risk");
  if (slugs.length === 0) {
    errors.push(`${DOC_PATH} must list the duplicate-risk postures in backticks on the \`duplicate_risk\` line`);
  }
  return { slugs, errors };
}

function evaluate({ doc, enumCode, parserCode }) {
  const errors = [];
  const fail = (message) => errors.push(message);

  const { slugs: enumSlugs, errors: enumErrors } = extractEnum(enumCode ?? "");
  for (const e of enumErrors) fail(e);

  const { spellings, errors: parserErrors } = extractParser(parserCode ?? "");
  for (const e of parserErrors) fail(e);

  const { slugs: docSlugs, errors: docErrors } = extractDoc(doc);
  for (const e of docErrors) fail(e);

  if (enumSlugs.length === 0 || spellings.length === 0 || docSlugs.length === 0) {
    return errors;
  }

  const codeSet = new Set([...enumSlugs, ...spellings]);
  const docSet = new Set(docSlugs);
  for (const slug of codeSet) {
    if (!docSet.has(slug)) fail(`A2ADuplicateRisk or parse_duplicate_risk accepts "${slug}" but docs/capabilities.md omits it from the duplicate-risk set`);
  }
  for (const slug of docSlugs) {
    if (!codeSet.has(slug)) fail(`docs/capabilities.md lists duplicate-risk posture "${slug}" but neither A2ADuplicateRisk nor parse_duplicate_risk accepts it; the published set names a posture the daemon never accepts`);
  }

  return errors;
}

function goodInputs() {
  return {
    enumCode: [
      "#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]",
      '#[serde(rename_all = "snake_case")]',
      `pub enum ${ENUM_NAME} {`,
      "    /// Safe to retry.",
      "    Idempotent,",
      "    /// Operator signed off.",
      "    OperatorAccepted,",
      "}",
    ].join("\n"),
    parserCode: [
      "fn parse_duplicate_risk(value: &str) -> Result<A2ADuplicateRisk> {",
      "    match value {",
      '        "idempotent" => Ok(A2ADuplicateRisk::Idempotent),',
      '        "operator-accepted" | "operator_accepted" => Ok(A2ADuplicateRisk::OperatorAccepted),',
      '        other => bail!("unknown duplicate risk \'{other}\' (expected idempotent|operator-accepted)"),',
      "    }",
      "}",
    ].join("\n"),
    doc: [
      "- `duplicate_risk` narrows `a2a.repair.requeue` posture and should be either `idempotent` or `operator-accepted`; the daemon also accepts the wire spelling `operator_accepted`.",
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
    ["doc omits a posture", (i) => { i.doc = i.doc.replace("or `operator-accepted`", ""); }],
    ["doc adds a phantom posture", (i) => { i.doc = i.doc.replace("`operator-accepted`", "`operator-accepted`, `pending`"); }],
    ["doc renames a posture slug", (i) => { i.doc = i.doc.replace("`operator_accepted`", "`operator_ok`"); }],
    ["doc drops the value list", (i) => { i.doc = i.doc.replace("should be either `idempotent` or `operator-accepted`; the daemon also accepts the wire spelling `operator_accepted`", "narrows the posture"); }],
    ["doc missing the duplicate_risk anchor", (i) => { i.doc = i.doc.replace("`duplicate_risk`", "`posture`"); }],
    ["code enum adds a variant the doc lacks", (i) => { i.enumCode = i.enumCode.replace("    OperatorAccepted,", "    OperatorAccepted,\n    /// Escalated.\n    OperatorEscalated,"); }],
    ["code enum renames a variant", (i) => { i.enumCode = i.enumCode.replace("OperatorAccepted", "OperatorApproved"); }],
    ["code enum emptied (parser drift)", (i) => { i.enumCode = i.enumCode.replace(/    [A-Z][A-Za-z0-9]*,/g, ""); }],
    ["code missing the enum", (i) => { i.enumCode = i.enumCode.replace(`enum ${ENUM_NAME}`, "enum Renamed"); }],
    ["code drops the serde rename_all", (i) => { i.enumCode = i.enumCode.replace('#[serde(rename_all = "snake_case")]\n', ""); }],
    ["code flips serde to camelCase", (i) => { i.enumCode = i.enumCode.replace('rename_all = "snake_case"', 'rename_all = "camelCase"'); }],
    ["code parser adds a spelling the doc lacks", (i) => { i.parserCode = i.parserCode.replace('"operator-accepted" | "operator_accepted"', '"operator-accepted" | "operator_accepted" | "op-accepted"'); }],
    ["code parser drops a spelling", (i) => { i.parserCode = i.parserCode.replace('"operator-accepted" | ', ""); }],
    ["code missing parse_duplicate_risk", (i) => { i.parserCode = i.parserCode.replace("fn parse_duplicate_risk", "fn renamed_parser"); }],
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
    console.error("usage: validate-capabilities-doc-duplicate-risk [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-capabilities-doc-duplicate-risk [--self-test]\n\nBinds docs/capabilities.md's duplicate-risk closed set to the A2ADuplicateRisk enum and parse_duplicate_risk spellings.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-capabilities-doc-duplicate-risk: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-capabilities-doc-duplicate-risk: self-test ok");
  process.exit(0);
}

const errors = evaluate({
  doc: readText(DOC_PATH),
  enumCode: readText(ENUM_PATH),
  parserCode: readText(PARSER_PATH),
});
if (errors.length > 0) {
  console.error("validate-capabilities-doc-duplicate-risk: capabilities.md duplicate-risk set drifted from A2ADuplicateRisk + parse_duplicate_risk");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-capabilities-doc-duplicate-risk: ok");
