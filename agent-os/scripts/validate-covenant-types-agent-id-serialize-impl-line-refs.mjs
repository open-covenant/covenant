#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-types AgentId Serialize impl line-ref drift guard.
// docs/ipc-and-http-gateway.md cites the impl block at
// agent-os/crates/covenant-types/src/lib.rs:124 in four field-block
// sentences that share a fixed `{display, pubkey}` shape phrasing and
// differ only by the leading field name:
//
//   - `payer`     (settlement receipt field, intent_result/receipt_list)
//   - `agent_id`  (peer summary field, peer_list)
//   - `issuer`    (audit event field, audit_recent)
//   - `owner`     (memory record field, memory_read)
//
// The existing validate-covenant-types-struct-line-refs.mjs explicitly
// excludes this impl-line citation because the anchor convention is
// different: the citation refers to an `impl Serialize for AgentId`
// block rather than a `pub struct <Name>` declaration. The docs path
// style is `covenant-types/src/lib.rs:NN` (without the
// `agent-os/crates/` prefix), matching the established impl/annotation
// citation convention in this docs surface.
//
// All four citations reference the same line, so any drift of the impl
// block (lib.rs growth, refactor away from a dedicated impl Serialize
// block) silently invalidates four reader-facing references at once.
//
// This validator derives the impl line number at run time by matching
// `impl Serialize for AgentId` with a strict top-level regex (no
// indentation, no preceding generics), then asserts each of the four
// docs citations cites the derived line.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-types/src/lib.rs";

const citations = [
  {
    field: "payer",
    regex: /`payer` \(object\) — `\{display: string, pubkey: string \(base58\)\}` per the `AgentId` Serialize impl at `covenant-types\/src\/lib\.rs:(\d+)`\./,
    template:
      "`payer` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:NN`.",
  },
  {
    field: "agent_id",
    regex: /`agent_id` \(object\) — `\{display: string, pubkey: string \(base58\)\}` per the `AgentId` Serialize impl at `covenant-types\/src\/lib\.rs:(\d+)`\./,
    template:
      "`agent_id` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:NN`.",
  },
  {
    field: "issuer",
    regex: /`issuer` \(object\) — `\{display: string, pubkey: string \(base58\)\}` per the `AgentId` Serialize impl at `covenant-types\/src\/lib\.rs:(\d+)`\./,
    template:
      "`issuer` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:NN`.",
  },
  {
    field: "owner",
    regex: /`owner` \(object\) — `\{display: string, pubkey: string \(base58\)\}` per the `AgentId` Serialize impl at `covenant-types\/src\/lib\.rs:(\d+)`\./,
    template:
      "`owner` (object) — `{display: string, pubkey: string (base58)}` per the `AgentId` Serialize impl at `covenant-types/src/lib.rs:NN`.",
  },
];

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

let implLine = null;
if (source) {
  const matches = [];
  const lines = source.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    if (/^impl\s+Serialize\s+for\s+AgentId\b/.test(lines[index])) {
      matches.push(index + 1);
    }
  }
  if (matches.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "impl Serialize for AgentId" but found ${matches.length}; remediation: confirm the impl block exists at top level in covenant-types/src/lib.rs and is not renamed, moved, or duplicated`,
    );
  } else {
    implLine = matches[0];
  }
}

if (docs) {
  for (const citation of citations) {
    const match = docs.match(citation.regex);
    if (!match) {
      fail(
        `${docsPath}: missing the ${citation.field} field citation ("${citation.template}"); remediation: restore the citation that records the AgentId Serialize impl line ref for the ${citation.field} field`,
      );
      continue;
    }
    if (implLine !== null) {
      const cited = parseInt(match[1], 10);
      if (cited !== implLine) {
        fail(
          `${docsPath}: the ${citation.field} field citation cites covenant-types/src/lib.rs:${cited} but "impl Serialize for AgentId" is at line ${implLine}; remediation: update the citation to :${implLine}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-types-agent-id-serialize-impl-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-types-agent-id-serialize-impl-line-refs: ok (impl Serialize for AgentId lib.rs:${implLine} [4 citations: payer, agent_id, issuer, owner])`,
);
