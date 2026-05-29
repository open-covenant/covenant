#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-a2a struct line-ref drift guard.
// docs/ipc-and-http-gateway.md cites seven `pub struct` declarations
// in agent-os/crates/covenant-a2a/src/lib.rs across docs sentences with
// mixed path-prefix styles:
//
//   - A2ATaskQueueEntry      (full-prefix, line 444)
//   - A2ATask                (full-prefix, line 453)
//   - A2ATaskResult          (full-prefix, line 464)
//   - A2AAutoRetryReport     (full-prefix, line 478)
//   - A2AAutoRetryPolicy     (relative,    line 489)
//   - A2AAutoRetryRequeued   (relative,    line 497)
//   - A2AAutoRetrySkipped    (relative,    line 504)
//
// All seven line numbers shift whenever covenant-a2a/src/lib.rs grows
// above the cited declarations, and the docs do not auto-update.
//
// This validator derives the struct line numbers at run time by
// matching `pub struct <Name>` with a strict top-level regex (no
// indentation), then asserts each docs citation contains the derived
// line. Line numbers are never hardcoded.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-a2a/src/lib.rs";

const targets = {
  A2ATaskQueueEntry: {
    regex:
      /The inner `A2ATaskQueueEntry` shape, defined at `agent-os\/crates\/covenant-a2a\/src\/lib\.rs:(\d+)`:/,
    label: "A2ATaskQueueEntry 'shape, defined at' citation",
    template:
      "The inner `A2ATaskQueueEntry` shape, defined at `agent-os/crates/covenant-a2a/src/lib.rs:NN`:",
  },
  A2ATask: {
    regex:
      /The nested `A2ATask` shape, defined at `agent-os\/crates\/covenant-a2a\/src\/lib\.rs:(\d+)`:/,
    label: "A2ATask 'shape, defined at' citation",
    template:
      "The nested `A2ATask` shape, defined at `agent-os/crates/covenant-a2a/src/lib.rs:NN`:",
  },
  A2ATaskResult: {
    regex:
      /The inner `A2ATaskResult` shape, defined at `agent-os\/crates\/covenant-a2a\/src\/lib\.rs:(\d+)`:/,
    label: "A2ATaskResult 'shape, defined at' citation",
    template:
      "The inner `A2ATaskResult` shape, defined at `agent-os/crates/covenant-a2a/src/lib.rs:NN`:",
  },
  A2AAutoRetryReport: {
    regex:
      /structured `A2AAutoRetryReport` \(defined at `agent-os\/crates\/covenant-a2a\/src\/lib\.rs:(\d+)`\)/,
    label: "A2AAutoRetryReport '(defined at)' citation",
    template:
      "structured `A2AAutoRetryReport` (defined at `agent-os/crates/covenant-a2a/src/lib.rs:NN`)",
  },
  A2AAutoRetryPolicy: {
    regex:
      /The inner `A2AAutoRetryPolicy` shape, defined at `covenant-a2a\/src\/lib\.rs:(\d+)`:/,
    label: "A2AAutoRetryPolicy 'shape, defined at' citation",
    template:
      "The inner `A2AAutoRetryPolicy` shape, defined at `covenant-a2a/src/lib.rs:NN`:",
  },
  A2AAutoRetryRequeued: {
    regex:
      /The inner `A2AAutoRetryRequeued` shape, defined at `covenant-a2a\/src\/lib\.rs:(\d+)`:/,
    label: "A2AAutoRetryRequeued 'shape, defined at' citation",
    template:
      "The inner `A2AAutoRetryRequeued` shape, defined at `covenant-a2a/src/lib.rs:NN`:",
  },
  A2AAutoRetrySkipped: {
    regex:
      /The inner `A2AAutoRetrySkipped` shape, defined at `covenant-a2a\/src\/lib\.rs:(\d+)`:/,
    label: "A2AAutoRetrySkipped 'shape, defined at' citation",
    template:
      "The inner `A2AAutoRetrySkipped` shape, defined at `covenant-a2a/src/lib.rs:NN`:",
  },
};

const errors = [];
const fail = (message) => errors.push(message);

const matches = {};
for (const name of Object.keys(targets)) {
  matches[name] = [];
}

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

const lineByName = {};
if (source) {
  const lines = source.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const m = lines[index].match(/^pub\s+struct\s+(\w+)\b/);
    if (m && matches[m[1]] !== undefined) {
      matches[m[1]].push(index + 1);
    }
  }
  for (const name of Object.keys(targets)) {
    if (matches[name].length !== 1) {
      fail(
        `${sourcePath}: expected exactly 1 "pub struct ${name}" at top level but found ${matches[name].length}; remediation: confirm the ${name} struct exists at top level in covenant-a2a/src/lib.rs and is not renamed, moved, or duplicated`,
      );
    } else {
      lineByName[name] = matches[name][0];
    }
  }
}

if (docs) {
  for (const [name, target] of Object.entries(targets)) {
    const match = docs.match(target.regex);
    if (!match) {
      fail(
        `${docsPath}: missing the ${target.label} ("${target.template}"); remediation: restore the citation that records the ${name} struct line ref`,
      );
      continue;
    }
    if (lineByName[name] !== undefined) {
      const cited = parseInt(match[1], 10);
      if (cited !== lineByName[name]) {
        fail(
          `${docsPath}: the ${target.label} cites covenant-a2a/src/lib.rs:${cited} but "pub struct ${name}" is at line ${lineByName[name]}; remediation: update the citation to :${lineByName[name]}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-a2a-struct-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const summary = Object.keys(targets)
  .map((name) => `${name} lib.rs:${lineByName[name]}`)
  .join(", ");
console.log(`validate-covenant-a2a-struct-line-refs: ok (${summary})`);
