#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Chain tx test line-ref drift guard. docs/ipc-and-http-gateway.md, in the
// Chain Transaction Envelopes section, cites two sets of main.rs line refs:
//
//   - Six helper-function line refs in the source-of-truth sentence
//     ("The verb-source-of-truth lives in the CLI emitters: ... at
//     `main.rs:NNN` ...") naming the six envelope emitters.
//   - Twelve unit-test line refs across both halves of the test anchor
//     sentence ("Six unit tests at ... pin the kind strings, and six
//     sibling *_pins_top_level_schema tests at ... assert the full
//     documented top-level key set").
//
// All eighteen line numbers shift whenever main.rs grows above the cited
// declarations, and the docs do not auto-update.
//
// This validator derives the line numbers from main.rs at run time by
// matching twelve expected test fn names — three copies each of
// `confirmed_envelope_pins_documented_shape`,
// `confirmed_envelope_pins_top_level_schema`, and
// `timeout_envelope_pins_top_level_schema` (one in each of the chain
// register_agent / stake / buy_credits test modules) plus one each of the
// three uniquely-named timeout-envelope kind-pinning tests — and six
// helper fn names (the chain register-agent / stake / buy-credits
// confirmed and timeout JSON emitters). Helper fns are matched with a
// strict `^fn` regex (no indentation) so they cannot collide with the
// indented test fns inside `mod` blocks. Every derived line number must
// appear inside the captured anchor sentence for its set (test or
// helper). Line numbers are never hardcoded; the validator self-corrects
// to wherever the declarations currently live.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const emittersPath = "agent-os/crates/covenant/src/main.rs";

const expectedTestFnNames = new Set([
  "confirmed_envelope_pins_documented_shape",
  "timeout_envelope_uses_distinct_kind_and_status",
  "timeout_envelope_includes_amount_lock_until_and_timeout_ms",
  "timeout_envelope_includes_amount_covnt_and_timeout_ms",
  "confirmed_envelope_pins_top_level_schema",
  "timeout_envelope_pins_top_level_schema",
]);

const expectedTotal = 12;
const expectedConfirmedDocumentedCount = 3;
const expectedConfirmedTopLevelSchemaCount = 3;
const expectedTimeoutTopLevelSchemaCount = 3;

const expectedHelperFnNames = new Set([
  "register_agent_confirmed_json",
  "register_agent_timeout_json",
  "stake_confirmed_json",
  "stake_timeout_json",
  "buy_credits_confirmed_json",
  "buy_credits_timeout_json",
]);
const expectedHelperTotal = 6;

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

const found = [];
const foundHelpers = [];
if (emitters) {
  const lines = emitters.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const testMatch = lines[index].match(/^\s*fn\s+(\w+)\s*\(/);
    if (testMatch && expectedTestFnNames.has(testMatch[1])) {
      found.push({ name: testMatch[1], line: index + 1 });
    }
    const helperMatch = lines[index].match(/^fn\s+(\w+)\s*\(/);
    if (helperMatch && expectedHelperFnNames.has(helperMatch[1])) {
      foundHelpers.push({ name: helperMatch[1], line: index + 1 });
    }
  }
}

if (emitters && found.length !== expectedTotal) {
  fail(
    `${emittersPath}: expected ${expectedTotal} chain tx test fn declarations matching the six expected names but found ${found.length}; remediation: confirm the test fn names in the chain register_agent/stake/buy_credits test modules still match the expectedTestFnNames set`,
  );
}

if (emitters) {
  const repeatedCounts = [
    ["confirmed_envelope_pins_documented_shape", expectedConfirmedDocumentedCount],
    ["confirmed_envelope_pins_top_level_schema", expectedConfirmedTopLevelSchemaCount],
    ["timeout_envelope_pins_top_level_schema", expectedTimeoutTopLevelSchemaCount],
  ];
  for (const [fnName, expected] of repeatedCounts) {
    const count = found.filter((entry) => entry.name === fnName).length;
    if (count !== expected) {
      fail(
        `${emittersPath}: expected ${expected} copies of "${fnName}" (one per chain verb module) but found ${count}; remediation: confirm each chain verb module (register_agent, stake, buy_credits) still has its ${fnName} test`,
      );
    }
  }
  for (const uniqueName of [
    "timeout_envelope_uses_distinct_kind_and_status",
    "timeout_envelope_includes_amount_lock_until_and_timeout_ms",
    "timeout_envelope_includes_amount_covnt_and_timeout_ms",
  ]) {
    const count = found.filter((entry) => entry.name === uniqueName).length;
    if (count !== 1) {
      fail(
        `${emittersPath}: expected exactly 1 "${uniqueName}" but found ${count}; remediation: confirm the timeout-envelope test exists in its matching chain verb module`,
      );
    }
  }
}

if (emitters && foundHelpers.length !== expectedHelperTotal) {
  fail(
    `${emittersPath}: expected ${expectedHelperTotal} chain tx helper fn declarations matching the six expected names but found ${foundHelpers.length}; remediation: confirm the helper fn names at top level still match the expectedHelperFnNames set (register_agent / stake / buy_credits, confirmed and timeout)`,
  );
}

if (emitters) {
  for (const helperName of expectedHelperFnNames) {
    const count = foundHelpers.filter((entry) => entry.name === helperName).length;
    if (count !== 1) {
      fail(
        `${emittersPath}: expected exactly 1 top-level "fn ${helperName}" but found ${count}; remediation: confirm the chain tx envelope emitter is a single top-level helper, not merged or duplicated`,
      );
    }
  }
}

let sentence = null;
if (docs) {
  const match = docs.match(
    /Six unit tests at[\s\S]{0,500}?pin the kind strings[\s\S]{0,400}?pins_top_level_schema[\s\S]{0,400}?assert the full documented top-level key set/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the two-part anchor sentence ("Six unit tests at ... pin the kind strings, and six sibling *_pins_top_level_schema tests at ... assert the full documented top-level key set") in the Chain Transaction Envelopes section; remediation: restore both halves of the sentence so every chain tx test line ref is cited`,
    );
  } else {
    sentence = match[0];
  }
}

if (sentence) {
  for (const entry of found) {
    if (!sentence.includes(`:${entry.line}`)) {
      fail(
        `${docsPath}: the chain tx anchor sentence does not cite main.rs:${entry.line} (fn ${entry.name}); remediation: update the sentence to include this line number in the matching half (kind-strings or top-level-schema)`,
      );
    }
  }
}

let helperSentence = null;
if (docs) {
  const match = docs.match(
    /The verb-source-of-truth lives in the CLI emitters:[\s\S]{0,600}?buy_credits_timeout_json` at `:\d+` and `:\d+`\./,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the source-of-truth sentence ("The verb-source-of-truth lives in the CLI emitters: ... \`buy_credits_confirmed_json\` and \`buy_credits_timeout_json\` at \`:NNNN\` and \`:NNNN\`.") in the Chain Transaction Envelopes section; remediation: restore the sentence that records the chain tx helper-fn line refs`,
    );
  } else {
    helperSentence = match[0];
  }
}

if (helperSentence) {
  for (const entry of foundHelpers) {
    if (!helperSentence.includes(`:${entry.line}`)) {
      fail(
        `${docsPath}: the chain tx source-of-truth sentence does not cite main.rs:${entry.line} (fn ${entry.name}); remediation: update the sentence to include this line number`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-chain-tx-test-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-chain-tx-test-line-refs: ok (${found.length} chain tx test fn line refs + ${foundHelpers.length} helper fn line refs match)`,
);
