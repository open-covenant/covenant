#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Intents-resume envelope line-ref drift guard. docs/ipc-and-http-gateway.md
// cites seven name-anchored main.rs line refs for the intents_resume envelope,
// which is the only two-shape envelope in this section (success and error
// envelopes share the `intents_resume` discriminator and branch on a flat
// `ok` boolean) and therefore has two helper fns plus a typed-slug
// classifier:
//
//   - Pins-anchor sentence ("Top-level keys are pinned by three tests at
//     `agent-os/crates/covenant/src/main.rs`: ... The typed-slug
//     enumeration is pinned by ..."): cites four test fn declarations:
//       * intents_resume_ok_json_pins_top_level_schema (success branch)
//       * intents_resume_error_json_pins_top_level_schema (error branch)
//       * intents_resume_error_json_pins_error_object_schema (inner shape)
//       * intents_resume_error_code_pins_documented_arms (typed-slug enum)
//   - Source-of-truth sentence ("The envelope source-of-truth lives at
//     `intents_resume_ok_json` (`main.rs:NNN`) and `intents_resume_error_json`
//     (`main.rs:NNN`), with the slug classifier at `intents_resume_error_code`
//     (`main.rs:NNN`)"): cites three top-level fn declarations.
//
// All seven line numbers shift whenever main.rs grows above the cited
// declarations, and the docs do not auto-update.
//
// This validator derives the line numbers from main.rs at run time by
// matching the three top-level helper/classifier fns (strict `^fn` regex,
// no indentation) and the four pins-test fns (indented `fn` inside a
// `mod tests` block). Every derived line number must appear in its
// expected anchor sentence. Line numbers are never hardcoded.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const emittersPath = "agent-os/crates/covenant/src/main.rs";

const okHelperFnName = "intents_resume_ok_json";
const errorHelperFnName = "intents_resume_error_json";
const classifierFnName = "intents_resume_error_code";
const okPinsTestFnName = "intents_resume_ok_json_pins_top_level_schema";
const errorPinsTestFnName = "intents_resume_error_json_pins_top_level_schema";
const errorObjectPinsTestFnName =
  "intents_resume_error_json_pins_error_object_schema";
const slugArmsTestFnName = "intents_resume_error_code_pins_documented_arms";

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

const topLevelFns = {
  [okHelperFnName]: { matches: [], line: null },
  [errorHelperFnName]: { matches: [], line: null },
  [classifierFnName]: { matches: [], line: null },
};
const testFns = {
  [okPinsTestFnName]: { matches: [], line: null },
  [errorPinsTestFnName]: { matches: [], line: null },
  [errorObjectPinsTestFnName]: { matches: [], line: null },
  [slugArmsTestFnName]: { matches: [], line: null },
};

