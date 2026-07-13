#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// docs/ipc-and-http-gateway.md documents the `PrivilegedAction.outcome` field as
// a closed set of nine slugs (authorized, denied, granted, grant_rejected,
// revoked, revoke_noop, scope_rejected, revoke_rejected, budget_exhausted) that
// operators filter provenance by. The set is not a Rust enum — it is produced by
// the match arms of `project_privileged_actions` in
// agent-os/crates/covenant-audit/src/lib.rs, where each arm hands an outcome
// literal to the `row(...)` helper paired with a `capability_*` kind literal.
// Nothing binds the doc's closed set to those arms: a tenth outcome arm would
// ship undocumented against a doc claiming a closed set, and a doc that named a
// phantom outcome would publish a filter value the daemon never emits. This
// guard extracts the outcome literals from the projection (never hard-coded) by
// their pairing with the following `capability_*` kind literal — plus the
// `if *removed { ... } else { ... }` branch — and asserts the doc's "one of ..."
// closed set matches exactly. Reads only committed files; fails loud on any empty
// extraction.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const DOC_PATH = "docs/ipc-and-http-gateway.md";
const CODE_PATH = "agent-os/crates/covenant-audit/src/lib.rs";
const FN_NAME = "project_privileged_actions";

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
    return { outcomes: [], errors: [`could not locate fn ${FN_NAME} in ${CODE_PATH}; the provenance projection moved out from under this guard`] };
  }

  // Each outcome literal is followed by its paired `capability_*` kind literal,
  // optionally closing an `if *removed { ... } else { ... }` brace first.
  const paired = new Set();
  for (const m of body.matchAll(/"([a-z0-9_]+)"\s*\}?\s*,\s*"capability_[a-z0-9_]+"/g)) {
    const slug = m[1];
    if (!slug.startsWith("capability_")) paired.add(slug);
  }
  // The first branch of the revoked/noop conditional precedes its `} else {`.
  for (const m of body.matchAll(/"([a-z0-9_]+)"\s*\}\s*else\s*\{/g)) {
    paired.add(m[1]);
  }

  if (paired.size === 0) {
    errors.push(`extracted no outcome literals from fn ${FN_NAME} in ${CODE_PATH}; the row/kind pairing this guard relies on drifted out from under it`);
  }

  return { outcomes: [...paired], errors };
}

function extractDoc(doc) {
  if (doc == null) return { slugs: [], errors: [`${DOC_PATH} must stay present`] };
  const errors = [];
  const anchor = "`outcome` is one of";
  const idx = doc.indexOf(anchor);
  if (idx < 0) {
    errors.push(`${DOC_PATH} must state the PrivilegedAction outcome closed set as "\`outcome\` is one of \`authorized\`, ..." near the provenance query`);
    return { slugs: [], errors };
  }
  const after = doc.slice(idx, idx + 400);
  const clause = after.match(/one of\s+([^.\n—]*?)(?:—|\.)/);
  if (clause == null) {
    errors.push(`${DOC_PATH} must list the outcome slugs in backticks inside the "\`outcome\` is one of ..." clause`);
    return { slugs: [], errors };
  }
  const slugs = [...clause[1].matchAll(/`([a-z0-9_]+)`/g)].map((m) => m[1]);
  if (slugs.length === 0) {
    errors.push(`${DOC_PATH} must list the outcome slugs in backticks inside the "\`outcome\` is one of ..." clause`);
  }
  return { slugs, errors };
}

function evaluate({ doc, code }) {
  const errors = [];
  const fail = (message) => errors.push(message);

  const { outcomes, errors: codeErrors } = extractCode(code);
  for (const codeError of codeErrors) fail(codeError);

  const { slugs: docSlugs, errors: docErrors } = extractDoc(doc);
  for (const docError of docErrors) fail(docError);

  if (outcomes.length === 0 || docSlugs.length === 0) {
    return errors;
  }

  const codeSet = new Set(outcomes);
  const docSet = new Set(docSlugs);
  for (const slug of outcomes) {
    if (!docSet.has(slug)) fail(`project_privileged_actions emits outcome "${slug}" but docs/ipc-and-http-gateway.md omits it from the outcome closed set`);
  }
  for (const slug of docSlugs) {
    if (!codeSet.has(slug)) fail(`docs/ipc-and-http-gateway.md lists outcome "${slug}" but project_privileged_actions never emits it; the published filter value matches no real row`);
  }

  return errors;
}

