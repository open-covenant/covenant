#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenantd Server::recent_capabilities fn block range line-ref drift guard.
// docs/ipc-and-http-gateway.md line 251 cites the peer-visibility filter
// as a range citation:
//
//   The daemon applies a **peer-visibility filter** before returning the
//   list (see `recent_capabilities` at
//   `agent-os/crates/covenantd/src/lib.rs:NNNN-MMMM`): ...
//
// The range spans the `async fn recent_capabilities(&self, ...) -> Response {`
// opener through its matching closing brace.
//
// covenantd/src/lib.rs contains more than one `impl Server { ... }` block,
// so the fn lookup is scoped to the unique impl Server block whose
// brace-balanced body contains the documented fn — mirroring the
// covenant-a2a fn-block-range validators' impl-scoped convention. The fn
// body's end line is then located by a second brace-balance scan from
// the fn opener.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenantd/src/lib.rs";

const target = {
  implType: "Server",
  fnName: "recent_capabilities",
  fnSignature: "async fn recent_capabilities(",
  docsRegex:
    /\(see `recent_capabilities` at `agent-os\/crates\/covenantd\/src\/lib\.rs:(\d+)-(\d+)`\)/,
  docsLabel: "Server::recent_capabilities fn block range citation",
  docsTemplate:
    "(see `recent_capabilities` at `agent-os/crates/covenantd/src/lib.rs:N-M`)",
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
  if (implOpeners.length === 0) {
    fail(
      `${sourcePath}: expected at least 1 "impl ${target.implType}" inherent impl block at top level but found 0; remediation: confirm the impl block containing ${target.fnName} exists at top level`,
    );
  } else {
    const fnContainers = [];
    for (const implStart of implOpeners) {
      const implEnd = scanBraceBalance(lines, implStart);
      if (implEnd === null) {
        fail(
          `${sourcePath}: could not find the matching closing brace for "impl ${target.implType}" starting at line ${implStart}; remediation: confirm the impl body is brace-balanced`,
        );
        continue;
      }
      const fnMatches = [];
      for (let index = implStart; index < implEnd; index += 1) {
        if (lines[index].includes(target.fnSignature)) {
          fnMatches.push(index + 1);
        }
      }
      if (fnMatches.length === 1) {
        fnContainers.push({ implStart, implEnd, fnLine: fnMatches[0] });
      } else if (fnMatches.length > 1) {
        fail(
          `${sourcePath}: found ${fnMatches.length} occurrences of "${target.fnSignature}" inside the impl ${target.implType} block (lines ${implStart}-${implEnd}); remediation: confirm the ${target.fnName} fn is not duplicated inside one impl block`,
        );
      }
    }
    if (errors.length === 0) {
      if (fnContainers.length !== 1) {
        fail(
          `${sourcePath}: expected exactly 1 "impl ${target.implType}" block containing "${target.fnSignature}" but found ${fnContainers.length}; remediation: confirm the documented ${target.fnName} fn lives in exactly one impl ${target.implType} block`,
        );
      } else {
        startLine = fnContainers[0].fnLine;
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
      `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the recent_capabilities fn block range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${target.docsLabel} cites covenantd/src/lib.rs:${citedStart}-${citedEnd} but Server::${target.fnName} spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenantd-recent-capabilities-fn-block-range-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenantd-recent-capabilities-fn-block-range-line-refs: ok (Server::${target.fnName} lib.rs:${startLine}-${endLine})`,
);
