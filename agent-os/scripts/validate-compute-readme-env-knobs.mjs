#!/usr/bin/env node
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Compute README <-> env-knob binding guard. Each compute crate's
// README names the operator-facing COVENANT_COMPUTE_* knobs; the code
// reads a larger set (internal tuning, wizard-written anchors). Both
// sets are pinned here and re-extracted from the committed files on
// every run, so all four drift directions fail loud:
//   - the code adds/renames/drops a knob the pin doesn't know -> update
//     `inventory`, and decide whether the README should name it;
//   - a README names a knob the crate no longer reads -> stale docs;
//   - a README drops a knob this pin says it documents -> doc
//     regression, not a silent shrink;
//   - the protocol crate starts reading env at all -> its wire types
//     are deliberately environment-free.
// Inventory truth is every quoted COVENANT_COMPUTE_* / COVENANT_COMPUTE_*
// literal under the crate's src/ (reads and the setup wizard's node.env
// template alike; the COMPUTE prefix is the covenant-compute-mcp server's
// buyer-facing family); examples/ and tests/ are deliberately out of scope.

const here = dirname(fileURLToPath(import.meta.url));
const agentOsRoot = resolve(here, "..");

const KNOB_LITERAL = /"(COVENANT_(?:COMPUTE|COMPUTE)[A-Z0-9_]*)"/g;
const KNOB_MENTION = /COVENANT_(?:COMPUTE|COMPUTE)[A-Z0-9_]*/g;

