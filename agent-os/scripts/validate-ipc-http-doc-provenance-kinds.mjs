#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// docs/ipc-and-http-gateway.md states that QueryProvenance projects "the
// capability family of audit kinds" and enumerates exactly seven PascalCase
// kinds (CapabilityCheck, CapabilityGranted, CapabilityGrantRejected,
// CapabilityRevoked, CapabilityScopeRejected, CapabilityRevokeRejected,
// CapabilityBudgetExhausted). The handled family is not a Rust enum — it is the
// set of `AuditKind::Capability*` match arms in `project_privileged_actions` in
// agent-os/crates/covenant-audit/src/lib.rs (every other kind falls through to
// `_ => Vec::new()`). Nothing binds the doc's parenthetical to those arms: a new
// capability kind added to the projection (even one reusing an existing outcome
// like `denied`, which the outcome guard cannot see) would ship undocumented, and
// a doc that named a phantom kind would publish a family the daemon never
// projects. This guard extracts the handled `AuditKind::<Variant>` arms from the
// projection (never hard-coded) and asserts the doc's parenthetical matches
// exactly, both directions. Reads only committed files; fails loud on any empty
// extraction.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const DOC_PATH = "docs/ipc-and-http-gateway.md";
const CODE_PATH = "agent-os/crates/covenant-audit/src/lib.rs";
const FN_NAME = "project_privileged_actions";
const DOC_ANCHOR = "capability family of audit kinds";

function sliceFnBody(source, needle) {
  const start = source.indexOf(`fn ${needle}`);
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
  const body = sliceFnBody(source ?? "", FN_NAME);
  if (body == null) {
    return { kinds: [], errors: [`could not locate fn ${FN_NAME} in ${CODE_PATH}; the provenance projection moved out from under this guard`] };
  }

  const kinds = new Set();
  for (const m of body.matchAll(/AuditKind::([A-Z][A-Za-z0-9]*)/g)) {
    kinds.add(m[1]);
  }

  if (kinds.size === 0) {
    errors.push(`extracted no AuditKind arms from fn ${FN_NAME} in ${CODE_PATH}; the match this guard relies on drifted out from under it`);
  }

  return { kinds: [...kinds], errors };
}

function extractDoc(doc) {
  if (doc == null) return { kinds: [], errors: [`${DOC_PATH} must stay present`] };
  const errors = [];
  const idx = doc.indexOf(DOC_ANCHOR);
  if (idx < 0) {
    errors.push(`${DOC_PATH} must state the handled capability-kind family as "capability family of audit kinds (\`CapabilityCheck\`, ...)" near the provenance query`);
    return { kinds: [], errors };
  }
  const paren = doc.slice(idx, idx + 400);
  const list = paren.match(/\(([^)]*)\)/);
  if (list == null) {
    errors.push(`${DOC_PATH} must enumerate the capability-kind family in a parenthesized backticked list after "${DOC_ANCHOR}"`);
    return { kinds: [], errors };
  }
  const kinds = [...list[1].matchAll(/`([A-Z][A-Za-z0-9]*)`/g)].map((m) => m[1]);
  if (kinds.length === 0) {
    errors.push(`${DOC_PATH} must list the capability kinds in backticks inside the "${DOC_ANCHOR}" parenthetical`);
  }
  return { kinds, errors };
}

function evaluate({ doc, code }) {
  const errors = [];
  const fail = (message) => errors.push(message);

  const { kinds: codeKinds, errors: codeErrors } = extractCode(code);
  for (const codeError of codeErrors) fail(codeError);

  const { kinds: docKinds, errors: docErrors } = extractDoc(doc);
  for (const docError of docErrors) fail(docError);

  if (codeKinds.length === 0 || docKinds.length === 0) {
    return errors;
  }

  const codeSet = new Set(codeKinds);
  const docSet = new Set(docKinds);
  for (const kind of codeKinds) {
    if (!docSet.has(kind)) fail(`project_privileged_actions handles AuditKind::${kind} but docs/ipc-and-http-gateway.md omits it from the capability-kind family`);
  }
  for (const kind of docKinds) {
    if (!codeSet.has(kind)) fail(`docs/ipc-and-http-gateway.md lists AuditKind::${kind} in the capability-kind family but project_privileged_actions never handles it; the published family projects no real rows`);
  }

  return errors;
}

