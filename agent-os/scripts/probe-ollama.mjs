#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import process from "node:process";

const here = dirname(fileURLToPath(import.meta.url));
const target = join(here, "model-availability.mjs");
const rawArgs = process.argv.slice(2);

const args = [];
for (let i = 0; i < rawArgs.length; i += 1) {
  const arg = rawArgs[i];
  if (arg === "--model") {
    args.push("--require", rawArgs[i + 1]);
    i += 1;
    continue;
  }
  if (arg.startsWith("--model=")) {
    args.push(`--require=${arg.slice("--model=".length)}`);
    continue;
  }
  args.push(arg);
}

const result = spawnSync(process.execPath, [target, ...args], { stdio: "inherit" });
if (result.error) {
  console.error(`probe-ollama: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
