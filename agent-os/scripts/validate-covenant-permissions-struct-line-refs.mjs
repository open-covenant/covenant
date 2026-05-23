#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-permissions struct line-ref drift guard.
// docs/ipc-and-http-gateway.md (line 249) cites the SignedCapability
// struct at agent-os/crates/covenant-permissions/src/lib.rs:NN via the
// phrasing:
//
//   "and `SignedCapability` is defined at
//   `agent-os/crates/covenant-permissions/src/lib.rs:NN`."
//
// The same docs sentence also cites the sig_b58 module range
// (`lib.rs:64-83`), but that anchor convention (range over a `mod`
// block) is deferred to a future slice.
//
// The SignedCapability line shifts whenever covenant-permissions/src/lib.rs
// grows above the cited declaration, and the docs do not auto-update.
//
// This validator derives the line number from
// covenant-permissions/src/lib.rs at run time by matching
// `pub struct SignedCapability` with a strict top-level regex (no
// indentation).

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const structsPath = "agent-os/crates/covenant-permissions/src/lib.rs";

const structName = "SignedCapability";

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

let structLine = null;
const structMatches = [];

if (source) {
  const lines = source.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const match = lines[index].match(/^pub\s+struct\s+(\w+)\b/);
    if (match && match[1] === structName) {
      structMatches.push(index + 1);
    }
  }
  if (structMatches.length !== 1) {
    fail(
      `${structsPath}: expected exactly 1 "pub struct ${structName}" but found ${structMatches.length}; remediation: confirm the ${structName} struct exists at top level in covenant-permissions/src/lib.rs and is not renamed, moved, or duplicated`,
    );
  } else {
    structLine = structMatches[0];
  }
}

let citation = null;
if (docs) {
  const match = docs.match(
    /and `SignedCapability` is defined at `agent-os\/crates\/covenant-permissions\/src\/lib\.rs:(\d+)`\./,
  );
  if (!match) {
    fail(
      `${docsPath}: missing the SignedCapability citation ("and \`SignedCapability\` is defined at \`agent-os/crates/covenant-permissions/src/lib.rs:NN\`."); remediation: restore the citation that records the SignedCapability struct line ref`,
    );
  } else {
    citation = parseInt(match[1], 10);
  }
}

if (citation !== null && structLine !== null && citation !== structLine) {
  fail(
    `${docsPath}: the SignedCapability citation cites covenant-permissions/src/lib.rs:${citation} but pub struct ${structName} is at line ${structLine}; remediation: update the citation to :${structLine}`,
  );
}

if (errors.length > 0) {
  console.error("validate-covenant-permissions-struct-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-permissions-struct-line-refs: ok (SignedCapability lib.rs:${structLine})`,
);
