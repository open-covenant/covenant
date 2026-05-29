#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-ipc struct line-ref drift guard. docs/ipc-and-http-gateway.md
// cites four cross-file name-anchored line refs that resolve to public
// struct declarations in agent-os/crates/covenant-ipc/src/lib.rs:
//
//   - VerifyCheck shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:NN`:
//   - VerifyDrift shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:NN`:
//   - The inner `ChainStatus` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:NN`:
//   - `ReceiptBatchSummary` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:NN`:
//
// All four line numbers shift whenever covenant-ipc/src/lib.rs grows
// above the cited struct declarations, and the docs do not auto-update.
//
// This validator derives each line number from covenant-ipc/src/lib.rs
// at run time by matching `pub struct <Name>` with a strict top-level
// regex (no indentation). The expected citation must include the derived
// line. Line numbers are never hardcoded; the validator self-corrects to
// wherever the declarations currently live, and a struct rename or move
// fails the validator until either the rename is reverted, the docs are
// updated, or this validator's structFnNames list is updated to track
// the new name.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const structsPath = "agent-os/crates/covenant-ipc/src/lib.rs";

const structs = {
  VerifyCheck: {
    matches: [],
    line: null,
    docsRegex: /VerifyCheck` shape, defined at `agent-os\/crates\/covenant-ipc\/src\/lib\.rs:(\d+)`:/,
    docsLabel: "VerifyCheck citation",
    docsTemplate: "`VerifyCheck` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:NN`:",
  },
  VerifyDrift: {
    matches: [],
    line: null,
    docsRegex: /VerifyDrift` shape, defined at `agent-os\/crates\/covenant-ipc\/src\/lib\.rs:(\d+)`:/,
    docsLabel: "VerifyDrift citation",
    docsTemplate: "`VerifyDrift` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:NN`:",
  },
  ChainStatus: {
    matches: [],
    line: null,
    docsRegex: /The inner `ChainStatus` shape, defined at `agent-os\/crates\/covenant-ipc\/src\/lib\.rs:(\d+)`:/,
    docsLabel: "ChainStatus citation",
    docsTemplate: "The inner `ChainStatus` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:NN`:",
  },
  ReceiptBatchSummary: {
    matches: [],
    line: null,
    docsRegex: /`ReceiptBatchSummary` shape, defined at `agent-os\/crates\/covenant-ipc\/src\/lib\.rs:(\d+)`:/,
    docsLabel: "ReceiptBatchSummary citation",
    docsTemplate: "`ReceiptBatchSummary` shape, defined at `agent-os/crates/covenant-ipc/src/lib.rs:NN`:",
  },
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
  source = read(structsPath);
} catch (error) {
  fail(`cannot read ${structsPath}: ${error.message}`);
}

if (source) {
  const lines = source.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const match = lines[index].match(/^pub\s+struct\s+(\w+)\b/);
    if (match && structs[match[1]] !== undefined) {
      structs[match[1]].matches.push(index + 1);
    }
  }
  for (const [name, entry] of Object.entries(structs)) {
    if (entry.matches.length !== 1) {
      fail(
        `${structsPath}: expected exactly 1 "pub struct ${name}" but found ${entry.matches.length}; remediation: confirm the ${name} struct exists at top level in covenant-ipc/src/lib.rs and is not renamed, moved, or duplicated`,
      );
    } else {
      entry.line = entry.matches[0];
    }
  }
}

if (docs) {
  for (const [name, entry] of Object.entries(structs)) {
    const match = docs.match(entry.docsRegex);
    if (!match) {
      fail(
        `${docsPath}: missing the ${entry.docsLabel} ("${entry.docsTemplate}"); remediation: restore the citation that records the ${name} struct line ref`,
      );
      continue;
    }
    if (entry.line !== null) {
      const cited = parseInt(match[1], 10);
      if (cited !== entry.line) {
        fail(
          `${docsPath}: the ${entry.docsLabel} cites covenant-ipc/src/lib.rs:${cited} but pub struct ${name} is at line ${entry.line}; remediation: update the citation to :${entry.line}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-ipc-struct-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-ipc-struct-line-refs: ok (VerifyCheck lib.rs:${structs.VerifyCheck.line}, VerifyDrift lib.rs:${structs.VerifyDrift.line}, ChainStatus lib.rs:${structs.ChainStatus.line}, ReceiptBatchSummary lib.rs:${structs.ReceiptBatchSummary.line})`,
);
