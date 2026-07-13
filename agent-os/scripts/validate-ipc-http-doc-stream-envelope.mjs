#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// docs/ipc-and-http-gateway.md references the four `StreamEnvelope` wire
// discriminators (`stream_begin`, `stream_chunk`, `stream_end`, `stream_error`)
// when describing the v2 streaming frames. Nothing binds that doc list to the
// StreamEnvelope enum in agent-os/crates/covenant-ipc/src/lib.rs: a fifth
// variant would ship undocumented (the doc list silently incomplete), and a doc
// that named a phantom discriminator would publish a kind a consumer can never
// demultiplex. This guard extracts the enum variants and their serde
// snake_case rename from the code (never hard-coded) and asserts the doc's
// discriminator list matches exactly. Reads only committed files; fails loud on
// any empty extraction.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const DOC_PATH = "docs/ipc-and-http-gateway.md";
const CODE_PATH = "agent-os/crates/covenant-ipc/src/lib.rs";
const ENUM_NAME = "StreamEnvelope";
const DOC_ANCHOR = "`StreamEnvelope` variants";

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
    return { slugs: [], hasSnakeCase: false, errors: [`could not locate enum ${ENUM_NAME} in ${CODE_PATH}; the streaming envelope type moved out from under this guard`] };
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
    errors.push(`enum ${ENUM_NAME} must retain #[serde(rename_all = "snake_case")] or the wire discriminators drift from the doc list`);
  }

  const body = sliceBraces(source, `enum ${ENUM_NAME}`);
  const variants = body == null ? [] : [...body.matchAll(/^\s+([A-Z][A-Za-z0-9]*)\s*\{/gm)].map((m) => m[1]);
  if (variants.length === 0) {
    errors.push(`found no variants in enum ${ENUM_NAME} in ${CODE_PATH}; the parser drifted out from under this guard`);
  }
  const slugs = variants.map(snakeCase);
  return { slugs, hasSnakeCase, errors };
}

function extractDoc(doc) {
  if (doc == null) return { slugs: [], errors: [`${DOC_PATH} must stay present`] };
  const errors = [];
  // "`StreamEnvelope` variants" appears in more than one sentence; bind to the
  // one occurrence that actually carries the discriminator slugs, and fail loud
  // if that contract becomes ambiguous.
  const slugBearing = [];
  let cursor = 0;
  while (true) {
    const idx = doc.indexOf(DOC_ANCHOR, cursor);
    if (idx < 0) break;
    const after = doc.slice(idx, idx + 300);
    const stop = after.search(/[).\n]/);
    const window = stop < 0 ? after : after.slice(0, stop + 1);
    const slugs = [...window.matchAll(/`([a-z0-9_]+)`/g)].map((m) => m[1]).filter((s) => s !== "streamenvelope");
    if (slugs.length > 0) slugBearing.push(slugs);
    cursor = idx + DOC_ANCHOR.length;
  }
  if (slugBearing.length === 0) {
    errors.push(`${DOC_PATH} must list the ${ENUM_NAME} wire discriminators as "${DOC_ANCHOR}: \`stream_begin\`, ..." near the v2 streaming description`);
    return { slugs: [], errors };
  }
  if (slugBearing.length > 1) {
    errors.push(`${DOC_PATH} has ${slugBearing.length} slug-bearing "${DOC_ANCHOR}" mentions; consolidate the discriminator list into one closed set`);
    return { slugs: [], errors };
  }
  return { slugs: slugBearing[0], errors };
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
    if (!docSet.has(slug)) fail(`${ENUM_NAME} has variant "${slug}" but docs/ipc-and-http-gateway.md omits it from the discriminator list`);
  }
  for (const slug of docSlugs) {
    if (!codeSet.has(slug)) fail(`docs/ipc-and-http-gateway.md lists discriminator "${slug}" but ${ENUM_NAME} has no such variant; the published kind can never appear on the wire`);
  }

  return errors;
}

function goodInputs() {
  return {
    code: [
      "#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]",
      '#[serde(tag = "kind", rename_all = "snake_case")]',
      `pub enum ${ENUM_NAME} {`,
      "    StreamBegin { stream_id: Uuid },",
      "    StreamChunk { stream_id: Uuid },",
      "    StreamEnd { stream_id: Uuid },",
      "    StreamError { stream_id: Uuid },",
      "}",
    ].join("\n"),
    doc: [
      "Envelope-shape wire frames that have no natural version slot (`StreamEnvelope` variants: `stream_begin`, `stream_chunk`, `stream_end`, `stream_error`) are exempt.",
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
    ["doc omits a discriminator", (i) => { i.doc = i.doc.replace("`stream_chunk`, ", ""); }],
    ["doc adds a phantom discriminator", (i) => { i.doc = i.doc.replace("`stream_error`", "`stream_error`, `stream_cancel`"); }],
    ["doc renames a discriminator", (i) => { i.doc = i.doc.replace("`stream_error`", "`stream_fail`"); }],
    ["doc missing the anchor", (i) => { i.doc = i.doc.replace(DOC_ANCHOR, "the streaming variants"); }],
    ["doc has two slug-bearing mentions", (i) => { i.doc = i.doc + " Another `StreamEnvelope` variants: `stream_begin`, `stream_end`."; }],
    ["code adds a fifth variant the doc lacks", (i) => { i.code = i.code.replace("    StreamError { stream_id: Uuid },", "    StreamError { stream_id: Uuid },\n    StreamCancel { stream_id: Uuid },"); }],
    ["code renames a variant", (i) => { i.code = i.code.replace("    StreamError", "    StreamFail"); }],
    ["code enum emptied (parser drift)", (i) => { i.code = i.code.replace(/    [A-Z][A-Za-z0-9]* \{[^}]*\},/g, ""); }],
    ["code missing the enum", (i) => { i.code = i.code.replace(`enum ${ENUM_NAME}`, "enum Renamed"); }],
    ["code drops the serde rename_all", (i) => { i.code = i.code.replace('#[serde(tag = "kind", rename_all = "snake_case")]\n', ""); }],
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
    console.error("usage: validate-ipc-http-doc-stream-envelope [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-ipc-http-doc-stream-envelope [--self-test]\n\nBinds docs/ipc-and-http-gateway.md's StreamEnvelope discriminator list to the StreamEnvelope enum in covenant-ipc.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-ipc-http-doc-stream-envelope: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-ipc-http-doc-stream-envelope: self-test ok");
  process.exit(0);
}

const errors = evaluate({ doc: readText(DOC_PATH), code: readText(CODE_PATH) });
if (errors.length > 0) {
  console.error("validate-ipc-http-doc-stream-envelope: ipc-and-http-gateway.md discriminator list drifted from StreamEnvelope");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-ipc-http-doc-stream-envelope: ok");
