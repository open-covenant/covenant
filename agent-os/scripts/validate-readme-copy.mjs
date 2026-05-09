#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const readmePath = join(repoRoot, "README.md");
const statusPath = join(repoRoot, "docs", "status.md");
const standardPath = join(repoRoot, "docs", "readme-copy-standard.md");
const markerPattern = /<!-- covenant-readme-status-sha256: ([a-f0-9]{64}) -->/;
const update = process.argv.includes("--update");

const read = (path) => readFileSync(path, "utf8").replace(/\r\n/g, "\n");
let readme = read(readmePath);
const status = read(statusPath);
const lowerReadme = readme.toLowerCase();

const errors = [];
const fail = (message) => errors.push(message);
const rel = (path) => relative(repoRoot, path);

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function statusHash() {
  return sha256(status.trim() + "\n");
}

function assertIncludes(label, needle) {
  if (!lowerReadme.includes(needle.toLowerCase())) {
    fail(`missing required ${label}: ${needle}`);
  }
}

function assertHeading(heading) {
  if (!new RegExp(`^## ${heading}$`, "m").test(readme)) {
    fail(`missing required section: ## ${heading}`);
  }
}

function assertAny(label, needles) {
  if (!needles.some((needle) => lowerReadme.includes(needle.toLowerCase()))) {
    fail(`missing README coverage for ${label}; expected one of: ${needles.join(" | ")}`);
  }
}

const forbidden = [
  ["alpha framing", /\balpha\b/i],
  ["launch-contract framing", /release contract/i],
  ["old alpha contract title", /Alpha Release Contract/],
  ["old alpha contract sentence", /Covenant may only be presented as alpha/i],
  ["current-alpha boundary", /current alpha boundary/i],
  ["non-claims framing", /non-claims?/i],
  ["defensive claim framing", /\b(?:does not|do not|must not)\s+claim\b/i],
  ["production-sandbox disclaimer", /production sandboxing/i],
  ["public-signing disclaimer", /public release signing/i],
  ["stable-SDK disclaimer", /stable SDKs?/i],
  ["marketplace disclaimer", /marketplace operation/i],
  ["multi-host disclaimer", /multi-host production/i],
  ["internal approval process", /Human approval is required/i],
  ["pre-launch framing", /\bpre-alpha\b/i],
  ["post-launch research split", /\bpost-alpha\b/i],
  ["hobby framing", /\bhobby\b/i],
  ["toy-project framing", /\btoy project\b/i],
  ["embarrassing caveat phrasing", /not a production distribution/i],
];

for (const [label, pattern] of forbidden) {
  if (pattern.test(readme)) {
    fail(`forbidden README phrasing (${label}): ${pattern}`);
  }
}

if (!readme.startsWith("# Covenant\n\n")) {
  fail("README must start with the Covenant title");
}

assertIncludes(
  "tagline",
  "> Agent- and blockchain-native operating layer for governed autonomous systems.",
);
assertIncludes(
  "positioning",
  "agent- and blockchain-native operating layer for autonomous software engineering systems",
);
assertIncludes("audience", "research teams and engineering organizations");
assertIncludes("execution posture", "scoped, inspectable, resumable, and attributable");
assertIncludes("agent-native development posture", "developed through the same governed agent workflows it exposes");
assertIncludes("blockchain-native coordination posture", "verifiable state transitions, explicit authority, and durable coordination");

for (const heading of [
  "Why Covenant",
  "Architecture",
  "Capabilities",
  "Validation",
  "Research Direction",
  "Contributing",
  "Security",
]) {
  assertHeading(heading);
}

for (const link of [
  "docs/repo-map.md",
  "docs/status.md",
  "docs/audit-integrity.md",
  "docs/release-validation.md",
  "agent-os/README.md",
]) {
  assertIncludes("evidence link", link);
}

const expectedCapabilities = [
  "Local daemon and CLI",
  "IPC and HTTP gateway",
  "Identity and peer auth",
  "Signed capabilities",
  "Audit log",
  "Memory store",
  "Runtime execution",
  "MCP tools",
  "A2A messaging",
  "Budget ledger",
  "Local settlement receipts",
  "On-chain settlement",
  "Autonomous workflow",
  "Live boundary coverage",
  "Public provenance",
  "Distribution and SDK ecosystem",
];

const statusCapabilities = [...status.matchAll(/^\| ([^|]+) \| ([^|]+) \|/gm)]
  .map((match) => match[1].trim())
  .filter((name) => name !== "Capability");

const expectedSet = new Set(expectedCapabilities);
for (const capability of statusCapabilities) {
  if (!expectedSet.has(capability)) {
    fail(`docs/status.md capability is not covered by the README guard: ${capability}`);
  }
}
for (const capability of expectedCapabilities) {
  if (!statusCapabilities.includes(capability)) {
    fail(`README guard expects missing docs/status.md capability: ${capability}`);
  }
}

const capabilityCoverage = {
  "Local daemon and CLI": ["Rust daemon and CLI", "covenantd"],
  "IPC and HTTP gateway": ["IPC and local HTTP gateway", "HTTP gateway"],
  "Identity and peer auth": ["Peer authentication", "operator token rotation"],
  "Signed capabilities": ["Signed capability lifecycle", "signed capabilities"],
  "Audit log": ["Append-only audit log", "audit-root attestations"],
  "Memory store": ["SQLite-backed project memory", "durable memory"],
  "Runtime execution": ["runtime dispatch", "Linux gVisor runner support"],
  "MCP tools": ["MCP adapter", "MCP integration"],
  "A2A messaging": ["A2A mailbox", "A2A messaging"],
  "Budget ledger": ["budget ledger"],
  "Local settlement receipts": ["Local settlement receipts"],
  "On-chain settlement": ["protocol scaffolding for accountable resource use and agent coordination economics"],
  "Autonomous workflow": ["planning, execution, review, repair, and handoff", "resumable task state"],
  "Live boundary coverage": ["live tests", "live coverage inventory"],
  "Public provenance": ["commit-scoped provenance", "provenance envelopes"],
  "Distribution and SDK ecosystem": ["SDK packages", "supporting services"],
};

for (const [capability, needles] of Object.entries(capabilityCoverage)) {
  assertAny(capability, needles);
}

const hash = statusHash();
const marker = readme.match(markerPattern);
if (update) {
  const nextMarker = `<!-- covenant-readme-status-sha256: ${hash} -->`;
  if (marker) {
    readme = readme.replace(markerPattern, nextMarker);
  } else {
    readme = readme.replace("## Capabilities\n", `## Capabilities\n\n${nextMarker}\n`);
  }
  writeFileSync(readmePath, readme);
  console.log(`validate-readme-copy: updated README status marker (${hash})`);
  process.exit(0);
}

if (!marker) {
  fail(
    `missing README status marker; run node ${rel(join(here, "validate-readme-copy.mjs"))} --update`,
  );
} else if (marker[1] !== hash) {
  fail(
    `README status marker is stale for ${rel(statusPath)}; review README and run node ${rel(join(here, "validate-readme-copy.mjs"))} --update`,
  );
}

if (errors.length > 0) {
  console.error("validate-readme-copy: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  console.error(`copy standard: ${rel(standardPath)}`);
  process.exit(1);
}

console.log(
  `validate-readme-copy: ok (${statusCapabilities.length} status capabilities covered)`,
);
