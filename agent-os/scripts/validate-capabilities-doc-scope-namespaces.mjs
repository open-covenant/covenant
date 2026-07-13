#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// docs/capabilities.md publishes the grant-time scope grammar: a spelled-out
// namespace count ("Twelve namespaces are recognized at grant time") and the
// backtick slug list. The source of truth is ScopeNamespace::from_action in
// covenant-permissions/src/lib.rs, whose compile-time freeze test
// (scope_namespace_inventory_is_frozen) forces a Rust-test break on a 13th
// variant — but nothing forces the public doc to follow. This guard binds the
// doc's count word and slug list to the from_action slug literals (extracted
// from the code, never hard-coded) so a rename, an added/removed namespace, or
// a reordered list cannot leave the published security boundary stale. Reads
// only committed files; fails loud on any empty extraction.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const DOC_PATH = "docs/capabilities.md";
const CODE_PATH = "agent-os/crates/covenant-permissions/src/lib.rs";

const NUMBER_WORDS = [
  "zero", "one", "two", "three", "four", "five", "six", "seven", "eight",
  "nine", "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen",
  "sixteen", "seventeen", "eighteen", "nineteen", "twenty",
];

function numberWord(n) {
  if (n < 0 || n >= NUMBER_WORDS.length) return null;
  return NUMBER_WORDS[n];
}

