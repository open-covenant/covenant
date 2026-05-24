#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// capability_list per-capability format CLI print line-ref drift
// guard. docs/ipc-and-http-gateway.md line 255 cites a main.rs
// line range for the multi-line `println!("{} → {} ({}) [{}]", ...)`
// that emits one line per capability when the daemon returns a
// non-empty list. The cite anchors the documented format
// `<subject_display> → <action_label> (<granted_by_display>)
// [<expiry>]` to the source-of-truth so a refactor that shifts the
// CLI handler is caught at the docs-validator level — same drift
// class as the print-line and default-limit cites corrected at
// commits 0d6c0b0f through 7d3b0812, with the addition that this
// slice handles a multi-line println! macro using the
// assert!-opener-to-closer 4-line convention familiar from
// validate-audit-recent-type-level-pin-line-refs.mjs.
//
// Selector form: the single-line argument `c.capability.subject.display,`
// (with trailing comma) is UNIQUE inside main.rs — it appears
// exactly once, inside the capability_list per-capability println!
// arg list. The validator finds the selector line, then derives
// the range opener as the `println!(` line directly above the
// preceding format-string line, and the range closer as the next
// `);` on its own line. The full range becomes opener-to-closer.
//
// docsRegex anchoring: line 255 contains MULTIPLE `main.rs:N`
// cites in the source-of-truth sentence — the helper-fn cite
// (`main.rs:4344`), the renders-test cite (`main.rs:5862`), the
// pins-test cite (`main.rs:5902`), the CLI verb wiring range
// (`main.rs:2568-2624`), and the per-capability format print
// range (target of this validator). The regex anchors on the
// unique format-string `<subject_display> → <action_label>
// (<granted_by_display>) [<expiry>]` followed immediately by the
// cite slot. Per the IPC docs pin collision anchor feedback, the
// regex extends past the unique format literals through the cite
// to prevent first-match contamination from the surrounding
// cites in the same source-of-truth sentence.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const selector = "c.capability.subject.display,";

const docsRegex =
  /the same response prints one line per capability in the form `<subject_display> → <action_label> \(<granted_by_display>\) \[<expiry>\]` at `main\.rs:(\d+)-(\d+)`/;
const docsLabel =
  "capability_list per-capability format CLI print citation";
const docsTemplate =
  "the same response prints one line per capability in the form `<subject_display> → <action_label> (<granted_by_display>) [<expiry>]` at `main.rs:N-M`";

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
  const selectorMatches = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === selector) {
      selectorMatches.push(index + 1);
    }
  }
  if (selectorMatches.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` at top level but found ${selectorMatches.length}; remediation: confirm the capability_list per-capability println! arg list is present exactly once, not renamed or duplicated`,
    );
  } else {
    const selectorLine = selectorMatches[0];
    let opener = null;
    for (let scan = selectorLine - 2; scan >= 0 && scan >= selectorLine - 6; scan -= 1) {
      if (lines[scan].trim() === "println!(") {
        opener = scan + 1;
        break;
      }
    }
    if (opener === null) {
      fail(
        `${sourcePath}: could not find the \`println!(\` opener within 5 lines above \`${selector}\` at line ${selectorLine}; remediation: confirm the println! macro opens on its own line directly above the format-string line`,
      );
    } else {
      let closer = null;
      for (let scan = selectorLine; scan < lines.length && scan < selectorLine + 10; scan += 1) {
        if (lines[scan].trim() === ");") {
          closer = scan + 1;
          break;
        }
      }
      if (closer === null) {
        fail(
          `${sourcePath}: could not find the closing \`);\` within 10 lines below \`${selector}\` at line ${selectorLine}; remediation: confirm the println! macro closes on its own line`,
        );
      } else {
        startLine = opener;
        endLine = closer;
      }
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the per-capability format CLI print line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the per-capability format print spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-capability-list-print-format-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-capability-list-print-format-line-refs: ok (per-capability print main.rs:${startLine}-${endLine})`,
);