const CRATES = {
  "covenant-compute-protocol": {
    inventory: [],
    documented: [],
  },
  "covenant-compute-node": {
    inventory: [
      "COVENANT_COMPUTE_AGENT_ANTHROPIC_WORKSPACE_ID",
      "COVENANT_COMPUTE_AGENT_AUTH_TOKEN_FILE",
      "COVENANT_COMPUTE_AGENT_BUDGET_USD",
      "COVENANT_COMPUTE_AGENT_BUILDER",
      "COVENANT_COMPUTE_AGENT_BUILD_CPUS",
      "COVENANT_COMPUTE_AGENT_BUILD_FORWARDER",
      "COVENANT_COMPUTE_AGENT_BUILD_MEMORY",
      "COVENANT_COMPUTE_AGENT_BUILD_NETWORK",
      "COVENANT_COMPUTE_AGENT_BUILD_PIDS",
      "COVENANT_COMPUTE_AGENT_BUILD_PROXY_HOST",
      "COVENANT_COMPUTE_AGENT_CHECK_CPUS",
      "COVENANT_COMPUTE_AGENT_CHECK_IMAGES",
      "COVENANT_COMPUTE_AGENT_CHECK_MEMORY",
      "COVENANT_COMPUTE_AGENT_CHECK_PIDS",
      "COVENANT_COMPUTE_AGENT_CLAUDE_BIN",
      "COVENANT_COMPUTE_AGENT_COVGUARD_BIN",
      "COVENANT_COMPUTE_AGENT_GIT_BIN",
      "COVENANT_COMPUTE_AGENT_MODEL",
      "COVENANT_COMPUTE_AGENT_WORK_DIR",
      "COVENANT_COMPUTE_BENCHMARK_TIMEOUT_SECS",
      "COVENANT_COMPUTE_BROKER_IMAGE",
      "COVENANT_COMPUTE_BROKER_READY_POLL_SECS",
      "COVENANT_COMPUTE_BROKER_READY_TIMEOUT_SECS",
      "COVENANT_COMPUTE_COORDINATOR_PUBKEY",
      "COVENANT_COMPUTE_COORDINATOR_URL",
      "COVENANT_COMPUTE_LEASE_STUB_ENDPOINT",
      "COVENANT_COMPUTE_NODE_BACKEND_RETRY_SECS",
      "COVENANT_COMPUTE_NODE_CONTAINER_CPUS",
      "COVENANT_COMPUTE_NODE_CONTAINER_GPUS",
      "COVENANT_COMPUTE_NODE_CONTAINER_IMAGE",
      "COVENANT_COMPUTE_NODE_CONTAINER_MEMORY",
      "COVENANT_COMPUTE_NODE_CONTAINER_NETWORK",
      "COVENANT_COMPUTE_NODE_CONTAINER_OCI_RUNTIME",
      "COVENANT_COMPUTE_NODE_CONTAINER_PIDS",
      "COVENANT_COMPUTE_NODE_CONTAINER_RUNTIME",
      "COVENANT_COMPUTE_NODE_CONTAINER_USER",
      "COVENANT_COMPUTE_NODE_EXECUTOR",
      "COVENANT_COMPUTE_NODE_HARDWARE",
      "COVENANT_COMPUTE_NODE_HEARTBEAT_SECS",
      "COVENANT_COMPUTE_NODE_HOME",
      "COVENANT_COMPUTE_NODE_JOB_KINDS",
      "COVENANT_COMPUTE_NODE_KIND_PRICES",
      "COVENANT_COMPUTE_NODE_LOG_DIR",
      "COVENANT_COMPUTE_NODE_MAX_FEE_BPS",
      "COVENANT_COMPUTE_NODE_MAX_IN_FLIGHT",
      "COVENANT_COMPUTE_NODE_MODELS",
      "COVENANT_COMPUTE_NODE_PAYOUT_POLL_SECS",
      "COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC",
      "COVENANT_COMPUTE_NODE_PRICE_UNIT",
      "COVENANT_COMPUTE_NODE_RPC_URL",
      "COVENANT_COMPUTE_NODE_SKIP_BENCHMARK",
      "COVENANT_COMPUTE_NODE_VRAM_GB",
      "COVENANT_COMPUTE_OLLAMA_DEFAULT_MODEL",
      "COVENANT_COMPUTE_OLLAMA_KEEP_ALIVE",
      "COVENANT_COMPUTE_OLLAMA_URL",
      "COVENANT_COMPUTE_OPENAI_API_KEY",
      "COVENANT_COMPUTE_OPENAI_DEFAULT_MODEL",
      "COVENANT_COMPUTE_OPENAI_URL",
      "COVENANT_COMPUTE_PAYOUT_ADDRESS",
      "COVENANT_COMPUTE_REFERRAL_CODE",
      "COVENANT_COMPUTE_SAY_BIN",
      "COVENANT_COMPUTE_SAY_VOICE",
      "COVENANT_COMPUTE_TEST_SAY_BIN",
      "COVENANT_COMPUTE_TEST_WHISPER_BIN",
      "COVENANT_COMPUTE_TEST_WHISPER_MODEL",
      "COVENANT_COMPUTE_WHISPER_BIN",
      "COVENANT_COMPUTE_WHISPER_MODEL",
    ],
    documented: [
      "COVENANT_COMPUTE_BENCHMARK_TIMEOUT_SECS",
      "COVENANT_COMPUTE_BROKER_IMAGE",
      "COVENANT_COMPUTE_BROKER_READY_POLL_SECS",
      "COVENANT_COMPUTE_BROKER_READY_TIMEOUT_SECS",
      "COVENANT_COMPUTE_COORDINATOR_PUBKEY",
      "COVENANT_COMPUTE_COORDINATOR_URL",
      "COVENANT_COMPUTE_LEASE_STUB_ENDPOINT",
      "COVENANT_COMPUTE_NODE_BACKEND_RETRY_SECS",
      "COVENANT_COMPUTE_NODE_CONTAINER_CPUS",
      "COVENANT_COMPUTE_NODE_CONTAINER_GPUS",
      "COVENANT_COMPUTE_NODE_CONTAINER_IMAGE",
      "COVENANT_COMPUTE_NODE_CONTAINER_MEMORY",
      "COVENANT_COMPUTE_NODE_CONTAINER_NETWORK",
      "COVENANT_COMPUTE_NODE_CONTAINER_OCI_RUNTIME",
      "COVENANT_COMPUTE_NODE_CONTAINER_PIDS",
      "COVENANT_COMPUTE_NODE_CONTAINER_RUNTIME",
      "COVENANT_COMPUTE_NODE_CONTAINER_USER",
      "COVENANT_COMPUTE_NODE_EXECUTOR",
      "COVENANT_COMPUTE_NODE_HARDWARE",
      "COVENANT_COMPUTE_NODE_HEARTBEAT_SECS",
      "COVENANT_COMPUTE_NODE_HOME",
      "COVENANT_COMPUTE_NODE_JOB_KINDS",
      "COVENANT_COMPUTE_NODE_LOG_DIR",
      "COVENANT_COMPUTE_NODE_MAX_FEE_BPS",
      "COVENANT_COMPUTE_NODE_MODELS",
      "COVENANT_COMPUTE_NODE_PAYOUT_POLL_SECS",
      "COVENANT_COMPUTE_NODE_PRICE_MICRO_USDC",
      "COVENANT_COMPUTE_NODE_PRICE_UNIT",
      "COVENANT_COMPUTE_NODE_RPC_URL",
      "COVENANT_COMPUTE_NODE_SKIP_BENCHMARK",
      "COVENANT_COMPUTE_NODE_VRAM_GB",
      "COVENANT_COMPUTE_OLLAMA_DEFAULT_MODEL",
      "COVENANT_COMPUTE_OLLAMA_KEEP_ALIVE",
      "COVENANT_COMPUTE_OLLAMA_URL",
      "COVENANT_COMPUTE_OPENAI_API_KEY",
      "COVENANT_COMPUTE_OPENAI_DEFAULT_MODEL",
      "COVENANT_COMPUTE_OPENAI_URL",
      "COVENANT_COMPUTE_PAYOUT_ADDRESS",
      "COVENANT_COMPUTE_REFERRAL_CODE",
      "COVENANT_COMPUTE_SAY_BIN",
      "COVENANT_COMPUTE_SAY_VOICE",
      "COVENANT_COMPUTE_WHISPER_BIN",
      "COVENANT_COMPUTE_WHISPER_MODEL",
    ],
  },
  "covenant-compute-coordinator": {
    inventory: [
      "COVENANT_COMPUTE_ADMIN_TOKEN",
      "COVENANT_COMPUTE_AGENT_BUYERS",
      "COVENANT_COMPUTE_AGENT_CHECK_ACCEPT_TIMEOUT_MS",
      "COVENANT_COMPUTE_AGENT_CHECK_ATTEMPTS",
      "COVENANT_COMPUTE_AGENT_CHECK_PRICE_MICRO_USDC",
      "COVENANT_COMPUTE_AGENT_CONFIRM_FAILURES",
      "COVENANT_COMPUTE_AGENT_PASS_SAMPLE_BPS",
      "COVENANT_COMPUTE_AGENT_ROUNDS",
      "COVENANT_COMPUTE_AGENT_SETTLE_SECS",
      "COVENANT_COMPUTE_CANARY_DEADLINE_MS",
      "COVENANT_COMPUTE_CANARY_INTERVAL_SECS",
      "COVENANT_COMPUTE_CANARY_MAX_PRICE_MICRO_USDC",
      "COVENANT_COMPUTE_COORDINATOR_BIND_ADDR",
      "COVENANT_COMPUTE_COORDINATOR_HOME",
      "COVENANT_COMPUTE_COORDINATOR_PORT",
      "COVENANT_COMPUTE_DISPUTE_WINDOW_SECS",
      "COVENANT_COMPUTE_FEE_BPS",
      "COVENANT_COMPUTE_FUNDING_SOURCE",
      "COVENANT_COMPUTE_JOURNAL_COMPACT_SECS",
      "COVENANT_COMPUTE_LEASE_COORDINATOR_KEYPAIR",
      "COVENANT_COMPUTE_LEASE_ER_RPC_URL",
      "COVENANT_COMPUTE_LEASE_ER_VALIDATOR",
      "COVENANT_COMPUTE_LEASE_KEYPAIR",
      "COVENANT_COMPUTE_LEASE_METER",
      "COVENANT_COMPUTE_LEASE_MINT",
      "COVENANT_COMPUTE_LEASE_PROGRAM_ID",
      "COVENANT_COMPUTE_LEASE_RPC_URL",
      "COVENANT_COMPUTE_LEASE_SIGNER_BINARY",
      "COVENANT_COMPUTE_LEASE_TICK_SECS",
      "COVENANT_COMPUTE_LONG_POLL_SECS",
      "COVENANT_COMPUTE_MAX_INFLIGHT_PER_BUYER",
      "COVENANT_COMPUTE_MAX_OPERATORS",
      "COVENANT_COMPUTE_MIN_BOND_LEASE_HOURS",
      "COVENANT_COMPUTE_MIN_BOND_MICRO_USDC",
      "COVENANT_COMPUTE_MIN_OPERATOR_SCORE_BPS",
      "COVENANT_COMPUTE_MIN_PROTOCOL",
      "COVENANT_COMPUTE_MIN_STAKE",
      "COVENANT_COMPUTE_MIN_STAKE_LOCK_SECS",
      "COVENANT_COMPUTE_OBLIGATION_CAP_MICRO_USDC",
      "COVENANT_COMPUTE_PARTNERS",
      "COVENANT_COMPUTE_PAYOUT_BACKEND",
      "COVENANT_COMPUTE_PAYOUT_CAP_MICRO_USDC",
      "COVENANT_COMPUTE_PAYOUT_MINT",
      "COVENANT_COMPUTE_PAYOUT_RETRY_SECS",
      "COVENANT_COMPUTE_PAYOUT_SIGNER_BINARY",
      "COVENANT_COMPUTE_PUBLIC_PROOF_FEED",
      "COVENANT_COMPUTE_RAIL_DEPOSIT_OWNER",
      "COVENANT_COMPUTE_RAIL_MINT",
      "COVENANT_COMPUTE_RAIL_RPC_URL",
      "COVENANT_COMPUTE_REDUNDANCY_INFERENCE",
      "COVENANT_COMPUTE_REDUNDANCY_INTERVAL_SECS",
      "COVENANT_COMPUTE_REDUNDANCY_MAX_PRICE_MICRO_USDC",
      "COVENANT_COMPUTE_REDUNDANCY_MIRRORS",
      "COVENANT_COMPUTE_REOFFER_SECS",
      "COVENANT_COMPUTE_REQUIRE_PREFUNDED",
      "COVENANT_COMPUTE_STAKE_KEYPAIR",
      "COVENANT_COMPUTE_STAKE_PROGRAM_ID",
      "COVENANT_COMPUTE_STAKE_REFRESH_SECS",
      "COVENANT_COMPUTE_STAKE_RPC_URL",
      "COVENANT_COMPUTE_STAKE_SIGNER_BIN",
      "COVENANT_COMPUTE_STAKE_SLASH_KEYPAIR",
      "COVENANT_COMPUTE_STAKE_SLASH_PER_FAULT",
      "COVENANT_COMPUTE_SUBSIDY_FLOOR_MICRO_USDC",
      "COVENANT_COMPUTE_SUBSIDY_MAX_RATIO_BPS",
      "COVENANT_COMPUTE_UNBOND_SECS",
      "COVENANT_COMPUTE_VAULT",
      "COVENANT_COMPUTE_VAULT_MAX_OWNERS",
    ],
    documented: [
      "COVENANT_COMPUTE_ADMIN_TOKEN",
      "COVENANT_COMPUTE_CANARY_DEADLINE_MS",
      "COVENANT_COMPUTE_CANARY_INTERVAL_SECS",
      "COVENANT_COMPUTE_CANARY_MAX_PRICE_MICRO_USDC",
      "COVENANT_COMPUTE_COORDINATOR_BIND_ADDR",
      "COVENANT_COMPUTE_COORDINATOR_HOME",
      "COVENANT_COMPUTE_COORDINATOR_PORT",
      "COVENANT_COMPUTE_DISPUTE_WINDOW_SECS",
      "COVENANT_COMPUTE_FEE_BPS",
      "COVENANT_COMPUTE_FUNDING_SOURCE",
      "COVENANT_COMPUTE_JOURNAL_COMPACT_SECS",
      "COVENANT_COMPUTE_LONG_POLL_SECS",
      "COVENANT_COMPUTE_MAX_INFLIGHT_PER_BUYER",
      "COVENANT_COMPUTE_MAX_OPERATORS",
      "COVENANT_COMPUTE_MIN_BOND_LEASE_HOURS",
      "COVENANT_COMPUTE_MIN_BOND_MICRO_USDC",
      "COVENANT_COMPUTE_MIN_OPERATOR_SCORE_BPS",
      "COVENANT_COMPUTE_MIN_PROTOCOL",
      "COVENANT_COMPUTE_OBLIGATION_CAP_MICRO_USDC",
      "COVENANT_COMPUTE_PARTNERS",
      "COVENANT_COMPUTE_PAYOUT_BACKEND",
      "COVENANT_COMPUTE_PAYOUT_CAP_MICRO_USDC",
      "COVENANT_COMPUTE_PAYOUT_MINT",
      "COVENANT_COMPUTE_PAYOUT_RETRY_SECS",
      "COVENANT_COMPUTE_PAYOUT_SIGNER_BINARY",
      "COVENANT_COMPUTE_PUBLIC_PROOF_FEED",
      "COVENANT_COMPUTE_RAIL_DEPOSIT_OWNER",
      "COVENANT_COMPUTE_RAIL_MINT",
      "COVENANT_COMPUTE_RAIL_RPC_URL",
      "COVENANT_COMPUTE_REDUNDANCY_INFERENCE",
      "COVENANT_COMPUTE_REDUNDANCY_INTERVAL_SECS",
      "COVENANT_COMPUTE_REDUNDANCY_MAX_PRICE_MICRO_USDC",
      "COVENANT_COMPUTE_REDUNDANCY_MIRRORS",
      "COVENANT_COMPUTE_REOFFER_SECS",
      "COVENANT_COMPUTE_REQUIRE_PREFUNDED",
      "COVENANT_COMPUTE_SUBSIDY_FLOOR_MICRO_USDC",
      "COVENANT_COMPUTE_SUBSIDY_MAX_RATIO_BPS",
      "COVENANT_COMPUTE_UNBOND_SECS",
      "COVENANT_COMPUTE_VAULT",
      "COVENANT_COMPUTE_VAULT_MAX_OWNERS",
    ],
  },
  "covenant-compute-buyer": {
    inventory: [
      "COVENANT_COMPUTE_COORDINATOR_URL",
      "COVENANT_COMPUTE_DEADLINE_MS",
      "COVENANT_COMPUTE_MAX_ACTIVE_STREAMS",
      "COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC",
      "COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC",
      "COVENANT_COMPUTE_MCP_HOME",
      "COVENANT_COMPUTE_OPENAI_API_KEY",
      "COVENANT_COMPUTE_OPENAI_BIND",
      "COVENANT_COMPUTE_OPENAI_HOME",
      "COVENANT_COMPUTE_REFERRAL_CODE",
      "COVENANT_COMPUTE_RPC_URL",
    ],
    documented: [
      "COVENANT_COMPUTE_COORDINATOR_URL",
      "COVENANT_COMPUTE_DEADLINE_MS",
      "COVENANT_COMPUTE_MAX_ACTIVE_STREAMS",
      "COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC",
      "COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC",
      "COVENANT_COMPUTE_MCP_HOME",
      "COVENANT_COMPUTE_OPENAI_API_KEY",
      "COVENANT_COMPUTE_OPENAI_HOME",
      "COVENANT_COMPUTE_REFERRAL_CODE",
      "COVENANT_COMPUTE_RPC_URL",
    ],
  },
};

