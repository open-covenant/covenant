#!/usr/bin/env node
const DEFAULT_ENDPOINT = "http://localhost:11434";
const DEFAULT_REQUIRED = ["nomic-embed-text", "qwen2.5:7b"];

function fail(message, code = 1) {
  console.error(`model-availability: ${message}`);
  process.exit(code);
}

function parseArgs(argv) {
  const out = { endpoint: DEFAULT_ENDPOINT, json: false, required: [...DEFAULT_REQUIRED] };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--json") {
      out.json = true;
      continue;
    }
    if (arg === "--endpoint") {
      const value = argv[i + 1];
      if (!value) fail("--endpoint requires a value");
      out.endpoint = value;
      i += 1;
      continue;
    }
    if (arg.startsWith("--endpoint=")) {
      out.endpoint = arg.slice("--endpoint=".length);
      continue;
    }
    if (arg === "--require") {
      const value = argv[i + 1];
      if (!value) fail("--require requires a model name");
      out.required.push(value);
      i += 1;
      continue;
    }
    if (arg.startsWith("--require=")) {
      const value = arg.slice("--require=".length);
      if (!value) fail("--require requires a model name");
      out.required.push(value);
      continue;
    }
    if (arg === "--help" || arg === "-h") {
      return { ...out, help: true };
    }
    fail(`unknown argument: ${arg}`);
  }
  out.required = [...new Set(out.required)].filter(Boolean);
  return out;
}

function renderHelp() {
  console.log(`Usage: node agent-os/scripts/model-availability.mjs [options]

Options:
  --endpoint <url>    Ollama endpoint (default: ${DEFAULT_ENDPOINT})
  --require <model>   Require a model name (repeatable)
  --json              Emit machine-readable JSON
  -h, --help          Show this help

Defaults:
  required models: ${DEFAULT_REQUIRED.join(", ")}
`);
}

function normalizeModelName(name) {
  if (!name) return "";
  if (name.includes(":")) return name;
  return `${name}:latest`;
}

async function fetchJson(url) {
  let resp;
  try {
    resp = await fetch(url, { headers: { accept: "application/json" } });
  } catch (error) {
    fail(`cannot reach Ollama at ${url}: ${error.message}`, 2);
  }
  if (!resp.ok) {
    fail(`Ollama at ${url} returned HTTP ${resp.status}`, 2);
  }
  try {
    return await resp.json();
  } catch (error) {
    fail(`invalid JSON from Ollama at ${url}: ${error.message}`, 2);
  }
}

const args = parseArgs(process.argv.slice(2));
if (args.help) {
  renderHelp();
  process.exit(0);
}

const endpoint = args.endpoint.replace(/\/+$/, "");
const tagsUrl = `${endpoint}/api/tags`;
const versionUrl = `${endpoint}/api/version`;

const [tags, version] = await Promise.all([
  fetchJson(tagsUrl),
  fetchJson(versionUrl).catch(() => null),
]);

const models = Array.isArray(tags?.models) ? tags.models : [];
const available = new Set();
for (const model of models) {
  if (!model || typeof model !== "object") continue;
  if (typeof model.name === "string" && model.name.length > 0) available.add(model.name);
  if (typeof model.model === "string" && model.model.length > 0) available.add(model.model);
}

const required = args.required.map(normalizeModelName);
const missing = required.filter((name) => !available.has(name));

if (args.json) {
  console.log(
    JSON.stringify(
      {
        provider: "ollama",
        endpoint,
        ok: missing.length === 0,
        required,
        missing,
        available: Array.from(available).sort(),
        version: typeof version?.version === "string" ? version.version : null,
      },
      null,
      2,
    ),
  );
  process.exit(missing.length === 0 ? 0 : 3);
}

const versionText = typeof version?.version === "string" ? ` (version ${version.version})` : "";
console.log(`Ollama: ${endpoint}${versionText}`);
if (required.length > 0) console.log(`Required: ${required.join(", ")}`);

if (missing.length === 0) {
  console.log("Status: ok (all required models are available)");
  process.exit(0);
}

console.log(`Status: missing ${missing.length} model(s): ${missing.join(", ")}`);
console.log("Fix:");
for (const name of missing) {
  console.log(`  ollama pull ${name}`);
}
process.exit(3);