function goodInputs() {
  return {
    code: [
      `pub fn ${FN_NAME}(event: &AuditEvent) -> Vec<PrivilegedAction> {`,
      "    match &event.kind {",
      "        AuditKind::CapabilityCheck { .. } => vec![],",
      "        AuditKind::CapabilityGranted { .. } => vec![],",
      "        AuditKind::CapabilityGrantRejected { .. } => vec![],",
      "        AuditKind::CapabilityRevoked { .. } => vec![],",
      "        AuditKind::CapabilityScopeRejected { .. } => vec![],",
      "        AuditKind::CapabilityRevokeRejected { .. } => vec![],",
      "        AuditKind::CapabilityBudgetExhausted { .. } => vec![],",
      "        _ => Vec::new(),",
      "    }",
      "}",
    ].join("\n"),
    doc: [
      "It returns every privileged action in a window, projected from the capability family of audit kinds (`CapabilityCheck`, `CapabilityGranted`, `CapabilityGrantRejected`, `CapabilityRevoked`, `CapabilityScopeRejected`, `CapabilityRevokeRejected`, `CapabilityBudgetExhausted`) into uniform rows.",
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
    ["doc omits a kind", (i) => { i.doc = i.doc.replace("`CapabilityRevoked`, ", ""); }],
    ["doc adds a phantom kind", (i) => { i.doc = i.doc.replace("`CapabilityBudgetExhausted`", "`CapabilityBudgetExhausted`, `CapabilityPending`"); }],
    ["doc renames a kind", (i) => { i.doc = i.doc.replace("`CapabilityBudgetExhausted`", "`CapabilityBudgetSpent`"); }],
    ["doc missing the anchor", (i) => { i.doc = i.doc.replace("capability family of audit kinds", "the audit kinds that matter"); }],
    ["doc missing the parenthetical", (i) => { i.doc = i.doc.replace("(`CapabilityCheck`, `CapabilityGranted`, `CapabilityGrantRejected`, `CapabilityRevoked`, `CapabilityScopeRejected`, `CapabilityRevokeRejected`, `CapabilityBudgetExhausted`)", "several capability kinds"); }],
    ["code adds an eighth capability arm the doc lacks", (i) => { i.code = i.code.replace("AuditKind::CapabilityBudgetExhausted { .. } => vec![],", "AuditKind::CapabilityBudgetExhausted { .. } => vec![],\n        AuditKind::CapabilityPending { .. } => vec![row(\"denied\")],"); }],
    ["code renames a handled kind", (i) => { i.code = i.code.replace("AuditKind::CapabilityBudgetExhausted", "AuditKind::CapabilityBudgetSpent"); }],
    ["code drops a handled kind to the wildcard only", (i) => { i.code = i.code.replace("        AuditKind::CapabilityBudgetExhausted { .. } => vec![],\n", ""); }],
    ["code projection renamed (parser drift)", (i) => { i.code = i.code.replace(`fn ${FN_NAME}`, "fn renamed_projection"); }],
    ["code AuditKind arms stripped (extraction drift)", (i) => { i.code = i.code.replace(/AuditKind::/g, "Kind::"); }],
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
    console.error("usage: validate-ipc-http-doc-provenance-kinds [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-ipc-http-doc-provenance-kinds [--self-test]\n\nBinds docs/ipc-and-http-gateway.md's capability-kind family parenthetical to the AuditKind arms handled by project_privileged_actions in covenant-audit.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-ipc-http-doc-provenance-kinds: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-ipc-http-doc-provenance-kinds: self-test ok");
  process.exit(0);
}

const errors = evaluate({ doc: readText(DOC_PATH), code: readText(CODE_PATH) });
if (errors.length > 0) {
  console.error("validate-ipc-http-doc-provenance-kinds: ipc-and-http-gateway.md capability-kind family drifted from project_privileged_actions");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-ipc-http-doc-provenance-kinds: ok");