function rustFilesUnder(dir) {
  const files = [];
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) {
      files.push(...rustFilesUnder(path));
    } else if (entry.endsWith(".rs")) {
      files.push(path);
    }
  }
  return files.sort();
}

function knobsInSource(text) {
  return [...text.matchAll(KNOB_LITERAL)].map((m) => m[1]);
}

function knobsInReadme(text) {
  return [...text.matchAll(KNOB_MENTION)].map((m) => m[0]);
}

function sortedUnique(items) {
  return [...new Set(items)].sort();
}

function diff(label, got, want, errors, remediation) {
  const gotSet = new Set(got);
  const wantSet = new Set(want);
  for (const knob of want) {
    if (!gotSet.has(knob)) {
      errors.push(`${label}: missing ${knob} — ${remediation.missing}`);
    }
  }
  for (const knob of got) {
    if (!wantSet.has(knob)) {
      errors.push(`${label}: unexpected ${knob} — ${remediation.unexpected}`);
    }
  }
}

// Always-on self-test: prove both extractors actually extract before
// trusting any comparison — a regex that silently matches nothing would
// make every direction vacuously green.
{
  const rs = [
    'std::env::var("COVENANT_COMPUTE_SELFTEST_URL")',
    '// error text naming "COVENANT_COMPUTE_SELFTEST_KEY" counts too',
    'std::env::var("COVENANT_COMPUTE_SELFTEST_CAP")',
    'let unrelated = "NOT_A_KNOB";',
  ].join("\n");
  const md =
    "Set `COVENANT_COMPUTE_SELFTEST_URL` (and `$COVENANT_COMPUTE_SELFTEST_HOME`, " +
    "plus `COVENANT_COMPUTE_SELFTEST_CAP`).";
  const rsGot = sortedUnique(knobsInSource(rs)).join(",");
  const rsWant =
    "COVENANT_COMPUTE_SELFTEST_CAP,COVENANT_COMPUTE_SELFTEST_KEY,COVENANT_COMPUTE_SELFTEST_URL";
  const mdGot = sortedUnique(knobsInReadme(md)).join(",");
  const mdWant =
    "COVENANT_COMPUTE_SELFTEST_CAP,COVENANT_COMPUTE_SELFTEST_HOME,COVENANT_COMPUTE_SELFTEST_URL";
  if (rsGot !== rsWant || mdGot !== mdWant) {
    console.error("validate-compute-readme-env-knobs: self-test failed");
    console.error(`- source extractor: got [${rsGot}], want [${rsWant}]`);
    console.error(`- readme extractor: got [${mdGot}], want [${mdWant}]`);
    process.exit(1);
  }
}

