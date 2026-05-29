#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Intents-resume envelope line-ref drift guard. docs/ipc-and-http-gateway.md
// cites ten name-anchored main.rs line refs for the intents_resume envelope,
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
//   - Three EXPECTED_KEYS sentences cite the `const EXPECTED_KEYS`
//     declaration line inside each of the three pins tests:
//       * Success branch ("**Success branch (`ok=true`)** carries these
//         eight top-level keys per the test EXPECTED_KEYS at
//         `main.rs:NNN-MMM`:") — cited as a const-opener-to-closer
//         multi-line range because the success-branch EXPECTED_KEYS
//         array spans 10 lines (one entry per line). The range cite
//         fails loudly the moment the array grows or shrinks, catching
//         drift the inner test only catches at runtime.
//       * Error branch ("**Error branch (`ok=false`)** carries these five
//         top-level keys per the test EXPECTED_KEYS at `main.rs:NNN`:") —
//         single-line cite because the const is a single-line literal.
//       * Inner error ("The inner `error` object has exactly two keys per
//         the test EXPECTED_KEYS at `main.rs:NNN`:") — single-line cite
//         because the const is a single-line literal.
//
// All ten line numbers shift whenever main.rs grows above the cited
// declarations or a blank line or comment is inserted between a pins-test
// fn declaration and its EXPECTED_KEYS const, and the docs do not auto-update.
//
// This validator derives the line numbers from main.rs at run time by
// matching the three top-level helper/classifier fns (strict `^fn` regex,
// no indentation), the four pins-test fns (indented `fn` inside a
// `mod tests` block), and the `const EXPECTED_KEYS` line inside each of
// the three pins tests (scanned forward from the fn line up to the
// matching closing brace at the fn's indentation). Every derived line
// number must appear in its expected anchor sentence. Line numbers are
// never hardcoded.

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
  [okPinsTestFnName]: { matches: [], line: null, indent: null, expectedKeysLine: null, expectedKeysEndLine: null },
  [errorPinsTestFnName]: { matches: [], line: null, indent: null, expectedKeysLine: null, expectedKeysEndLine: null },
  [errorObjectPinsTestFnName]: { matches: [], line: null, indent: null, expectedKeysLine: null, expectedKeysEndLine: null },
  [slugArmsTestFnName]: { matches: [], line: null, indent: null, expectedKeysLine: null, expectedKeysEndLine: null },
};

