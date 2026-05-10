#!/usr/bin/env node
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const repoRoot = resolve(root, "..");
const summaryScript = resolve(here, "autonomy-summary.mjs");

function usage() {
  console.error(`usage:
  node agent-os/scripts/autonomy-publish-summary.mjs --stdout [--since YYYY-MM-DD] [--limit N]
  node agent-os/scripts/autonomy-publish-summary.mjs --out PATH [--since YYYY-MM-DD] [--limit N]
  node agent-os/scripts/autonomy-publish-summary.mjs --check PATH [--since YYYY-MM-DD] [--limit N]
  node agent-os/scripts/autonomy-publish-summary.mjs --check --out PATH [--since YYYY-MM-DD] [--limit N]`);
}

function takeValue(args, index, flag) {
  const value = args[index + 1];
  if (!value || value.startsWith("--")) {
    throw new Error(`${flag} requires a value`);
  }
  return value;
}

function setMode(flags, mode, target = null) {
  if (flags.mode) {
    throw new Error("choose exactly one of --stdout, --out, or --check");
  }
  flags.mode = mode;
  flags.target = target;
}

function parseFlags(args) {
  const flags = { since: null, limit: 12, mode: null, target: null };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--stdout") {
      setMode(flags, "stdout");
      continue;
    }
    if (arg === "--out") {
      const value = takeValue(args, index, arg);
      if (flags.mode === "check" && !flags.target) {
        flags.target = value;
      } else {
        setMode(flags, "out", value);
      }
      index += 1;
      continue;
    }
    if (arg === "--check") {
      const value = args[index + 1];
      if (value && !value.startsWith("--")) {
        setMode(flags, "check", value);
        index += 1;
      } else {
        setMode(flags, "check");
      }
      continue;
    }
    if (arg === "--since") {
      const value = takeValue(args, index, arg);
      if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) {
        throw new Error("--since must use YYYY-MM-DD");
      }
      flags.since = value;
      index += 1;
      continue;
    }
    if (arg === "--limit") {
      const value = takeValue(args, index, arg);
      const limit = Number.parseInt(value, 10);
      if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100) {
        throw new Error("--limit must be an integer from 1 to 100");
      }
      flags.limit = limit;
      index += 1;
      continue;
    }
    if (arg === "--help" || arg === "-h") {
      usage();
      process.exit(0);
    }
    throw new Error(`unknown argument: ${arg}`);
  }

  if (!flags.mode) {
    throw new Error("choose one of --stdout, --out, or --check");
  }
  if (flags.mode === "check" && !flags.target) {
    throw new Error("--check requires a target path");
  }

  return flags;
}

function repoTarget(value) {
  const target = resolve(repoRoot, value);
  const rel = relative(repoRoot, target);
  if (!rel || rel.startsWith("..") || isAbsolute(rel)) {
    throw new Error("output path must stay inside the repository");
  }
  if (rel.split(sep).includes(".git")) {
    throw new Error("output path must not touch Git metadata");
  }
  return { target, rel };
}

function checkCommand(flags, rel) {
  const args = ["node", "agent-os/scripts/autonomy-publish-summary.mjs", "--check", "--out", rel ?? "PATH"];
  if (flags.since) {
    args.push("--since", flags.since);
  }
  if (flags.limit !== 12) {
    args.push("--limit", String(flags.limit));
  }
  return args.join(" ");
}

function generate(flags, rel = null) {
  const args = [summaryScript, "--limit", String(flags.limit)];
  if (flags.since) {
    args.push("--since", flags.since);
  }

  const result = spawnSync(process.execPath, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.status !== 0) {
    throw new Error((result.stderr || result.stdout || "summary generation failed").trim());
  }
  return `<!-- Generated from autonomy task records. Do not edit by hand. -->\n\n${result.stdout.trimEnd()}\n\nValidate with: ${checkCommand(flags, rel)}\n`;
}

try {
  const flags = parseFlags(process.argv.slice(2));

  if (flags.mode === "stdout") {
    process.stdout.write(generate(flags));
    process.exit(0);
  }

  const { target, rel } = repoTarget(flags.target);
  const summary = generate(flags, rel);
  if (flags.mode === "check") {
    if (!existsSync(target)) {
      throw new Error(`${rel} does not exist`);
    }
    if (readFileSync(target, "utf8") !== summary) {
      throw new Error(`${rel} is stale`);
    }
    console.log(`autonomy-publish-summary: ${rel} is current`);
    process.exit(0);
  }

  mkdirSync(dirname(target), { recursive: true });
  writeFileSync(target, summary);
  console.log(`autonomy-publish-summary: wrote ${rel}`);
} catch (error) {
  console.error(`autonomy-publish-summary: ${error.message}`);
  process.exit(1);
}