const errors = [];
let pinned = 0;

for (const [crate, pins] of Object.entries(CRATES)) {
  const crateDir = join(agentOsRoot, "crates", crate);

  let sourceKnobs;
  try {
    sourceKnobs = sortedUnique(
      rustFilesUnder(join(crateDir, "src")).flatMap((path) =>
        knobsInSource(readFileSync(path, "utf8")),
      ),
    );
  } catch (error) {
    errors.push(`${crate}: cannot scan src/: ${error.message}`);
    continue;
  }

  let readmeKnobs;
  try {
    readmeKnobs = sortedUnique(
      knobsInReadme(readFileSync(join(crateDir, "README.md"), "utf8")),
    );
  } catch (error) {
    errors.push(`${crate}: cannot read README.md: ${error.message}`);
    continue;
  }

  diff(`${crate} src`, sourceKnobs, pins.inventory, errors, {
    missing:
      "the code no longer references this knob; drop it from `inventory` (and from the README if named there)",
    unexpected:
      "a knob the pin doesn't know; add it to `inventory` and decide whether the README should document it",
  });
  diff(`${crate} README`, readmeKnobs, pins.documented, errors, {
    missing:
      "the README stopped naming a knob this pin says it documents; restore the doc or shrink `documented` deliberately",
    unexpected:
      "the README names a knob outside its pinned set; if the knob is real, add it to `documented` — if not, the doc is stale",
  });
  for (const knob of pins.documented) {
    if (!pins.inventory.includes(knob)) {
      errors.push(
        `${crate}: documented knob ${knob} is not in the crate's inventory — a README may only name knobs its own crate reads`,
      );
    }
  }
  pinned += pins.inventory.length;
}

if (errors.length > 0) {
  console.error("validate-compute-readme-env-knobs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-compute-readme-env-knobs: ok (${pinned} knobs pinned across ${Object.keys(CRATES).length} crates, self-test passed)`,
);
