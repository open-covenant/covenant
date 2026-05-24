#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// settlement.backfill section-preamble intra-doc line-ref drift
// guard. docs/ipc-and-http-gateway.md line 630 (the
// `covenant settlement backfill-receipts` paragraph) carries the
// only intra-document cross-reference of its kind in the file:
// "consistent with the section preamble (line N)". The cited
// line is the leading sentence of the `## CLI Read Envelopes`
// section, which explains the two-discriminator-subfamily
// structure that the settlement.backfill paragraph relies on to
// justify its schema-suffix asymmetry from the older `kind`
// envelopes.
//
// Selector form: the docsRegex anchors on the full unique phrase
// `consistent with the section preamble \(line (\d+)\):` — the
// "(line N)" parenthetical alone is not unique enough for future
// maintenance (other intra-doc cites may land later). Per the
// IPC docs pin collision anchor feedback, the regex extends
// through the "section preamble" phrasing so a sibling
// intra-doc cite cannot capture the first match by accident.
//
// Target identity: the section preamble at line 86 begins with
// the literal `A separate family of \`--json\` envelopes covers
// read-side chain queries`. The validator pins that opening
// substring as the section-preamble identity (trailing prose may
// evolve — the preamble continues with "and most other CLI
// surfaces. The section is **structurally mixed**: ..." — but
// the leading clause is the structural anchor that the cite is
// claiming to reference).

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";

const docsRegex =
  /consistent with the section preamble \(line (\d+)\):/;
const docsLabel =
  "settlement.backfill section-preamble cross-reference";
const docsTemplate =
  "consistent with the section preamble (line N):";
const preambleLead =
  "A separate family of `--json` envelopes covers read-side chain queries";

const errors = [];
const fail = (message) => errors.push(message);

let docs;
try {
  docs = read(docsPath);
} catch (error) {
  fail(`cannot read ${docsPath}: ${error.message}`);
}

let preambleLine = null;
if (docs) {
  const lines = docs.split("\n");
  const candidates = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].startsWith(preambleLead)) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${docsPath}: expected exactly 1 line beginning with "${preambleLead}" but found ${candidates.length}; remediation: confirm the CLI Read Envelopes section preamble still leads with the documented opening clause`,
    );
  } else {
    preambleLine = candidates[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the section-preamble line number`,
    );
  } else if (preambleLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== preambleLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites line ${citedLine} but the section preamble ("${preambleLead}") lives at line ${preambleLine}; remediation: update the citation to line ${preambleLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-settlement-backfill-section-preamble-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-settlement-backfill-section-preamble-line-refs: ok (section preamble at line ${preambleLine})`,
);
