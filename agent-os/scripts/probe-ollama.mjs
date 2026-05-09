#!/usr/bin/env node
import process from "node:process";

const defaultEndpoint = "http://localhost:11434";
const defaultModels = ["nomic-embed-text", "qwen2.5:7b"];

function usage() {
  console.log(`usage: probe-ollama [--endpoint url] [--model name ...] [--json]\n\nDefaults:\n  endpoint: ${defaultEndpoint}\n  models:   ${defaultModels.join(", ")}`);
}

function parseArgs(argv) {
  const args = {
    endpoint: defaultEndpoint,
    models: [...defaultModels],
    json: false,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--help" || a === "-h") {
      usage();
      process.exit(0);
    }
    if (a === "--json") {
      args.json = true;
      continue;
    }
    if (a === "--endpoint") {
      const v = argv[i + 1];
      if (!v) throw new Error("--endpoint requires a value");
      args.endpoint = v;
      i += 1;
      continue;
    }
    if (a === "--model") {
      const v = argv[i + 1];
      if (!v) throw new Error("--model requires a value");
      if (!args.models.includes(v)) args.models.push(v);
      i += 1;
      continue;
    }
    throw new Error(`unknown arg: ${a}`);
  }

  return args;
}

function fail(message, details = null) {
  console.error(`probe-ollama: ${message}`);
  if (details) console.error(details);
  process.exit(1);
}

async function fetchWithTimeout(url, timeoutMs) {
  const controller = new AbortController();
  const t = setTimeout(() => controller.abort(), timeoutMs);
  try {
    return await fetch(url, { signal: controller.signal });
  } finally {
    clearTimeout(t);
  }
}

async function main() {
  let args;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (error) {
    fail(error.message);
  }

  const endpoint = process.env.COVENANT_OLLAMA_ENDPOINT || args.endpoint;

  let tagResp;
  try {
    tagResp = await fetchWithTimeout(`${endpoint}/api/tags`, 1500);
  } catch (error) {
    fail(
      `cannot reach Ollama at ${endpoint}`,
      `Start it with: ollama serve\nThen pull required models:\n  ${args.models
        .map((m) => `ollama pull ${m}`)
        .join("\n  ")}`,
    );
  }

  if (!tagResp.ok) {
    fail(`unexpected status ${tagResp.status} from ${endpoint}/api/tags`);
  }

  let payload;
  try {
    payload = await tagResp.json();
  } catch (error) {
    fail(`invalid JSON from ${endpoint}/api/tags`, error.message);
  }

  const models = Array.isArray(payload?.models) ? payload.models : [];
  const available = new Set(
    models
      .map((m) => (typeof m?.name === "string" ? m.name : null))
      .filter((v) => v),
  );

  const missing = args.models.filter((m) => !available.has(m));
  const ok = missing.length === 0;

  if (args.json) {
    console.log(
      JSON.stringify(
        {
          kind: "ollama_probe",
          endpoint,
          required_models: args.models,
          available_models: [...available].sort(),
          missing_models: missing,
          ok,
        },
        null,
        2,
      ),
    );
  } else if (ok) {
    console.log(`probe-ollama: ok (${args.models.length} model(s) available)`);
  } else {
    console.error(`probe-ollama: missing ${missing.length} model(s): ${missing.join(", ")}`);
    console.error("Pull them with:");
    for (const model of missing) {
      console.error(`  ollama pull ${model}`);
    }
    process.exit(1);
  }
}

await main();
