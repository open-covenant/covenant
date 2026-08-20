#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// The daemon must re-adopt in-flight subprocess pids left by a prior instance
// before the projection-tick driver starts, or the first budget-preemption
// ticks run against a half-populated tracker and a restart-recovered,
// over-budget subprocess survives un-preempted. covenantd/src/main.rs encodes
// that as: construct the tracker with_persistence -> recover() -> spawn the
// projection tick driver. A code comment promises the ordering; this guard
// enforces it so a refactor cannot silently reorder the startup wiring. It
// reads only committed source.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const MAIN_PATH = "agent-os/crates/covenantd/src/main.rs";
const WITH_PERSISTENCE = "SubprocessTracker::with_persistence(";
const RECOVER = "subprocess_tracker.recover()";
const TICK_DRIVER = "spawn_projection_tick_driver(";

function firstIndexOf(source, needle) {
  return source.split("\n").findIndex((line) => line.includes(needle));
}

function evaluate({ source }) {
  const errors = [];
  const fail = (message) => errors.push(message);

  if (source == null) {
    fail(`${MAIN_PATH} must stay present`);
    return errors;
  }

  const persistence = firstIndexOf(source, WITH_PERSISTENCE);
  const recover = firstIndexOf(source, RECOVER);
  const driver = firstIndexOf(source, TICK_DRIVER);

  if (persistence < 0) {
    fail("daemon must construct the subprocess tracker with_persistence so recover() has a snapshot to re-adopt");
  }
  if (recover < 0) {
    fail("daemon must call subprocess_tracker.recover() on startup");
  }
  if (driver < 0) {
    fail("daemon must spawn the projection tick driver");
  }

  if (persistence >= 0 && recover >= 0 && persistence > recover) {
    fail("SubprocessTracker::with_persistence(...) must be constructed before recover() so recover() reads persisted pids");
  }
  if (recover >= 0 && driver >= 0 && recover > driver) {
    fail("subprocess_tracker.recover() must run before spawn_projection_tick_driver(...) so restart-recovered pids are preemptable from the first tick");
  }

  return errors;
}

function goodInputs() {
  return {
    source: [
      "fn main() {",
      "    let subprocess_tracker = Arc::new(covenant_runtime::SubprocessTracker::with_persistence(",
      '        home.join("runtime").join("subprocess-tracker.jsonl"),',
      "    ));",
      "    subprocess_tracker.recover();",
      "    // ... server wiring ...",
      "    covenantd::spawn_projection_tick_driver(server.clone(), projection_tick);",
      "}",
    ].join("\n"),
  };
}

function runSelfTest() {
  const failures = [];

  if (evaluate(goodInputs()).length > 0) {
    failures.push(`good fixture should pass but reported: ${evaluate(goodInputs()).join("; ")}`);
  }

  const badCases = [
    ["main.rs missing", (i) => (i.source = null)],
    ["tracker built without persistence", (i) => (i.source = i.source.replace(WITH_PERSISTENCE, "SubprocessTracker::new("))],
    ["recover() removed", (i) => (i.source = i.source.replace("    subprocess_tracker.recover();\n", ""))],
    ["tick driver removed", (i) => (i.source = i.source.replace(/.*spawn_projection_tick_driver\(.*\n/, ""))],
    [
      "recover() moved after the tick driver",
      (i) =>
        (i.source = [
          "fn main() {",
          "    let subprocess_tracker = Arc::new(covenant_runtime::SubprocessTracker::with_persistence(path));",
          "    covenantd::spawn_projection_tick_driver(server.clone(), projection_tick);",
          "    subprocess_tracker.recover();",
          "}",
        ].join("\n")),
    ],
    [
      "persistence constructed after recover()",
      (i) =>
        (i.source = [
          "fn main() {",
          "    subprocess_tracker.recover();",
          "    let t = SubprocessTracker::with_persistence(path);",
          "    covenantd::spawn_projection_tick_driver(server.clone(), projection_tick);",
          "}",
        ].join("\n")),
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
    console.error("usage: validate-daemon-subprocess-recover-before-tick [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-daemon-subprocess-recover-before-tick [--self-test]\n\nEnforces the daemon startup ordering: tracker with_persistence -> recover() -> projection tick driver.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-daemon-subprocess-recover-before-tick: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-daemon-subprocess-recover-before-tick: self-test ok");
  process.exit(0);
}

const errors = evaluate({ source: readText(MAIN_PATH) });
if (errors.length > 0) {
  console.error("validate-daemon-subprocess-recover-before-tick: daemon startup ordering drifted");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-daemon-subprocess-recover-before-tick: ok");
