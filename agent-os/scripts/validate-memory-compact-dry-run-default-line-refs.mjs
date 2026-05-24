#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory compact dry-run-default request line-ref drift guard.
// docs/ipc-and-http-gateway.md line 543 (the memory compact
// `Dry-run by default, mutates only with --apply` sentence) carries two
// main.rs cites no prior validator binds:
//   1. the `MemoryCompactionRequest` construction block (range
//      2263-2271) where `mode` defaults to `MemoryRepairMode::DryRun`
//      unless `--apply` is passed;
//   2. the mandatory `--reason` bail (single line 2270,
//      `reason: reason.context("missing --reason")?,`).
//
// Cite 2's source line is NOT globally unique — the
// `reason.context("missing --reason")?` pattern recurs across every
// --reason-taking verb (memory compact, memory/settlement backfill), so
// it is scoped to the cite-1 block range and asserted to occur exactly
// once inside it. Cite 1's closer is matched at the opener indentation
// (`};`) so the nested `mode: if apply { ... } else { ... }` arms — which
// close with `},` at deeper indent — cannot mis-close the range.
//
// docsRegex anchoring: `MemoryRepairMode::DryRun`, the `missing --reason`
// literal, and `when omitted` each appear only at line 543, so first-match
// capture cannot drift to the sister memory_compaction_plan section or any
// other verb.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const requestOpener = "let request = MemoryCompactionRequest {";
const reasonLine = 'reason: reason.context("missing --reason")?,';
const closerWindowLines = 40;

const rangeRegex =
  /defaults to `MemoryRepairMode::DryRun` \(per `main\.rs:(\d+)-(\d+)`\)/;
const reasonRegex =
  /the CLI bails with `"missing --reason"` at `main\.rs:(\d+)` when omitted/;

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

let requestRange = null;
let reasonAtLine = null;
if (source) {
  const lines = source.split("\n");
  const openers = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === requestOpener) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${requestOpener}\` but found ${openers.length}; remediation: confirm the memory compact CLI builds MemoryCompactionRequest exactly once`,
    );
  } else {
    const openerLine = openers[0];
    const indent = lines[openerLine - 1].match(/^\s*/)[0];
    const closer = `${indent}};`;
    let closerLine = null;
    for (let offset = 1; offset <= closerWindowLines; offset += 1) {
      const targetIndex = openerLine - 1 + offset;
      if (targetIndex >= lines.length) {
        break;
      }
      if (lines[targetIndex] === closer) {
        closerLine = targetIndex + 1;
        break;
      }
    }
    if (closerLine === null) {
      fail(
        `${sourcePath}:${openerLine}: no \`};\` at the opener indentation within ${closerWindowLines} lines after the MemoryCompactionRequest opener; remediation: confirm the request literal is closed at its own indentation`,
      );
    } else {
      requestRange = [openerLine, closerLine];
      const reasonHits = [];
      for (let line = openerLine; line <= closerLine; line += 1) {
        if (lines[line - 1].trim() === reasonLine) {
          reasonHits.push(line);
        }
      }
      if (reasonHits.length !== 1) {
        fail(
          `${sourcePath}: expected exactly 1 \`${reasonLine}\` within the MemoryCompactionRequest block (${openerLine}-${closerLine}) but found ${reasonHits.length}; remediation: confirm the memory compact request still requires --reason inside this block (the line is non-unique file-wide and must be scoped here)`,
        );
      } else {
        reasonAtLine = reasonHits[0];
      }
    }
  }
}

if (docs) {
  const rangeMatch = docs.match(rangeRegex);
  if (!rangeMatch) {
    fail(
      `${docsPath}: missing the memory compact dry-run-default range citation ("defaults to \`MemoryRepairMode::DryRun\` (per \`main.rs:N-M\`)"); remediation: restore the request-block cite in the Dry-run by default sentence`,
    );
  } else if (requestRange !== null) {
    const cited = [parseInt(rangeMatch[1], 10), parseInt(rangeMatch[2], 10)];
    if (cited[0] !== requestRange[0] || cited[1] !== requestRange[1]) {
      fail(
        `${docsPath}: the request-block cite is main.rs:${cited[0]}-${cited[1]} but the MemoryCompactionRequest block spans :${requestRange[0]}-${requestRange[1]}; remediation: update the citation to :${requestRange[0]}-${requestRange[1]}`,
      );
    }
  }

  const reasonMatch = docs.match(reasonRegex);
  if (!reasonMatch) {
    fail(
      `${docsPath}: missing the missing-reason bail citation ("the CLI bails with \`"missing --reason"\` at \`main.rs:N\` when omitted"); remediation: restore the --reason bail cite in the Dry-run by default sentence`,
    );
  } else if (reasonAtLine !== null) {
    const cited = parseInt(reasonMatch[1], 10);
    if (cited !== reasonAtLine) {
      fail(
        `${docsPath}: the missing-reason bail cite is main.rs:${cited} but the bail lives at :${reasonAtLine}; remediation: update the citation to :${reasonAtLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-memory-compact-dry-run-default-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-compact-dry-run-default-line-refs: ok (request block main.rs:${requestRange[0]}-${requestRange[1]}, --reason bail main.rs:${reasonAtLine})`,
);
