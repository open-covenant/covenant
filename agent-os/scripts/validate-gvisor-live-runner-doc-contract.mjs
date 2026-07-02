#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// The public gVisor live-runner doc (landing/app/docs/gvisor-live-runner/page.tsx)
// publishes a copy-pasteable operational contract: the exact command
// `cargo test -p covenant-runtime --test live_gvisor -- --ignored <name>` and the
// COVENANT_LIVE_* env vars a contributor must set. `validate-gvisor-live-test-target`
// binds the CI *workflow*'s copy of that command to the test source; nothing binds
// the *doc*'s copy. So a rename that drops the test, its #[ignore], or an env var
// leaves the doc pointing contributors at a command that matches zero tests (green
// no-op) or an env var the test ignores (silent skip) — either way a false green on
// the sandbox-validation path. This guard binds the doc to the committed test source
// and CI workflow. It reads only committed files, so it runs the same on a fresh
// checkout as in a full deployment.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const DOC_PATH = "landing/app/docs/gvisor-live-runner/page.tsx";
const WORKFLOW_PATH = ".github/workflows/gvisor-live.yml";
const TESTS_DIR = "agent-os/crates/covenant-runtime/tests";
const EXPECTED_CRATE = "covenant-runtime";

function envVarsRead(source) {
  const names = new Set();
  for (const match of source.matchAll(/env::var(?:_os)?\("(COVENANT_LIVE_[A-Z0-9_]+)"\)/g)) {
    names.add(match[1]);
  }
  return [...names];
}

function evaluate({ doc, workflow, resolveTest }) {
  const errors = [];
  const fail = (message) => errors.push(message);

  if (doc == null) {
    fail(`${DOC_PATH} must stay present`);
    return errors;
  }

  if (!doc.includes(`-p ${EXPECTED_CRATE}`)) {
    fail(`doc must document \`cargo test -p ${EXPECTED_CRATE}\` so the published command targets the runtime crate`);
  }

  const docTest = doc.match(/--test\s+([A-Za-z0-9_]+)/);
  const docIgnored = doc.match(/--ignored\s+([A-Za-z0-9_]+)/);
  if (!docTest) {
    fail("doc must publish a `--test <file>` target so contributors run a real test file");
  }
  if (!docIgnored) {
    fail("doc must publish a specific `--ignored <test>` so the documented run is not a silent no-op");
  }

  if (docTest && docIgnored) {
    const base = docTest[1];
    const name = docIgnored[1];
    const source = resolveTest(base);
    if (source == null) {
      fail(`doc runs --test ${base} but ${TESTS_DIR}/${base}.rs does not exist`);
    } else {
      const lines = source.split("\n");
      const fnIndex = lines.findIndex((line) => new RegExp(`\\bfn\\s+${name}\\b`).test(line));
      if (fnIndex < 0) {
        fail(`doc runs --ignored ${name} but no \`fn ${name}\` exists in ${TESTS_DIR}/${base}.rs; a contributor copy-pasting the doc would match zero tests and see a green no-op`);
      } else {
        const preceding = lines.slice(Math.max(0, fnIndex - 6), fnIndex).join("\n");
        if (!/#\[\s*ignore/.test(preceding)) {
          fail(`fn ${name} must keep an #[ignore] attribute or the documented --ignored run would skip it, turning the published command into a silent no-op`);
        }
      }
      for (const envName of envVarsRead(source)) {
        if (!doc.includes(envName)) {
          fail(`the live test reads ${envName} but the doc setup never mentions it; a contributor following the doc would not set it and the test would silently skip`);
        }
      }
    }
  }

  if (workflow != null) {
    const wfTest = workflow.match(/--test\s+([A-Za-z0-9_]+)/);
    const wfIgnored = workflow.match(/--ignored\s+([A-Za-z0-9_]+)/);
    if (wfTest && docTest && wfTest[1] !== docTest[1]) {
      fail(`doc --test ${docTest[1]} != workflow --test ${wfTest[1]}; the documented command must match what CI runs`);
    }
    if (wfIgnored && docIgnored && wfIgnored[1] !== docIgnored[1]) {
      fail(`doc --ignored ${docIgnored[1]} != workflow --ignored ${wfIgnored[1]}; the documented command must match what CI runs`);
    }
  }

  return errors;
}

