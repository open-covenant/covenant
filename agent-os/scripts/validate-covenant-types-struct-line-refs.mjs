#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-types struct line-ref drift guard.
// docs/ipc-and-http-gateway.md cites four cross-file name-anchored struct
// declarations in agent-os/crates/covenant-types/src/lib.rs across six
// docs sentences:
//
//   - SettlementReceipt — three citations (one "shape, defined at"
//     sentence and two "(defined at ...)" parenthetical sentences).
//   - Capability — one "is defined at" sentence.
//   - MemoryRecord — one "shape, defined at" sentence.
//   - MemoryCompactionOutcome — one "(defined at ...)" parenthetical
//     sentence.
//
// Other covenant-types citations use different anchor conventions
// (annotation-line citations for MemoryTier/ResourceKind, impl-line
// citation for AgentId Serialize impl, range citations for
// MemoryRepairMode) and are out of scope for this validator.
//
// All four struct line numbers shift whenever
// covenant-types/src/lib.rs grows above the cited declarations, and the
// docs do not auto-update. SettlementReceipt drift would silently
// affect all three of its citations at once.
//
// This validator derives the struct line numbers at run time by
// matching `pub struct <Name>` with a strict top-level regex (no
// indentation), then asserts every docs citation associated with that
// struct cites the derived line. Line numbers are never hardcoded.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const structsPath = "agent-os/crates/covenant-types/src/lib.rs";

const targets = {
  SettlementReceipt: {
    citations: [
      {
        regex: /The inner `SettlementReceipt` shape, defined at `agent-os\/crates\/covenant-types\/src\/lib\.rs:(\d+)`:/,
        label: "SettlementReceipt 'shape, defined at' citation",
        template: "The inner `SettlementReceipt` shape, defined at `agent-os/crates/covenant-types/src/lib.rs:NN`:",
      },
      {
        regex: /optional `SettlementReceipt` \(defined at `agent-os\/crates\/covenant-types\/src\/lib\.rs:(\d+)`\) carrying the on-chain or local settlement evidence when the intent consumed credits/,
        label: "SettlementReceipt 'optional ... (defined at)' citation (intent_result)",
        template: "optional `SettlementReceipt` (defined at `agent-os/crates/covenant-types/src/lib.rs:NN`) carrying the on-chain or local settlement evidence when the intent consumed credits",
      },
      {
        regex: /optional `SettlementReceipt` \(defined at `agent-os\/crates\/covenant-types\/src\/lib\.rs:(\d+)` and documented in the `receipt_list` block above\)/,
        label: "SettlementReceipt 'optional ... (defined at ... and documented in the receipt_list block above)' citation (intents_resume)",
        template: "optional `SettlementReceipt` (defined at `agent-os/crates/covenant-types/src/lib.rs:NN` and documented in the `receipt_list` block above)",
      },
    ],
    matches: [],
    line: null,
  },
  Capability: {
    citations: [
      {
        regex: /`Capability` is defined at `agent-os\/crates\/covenant-types\/src\/lib\.rs:(\d+)` \(fields: `subject`, `action`, `scope`, `granted_by`, `expires_at`\)/,
        label: "Capability 'is defined at' citation",
        template: "`Capability` is defined at `agent-os/crates/covenant-types/src/lib.rs:NN` (fields: `subject`, `action`, `scope`, `granted_by`, `expires_at`)",
      },
    ],
    matches: [],
    line: null,
  },
  MemoryRecord: {
    citations: [
      {
        regex: /The inner `MemoryRecord` shape, defined at `agent-os\/crates\/covenant-types\/src\/lib\.rs:(\d+)`:/,
        label: "MemoryRecord 'shape, defined at' citation",
        template: "The inner `MemoryRecord` shape, defined at `agent-os/crates/covenant-types/src/lib.rs:NN`:",
      },
    ],
    matches: [],
    line: null,
  },
  MemoryCompactionOutcome: {
    citations: [
      {
        regex: /structured `MemoryCompactionOutcome` \(defined at `agent-os\/crates\/covenant-types\/src\/lib\.rs:(\d+)`\), never a string blob/,
        label: "MemoryCompactionOutcome '(defined at)' citation",
        template: "structured `MemoryCompactionOutcome` (defined at `agent-os/crates/covenant-types/src/lib.rs:NN`), never a string blob",
      },
    ],
    matches: [],
    line: null,
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
    if (match && targets[match[1]] !== undefined) {
      targets[match[1]].matches.push(index + 1);
    }
  }
  for (const [name, entry] of Object.entries(targets)) {
    if (entry.matches.length !== 1) {
      fail(
        `${structsPath}: expected exactly 1 "pub struct ${name}" but found ${entry.matches.length}; remediation: confirm the ${name} struct exists at top level in covenant-types/src/lib.rs and is not renamed, moved, or duplicated`,
      );
    } else {
      entry.line = entry.matches[0];
    }
  }
}

if (docs) {
  for (const [name, entry] of Object.entries(targets)) {
    for (const citation of entry.citations) {
      const match = docs.match(citation.regex);
      if (!match) {
        fail(
          `${docsPath}: missing the ${citation.label} ("${citation.template}"); remediation: restore the citation that records the ${name} struct line ref`,
        );
        continue;
      }
      if (entry.line !== null) {
        const cited = parseInt(match[1], 10);
        if (cited !== entry.line) {
          fail(
            `${docsPath}: the ${citation.label} cites covenant-types/src/lib.rs:${cited} but pub struct ${name} is at line ${entry.line}; remediation: update the citation to :${entry.line}`,
          );
        }
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-types-struct-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-types-struct-line-refs: ok (SettlementReceipt lib.rs:${targets.SettlementReceipt.line} [3 citations], Capability lib.rs:${targets.Capability.line}, MemoryRecord lib.rs:${targets.MemoryRecord.line}, MemoryCompactionOutcome lib.rs:${targets.MemoryCompactionOutcome.line})`,
);