function goodInputs() {
  return {
    code: [
      `pub fn ${FN_NAME}(event: &AuditEvent) -> Vec<PrivilegedAction> {`,
      "    let row = |actor, action, approver, rule, outcome: &str, kind: &str| PrivilegedAction {",
      "        outcome: outcome.to_string(),",
      "        kind: kind.to_string(),",
      "    };",
      "    match &event.kind {",
      "        AuditKind::CapabilityCheck { .. } => {",
      "            row(a, action, approver, rule, \"authorized\", \"capability_check\");",
      "            row(a, action, None, None, \"denied\", \"capability_check\");",
      "        }",
      "        AuditKind::CapabilityGranted { .. } => vec![row(a, action, approver, rule, \"granted\", \"capability_granted\")],",
      "        AuditKind::CapabilityGrantRejected { .. } => vec![row(a, action, None, None, \"grant_rejected\", \"capability_grant_rejected\")],",
      "        AuditKind::CapabilityRevoked { removed, .. } => vec![row(a, action, None, rule, if *removed { \"revoked\" } else { \"revoke_noop\" }, \"capability_revoked\")],",
      "        AuditKind::CapabilityScopeRejected { .. } => vec![row(a, action, None, None, \"scope_rejected\", \"capability_scope_rejected\")],",
      "        AuditKind::CapabilityRevokeRejected { .. } => vec![row(a, action, None, rule, \"revoke_rejected\", \"capability_revoke_rejected\")],",
      "        AuditKind::CapabilityBudgetExhausted { .. } => vec![row(a, action, None, rule, \"budget_exhausted\", \"capability_budget_exhausted\")],",
      "        _ => Vec::new(),",
      "    }",
      "}",
    ].join("\n"),
    doc: [
      "Each `PrivilegedAction` carries `event_id`, `kind`, `actor`, and `outcome`, where `outcome` is one of `authorized`, `denied`, `granted`, `grant_rejected`, `revoked`, `revoke_noop`, `scope_rejected`, `revoke_rejected`, or `budget_exhausted`.",
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
    ["doc omits an outcome", (i) => { i.doc = i.doc.replace("`revoked`, ", ""); }],
    ["doc adds a phantom outcome", (i) => { i.doc = i.doc.replace("`budget_exhausted`", "`budget_exhausted`, or `pending`"); }],
    ["doc renames an outcome slug", (i) => { i.doc = i.doc.replace("`budget_exhausted`", "`spent`"); }],
    ["doc missing the one-of clause", (i) => { i.doc = i.doc.replace("`outcome` is one of `authorized`, `denied`, `granted`, `grant_rejected`, `revoked`, `revoke_noop`, `scope_rejected`, `revoke_rejected`, or `budget_exhausted`", "`outcome` is a verdict"); }],
    ["doc missing the outcome anchor", (i) => { i.doc = i.doc.replace("`outcome` is one of", "the-result is one of"); }],
    ["code adds a tenth outcome arm the doc lacks", (i) => { i.code = i.code.replace("\"budget_exhausted\", \"capability_budget_exhausted\")],", "\"budget_exhausted\", \"capability_budget_exhausted\")],\n        AuditKind::CapabilityPending { .. } => vec![row(a, action, None, rule, \"pending\", \"capability_pending\")],"); }],
    ["code renames an outcome literal", (i) => { i.code = i.code.replace("\"budget_exhausted\", \"capability_budget_exhausted\"", "\"spent\", \"capability_budget_exhausted\""); }],
    ["code drops the revoked conditional to a single arm", (i) => { i.code = i.code.replace("if *removed { \"revoked\" } else { \"revoke_noop\" }", "\"revoked\""); }],
    ["code projection renamed (parser drift)", (i) => { i.code = i.code.replace(`fn ${FN_NAME}`, "fn renamed_projection"); }],
    ["code outcome literals stripped (pairing drift)", (i) => { i.code = i.code.replace(/"([a-z0-9_]+)"\s*\}?\s*,\s*"capability_[a-z0-9_]+"/g, "X, \"capability_kind\""); }],
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
    console.error("usage: validate-ipc-http-doc-provenance-outcome [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-ipc-http-doc-provenance-outcome [--self-test]\n\nBinds docs/ipc-and-http-gateway.md's PrivilegedAction outcome closed set to the match arms of project_privileged_actions in covenant-audit.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-ipc-http-doc-provenance-outcome: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-ipc-http-doc-provenance-outcome: self-test ok");
  process.exit(0);
}

const errors = evaluate({ doc: readText(DOC_PATH), code: readText(CODE_PATH) });
if (errors.length > 0) {
  console.error("validate-ipc-http-doc-provenance-outcome: ipc-and-http-gateway.md outcome set drifted from project_privileged_actions");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-ipc-http-doc-provenance-outcome: ok");
