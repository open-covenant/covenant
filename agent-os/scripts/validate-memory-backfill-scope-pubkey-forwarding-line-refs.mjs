#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory.backfill --scope-pubkey forwarding double-citation line-ref
// drift guard. docs/ipc-and-http-gateway.md line 656 (the memory
// scope-pubkey reservation paragraph) carries two inline main.rs cites
// in one sentence:
//
//   * `main.rs:N1-M1` — the 6-line CLI parse arm that captures the
//     `--scope-pubkey <value>` flag into a local
//     `scope_pubkey: Option<String>` binding.
//   * `main.rs:N2` — the single-line `scope_pubkey,` field in the
//     `Request::BackfillMemoryRecords {` construction that forwards
//     the captured value to the daemon over the IPC wire.
//
// Mirror of validate-settlement-backfill-scope-pubkey-forwarding-line-refs.mjs.
// Same 6-line parse-arm shape and single-line field cite; the
// disambiguating scope anchor is the memory variant
// (`Request::BackfillMemoryRecords {`) instead of the settlement one.
//
// Region disambiguation: the two parse-arm anchor lines
// (`"--scope-pubkey" => {` and the `.context(...)?.clone(),` argument
// line) each appear exactly twice file-wide — once in the settlement
// backfill handler and once in the memory backfill handler. The
// validator scopes to the memory region via the unique
// `Request::BackfillMemoryRecords {` opener, then walks backward to
// find the parse arm and forward to find the field forwarding.
//
// docsRegex anchoring: line 656 anchors on memory-unique prose
// `Request::BackfillMemoryRecords.scope_pubkey (\`main.rs:N1-M1\`,
// \`main.rs:N2\`)`. The sibling settlement.backfill scope-pubkey
// paragraph at L639 uses `Request::BackfillSettlementReceipts.scope_pubkey`
// (different variant name), so first-match capture cannot drift, per
// the IPC docs pin collision anchor feedback. The trailing pair of
// help-text + file-header cites (`main.rs:1454` and `main.rs:12`)
// remain unbound and would belong to a separate slice.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const scopeAnchor = "&Request::BackfillMemoryRecords {";
const parseArmAnchor = '"--scope-pubkey" => {';
const fieldLine = "scope_pubkey,";

const parseArmShape = [
  { offset: 1, content: "i += 1;" },
  { offset: 2, content: "scope_pubkey = Some(" },
  {
    offset: 3,
    content:
      'args.get(i).context("--scope-pubkey needs a value")?.clone(),',
  },
  { offset: 4, content: ");" },
  { offset: 5, content: "}" },
];

const docsRegex =
  /forwards it through `Request::BackfillMemoryRecords\.scope_pubkey` \(`main\.rs:(\d+)-(\d+)`, `main\.rs:(\d+)`\)/;
const docsLabel =
  "memory.backfill --scope-pubkey forwarding double citation";
const docsTemplate =
  "forwards it through `Request::BackfillMemoryRecords.scope_pubkey` (`main.rs:N1-M1`, `main.rs:N2`)";

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

let parseArmRange = null;
let fieldLineNumber = null;
if (source) {
  const lines = source.split("\n");
  const scopeMatches = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === scopeAnchor) {
      scopeMatches.push(index + 1);
    }
  }
  if (scopeMatches.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${scopeAnchor}\` but found ${scopeMatches.length}; remediation: confirm the Request::BackfillMemoryRecords construction is present exactly once in the memory backfill-receipt-correlation CLI handler`,
    );
  } else {
    const scopeLine = scopeMatches[0];
    let armStart = null;
    for (let index = scopeLine - 2; index >= 0; index -= 1) {
      if (lines[index].trim() === parseArmAnchor) {
        armStart = index + 1;
        break;
      }
    }
    if (armStart === null) {
      fail(
        `${sourcePath}: could not find \`${parseArmAnchor}\` before \`${scopeAnchor}\` at line ${scopeLine}; remediation: confirm the --scope-pubkey parse arm precedes the Request construction in the memory handler`,
      );
    } else {
      let shapeOk = true;
      for (const expected of parseArmShape) {
        const targetLine = armStart + expected.offset;
        const actual = lines[targetLine - 1];
        if (actual === undefined || actual.trim() !== expected.content) {
          fail(
            `${sourcePath}:${targetLine}: expected \`${expected.content}\` (offset ${expected.offset} from parse-arm opener) but found \`${(actual ?? "").trim()}\`; remediation: the 6-line parse-arm convention requires this layout — restore the structural shape or update this validator if the convention intentionally changed`,
          );
          shapeOk = false;
        }
      }
      if (shapeOk) {
        parseArmRange = [armStart, armStart + 5];
      }
    }

    const fieldTargetLine = scopeLine + 2;
    const fieldActual = lines[fieldTargetLine - 1];
    if (fieldActual === undefined || fieldActual.trim() !== fieldLine) {
      fail(
        `${sourcePath}:${fieldTargetLine}: expected \`${fieldLine}\` (offset +2 from \`${scopeAnchor}\` at line ${scopeLine}) but found \`${(fieldActual ?? "").trim()}\`; remediation: confirm the scope_pubkey field appears as the second field of Request::BackfillMemoryRecords (after dry_run)`,
      );
    } else {
      fieldLineNumber = fieldTargetLine;
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the two inline cites in the memory --scope-pubkey reservation paragraph`,
    );
  } else if (parseArmRange !== null && fieldLineNumber !== null) {
    const citedArmStart = parseInt(match[1], 10);
    const citedArmEnd = parseInt(match[2], 10);
    const citedField = parseInt(match[3], 10);
    if (
      citedArmStart !== parseArmRange[0] ||
      citedArmEnd !== parseArmRange[1]
    ) {
      fail(
        `${docsPath}: the parse-arm cite is main.rs:${citedArmStart}-${citedArmEnd} but the --scope-pubkey parse arm lives at :${parseArmRange[0]}-${parseArmRange[1]}; remediation: update the citation to :${parseArmRange[0]}-${parseArmRange[1]}`,
      );
    }
    if (citedField !== fieldLineNumber) {
      fail(
        `${docsPath}: the scope_pubkey field cite is main.rs:${citedField} but the field forwarding lives at :${fieldLineNumber}; remediation: update the citation to :${fieldLineNumber}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-memory-backfill-scope-pubkey-forwarding-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-backfill-scope-pubkey-forwarding-line-refs: ok (parse arm main.rs:${parseArmRange[0]}-${parseArmRange[1]}, scope_pubkey field main.rs:${fieldLineNumber})`,
);
