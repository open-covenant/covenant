#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const agentRoot = resolve(here, "..");
const repoRoot = resolve(agentRoot, "..");
const provenanceScript = join(agentRoot, "scripts", "provenance.mjs");
const fixture = join(
  repoRoot,
  "docs",
  "provenance",
  "attestations",
  "20ff55e-memory-drift-reports.json",
);

function run(args) {
  return spawnSync(process.execPath, [provenanceScript, ...args], {
    cwd: repoRoot,
    encoding: "utf8",
  });
}

function expectOk(args) {
  const result = run(args);
  if (result.status !== 0) {
    throw new Error(`expected success: ${result.stderr || result.stdout}`);
  }
}

function expectFail(args, expected) {
  const result = run(args);
  if (result.status === 0) {
    throw new Error(`expected failure for ${args.join(" ")}`);
  }
  const output = `${result.stderr}\n${result.stdout}`;
  if (!output.includes(expected)) {
    throw new Error(`expected failure to mention ${expected}; got ${output}`);
  }
}

const tempDir = mkdtempSync(join(tmpdir(), "covenant-provenance-"));
try {
  expectOk(["verify-all"]);

  const original = JSON.parse(readFileSync(fixture, "utf8"));

  const digestTamper = join(tempDir, "digest-tamper.json");
  writeFileSync(
    digestTamper,
    `${JSON.stringify({ ...original, payloadSha256: "0".repeat(64) }, null, 2)}\n`,
  );
  expectFail(["verify", "--file", digestTamper], "payloadSha256 mismatch");

  const identityTamper = join(tempDir, "identity-tamper.json");
  writeFileSync(
    identityTamper,
    `${JSON.stringify(
      { ...original, localPath: ["", "Users", "example", ".ssh", "key"].join("/") },
      null,
      2,
    )}\n`,
  );
  expectFail(["verify", "--file", identityTamper], "forbidden local identity pattern");

  console.log("provenance-self-test: ok");
} finally {
  rmSync(tempDir, { recursive: true, force: true });
}