if (emitters) {
  const lines = emitters.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const helperMatch = lines[index].match(/^fn\s+(\w+)\s*[(<]/);
    if (helperMatch && topLevelFns[helperMatch[1]] !== undefined) {
      topLevelFns[helperMatch[1]].matches.push(index + 1);
    }
    const testMatch = lines[index].match(/^(\s+)fn\s+(\w+)\s*\(/);
    if (testMatch && testFns[testMatch[2]] !== undefined) {
      testFns[testMatch[2]].matches.push({ line: index + 1, indent: testMatch[1] });
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
      entry.line = entry.matches[0].line;
      entry.indent = entry.matches[0].indent;
    }
  }

  const expectedKeysTests = [
    okPinsTestFnName,
    errorPinsTestFnName,
    errorObjectPinsTestFnName,
  ];
  for (const name of expectedKeysTests) {
    const entry = testFns[name];
    if (entry.line === null) continue;
    const closing = new RegExp(`^${entry.indent}}\\s*$`);
    let endLine = null;
    for (let index = entry.line; index < lines.length; index += 1) {
      if (closing.test(lines[index])) {
        endLine = index + 1;
        break;
      }
    }
    if (endLine === null) {
      fail(
        `${emittersPath}: could not find the matching closing brace for "fn ${name}" (started at line ${entry.line}); remediation: confirm the test body is well-formed`,
      );
      continue;
    }
    const keysMatches = [];
    for (let index = entry.line; index < endLine - 1; index += 1) {
      if (/^\s+const\s+EXPECTED_KEYS\b/.test(lines[index])) {
        keysMatches.push(index + 1);
      }
    }
    if (keysMatches.length !== 1) {
      fail(
        `${emittersPath}: expected exactly 1 "const EXPECTED_KEYS" inside "fn ${name}" but found ${keysMatches.length}; remediation: confirm the test still pins its top-level key set via a single EXPECTED_KEYS const declaration`,
      );
    } else {
      const openerLine = keysMatches[0];
      entry.expectedKeysLine = openerLine;
      const openerTrimmed = lines[openerLine - 1].trim();
      if (openerTrimmed.endsWith("];")) {
        entry.expectedKeysEndLine = openerLine;
      } else {
        let constEndLine = null;
        for (let index = openerLine; index < endLine - 1; index += 1) {
          if (lines[index].trim() === "];") {
            constEndLine = index + 1;
            break;
          }
        }
        if (constEndLine === null) {
          fail(
            `${emittersPath}: could not find the closing \`];\` for "const EXPECTED_KEYS" inside "fn ${name}" (opener at line ${openerLine}); remediation: confirm the const is either a single-line literal ending in \`];\` on the opener line or a multi-line array literal with the closing \`];\` on its own line`,
          );
        } else {
          entry.expectedKeysEndLine = constEndLine;
        }
      }
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

let successBranchSentence = null;
let successBranchCite = null;
if (docs) {
  const match = docs.match(
    /\*\*Success branch \(`ok=true`\)\*\* carries these eight top-level keys per the test EXPECTED_KEYS at `main\.rs:(\d+)-(\d+)`:/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the intents_resume success-branch EXPECTED_KEYS sentence ("**Success branch (\`ok=true\`)** carries these eight top-level keys per the test EXPECTED_KEYS at \`main.rs:NNN-MMM\`:"); remediation: restore the sentence that records the success-branch EXPECTED_KEYS const-opener-to-closer line range`,
    );
  } else {
    successBranchSentence = match[0];
    successBranchCite = {
      start: parseInt(match[1], 10),
      end: parseInt(match[2], 10),
    };
  }
}

let errorBranchSentence = null;
if (docs) {
  const match = docs.match(
    /\*\*Error branch \(`ok=false`\)\*\* carries these five top-level keys per the test EXPECTED_KEYS at `main\.rs:\d+`:/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the intents_resume error-branch EXPECTED_KEYS sentence ("**Error branch (\`ok=false\`)** carries these five top-level keys per the test EXPECTED_KEYS at \`main.rs:NNN\`:"); remediation: restore the sentence that records the error-branch EXPECTED_KEYS line ref`,
    );
  } else {
    errorBranchSentence = match[0];
  }
}

let innerErrorSentence = null;
if (docs) {
  const match = docs.match(
    /The inner `error` object has exactly two keys per the test EXPECTED_KEYS at `main\.rs:\d+`:/,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the intents_resume inner-error EXPECTED_KEYS sentence ("The inner \`error\` object has exactly two keys per the test EXPECTED_KEYS at \`main.rs:NNN\`:"); remediation: restore the sentence that records the inner-error EXPECTED_KEYS line ref`,
    );
  } else {
    innerErrorSentence = match[0];
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

if (successBranchSentence && successBranchCite) {
  const start = testFns[okPinsTestFnName].expectedKeysLine;
  const end = testFns[okPinsTestFnName].expectedKeysEndLine;
  if (start !== null && end !== null) {
    if (successBranchCite.start !== start || successBranchCite.end !== end) {
      fail(
        `${docsPath}: the intents_resume success-branch EXPECTED_KEYS sentence cites main.rs:${successBranchCite.start}-${successBranchCite.end} but the const spans :${start}-${end} (fn ${okPinsTestFnName}); remediation: update the sentence to cite \`main.rs:${start}-${end}\``,
      );
    }
  }
}
assertCitation(
  errorBranchSentence,
  `${errorPinsTestFnName} EXPECTED_KEYS`,
  testFns[errorPinsTestFnName].expectedKeysLine,
  "error-branch EXPECTED_KEYS sentence",
);
assertCitation(
  innerErrorSentence,
  `${errorObjectPinsTestFnName} EXPECTED_KEYS`,
  testFns[errorObjectPinsTestFnName].expectedKeysLine,
  "inner-error EXPECTED_KEYS sentence",
);

if (errors.length > 0) {
  console.error("validate-intents-resume-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-line-refs: ok (ok-helper main.rs:${topLevelFns[okHelperFnName].line}, error-helper main.rs:${topLevelFns[errorHelperFnName].line}, classifier main.rs:${topLevelFns[classifierFnName].line}, ok-pins main.rs:${testFns[okPinsTestFnName].line}, error-pins main.rs:${testFns[errorPinsTestFnName].line}, error-object-pins main.rs:${testFns[errorObjectPinsTestFnName].line}, slug-arms main.rs:${testFns[slugArmsTestFnName].line}, ok-pins-keys main.rs:${testFns[okPinsTestFnName].expectedKeysLine}-${testFns[okPinsTestFnName].expectedKeysEndLine}, error-pins-keys main.rs:${testFns[errorPinsTestFnName].expectedKeysLine}, error-object-pins-keys main.rs:${testFns[errorObjectPinsTestFnName].expectedKeysLine})`,
);