function sliceFnBody(source, fnName) {
  const start = source.indexOf(fnName);
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

function extractCode(source) {
  const errors = [];
  const body = sliceFnBody(source, "fn from_action");
  if (body == null) {
    return { slugs: [], errors: [`could not locate fn from_action in ${CODE_PATH}; the namespace grammar moved out from under this guard`] };
  }
  const slugs = [...body.matchAll(/starts_with\("([a-z0-9_]+)\."\)/g)].map((m) => m[1]);
  if (slugs.length === 0) {
    errors.push(`found no starts_with("<slug>.") arms in from_action in ${CODE_PATH}; the namespace grammar drifted out from under this guard`);
  }
  return { slugs, errors };
}

function extractDoc(doc) {
  if (doc == null) return { countWord: null, slugs: [], errors: [`${DOC_PATH} must stay present`] };
  const errors = [];
  const heading = doc.match(/([A-Za-z]+)\s+namespaces\s+are\s+recognized\s+at\s+grant\s+time:/);
  if (heading == null) {
    errors.push(`${DOC_PATH} must keep an "<N> namespaces are recognized at grant time:" line pinning the namespace count`);
  }
  const countWord = heading?.[1] ?? null;
  const after = heading ? doc.slice(heading.index + heading[0].length) : "";
  const lines = after.split("\n");
  const block = [];
  let started = false;
  for (const ln of lines) {
    if (ln.trim() === "") {
      if (started) break;
      continue;
    }
    started = true;
    block.push(ln);
  }
  const slugs = [...block.join("\n").matchAll(/`([a-z0-9_]+)`/g)].map((m) => m[1]);
  if (slugs.length === 0) {
    errors.push(`${DOC_PATH} must list the recognized namespace slugs in backticks immediately after the count line`);
  }
  return { countWord, slugs, errors };
}

function evaluate({ doc, code }) {
  const errors = [];
  const fail = (message) => errors.push(message);

  const { slugs: codeSlugs, errors: codeErrors } = extractCode(code ?? "");
  for (const codeError of codeErrors) fail(codeError);

  const { countWord, slugs: docSlugs, errors: docErrors } = extractDoc(doc);
  for (const docError of docErrors) fail(docError);

  if (codeSlugs.length === 0 || docSlugs.length === 0 || countWord == null) {
    return errors;
  }

  const codeSet = new Set(codeSlugs);
  const docSet = new Set(docSlugs);
  for (const slug of codeSlugs) {
    if (!docSet.has(slug)) fail(`from_action recognizes namespace "${slug}" but docs/capabilities.md omits it from the grant-time slug list`);
  }
  for (const slug of docSlugs) {
    if (!codeSet.has(slug)) fail(`docs/capabilities.md lists namespace "${slug}" but from_action does not recognize it; the published grammar names a slug the daemon no longer accepts`);
  }

  for (let i = 0; i < Math.min(codeSlugs.length, docSlugs.length); i++) {
    if (codeSlugs[i] !== docSlugs[i]) {
      fail(`docs/capabilities.md lists namespace ${i + 1} as "${docSlugs[i]}" but from_action enumerates it as "${codeSlugs[i]}"; the published order drifted`);
      break;
    }
  }

  const expected = numberWord(codeSlugs.length);
  if (expected == null) {
    fail(`from_action recognizes ${codeSlugs.length} namespaces but this guard has no English word for that count; extend NUMBER_WORDS and update the doc`);
  } else if (countWord.toLowerCase() !== expected) {
    fail(`docs/capabilities.md says "${countWord}" namespaces are recognized but from_action enumerates ${codeSlugs.length} (${expected})`);
  }

  return errors;
}

function goodInputs() {
  return {
    code: [
      "impl ScopeNamespace {",
      "    fn from_action(action: &str) -> Option<Self> {",
      '        if action.starts_with("intent.") {',
      "            Some(Self::Intent)",
      '        } else if action.starts_with("tool.") {',
      "            Some(Self::Tool)",
      '        } else if action.starts_with("memory.") {',
      "            Some(Self::Memory)",
      '        } else if action.starts_with("agent.") {',
      "            Some(Self::Agent)",
      '        } else if action.starts_with("a2a.") {',
      "            Some(Self::A2a)",
      '        } else if action.starts_with("audit.") {',
      "            Some(Self::Audit)",
      '        } else if action.starts_with("peers.") {',
      "            Some(Self::Peers)",
      '        } else if action.starts_with("identity.") {',
      "            Some(Self::Identity)",
      '        } else if action.starts_with("chain.") {',
      "            Some(Self::Chain)",
      '        } else if action.starts_with("settlement.") {',
      "            Some(Self::Settlement)",
      '        } else if action.starts_with("x402.") {',
      "            Some(Self::X402)",
      '        } else if action.starts_with("secret.") {',
      "            Some(Self::Secret)",
      "        } else {",
      "            None",
      "        }",
      "    }",
      "}",
    ].join("\n"),
    doc: [
      "The leading segment selects the scope validator. Twelve namespaces are recognized at grant time:",
      "",
      "`intent`, `tool`, `memory`, `agent`, `a2a`, `audit`, `peers`, `identity`, `chain`, `settlement`, `x402`, `secret`.",
      "",
      "`capabilities.*` is enforced only at dispatch.",
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
    ["doc count word wrong", (i) => { i.doc = i.doc.replace("Twelve", "Thirteen"); }],
    ["doc omits a slug", (i) => { i.doc = i.doc.replace("`tool`, ", ""); }],
    ["doc adds a phantom slug", (i) => { i.doc = i.doc.replace("`secret`.", "`secret`, `bogus`."); }],
    ["doc list reordered", (i) => { i.doc = i.doc.replace("`intent`, `tool`", "`tool`, `intent`"); }],
    ["doc slug renamed", (i) => { i.doc = i.doc.replace("`secret`", "`secrets`"); }],
    ["doc missing recognized line", (i) => { i.doc = i.doc.replace("Twelve namespaces are recognized at grant time:", "Namespaces are recognized at grant time:"); }],
    ["doc missing list", (i) => { i.doc = i.doc.replace("`intent`, `tool`, `memory`, `agent`, `a2a`, `audit`, `peers`, `identity`, `chain`, `settlement`, `x402`, `secret`.", ""); }],
    ["code adds a namespace the doc lacks", (i) => { i.code = i.code.replace('Some(Self::Secret)', 'Some(Self::Secret)\n        } else if action.starts_with("relay.") {\n            Some(Self::Relay)'); }],
    ["code renames a slug the doc lacks", (i) => { i.code = i.code.replace('starts_with("secret.")', 'starts_with("secrets.")'); }],
    ["code from_action emptied (parser drift)", (i) => { i.code = i.code.replace(/starts_with\("[a-z0-9_]+\."\)/g, "matches(dead)"); }],
    ["code missing from_action", (i) => { i.code = i.code.replace("fn from_action", "fn renamed_action"); }],
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
    console.error("usage: validate-capabilities-doc-scope-namespaces [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-capabilities-doc-scope-namespaces [--self-test]\n\nBinds docs/capabilities.md's grant-time namespace count and slug list to ScopeNamespace::from_action in covenant-permissions.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-capabilities-doc-scope-namespaces: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-capabilities-doc-scope-namespaces: self-test ok");
  process.exit(0);
}

const errors = evaluate({ doc: readText(DOC_PATH), code: readText(CODE_PATH) });
if (errors.length > 0) {
  console.error("validate-capabilities-doc-scope-namespaces: capabilities.md namespace grammar drifted from from_action");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-capabilities-doc-scope-namespaces: ok");
