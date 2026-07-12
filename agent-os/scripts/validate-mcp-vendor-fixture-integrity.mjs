#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

const NAME = "validate-mcp-vendor-fixture-integrity";
const VENDOR_ROOT = "agent-os/vendor/mcp-server-time";

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

// Tree digest over the vendored files: sha256 of the sorted, newline-terminated
// `<file-sha256>  <relpath>` lines. This mirrors the recompute command recorded
// in VENDOR.md but enumerates git-tracked files rather than walking the tree, so
// build artifacts left behind by an opt-in live-test run (.venv, __pycache__,
// *.pyc — all gitignored) can never perturb the result.
function treeHash(entries) {
  const lines = entries.map((e) => `${sha256(e.bytes)}  ${e.relpath}`).sort();
  return sha256(Buffer.from(`${lines.join("\n")}\n`));
}

function parseManifest(text) {
  const marker = text.indexOf("tree_sha256");
  const tree = marker >= 0 ? text.slice(marker).match(/[0-9a-f]{64}/) : null;
  const commit = text.match(/Pinned commit\s*\|\s*`?([0-9a-f]{40})`?/);
  return {
    treeSha256: tree ? tree[0] : null,
    pinnedCommit: commit ? commit[1] : null,
  };
}

function trackedUnder(relDir) {
  const out = execFileSync("git", ["ls-files", "-z", relDir], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  return out.split("\0").filter(Boolean);
}

function vendoredSubdirs(trackedPaths, rootRel) {
  const prefix = `${rootRel}/`;
  const dirs = new Set();
  for (const path of trackedPaths) {
    if (!path.startsWith(prefix)) continue;
    const rest = path.slice(prefix.length);
    const slash = rest.indexOf("/");
    if (slash > 0) dirs.add(rest.slice(0, slash));
  }
  return [...dirs];
}

function runRepoCheck() {
  const errors = [];
  const manifestPath = join(repoRoot, VENDOR_ROOT, "VENDOR.md");
  if (!existsSync(manifestPath)) {
    return { dormant: true, errors };
  }

  const manifest = parseManifest(readFileSync(manifestPath, "utf8"));
  if (!manifest.treeSha256) {
    errors.push("VENDOR.md is missing a 64-hex tree_sha256");
  }
  if (!manifest.pinnedCommit) {
    errors.push("VENDOR.md is missing a 40-hex pinned commit");
  }

  const tracked = trackedUnder(VENDOR_ROOT);
  const subdirs = vendoredSubdirs(tracked, VENDOR_ROOT);
  if (subdirs.length !== 1) {
    errors.push(
      `expected exactly one vendored subdirectory under ${VENDOR_ROOT}, found ${subdirs.length}: ${subdirs.join(", ") || "(none)"}`,
    );
    return { dormant: false, errors };
  }

  const subdir = subdirs[0];
  if (manifest.pinnedCommit && !manifest.pinnedCommit.startsWith(subdir)) {
    errors.push(
      `vendored subdirectory '${subdir}' is not a prefix of the pinned commit '${manifest.pinnedCommit}'`,
    );
  }

  const prefix = `${VENDOR_ROOT}/${subdir}/`;
  const entries = tracked
    .filter((path) => path.startsWith(prefix))
    .map((path) => ({
      relpath: path.slice(prefix.length),
      bytes: readFileSync(join(repoRoot, path)),
    }));

  const license = entries.find((entry) => entry.relpath === "LICENSE");
  if (!license) {
    errors.push(`vendored fixture must ship a LICENSE at ${VENDOR_ROOT}/${subdir}/LICENSE`);
  } else if (license.bytes.length === 0) {
    errors.push(`${VENDOR_ROOT}/${subdir}/LICENSE is empty`);
  }

  if (manifest.treeSha256) {
    const computed = treeHash(entries);
    if (computed !== manifest.treeSha256) {
      errors.push(
        `vendored tree hash ${computed} does not match VENDOR.md tree_sha256 ${manifest.treeSha256}; the committed fixture bytes drifted from the recorded provenance`,
      );
    }
  }

  return { dormant: false, errors };
}

function runSelfTest() {
  const failures = [];
  const good = [
    { relpath: "LICENSE", bytes: Buffer.from("MIT") },
    { relpath: "src/server.py", bytes: Buffer.from("print(1)\n") },
  ];
  const base = treeHash(good);

  if (treeHash(good) !== base) {
    failures.push("treeHash is not deterministic for identical input");
  }

  const mutations = {
    "mutated bytes": [
      { relpath: "LICENSE", bytes: Buffer.from("MIT") },
      { relpath: "src/server.py", bytes: Buffer.from("print(2)\n") },
    ],
    "renamed file": [
      { relpath: "LICENSE", bytes: Buffer.from("MIT") },
      { relpath: "src/server_v2.py", bytes: Buffer.from("print(1)\n") },
    ],
    "added file": [...good, { relpath: "extra.py", bytes: Buffer.from("x") }],
    "removed file": good.slice(0, 1),
  };
  for (const [label, entries] of Object.entries(mutations)) {
    if (treeHash(entries) === base) {
      failures.push(`treeHash did not change on ${label}`);
    }
  }

  const goodManifest = [
    "`tree_sha256` of `abcd1234/`:",
    "",
    "```",
    "a".repeat(64),
    "```",
    "",
    `| Pinned commit | \`${"a".repeat(40)}\` (date) |`,
  ].join("\n");
  const parsed = parseManifest(goodManifest);
  if (parsed.treeSha256 !== "a".repeat(64)) {
    failures.push("parseManifest did not extract the tree_sha256");
  }
  if (parsed.pinnedCommit !== "a".repeat(40)) {
    failures.push("parseManifest did not extract the pinned commit");
  }
  if (parseManifest(`| Pinned commit | \`${"b".repeat(40)}\` |`).treeSha256 !== null) {
    failures.push("parseManifest should report a missing tree_sha256 as null");
  }
  if (parseManifest(`tree_sha256\n\`\`\`\n${"c".repeat(64)}\n\`\`\``).pinnedCommit !== null) {
    failures.push("parseManifest should report a missing pinned commit as null");
  }

  const dirs = vendoredSubdirs(
    [`${VENDOR_ROOT}/VENDOR.md`, `${VENDOR_ROOT}/dead/a.py`, `${VENDOR_ROOT}/dead/b.py`],
    VENDOR_ROOT,
  );
  if (dirs.length !== 1 || dirs[0] !== "dead") {
    failures.push(`vendoredSubdirs should collapse to the single subdirectory, got ${JSON.stringify(dirs)}`);
  }

  const commit = "a1e5a9a9b186f00462a8a2448ee041728ce052d5";
  if (!commit.startsWith("a1e5a9a9")) {
    failures.push("prefix check should accept the matching short hash");
  }
  if (commit.startsWith("deadbeef")) {
    failures.push("prefix check should reject a mismatched subdirectory");
  }

  return failures;
}

const args = new Set(process.argv.slice(2));
for (const arg of args) {
  if (!["--self-test", "--help", "-h"].includes(arg)) {
    console.error(`usage: ${NAME} [--self-test]`);
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log(
    `usage: ${NAME} [--self-test]\n\nBinds the committed mcp-server-time vendor fixture bytes to the tree_sha256 recorded in its VENDOR.md, over git-tracked files only.`,
  );
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error(`${NAME}: self-test failed`);
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log(`${NAME}: self-test ok`);
  process.exit(0);
}

const { dormant, errors } = runRepoCheck();
if (dormant) {
  console.log(`${NAME}: dormant (vendor fixture absent)`);
  process.exit(0);
}
if (errors.length > 0) {
  console.error(`${NAME}: failed`);
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}
console.log(`${NAME}: ok`);