function goodInputs() {
  return {
    doc: [
      "export COVENANT_LIVE_GVISOR_ROOTFS=\"$PWD/rootfs\"",
      "export COVENANT_LIVE_RUNSC=\"${COVENANT_LIVE_RUNSC:-runsc}\"",
      "cargo test -p covenant-runtime --test live_gvisor -- --ignored live_gvisor_runner_dispatches_with_runsc",
    ].join("\n"),
    workflow: "cargo test -p covenant-runtime --test live_gvisor -- --ignored live_gvisor_runner_dispatches_with_runsc",
    resolveTest: (base) =>
      base === "live_gvisor"
        ? [
            'env::var_os("COVENANT_LIVE_GVISOR_ROOTFS").map(PathBuf::from)',
            'env::var_os("COVENANT_LIVE_RUNSC")',
            "#[tokio::test]",
            '#[ignore = "live: needs Linux + runsc"]',
            "async fn live_gvisor_runner_dispatches_with_runsc() {}",
          ].join("\n")
        : null,
  };
}

function runSelfTest() {
  const failures = [];

  if (evaluate(goodInputs()).length > 0) {
    failures.push(`good fixture should pass but reported: ${evaluate(goodInputs()).join("; ")}`);
  }

  const badCases = [
    ["doc removed", (i) => (i.doc = null)],
    ["doc drops the covenant-runtime crate", (i) => (i.doc = i.doc.replace("-p covenant-runtime", "-p covenant-other"))],
    ["doc lost --test", (i) => (i.doc = i.doc.replace("--test live_gvisor ", ""))],
    ["doc lost --ignored target", (i) => (i.doc = i.doc.replace(" --ignored live_gvisor_runner_dispatches_with_runsc", ""))],
    ["test file removed", (i) => (i.resolveTest = () => null)],
    [
      "test function renamed away",
      (i) => (i.resolveTest = () => ["#[tokio::test]", "#[ignore]", "async fn something_else() {}"].join("\n")),
    ],
    [
      "test lost its #[ignore]",
      (i) => (i.resolveTest = () => ["#[tokio::test]", "async fn live_gvisor_runner_dispatches_with_runsc() {}"].join("\n")),
    ],
    ["doc omits an env var the test reads", (i) => (i.doc = i.doc.replace(/^export COVENANT_LIVE_RUNSC.*$/m, ""))],
    [
      "test reads a new env var the doc never documents",
      (i) => {
        const prev = i.resolveTest;
        i.resolveTest = (base) => {
          const src = prev(base);
          return src == null ? null : `${src}\nenv::var_os("COVENANT_LIVE_NETNS")`;
        };
      },
    ],
    ["doc command diverges from the CI workflow", (i) => (i.workflow = i.workflow.replace("live_gvisor_runner_dispatches_with_runsc", "live_gvisor_runner_dispatches_with_runsc_renamed"))],
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
    console.error("usage: validate-gvisor-live-runner-doc-contract [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-gvisor-live-runner-doc-contract [--self-test]\n\nBinds the public gVisor live-runner doc's published command and env vars to the committed test source and CI workflow so the docs cannot send contributors at a silent no-op.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-gvisor-live-runner-doc-contract: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-gvisor-live-runner-doc-contract: self-test ok");
  process.exit(0);
}

const errors = evaluate({
  doc: readText(DOC_PATH),
  workflow: readText(WORKFLOW_PATH),
  resolveTest: (base) => readText(`${TESTS_DIR}/${base}.rs`),
});
if (errors.length > 0) {
  console.error("validate-gvisor-live-runner-doc-contract: doc contract drifted from the live test");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-gvisor-live-runner-doc-contract: ok");
