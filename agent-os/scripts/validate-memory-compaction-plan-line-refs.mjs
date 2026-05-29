#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Memory-compaction-plan envelope line-ref drift guard.
// docs/ipc-and-http-gateway.md cites four name-anchored main.rs line refs
// for the memory_compaction_plan envelope (an extra test compared to the
// standard three-line-ref shape, because the envelope carries a forward-
// compatibility `expected_receipt_changes` placeholder block that has its
// own pin test):
//
//   - The `memory_compaction_plan_json` helper fn declaration line, cited
//     in the source-of-truth sentence ("The envelope source-of-truth lives
//     at `memory_compaction_plan_json` in
//     `agent-os/crates/covenant/src/main.rs:NNN`").
//   - The `memory_compaction_plan_json_renders_stable_shape` unit test
//     line, cited in the source-of-truth sentence ("Three unit tests at
//     `main.rs:NNN` (`memory_compaction_plan_json_renders_stable_shape`),
//     ...").
//   - The `memory_compaction_plan_json_pins_top_level_schema` unit test
//     line, cited both in the source-of-truth sentence ("..., `main.rs:NNN`
//     (`memory_compaction_plan_json_pins_top_level_schema`), and ...") and
//     in the top-level pins-anchor sentence ("Top-level keys are pinned to
//     exactly these three by the test at
//     `agent-os/crates/covenant/src/main.rs:NNN`
//     (`memory_compaction_plan_json_pins_top_level_schema`)").
//   - The `memory_compaction_plan_json_pins_expected_receipt_changes_schema`
//     unit test line, cited both in the source-of-truth sentence ("..., and
//     `main.rs:NNN`
//     (`memory_compaction_plan_json_pins_expected_receipt_changes_schema`)
//     cover ...") and in the placeholder-anchor sentence (the
//     `expected_receipt_changes` field bullet: "a forward-compatibility
//     placeholder pinned by the schema test at `main.rs:NNN`
//     (`memory_compaction_plan_json_pins_expected_receipt_changes_schema`)").
//
// All four line numbers shift whenever main.rs grows above the cited
// declarations, and the docs do not auto-update.
//
// This validator derives the line numbers from main.rs at run time by
// matching the helper fn (strict `^fn` regex, no indentation) and the
// three test fns (indented `fn` inside a `mod tests` block). Every derived
// line number must appear in its expected anchor sentence. Line numbers
// are never hardcoded; the validator self-corrects to wherever the
// declarations currently live.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const emittersPath = "agent-os/crates/covenant/src/main.rs";

const helperFnName = "memory_compaction_plan_json";
const rendersTestFnName = "memory_compaction_plan_json_renders_stable_shape";
const pinsTopLevelTestFnName =
  "memory_compaction_plan_json_pins_top_level_schema";
const pinsExpectedReceiptChangesTestFnName =
  "memory_compaction_plan_json_pins_expected_receipt_changes_schema";

const errors = [];
const fail = (message) => errors.push(message);

let docs;
let emitters;
try {
  docs = read(docsPath);
} catch (error) {
  fail(`cannot read ${docsPath}: ${error.message}`);
}
try {
  emitters = read(emittersPath);
} catch (error) {
  fail(`cannot read ${emittersPath}: ${error.message}`);
}

let helperLine = null;
let rendersLine = null;
let pinsTopLevelLine = null;
let pinsExpectedLine = null;
const helperMatches = [];
const rendersMatches = [];
const pinsTopLevelMatches = [];
const pinsExpectedMatches = [];

if (emitters) {
  const lines = emitters.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const helperMatch = lines[index].match(/^fn\s+(\w+)\s*\(/);
    if (helperMatch && helperMatch[1] === helperFnName) {
      helperMatches.push(index + 1);
    }
    const testMatch = lines[index].match(/^\s+fn\s+(\w+)\s*\(/);
    if (testMatch && testMatch[1] === rendersTestFnName) {
      rendersMatches.push(index + 1);
    }
    if (testMatch && testMatch[1] === pinsTopLevelTestFnName) {
      pinsTopLevelMatches.push(index + 1);
    }
    if (testMatch && testMatch[1] === pinsExpectedReceiptChangesTestFnName) {
      pinsExpectedMatches.push(index + 1);
    }
  }

  if (helperMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 top-level "fn ${helperFnName}" but found ${helperMatches.length}; remediation: confirm the memory_compaction_plan envelope emitter is a single top-level helper, not renamed or duplicated`,
    );
  } else {
    helperLine = helperMatches[0];
  }
  if (rendersMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${rendersTestFnName}" but found ${rendersMatches.length}; remediation: confirm the memory_compaction_plan renders-stable-shape test still exists inside the tests module`,
    );
  } else {
    rendersLine = rendersMatches[0];
  }
  if (pinsTopLevelMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${pinsTopLevelTestFnName}" but found ${pinsTopLevelMatches.length}; remediation: confirm the memory_compaction_plan top-level-schema pinning test still exists inside the tests module`,
    );
  } else {
    pinsTopLevelLine = pinsTopLevelMatches[0];
  }
  if (pinsExpectedMatches.length !== 1) {
    fail(
      `${emittersPath}: expected exactly 1 "fn ${pinsExpectedReceiptChangesTestFnName}" but found ${pinsExpectedMatches.length}; remediation: confirm the memory_compaction_plan expected_receipt_changes-schema pinning test still exists inside the tests module`,
    );
  } else {
    pinsExpectedLine = pinsExpectedMatches[0];
  }
}

