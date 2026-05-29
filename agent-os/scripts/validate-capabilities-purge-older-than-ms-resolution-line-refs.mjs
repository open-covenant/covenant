#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// capabilities_purge `--older-than-ms` clock-resolution line-ref drift guard.
// docs/ipc-and-http-gateway.md line 283 (the capabilities_purged `before_ms`
// bullet) cites the source that resolves `--older-than-ms <D>` against the
// system clock as `now - D`. That resolution is the five-line block
//   let now = std::time::SystemTime::now()
//       .duration_since(std::time::UNIX_EPOCH)
//       .map(|d| d.as_millis() as u64)
//       .unwrap_or(0);
//   before_ms = Some(now.saturating_sub(dur));
// not the arg-parse loop tail the cite previously pointed at.
//
// The `--older-than-ms` arm and both resolution statements recur across the
// sibling purge verbs, so the block is scoped through the unique
// `Request::PurgeCapabilities {` opener: walk back to the nearest
// `before_ms = Some(now.saturating_sub(dur));` (range end) and then to the
// nearest `let now = std::time::SystemTime::now()` (range start), and assert
// the intervening `duration_since`/`unwrap_or` chain so an unrelated match
// cannot pass.
//
// L283 also carries the `Pinned as u64 by main.rs:N-M` before_ms type cite,
// which the capabilities-purge type-level validator guards. The docsRegex here
// anchors on the L283-unique `as \`now - D\` per` phrase so first-match capture
// binds the resolution cite, not the u64 pin.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const verbAnchor = "Request::PurgeCapabilities {";
const resolutionEnd = "before_ms = Some(now.saturating_sub(dur));";
const resolutionStart = "let now = std::time::SystemTime::now()";
const durationLine = ".duration_since(std::time::UNIX_EPOCH)";
const unwrapLine = ".unwrap_or(0);";
const backWindow = 60;

const docsRegex = /as `now - D` per `main\.rs:(\d+)-(\d+)`/;
const docsLabel = "capabilities_purge older-than-ms resolution citation";
const docsTemplate = "resolved against the system clock as `now - D` per `main.rs:N-M`";

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

let blockRange = null;
if (source) {
  const lines = source.split("\n");
  const verbLines = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].includes(verbAnchor)) {
      verbLines.push(index + 1);
    }
  }
  if (verbLines.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${verbAnchor}\` but found ${verbLines.length}; remediation: confirm the capabilities purge verb dispatches PurgeCapabilities exactly once`,
    );
  } else {
    const verbLine = verbLines[0];
    let endLine = null;
    for (let line = verbLine - 1; line >= Math.max(1, verbLine - backWindow); line -= 1) {
      if ((lines[line - 1] ?? "").trim() === resolutionEnd) {
        endLine = line;
        break;
      }
    }
    if (endLine === null) {
      fail(
        `${sourcePath}:${verbLine}: no \`${resolutionEnd}\` within ${backWindow} lines above the PurgeCapabilities dispatch; remediation: confirm --older-than-ms still resolves before_ms via now.saturating_sub(dur)`,
      );
    } else {
      let startLine = null;
      for (let line = endLine - 1; line >= Math.max(1, endLine - 8); line -= 1) {
        if ((lines[line - 1] ?? "").trim() === resolutionStart) {
          startLine = line;
          break;
        }
      }
      if (startLine === null) {
        fail(
          `${sourcePath}:${endLine}: no \`${resolutionStart}\` opener within 8 lines above the now - D assignment; remediation: confirm the system-clock read precedes the saturating_sub`,
        );
      } else if ((lines[startLine].trim() !== durationLine) || (lines[endLine - 2].trim() !== unwrapLine)) {
        fail(
          `${sourcePath}:${startLine}-${endLine}: the resolution block does not match the expected duration_since/unwrap_or chain; remediation: confirm the --older-than-ms clock resolution shape before re-citing`,
        );
      } else {
        blockRange = [startLine, endLine];
      }
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the older-than-ms resolution range cite in the capabilities_purged before_ms bullet`,
    );
  } else if (blockRange !== null) {
    const cited = [parseInt(match[1], 10), parseInt(match[2], 10)];
    if (cited[0] !== blockRange[0] || cited[1] !== blockRange[1]) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${cited[0]}-${cited[1]} but the older-than-ms resolution spans :${blockRange[0]}-${blockRange[1]}; remediation: update the citation to :${blockRange[0]}-${blockRange[1]}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-capabilities-purge-older-than-ms-resolution-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-capabilities-purge-older-than-ms-resolution-line-refs: ok (older-than-ms resolution main.rs:${blockRange[0]}-${blockRange[1]})`,
);
