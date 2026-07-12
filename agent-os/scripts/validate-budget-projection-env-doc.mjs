#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// docs/runtime-sandbox-security.md's "Budget-Driven Preempt" section publishes
// the operator contract for budget projection + hard-preempt: the COVENANT_BUDGET_*
// env vars, their default values, and the accepted projection-policy strings. The
// source of truth is covenantd/src/lib.rs (ProjectionTickConfig::default, the
// DEFAULT_PROJECTION_MIN_* consts, the env::var reads, and parse_projection_policy),
// which is exhaustively unit-tested — so a code change breaks a Rust test and forces
// a dev's attention, but nothing forces the DOC to follow. This guard binds the doc
// section to the daemon source so a rename, a new/removed policy, or a changed default
// cannot silently leave the published contract stale (worst case: an operator copies a
// policy string the daemon no longer accepts and startup hard-fails). It extracts the
// expected set FROM the code rather than hard-coding it, and reads only committed files.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const DOC_PATH = "docs/runtime-sandbox-security.md";
const CODE_PATH = "agent-os/crates/covenantd/src/lib.rs";
const SECTION_HEADING = "## Budget-Driven Preempt";

const norm = (literal) => literal.replace(/_/g, "");

function sliceItem(source, startNeedle) {
  const start = source.indexOf(startNeedle);
  if (start < 0) return null;
  const rest = source.slice(start);
  const next = rest.slice(startNeedle.length).search(/\n(?:pub\s+)?(?:fn|impl|const)\s/);
  return next < 0 ? rest : rest.slice(0, startNeedle.length + next);
}

// Pull the authoritative env vars, policy strings, and defaults straight from the
// daemon source. Any extraction that comes back empty is itself a failure: a refactor
// that moved these out from under the regexes must not let the guard pass vacuously.
function extractCode(source) {
  const errors = [];

  const envVars = [...new Set([...source.matchAll(/env::var\("(COVENANT_BUDGET_[A-Z0-9_]+)"\)/g)].map((m) => m[1]))];
  if (envVars.length === 0) {
    errors.push(`found no env::var("COVENANT_BUDGET_*") reads in ${CODE_PATH}; the projection-config parser drifted out from under this guard`);
  }

  const policyBody = sliceItem(source, "fn parse_projection_policy");
  let policies = [];
  if (policyBody == null) {
    errors.push(`could not locate fn parse_projection_policy in ${CODE_PATH}`);
  } else {
    policies = [...new Set([...policyBody.matchAll(/Some\("([a-z_]+)"\)/g)].map((m) => m[1]))];
    if (policies.length === 0) {
      errors.push("parse_projection_policy exposes no accepted Some(\"...\") policy strings; the policy grammar drifted");
    }
  }

  const defaultBlock = sliceItem(source, "impl Default for ProjectionTickConfig");
  const defaults = [];
  const takeDefault = (label, text, re) => {
    const value = text?.match(re)?.[1];
    if (value == null) {
      errors.push(`could not read the ${label} default from ${CODE_PATH}`);
    } else {
      defaults.push({ label, value: norm(value) });
    }
  };
  takeDefault("projection tick period", defaultBlock, /period_ms:\s*([0-9_]+)/);
  takeDefault("preempt grace window", defaultBlock, /grace_ms:\s*([0-9_]+)/);
  takeDefault("min observation window", source, /DEFAULT_PROJECTION_MIN_WINDOW_MS\s*:\s*u64\s*=\s*([0-9_]+)/);
  takeDefault("min debit samples", source, /DEFAULT_PROJECTION_MIN_SAMPLES\s*:\s*u32\s*=\s*([0-9_]+)/);

  return { envVars, policies, defaults, errors };
}

