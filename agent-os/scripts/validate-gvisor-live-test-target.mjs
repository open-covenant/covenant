#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// The advisory gVisor CI workflow (.github/workflows/gvisor-live.yml) runs one
// live test by name: `cargo test -p covenant-runtime --test <file> -- --ignored
// <name>`. `--ignored` runs only #[ignore]-attributed tests, so if that test is
// renamed, removed, or loses its #[ignore], the invocation matches zero tests
// and the job exits green having tested nothing — a silent no-op gate. This
// guard binds the workflow's named target to the committed test source so that
// drift fails the gate instead. It reads only committed files.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const WORKFLOW_PATH = ".github/workflows/gvisor-live.yml";
const TESTS_DIR = "agent-os/crates/covenant-runtime/tests";

function evaluate({ workflow, resolveTest }) {
  const errors = [];
  const fail = (message) => errors.push(message);

  if (workflow == null) {
    fail(`${WORKFLOW_PATH} must stay present`);
    return errors;
  }
  if (!workflow.includes("cargo test") || !workflow.includes("covenant-runtime")) {
    fail("gvisor-live.yml must run `cargo test -p covenant-runtime`");
    return errors;
  }

  const testMatch = workflow.match(/--test\s+([A-Za-z0-9_]+)/);
  const ignoredMatch = workflow.match(/--ignored\s+([A-Za-z0-9_]+)/);
  if (!testMatch) {
    fail("gvisor-live.yml must pin a `--test <file>` target");
  }
  if (!ignoredMatch) {
    fail("gvisor-live.yml must invoke a specific `--ignored <test>` so the live gate runs a real test");
  }
  if (!testMatch || !ignoredMatch) {
    return errors;
  }

  const base = testMatch[1];
  const name = ignoredMatch[1];
  const source = resolveTest(base);
  if (source == null) {
    fail(`--test ${base} points at a missing ${TESTS_DIR}/${base}.rs`);
    return errors;
  }

  const lines = source.split("\n");
  const fnIndex = lines.findIndex((line) => new RegExp(`\\bfn\\s+${name}\\b`).test(line));
  if (fnIndex < 0) {
    fail(`gvisor-live.yml runs --ignored ${name} but no \`fn ${name}\` exists in ${TESTS_DIR}/${base}.rs; the live gate would match zero tests and pass silently`);
    return errors;
  }
  const preceding = lines.slice(Math.max(0, fnIndex - 6), fnIndex).join("\n");
  if (!/#\[\s*ignore/.test(preceding)) {
    fail(`fn ${name} must keep an #[ignore] attribute or --ignored would skip it, turning the live gate into a silent no-op`);
  }

  return errors;
}

function goodInputs() {
  return {
    workflow: [
      "      - name: run live gvisor test",
      "        run: |",
      "          cargo test -p covenant-runtime --test live_gvisor -- --ignored live_gvisor_runner_dispatches_with_runsc",
    ].join("\n"),
    resolveTest: (base) =>
      base === "live_gvisor"
        ? ['#[tokio::test]', '#[ignore = "live: needs Linux + runsc"]', "async fn live_gvisor_runner_dispatches_with_runsc() {}"].join("\n")
        : null,
  };
}

function runSelfTest() {
  const failures = [];

  if (evaluate(goodInputs()).length > 0) {
    failures.push(`good fixture should pass but reported: ${evaluate(goodInputs()).join("; ")}`);
  }

  const badCases = [
    ["workflow deleted", (i) => (i.workflow = null)],
    ["workflow runs no cargo test", (i) => (i.workflow = "run: echo hi")],
    ["workflow lost --test", (i) => (i.workflow = i.workflow.replace("--test live_gvisor ", ""))],
    ["workflow lost --ignored target", (i) => (i.workflow = i.workflow.replace(" --ignored live_gvisor_runner_dispatches_with_runsc", ""))],
    ["test file removed", (i) => (i.resolveTest = () => null)],
    [
      "test function renamed away",
      (i) => (i.resolveTest = () => ['#[tokio::test]', "#[ignore]", "async fn something_else() {}"].join("\n")),
    ],
    [
      "test lost its #[ignore]",
      (i) => (i.resolveTest = () => ["#[tokio::test]", "async fn live_gvisor_runner_dispatches_with_runsc() {}"].join("\n")),
    ],
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
    console.error("usage: validate-gvisor-live-test-target [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-gvisor-live-test-target [--self-test]\n\nBinds gvisor-live.yml's named live test to the committed test source so the advisory job cannot become a silent no-op.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-gvisor-live-test-target: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-gvisor-live-test-target: self-test ok");
  process.exit(0);
}

const errors = evaluate({
  workflow: readText(WORKFLOW_PATH),
  resolveTest: (base) => readText(`${TESTS_DIR}/${base}.rs`),
});
if (errors.length > 0) {
  console.error("validate-gvisor-live-test-target: gVisor live gate would run no real test");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-gvisor-live-test-target: ok");