let sourceOfTruthSentence = null;
if (docs) {
  const match = docs.match(
    /The envelope source-of-truth lives at `memory_compaction_plan_json` in `agent-os\/crates\/covenant\/src\/main\.rs:\d+`\. Three unit tests at `main\.rs:\d+` \(`memory_compaction_plan_json_renders_stable_shape`\), `main\.rs:\d+` \(`memory_compaction_plan_json_pins_top_level_schema`\), and `main\.rs:\d+` \(`memory_compaction_plan_json_pins_expected_receipt_changes_schema`\) cover/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the memory_compaction_plan source-of-truth sentence ("The envelope source-of-truth lives at \`memory_compaction_plan_json\` in \`agent-os/crates/covenant/src/main.rs:NNN\`. Three unit tests at \`main.rs:NNN\` (\`memory_compaction_plan_json_renders_stable_shape\`), \`main.rs:NNN\` (\`memory_compaction_plan_json_pins_top_level_schema\`), and \`main.rs:NNN\` (\`memory_compaction_plan_json_pins_expected_receipt_changes_schema\`) cover ..."); remediation: restore the sentence so all four memory_compaction_plan line refs are cited`,
    );
  } else {
    sourceOfTruthSentence = match[0];
  }
}

let pinsTopLevelAnchorSentence = null;
if (docs) {
  const match = docs.match(
    /Top-level keys are pinned to exactly these three by the test at `agent-os\/crates\/covenant\/src\/main\.rs:\d+` \(`memory_compaction_plan_json_pins_top_level_schema`\)/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the memory_compaction_plan top-level pins-anchor sentence ("Top-level keys are pinned to exactly these three by the test at \`agent-os/crates/covenant/src/main.rs:NNN\` (\`memory_compaction_plan_json_pins_top_level_schema\`)"); remediation: restore the sentence that records the top-level pins-test line ref`,
    );
  } else {
    pinsTopLevelAnchorSentence = match[0];
  }
}

let pinsExpectedAnchorSentence = null;
if (docs) {
  const match = docs.match(
    /a forward-compatibility placeholder pinned by the schema test at `main\.rs:\d+` \(`memory_compaction_plan_json_pins_expected_receipt_changes_schema`\)/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the memory_compaction_plan expected_receipt_changes pins-anchor sentence ("a forward-compatibility placeholder pinned by the schema test at \`main.rs:NNN\` (\`memory_compaction_plan_json_pins_expected_receipt_changes_schema\`)"); remediation: restore the field-bullet sentence that records the placeholder pins-test line ref`,
    );
  } else {
    pinsExpectedAnchorSentence = match[0];
  }
}

if (sourceOfTruthSentence && helperLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${helperLine}`)) {
    fail(
      `${docsPath}: the memory_compaction_plan source-of-truth sentence does not cite main.rs:${helperLine} (fn ${helperFnName}); remediation: update the sentence to include this helper line number`,
    );
  }
}
if (sourceOfTruthSentence && rendersLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${rendersLine}`)) {
    fail(
      `${docsPath}: the memory_compaction_plan source-of-truth sentence does not cite main.rs:${rendersLine} (fn ${rendersTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}
if (sourceOfTruthSentence && pinsTopLevelLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${pinsTopLevelLine}`)) {
    fail(
      `${docsPath}: the memory_compaction_plan source-of-truth sentence does not cite main.rs:${pinsTopLevelLine} (fn ${pinsTopLevelTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}
if (sourceOfTruthSentence && pinsExpectedLine !== null) {
  if (!sourceOfTruthSentence.includes(`main.rs:${pinsExpectedLine}`)) {
    fail(
      `${docsPath}: the memory_compaction_plan source-of-truth sentence does not cite main.rs:${pinsExpectedLine} (fn ${pinsExpectedReceiptChangesTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}

if (pinsTopLevelAnchorSentence && pinsTopLevelLine !== null) {
  if (!pinsTopLevelAnchorSentence.includes(`main.rs:${pinsTopLevelLine}`)) {
    fail(
      `${docsPath}: the memory_compaction_plan top-level pins-anchor sentence does not cite main.rs:${pinsTopLevelLine} (fn ${pinsTopLevelTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}

if (pinsExpectedAnchorSentence && pinsExpectedLine !== null) {
  if (!pinsExpectedAnchorSentence.includes(`main.rs:${pinsExpectedLine}`)) {
    fail(
      `${docsPath}: the memory_compaction_plan expected_receipt_changes pins-anchor sentence does not cite main.rs:${pinsExpectedLine} (fn ${pinsExpectedReceiptChangesTestFnName}); remediation: update the sentence to include this test line number`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-memory-compaction-plan-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-compaction-plan-line-refs: ok (helper main.rs:${helperLine}, renders main.rs:${rendersLine}, pins-top-level main.rs:${pinsTopLevelLine}, pins-expected main.rs:${pinsExpectedLine})`,
);
