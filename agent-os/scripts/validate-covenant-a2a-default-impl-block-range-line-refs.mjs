#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-a2a Default impl block range line-ref drift guard.
// docs/ipc-and-http-gateway.md line 480 cites the
// `impl Default for A2AAutoRetryPolicy` block in
// agent-os/crates/covenant-a2a/src/lib.rs as a range citation:
//
//   per `Default for A2AAutoRetryPolicy` at `covenant-a2a/src/lib.rs:228-238`
//
// The range spans the `impl Default for A2AAutoRetryPolicy {` opener
// (line 228), the `fn default() -> Self { ... }` body, and the
// matching closing brace (line 238).
//
// Unlike the struct/enum block range validators, this impl has no
// `#[...]` attribute block above it, so the range starts at the impl
// line itself rather than walking backwards. The end is located by a
// brace-balance scan from the opener.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-a2a/src/lib.rs";

const target = {
  traitName: "Default",
  typeName: "A2AAutoRetryPolicy",
  docsRegex:
    /per `Default for A2AAutoRetryPolicy` at `covenant-a2a\/src\/lib\.rs:(\d+)-(\d+)`/,
  docsLabel: "Default for A2AAutoRetryPolicy impl block range citation",
  docsTemplate:
    "per `Default for A2AAutoRetryPolicy` at `covenant-a2a/src/lib.rs:N-M`",
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

let startLine = null;
let endLine = null;
if (source) {
  const lines = source.split("\n");
  const openerRegex = new RegExp(
    `^impl\\s+${target.traitName}\\s+for\\s+${target.typeName}\\s*\\{`,
  );
  const openers = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (openerRegex.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "impl ${target.traitName} for ${target.typeName}" at top level but found ${openers.length}; remediation: confirm the impl exists at top level and has not been replaced by #[derive(${target.traitName})] or moved to a submodule`,
    );
  } else {
    startLine = openers[0];
    let depth = 0;
    let opened = false;
    for (let index = startLine - 1; index < lines.length; index += 1) {
      for (const char of lines[index]) {
        if (char === "{") {
          depth += 1;
          opened = true;
        } else if (char === "}") {
          depth -= 1;
        }
      }
      if (opened && depth === 0) {
        endLine = index + 1;
        break;
      }
    }
    if (endLine === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "impl ${target.traitName} for ${target.typeName}" starting at line ${startLine}; remediation: confirm the impl body is brace-balanced`,
      );
    }
  }
}

if (docs) {
  const match = docs.match(target.docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the impl ${target.traitName} for ${target.typeName} block range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${target.docsLabel} cites covenant-a2a/src/lib.rs:${citedStart}-${citedEnd} but the impl ${target.traitName} for ${target.typeName} block spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-a2a-default-impl-block-range-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-a2a-default-impl-block-range-line-refs: ok (impl ${target.traitName} for ${target.typeName} lib.rs:${startLine}-${endLine})`,
);
