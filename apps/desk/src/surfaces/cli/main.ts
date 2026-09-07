#!/usr/bin/env node
/** Entry point for the `covenant-desk` command. */

import './quiet.js';

// Loaded after the notice above is handled: Node announces its SQLite module
// while the import graph is being linked, which is before any module body runs.
const { runCli } = await import('./index.js');

const code = await runCli({
  argv: process.argv.slice(2),
  env: process.env,
  out: (text) => process.stdout.write(`${text}\n`),
  err: (text) => process.stderr.write(`${text}\n`),
});

process.exitCode = code;
