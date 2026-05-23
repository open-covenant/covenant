#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-a2a A2ATaskResult::error fn block range line-ref drift guard.
// docs/ipc-and-http-gateway.md line 468 cites the
// `A2ATaskResult::error` associated fn in
// agent-os/crates/covenant-a2a/src/lib.rs as a range citation:
//
//   empty for `error` results per `A2ATaskResult::error`
//   at `covenant-a2a/src/lib.rs:406-413`
//
// The range spans the `pub fn error(task_id: Uuid, message: impl
// Into<String>) -> Self {` opener (line 406), the fn body, and the
// matching closing brace (line 413).
//
// The fn lives inside an `impl A2ATaskResult { ... }` block. To
// prevent a future `pub fn error` added to a different impl block
// (e.g. impl A2ATask or impl A2AAutoRetryReport) from collapsing onto
// the same lookup, the validator scopes the fn search to the
// containing impl span via brace-balance — same convention as the
// covenant-a2a field-attribute-range validator's struct-scoped field
// lookup. The fn body's end line is then located by a second
// brace-balance scan from the fn opener.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-a2a/src/lib.rs";

const target = {
  implType: "A2ATaskResult",
  fnName: "error",
  fnSignature: "pub fn error(task_id: Uuid",
  docsRegex:
    /empty for `error` results per `A2ATaskResult::error` at `covenant-a2a\/src\/lib\.rs:(\d+)-(\d+)`/,
  docsLabel: "A2ATaskResult::error fn block range citation",
  docsTemplate:
    "empty for `error` results per `A2ATaskResult::error` at `covenant-a2a/src/lib.rs:N-M`",
};

const errors = [];
const fail = (message) => errors.push(message);

let docs;
let source;
try {
  docs = read(docsPath);
} catch (error) {
  fail(`cannot read ${docsPath}: ${error.message}`);
}
try {
  source = read(sourcePath);
} catch (error) {
  fail(`cannot read ${sourcePath}: ${error.message}`);
}

function scanBraceBalance(lines, openerLine) {
  let depth = 0;
  let opened = false;
  for (let index = openerLine - 1; index < lines.length; index += 1) {
    for (const char of lines[index]) {
      if (char === "{") {
        depth += 1;
        opened = true;
      } else if (char === "}") {
        depth -= 1;
      }
    }
    if (opened && depth === 0) {
      return index + 1;
    }
  }
  return null;
}

let startLine = null;
let endLine = null;
if (source) {
  const lines = source.split("\n");
  const implOpenerRegex = new RegExp(`^impl\\s+${target.implType}\\s*\\{`);
  const implOpeners = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (implOpenerRegex.test(lines[index])) {
      implOpeners.push(index + 1);
    }
  }
  if (implOpeners.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "impl ${target.implType}" inherent impl block at top level but found ${implOpeners.length}; remediation: confirm the inherent impl block exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    const implStart = implOpeners[0];
    const implEnd = scanBraceBalance(lines, implStart);
    if (implEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "impl ${target.implType}" starting at line ${implStart}; remediation: confirm the impl body is brace-balanced`,
      );
    } else {
      const fnMatches = [];
      for (let index = implStart; index < implEnd; index += 1) {
        if (lines[index].includes(target.fnSignature)) {
          fnMatches.push(index + 1);
        }
      }
      if (fnMatches.length !== 1) {
        fail(
          `${sourcePath}: expected exactly 1 "${target.fnSignature}" inside the impl ${target.implType} block (lines ${implStart}-${implEnd}) but found ${fnMatches.length}; remediation: confirm the ${target.fnName} fn signature has not changed and is not duplicated`,
        );
      } else {
        startLine = fnMatches[0];
        endLine = scanBraceBalance(lines, startLine);
        if (endLine === null) {
          fail(
            `${sourcePath}: could not find the matching closing brace for "${target.fnSignature}" starting at line ${startLine}; remediation: confirm the fn body is brace-balanced`,
          );
        }
      }
    }
  }
}

if (docs) {
  const match = docs.match(target.docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the A2ATaskResult::error fn block range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${target.docsLabel} cites covenant-a2a/src/lib.rs:${citedStart}-${citedEnd} but A2ATaskResult::${target.fnName} spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-a2a-task-result-error-fn-block-range-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-a2a-task-result-error-fn-block-range-line-refs: ok (A2ATaskResult::${target.fnName} lib.rs:${startLine}-${endLine})`,
);
