#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory.backfill savepoint_name &str field-type cite line-ref drift
// guard. docs/ipc-and-http-gateway.md line 651 (the savepoint_name
// bullet) carries a parenthetical inline cite proving the assertion
// `the field type at memory_backfill_json (main.rs:N) is &str, not
// Option<&str>`. The cited line is the function signature for
// `memory_backfill_json`, whose `savepoint_name: &str` parameter type
// is the source-of-truth that pins the non-null wire shape at compile
// time (the &str emitter type forbids null serialization).
//
// The same line number (4571) is also cited at L660 in the
// envelope-source-of-truth sentence (`The envelope source-of-truth
// lives at memory_backfill_json in agent-os/crates/covenant/src/main.rs:N`).
// That L660 cite is already bound by validate-memory-backfill-line-refs.mjs;
// this validator targets only the L651 parenthetical cite. The
// existing validate-memory-backfill-type-level-pin-line-refs.mjs
// savepoint_name docsRegex matches the L651 bullet but uses
// non-capturing `\d+` for this parenthetical cite and captures only
// the trailing Pinned-as type-pin cite (5641-5644). The two validators
// coexist with disjoint capture targets.
//
// Mirror of the settlement.backfill rollback_path inline-cite slice
// shipped immediately prior. Settlement uses `rollback_path` with
// Option<&str>/inline .as_deref() conversion; memory uses the simpler
// `savepoint_name: &str` so the inline-cite is the function signature
// itself rather than a conversion call site.
//
// Anchor: the trimmed full line
// `fn memory_backfill_json(row_count: u64, savepoint_name: &str, dry_run: bool) -> serde_json::Value {`
// appears exactly once in main.rs (verified by grep -nF). The full-line
// match implicitly verifies the docs' field-type claim because the
// anchor includes the `savepoint_name: &str` substring — an
// Option-wrapping refactor that broke the docs claim would also break
// the anchor and fail this validator.
//
// docsRegex anchoring: line 651 contains the parenthetical cite inside
// the savepoint_name field-type sentence. The regex anchors on
// `the field type at \`memory_backfill_json\` (\`main.rs:N\`) is
// \`&str\`, not \`Option<&str>\`` — memory-specific prose with the
// uniquely-identifying `Option<&str>` clause. The L660 source-of-truth
// cite uses different prose (`The envelope source-of-truth lives at
// memory_backfill_json in agent-os/crates/covenant/src/main.rs:N`), so
// first-match capture cannot drift between the two L651/L660 cites.
// Sibling settlement.backfill bullets use `rollback_path` (no
// savepoint_name references at all), so no sibling-envelope drift
// risk, per the IPC docs pin collision anchor feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const signatureAnchor =
  "fn memory_backfill_json(row_count: u64, savepoint_name: &str, dry_run: bool) -> serde_json::Value {";

const docsRegex =
  /the field type at `memory_backfill_json` \(`main\.rs:(\d+)`\) is `&str`, not `Option<&str>`/;
const docsLabel =
  "memory.backfill savepoint_name &str field-type citation";
const docsTemplate =
  "the field type at `memory_backfill_json` (`main.rs:N`) is `&str`, not `Option<&str>`";

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

let signatureLine = null;
if (source) {
  const lines = source.split("\n");
  const candidates = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === signatureAnchor) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${signatureAnchor}\` but found ${candidates.length}; remediation: confirm the memory_backfill_json signature is present exactly once with savepoint_name typed as &str (not Option<&str>) — an Option-wrapping refactor would break the docs' field-type claim and must update both the signature and the L651 docs prose in the same change`,
    );
  } else {
    signatureLine = candidates[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the parenthetical inline cite in the savepoint_name field-type sentence`,
    );
  } else if (signatureLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== signatureLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the memory_backfill_json signature lives at :${signatureLine}; remediation: update the citation to :${signatureLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-memory-backfill-savepoint-name-field-type-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-backfill-savepoint-name-field-type-line-refs: ok (savepoint_name &str signature main.rs:${signatureLine})`,
);
