#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Chain tx test line-ref drift guard. docs/ipc-and-http-gateway.md, in the
// Chain Transaction Envelopes section, cites six unit-test line numbers that
// pin the chain tx envelope kind strings. The line numbers shift whenever
// main.rs grows above the cited tests, and the docs do not auto-update.
//
// This validator derives the line numbers from main.rs at run time by
// matching the six expected test fn names — three copies of
// `confirmed_envelope_pins_documented_shape` (one in each of the chain
// register_agent / stake / buy_credits test modules) and one each of the
// three uniquely-named timeout-envelope tests — and asserts every derived
// line number appears in the docs' kind-pinning sentence. Line numbers are
// never hardcoded; the validator self-corrects to wherever the tests
// currently live.

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
]);

const expectedTotal = 6;
const expectedConfirmedCount = 3;

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
if (emitters) {
  const lines = emitters.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const match = lines[index].match(/^\s*fn\s+(\w+)\s*\(/);
    if (match && expectedTestFnNames.has(match[1])) {
      found.push({ name: match[1], line: index + 1 });
    }
  }
}

if (emitters && found.length !== expectedTotal) {
  fail(
    `${emittersPath}: expected ${expectedTotal} chain tx test fn declarations matching the four expected names but found ${found.length}; remediation: confirm the test fn names in the chain register_agent/stake/buy_credits test modules still match the expectedTestFnNames set`,
  );
}

if (emitters) {
  const confirmedCount = found.filter((entry) => entry.name === "confirmed_envelope_pins_documented_shape").length;
  if (confirmedCount !== expectedConfirmedCount) {
    fail(
      `${emittersPath}: expected ${expectedConfirmedCount} copies of "confirmed_envelope_pins_documented_shape" (one per chain verb module) but found ${confirmedCount}; remediation: confirm each chain verb module (register_agent, stake, buy_credits) still has its confirmed-envelope shape test`,
    );
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

let sentence = null;
if (docs) {
  const match = docs.match(/Six unit tests at[\s\S]{0,500}?pin the kind strings/);
  if (!match) {
    fail(
      `${docsPath}: missing the "Six unit tests at main.rs:NNNN, ... pin the kind strings" sentence in the Chain Transaction Envelopes section; remediation: restore the sentence that records the chain tx kind-pinning test line refs`,
    );
  } else {
    sentence = match[0];
  }
}

if (sentence) {
  for (const entry of found) {
    if (!sentence.includes(`:${entry.line}`)) {
      fail(
        `${docsPath}: the "Six unit tests at main.rs:..." sentence does not cite main.rs:${entry.line} (fn ${entry.name}); remediation: update the sentence to include this line number`,
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
  `validate-chain-tx-test-line-refs: ok (${found.length} chain tx test fn line refs match)`,
);