if (emitters) {
  const lines = emitters.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const helperMatch = lines[index].match(/^fn\s+(\w+)\s*[(<]/);
    if (helperMatch && topLevelFns[helperMatch[1]] !== undefined) {
      topLevelFns[helperMatch[1]].matches.push(index + 1);
    }
    const testMatch = lines[index].match(/^\s+fn\s+(\w+)\s*\(/);
    if (testMatch && testFns[testMatch[1]] !== undefined) {
      testFns[testMatch[1]].matches.push(index + 1);
    }
  }

  for (const [name, entry] of Object.entries(topLevelFns)) {
    if (entry.matches.length !== 1) {
      fail(
        `${emittersPath}: expected exactly 1 top-level "fn ${name}" but found ${entry.matches.length}; remediation: confirm the intents_resume ${name === classifierFnName ? "slug classifier" : "envelope helper"} is a single top-level fn, not renamed, merged, or duplicated`,
      );
    } else {
      entry.line = entry.matches[0];
    }
  }
  for (const [name, entry] of Object.entries(testFns)) {
    if (entry.matches.length !== 1) {
      fail(
        `${emittersPath}: expected exactly 1 "fn ${name}" but found ${entry.matches.length}; remediation: confirm the intents_resume ${name} pinning test still exists inside the tests module`,
      );
    } else {
      entry.line = entry.matches[0];
    }
  }
}

let sourceOfTruthSentence = null;
if (docs) {
  const match = docs.match(
    /The envelope source-of-truth lives at `intents_resume_ok_json` \(`main\.rs:\d+`\) and `intents_resume_error_json` \(`main\.rs:\d+`\), with the slug classifier at `intents_resume_error_code` \(`main\.rs:\d+`\)\./,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the intents_resume source-of-truth sentence ("The envelope source-of-truth lives at \`intents_resume_ok_json\` (\`main.rs:NNN\`) and \`intents_resume_error_json\` (\`main.rs:NNN\`), with the slug classifier at \`intents_resume_error_code\` (\`main.rs:NNN\`)."); remediation: restore the sentence so all three helper/classifier line refs are cited`,
    );
  } else {
    sourceOfTruthSentence = match[0];
  }
}

let pinsAnchorSentence = null;
if (docs) {
  const match = docs.match(
    /Top-level keys are pinned by three tests at `agent-os\/crates\/covenant\/src\/main\.rs`: `intents_resume_ok_json_pins_top_level_schema` at `main\.rs:\d+` \(success branch.*?\), `intents_resume_error_json_pins_top_level_schema` at `main\.rs:\d+` \(error branch.*?\), and `intents_resume_error_json_pins_error_object_schema` at `main\.rs:\d+` \(inner `error` object's two-key shape\)\. The typed-slug enumeration is pinned by `intents_resume_error_code_pins_documented_arms` at `main\.rs:\d+`\./,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the intents_resume pins-anchor sentence ("Top-level keys are pinned by three tests at \`agent-os/crates/covenant/src/main.rs\`: \`intents_resume_ok_json_pins_top_level_schema\` at \`main.rs:NNN\` (success branch ...), \`intents_resume_error_json_pins_top_level_schema\` at \`main.rs:NNN\` (error branch ...), and \`intents_resume_error_json_pins_error_object_schema\` at \`main.rs:NNN\` (inner \`error\` object's two-key shape). The typed-slug enumeration is pinned by \`intents_resume_error_code_pins_documented_arms\` at \`main.rs:NNN\`."); remediation: restore the sentence so all four pins-test line refs are cited`,
    );
  } else {
    pinsAnchorSentence = match[0];
  }
}

const assertCitation = (sentence, name, line, sentenceLabel) => {
  if (sentence && line !== null) {
    if (!sentence.includes(`main.rs:${line}`)) {
      fail(
        `${docsPath}: the intents_resume ${sentenceLabel} does not cite main.rs:${line} (fn ${name}); remediation: update the sentence to include this line number`,
      );
    }
  }
};

assertCitation(
  sourceOfTruthSentence,
  okHelperFnName,
  topLevelFns[okHelperFnName].line,
  "source-of-truth sentence",
);
assertCitation(
  sourceOfTruthSentence,
  errorHelperFnName,
  topLevelFns[errorHelperFnName].line,
  "source-of-truth sentence",
);
assertCitation(
  sourceOfTruthSentence,
  classifierFnName,
  topLevelFns[classifierFnName].line,
  "source-of-truth sentence",
);
assertCitation(
  pinsAnchorSentence,
  okPinsTestFnName,
  testFns[okPinsTestFnName].line,
  "pins-anchor sentence",
);
assertCitation(
  pinsAnchorSentence,
  errorPinsTestFnName,
  testFns[errorPinsTestFnName].line,
  "pins-anchor sentence",
);
assertCitation(
  pinsAnchorSentence,
  errorObjectPinsTestFnName,
  testFns[errorObjectPinsTestFnName].line,
  "pins-anchor sentence",
);
assertCitation(
  pinsAnchorSentence,
  slugArmsTestFnName,
  testFns[slugArmsTestFnName].line,
  "pins-anchor sentence",
);

if (errors.length > 0) {
  console.error("validate-intents-resume-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-line-refs: ok (ok-helper main.rs:${topLevelFns[okHelperFnName].line}, error-helper main.rs:${topLevelFns[errorHelperFnName].line}, classifier main.rs:${topLevelFns[classifierFnName].line}, ok-pins main.rs:${testFns[okPinsTestFnName].line}, error-pins main.rs:${testFns[errorPinsTestFnName].line}, error-object-pins main.rs:${testFns[errorObjectPinsTestFnName].line}, slug-arms main.rs:${testFns[slugArmsTestFnName].line})`,
);