function extractSection(doc) {
  const start = doc.indexOf(SECTION_HEADING);
  if (start < 0) return null;
  const rest = doc.slice(start + SECTION_HEADING.length);
  const next = rest.search(/\n##\s/);
  return next < 0 ? rest : rest.slice(0, next);
}

function evaluate({ doc, code }) {
  const errors = [];
  const fail = (message) => errors.push(message);

  if (doc == null) {
    fail(`${DOC_PATH} must stay present`);
    return errors;
  }
  const section = extractSection(doc);
  if (section == null) {
    fail(`${DOC_PATH} must keep a "${SECTION_HEADING}" section documenting the projection/preempt contract`);
    return errors;
  }

  const { envVars, policies, defaults, errors: codeErrors } = extractCode(code);
  for (const codeError of codeErrors) {
    fail(codeError);
  }

  for (const envVar of envVars) {
    if (!section.includes(envVar)) {
      fail(`covenantd reads ${envVar} but the Budget-Driven Preempt doc section never names it; an operator would not know to set it`);
    }
  }
  for (const policy of policies) {
    if (!section.includes(policy)) {
      fail(`parse_projection_policy accepts COVENANT_BUDGET_PROJECTION_POLICY=${policy} but the doc section never lists it; the published accepted-policy set is stale`);
    }
  }
  for (const { label, value } of defaults) {
    if (!new RegExp(`(?<![0-9])${value}(?![0-9])`).test(section)) {
      fail(`the ${label} default is ${value} in ${CODE_PATH} but the doc section does not state it; the published default drifted`);
    }
  }

  return errors;
}

function goodInputs() {
  return {
    code: [
      "const DEFAULT_PROJECTION_MIN_WINDOW_MS: u64 = 1_000;",
      "const DEFAULT_PROJECTION_MIN_SAMPLES: u32 = 3;",
      "impl Default for ProjectionTickConfig {",
      "    fn default() -> Self {",
      "        Self {",
      "            period_ms: 250,",
      "            grace_ms: 2_000,",
      "            policy: BudgetProjectionPolicy::NoExtrapolation,",
      "        }",
      "    }",
      "}",
      "pub fn projection_tick_config_from_env() -> Result<ProjectionTickConfig> {",
      '    let _ = std::env::var("COVENANT_BUDGET_PROJECTION_TICK_MS");',
      '    let _ = std::env::var("COVENANT_BUDGET_PREEMPT_GRACE_MS");',
      '    let _ = std::env::var("COVENANT_BUDGET_PROJECTION_POLICY");',
      '    let _ = std::env::var("COVENANT_BUDGET_PROJECTION_MIN_WINDOW_MS");',
      '    let _ = std::env::var("COVENANT_BUDGET_PROJECTION_MIN_SAMPLES");',
      "    Ok(ProjectionTickConfig::default())",
      "}",
      "fn parse_projection_policy(policy: Option<&str>) -> Result<BudgetProjectionPolicy> {",
      "    match policy {",
      '        None | Some("none") | Some("no_extrapolation") => Ok(BudgetProjectionPolicy::NoExtrapolation),',
      '        Some("linear") | Some("linear_extrapolation") => Ok(linear()),',
      '        Some(other) => anyhow::bail!("must be one of none, no_extrapolation, linear, linear_extrapolation"),',
      "    }",
      "}",
    ].join("\n"),
    doc: [
      "## Budget-Driven Preempt",
      "",
      "- Tick period default 250ms via `COVENANT_BUDGET_PROJECTION_TICK_MS`; policy via",
      "  `COVENANT_BUDGET_PROJECTION_POLICY` (`none`/`no_extrapolation`, the default, or",
      "  `linear`/`linear_extrapolation`); `COVENANT_BUDGET_PROJECTION_MIN_WINDOW_MS` default",
      "  `1000` and `COVENANT_BUDGET_PROJECTION_MIN_SAMPLES` default `3` gate extrapolation.",
      "- `COVENANT_BUDGET_PREEMPT_GRACE_MS` default `2000` ms.",
      "",
      "## Security Review Checklist",
    ].join("\n"),
  };
}

function runSelfTest() {
  const failures = [];

  if (evaluate(goodInputs()).length > 0) {
    failures.push(`good fixture should pass but reported: ${evaluate(goodInputs()).join("; ")}`);
  }

  const badCases = [
    ["doc removed", (i) => (i.doc = null)],
    ["doc section removed", (i) => (i.doc = i.doc.replace("## Budget-Driven Preempt", "## Something Else"))],
    ["doc omits an env var", (i) => (i.doc = i.doc.replace("COVENANT_BUDGET_PREEMPT_GRACE_MS", "COVENANT_BUDGET_RENAMED"))],
    ["doc omits a policy string", (i) => (i.doc = i.doc.replace("no_extrapolation", "off"))],
    ["doc default drifted stale", (i) => (i.doc = i.doc.replace("`2000`", "`5000`"))],
    ["code adds an env var the doc lacks", (i) => (i.code = i.code.replace('Ok(ProjectionTickConfig::default())', 'let _ = std::env::var("COVENANT_BUDGET_PROJECTION_MAX_KILLS");\n    Ok(ProjectionTickConfig::default())'))],
    ["code renames a policy the doc lacks", (i) => (i.code = i.code.replace('Some("linear_extrapolation")', 'Some("ramp")'))],
    ["code default changed, doc stale", (i) => (i.code = i.code.replace("period_ms: 250,", "period_ms: 300,"))],
    ["code exposes no env reads (parser drift)", (i) => (i.code = i.code.replace(/std::env::var\("COVENANT_BUDGET_[A-Z_]+"\)/g, 'read_config()'))],
    ["code missing a default const", (i) => (i.code = i.code.replace("const DEFAULT_PROJECTION_MIN_SAMPLES: u32 = 3;", ""))],
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
    console.error("usage: validate-budget-projection-env-doc [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-budget-projection-env-doc [--self-test]\n\nBinds the Budget-Driven Preempt doc section's env vars, policy strings, and defaults to the covenantd projection-tick source of truth.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-budget-projection-env-doc: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-budget-projection-env-doc: self-test ok");
  process.exit(0);
}

const errors = evaluate({ doc: readText(DOC_PATH), code: readText(CODE_PATH) });
if (errors.length > 0) {
  console.error("validate-budget-projection-env-doc: doc drifted from the projection/preempt source of truth");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-budget-projection-env-doc: ok");
