#!/usr/bin/env node
import { readFileSync, writeFileSync, existsSync, statSync } from "node:fs";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const manifestPath = join(here, "..", "autonomy", "immutable-core.json");

const sha256 = (abs) => createHash("sha256").update(readFileSync(abs)).digest("hex");

function globToRegex(glob) {
  let re = "";
  for (let i = 0; i < glob.length; i += 1) {
    if (glob[i] === "*" && glob[i + 1] === "*" && glob[i + 2] === "/") { re += "(?:.*/)?"; i += 2; continue; }
    const c = glob[i];
    if (c === "*") {
      if (glob[i + 1] === "*") { re += ".*"; i += 1; } else re += "[^/]*";
    } else if ("\\^$.|?+()[]{}".includes(c)) {
      re += `\\${c}`;
    } else {
      re += c;
    }
  }
  return new RegExp(`^${re}$`);
}

const gitLines = (args) =>
  spawnSync("git", args, { cwd: repoRoot, encoding: "utf8" }).stdout
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean);

const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
const protectedFiles = manifest.protectedFiles ?? [];
const protectedGlobs = (manifest.protectedGlobs ?? []).map(globToRegex);
const mode = process.argv.includes("--update")
  ? "update"
  : process.argv.includes("--staged")
    ? "staged"
    : "worktree";

if (mode === "update") {
  const baseline = {};
  for (const rel of protectedFiles) {
    const abs = join(repoRoot, rel);
    if (existsSync(abs) && statSync(abs).isFile()) baseline[rel] = sha256(abs);
  }
  manifest.baseline = baseline;
  writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  console.log(`validate-immutable-core: baseline updated (${Object.keys(baseline).length} files)`);
  process.exit(0);
}

const violations = [];
const baseline = manifest.baseline ?? {};

for (const rel of protectedFiles) {
  const abs = join(repoRoot, rel);
  if (!existsSync(abs)) continue;
  const expected = baseline[rel];
  if (!expected) {
    violations.push(`${rel}: no baseline hash (operator must run --update)`);
  } else if (sha256(abs) !== expected) {
    violations.push(`${rel}: content changed from baseline`);
  }
}

const changed = mode === "staged"
  ? gitLines(["diff", "--cached", "--name-only"])
  : gitLines(["status", "--porcelain"]).map((l) => l.slice(3).trim()).filter(Boolean);
const protectedSet = new Set(protectedFiles);
for (const f of changed) {
  if (protectedSet.has(f) || protectedGlobs.some((re) => re.test(f))) {
    violations.push(`${f}: protected path edited`);
  }
}

if (violations.length > 0) {
  console.error("validate-immutable-core: autonomous edit to operator-owned core:");
  for (const v of [...new Set(violations)]) console.error(`  ${v}`);
  console.error("These govern fitness/selection/identity/security. The loop may not self-edit them — revert and continue.");
  process.exit(2);
}

console.log(`validate-immutable-core: ok (${protectedFiles.length} files intact, ${changed.length} changed paths clean)`);
