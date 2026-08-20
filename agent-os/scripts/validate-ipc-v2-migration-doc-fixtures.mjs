#!/usr/bin/env node
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// docs/protocol-migrations/v2.md is the public wire contract external SDK
// authors encode against without reading the Rust source; its "Fixture files"
// section names the frozen *.v2.json golden vectors for the ADR 0010 v2
// streaming envelopes. The covenant-ipc suite only checks that at least one
// v2 fixture exists and that v2.md is present -- it never binds the doc's
// named set to the committed fixtures. So a deleted or renamed v2 fixture the
// doc still promises, or a fixture added to the frozen dir without being
// documented, both ship silently. This guard enforces bidirectional
// consistency: every *.v2.json the migration doc names exists on disk, and
// every committed *.v2.json is named in the doc. It reads only committed
// files.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const MIGRATION_DOC = "docs/protocol-migrations/v2.md";
const FIXTURE_DIR = "agent-os/crates/covenant-ipc/tests/fixtures/v2";
const V2_FIXTURE = /[A-Za-z0-9._-]+\.v2\.json/g;

function documentedFixtures(doc) {
  return new Set(doc.match(V2_FIXTURE) ?? []);
}

function evaluate({ doc, fixtureFiles }) {
  const errors = [];
  const fail = (message) => errors.push(message);

  if (doc == null) {
    fail(`${MIGRATION_DOC} must stay present -- it is the public v2 wire contract`);
    return errors;
  }
  if (fixtureFiles == null) {
    fail(`${FIXTURE_DIR} must stay present -- it holds the committed v2 fixtures`);
    return errors;
  }

  const documented = documentedFixtures(doc);
  const onDisk = new Set(fixtureFiles.filter((name) => name.endsWith(".v2.json")));

  if (documented.size === 0) {
    fail(`${MIGRATION_DOC} must name the *.v2.json fixtures it freezes as the wire contract`);
  }

  for (const name of documented) {
    if (!onDisk.has(name)) {
      fail(`${MIGRATION_DOC} promises ${name} but it is missing from ${FIXTURE_DIR}/`);
    }
  }
  for (const name of onDisk) {
    if (!documented.has(name)) {
      fail(`${FIXTURE_DIR}/${name} is committed but not documented in ${MIGRATION_DOC}`);
    }
  }

  return errors;
}

function goodInputs() {
  return {
    doc: [
      "## Fixture files",
      "",
      "- `agent-os/crates/covenant-ipc/tests/fixtures/v2/stream-envelope-begin.v2.json` added.",
      "- `agent-os/crates/covenant-ipc/tests/fixtures/v2/stream-envelope-end.v2.json` added.",
    ].join("\n"),
    fixtureFiles: ["README.md", "stream-envelope-begin.v2.json", "stream-envelope-end.v2.json"],
  };
}

function runSelfTest() {
  const failures = [];

  if (evaluate(goodInputs()).length > 0) {
    failures.push(`good fixture should pass but reported: ${evaluate(goodInputs()).join("; ")}`);
  }

  const badCases = [
    ["migration doc missing", (i) => (i.doc = null)],
    ["fixture dir missing", (i) => (i.fixtureFiles = null)],
    ["doc names no v2 fixtures", (i) => (i.doc = "## Fixture files\n\nnone yet.")],
    [
      "doc promises a fixture absent from disk",
      (i) => (i.fixtureFiles = i.fixtureFiles.filter((n) => n !== "stream-envelope-end.v2.json")),
    ],
    [
      "committed fixture is undocumented",
      (i) => i.fixtureFiles.push("stream-envelope-error.v2.json"),
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

function readDir(relativePath) {
  try {
    return readdirSync(join(repoRoot, relativePath));
  } catch {
    return null;
  }
}

const args = new Set(process.argv.slice(2));
for (const arg of args) {
  if (!["--self-test", "--help", "-h"].includes(arg)) {
    console.error("usage: validate-ipc-v2-migration-doc-fixtures [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log(
    "usage: validate-ipc-v2-migration-doc-fixtures [--self-test]\n\nBinds docs/protocol-migrations/v2.md to the committed covenant-ipc v2 fixtures both ways.",
  );
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-ipc-v2-migration-doc-fixtures: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-ipc-v2-migration-doc-fixtures: self-test ok");
  process.exit(0);
}

const errors = evaluate({ doc: readText(MIGRATION_DOC), fixtureFiles: readDir(FIXTURE_DIR) });
if (errors.length > 0) {
  console.error("validate-ipc-v2-migration-doc-fixtures: v2 migration doc drifted from the committed fixtures");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-ipc-v2-migration-doc-fixtures: ok");
