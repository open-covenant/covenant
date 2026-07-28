#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const agentOsRoot = resolve(here, "..");
const repoRoot = resolve(agentOsRoot, "..");

function usage() {
  console.log(`usage: validate-agentcard-conformance [--self-test]

Keep the published Covenant Foundation agent card conformant to the
covenant-identity dual-shape (A2A AgentCard + ERC-8004 registration).

The golden fixture (crates/covenant-identity/tests/fixtures/
covenant-foundation.unsigned.json) is hard-gated: it must satisfy every
structural rule the crate's deny_unknown_fields deserialization enforces,
plus the pinned identity values (Base ERC-8004 agentId 58403 in the
0x8004A169... registry, the did:pkh Solana subject, CAIP-2 genesis-form
registries, never a network-name form). The crate's own tests pin the
fixture byte-for-byte to the generator, so this validator and the crate
cannot drift apart.

The live served card (landing/public/agents/covenant-foundation.json — the
agentURI content of the Base registration) is compared against the golden
with signatures stripped: replacing it is a deliberate, operator-reviewed
release step, so divergence is reported loudly per field but does not veto
the commit gate. A missing tracked file does.

The embedded self-test always runs; pass --self-test to run only it.`);
}

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  usage();
  process.exit(0);
}
const selfTestOnly = args.includes("--self-test");
const unknown = args.filter((arg) => arg !== "--self-test");
if (unknown.length > 0) {
  usage();
  process.exit(2);
}

const REGISTRATION_TYPE = "https://eips.ethereum.org/EIPS/eip-8004#registration-v1";
const A2A_PROTOCOL_VERSION = "0.3.0";
const SOLANA_CAIP2 = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
// Pinned live identity values (agent-os/evm/deployments.json
// covenantMainnet.erc8004Registration, and the Foundation Solana identity).
const BASE_AGENT_ID = 58403;
const BASE_REGISTRY = "eip155:8453:0x8004A169FB4a3325136EB29fA0ceB6D2e539a432";
const FOUNDATION_PUBKEY = "4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc";
// MPL Core — same id covenant-metaplex pins as MPL_CORE_PROGRAM_ID.
const HOME_REGISTRY = `${SOLANA_CAIP2}:CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d`;

const REQUIRED_FIELDS = [
  "type",
  "protocolVersion",
  "name",
  "description",
  "url",
  "version",
  "capabilities",
  "defaultInputModes",
  "defaultOutputModes",
  "skills",
  "image",
  "services",
  "x402Support",
  "active",
  "registrations",
  // Vec<String> with skip_serializing_if but no serde default: the crate
  // requires it on deserialization whenever it serialized it, and the
  // Foundation card always carries it.
  "supportedTrust",
];
const STRING_FIELDS = ["type", "protocolVersion", "name", "description", "url", "version", "image"];
const KNOWN_FIELDS = new Set([...REQUIRED_FIELDS, "signatures"]);
const SERVICE_KEYS = new Set(["name", "endpoint", "version"]);
const REGISTRATION_KEYS = new Set(["agentId", "agentRegistry"]);
const CAPABILITY_KEYS = new Set(["streaming", "pushNotifications", "stateTransitionHistory"]);
const SKILL_KEYS = new Set(["id", "name", "description", "tags", "examples"]);

/// Structural conformance mirror of the crate's AgentRegistration shape
/// (deny_unknown_fields everywhere) plus the pinned Foundation identity
/// values. Returns human-readable problems; empty means conformant.
function checkCard(card) {
  const problems = [];
  if (typeof card !== "object" || card === null || Array.isArray(card)) {
    return ["card is not a JSON object"];
  }

  for (const field of REQUIRED_FIELDS) {
    if (!(field in card)) problems.push(`missing required field ${field}`);
  }
  for (const key of Object.keys(card)) {
    if (!KNOWN_FIELDS.has(key)) {
      problems.push(`unknown top-level field ${key} (crate parses with deny_unknown_fields)`);
    }
  }
  for (const field of STRING_FIELDS) {
    if (field in card && typeof card[field] !== "string") {
      problems.push(`${field} must be a string`);
    }
  }
  if ("type" in card && card.type !== REGISTRATION_TYPE) {
    problems.push(`type "${card.type}" != ERC-8004 registration-v1 discriminator`);
  }
  if ("protocolVersion" in card && card.protocolVersion !== A2A_PROTOCOL_VERSION) {
    problems.push(`protocolVersion "${card.protocolVersion}" != pinned A2A ${A2A_PROTOCOL_VERSION}`);
  }
  for (const flag of ["x402Support", "active"]) {
    if (flag in card && typeof card[flag] !== "boolean") {
      problems.push(`${flag} must be a boolean`);
    }
  }
  if ("capabilities" in card) {
    const caps = card.capabilities;
    if (typeof caps !== "object" || caps === null || Array.isArray(caps)) {
      problems.push("capabilities must be an object");
    } else {
      for (const key of Object.keys(caps)) {
        if (!CAPABILITY_KEYS.has(key)) problems.push(`capabilities has unknown field ${key}`);
      }
    }
  }

  const registrations = Array.isArray(card.registrations) ? card.registrations : [];
  if ("registrations" in card && registrations.length === 0) {
    problems.push("registrations must be a non-empty array");
  }
  registrations.forEach((r, i) => {
    for (const key of Object.keys(r ?? {})) {
      if (!REGISTRATION_KEYS.has(key)) {
        problems.push(`registrations[${i}] has unknown field ${key}`);
      }
    }
    if (!Number.isSafeInteger(r?.agentId) || r.agentId < 0) {
      problems.push(
        `registrations[${i}].agentId ${JSON.stringify(r?.agentId)} is not a u64 JSON integer (Registration.agent_id is u64, never a pubkey string)`,
      );
    }
    const registry = r?.agentRegistry;
    const solanaForm = new RegExp(`^${SOLANA_CAIP2.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}:\\S+$`);
    const eipForm = /^eip155:\d+:0x[0-9a-fA-F]{40}$/;
    if (typeof registry !== "string" || (!solanaForm.test(registry) && !eipForm.test(registry))) {
      problems.push(
        `registrations[${i}].agentRegistry ${JSON.stringify(registry)} is not a CAIP-2 genesis-form solana or eip155 registry (network-name forms like solana:101 do not resolve)`,
      );
    }
  });

  const services = Array.isArray(card.services) ? card.services : [];
  services.forEach((s, i) => {
    for (const key of Object.keys(s ?? {})) {
      if (!SERVICE_KEYS.has(key)) {
        problems.push(
          `services[${i}] has unknown field ${key} (the ERC-8004 service entry is name/endpoint/version; skills are top-level A2A skills)`,
        );
      }
    }
    if (typeof s?.name !== "string" || typeof s?.endpoint !== "string") {
      problems.push(`services[${i}] must have string name and endpoint`);
    }
    if ("version" in (s ?? {}) && typeof s.version !== "string") {
      problems.push(`services[${i}].version must be a string when present`);
    }
  });

  const skills = Array.isArray(card.skills) ? card.skills : [];
  skills.forEach((s, i) => {
    for (const key of Object.keys(s ?? {})) {
      if (!SKILL_KEYS.has(key)) problems.push(`skills[${i}] has unknown field ${key}`);
    }
    for (const field of ["id", "name", "description"]) {
      if (typeof s?.[field] !== "string") problems.push(`skills[${i}].${field} must be a string`);
    }
    for (const list of ["tags", "examples"]) {
      const value = s?.[list];
      if (list === "examples" && !(list in (s ?? {}))) continue;
      if (!Array.isArray(value) || value.some((t) => typeof t !== "string")) {
        problems.push(`skills[${i}].${list} must be an array of strings`);
      }
    }
  });

  const signatures = card.signatures;
  if ("signatures" in card) {
    if (!Array.isArray(signatures)) {
      problems.push("signatures must be an array");
    } else {
      signatures.forEach((sig, i) => {
        if (typeof sig?.protected !== "string" || typeof sig?.signature !== "string") {
          problems.push(`signatures[${i}] must carry string protected and signature members`);
        }
      });
    }
  }

  // Pinned Foundation identity values.
  const home = registrations[0];
  if (
    registrations.length > 0 &&
    !(home?.agentId === 0 && home?.agentRegistry === HOME_REGISTRY)
  ) {
    problems.push(
      `registrations[0] must be the Solana home registry {agentId 0, ${HOME_REGISTRY}}; a re-pointed home registry must not survive to operator review`,
    );
  }
  const hasBase = registrations.some(
    (r) => r?.agentId === BASE_AGENT_ID && r?.agentRegistry === BASE_REGISTRY,
  );
  if (!hasBase) {
    problems.push(
      `no registrations entry binds the Base ERC-8004 agentId ${BASE_AGENT_ID} to ${BASE_REGISTRY}`,
    );
  }
  const expectedDid = `did:pkh:${SOLANA_CAIP2}:${FOUNDATION_PUBKEY}`;
  if (!services.some((s) => s?.name === "DID" && s?.endpoint === expectedDid)) {
    problems.push(`no DID service entry carries the Solana identity ${expectedDid}`);
  }

  return problems;
}

function withoutSignatures(card) {
  const { signatures, ...body } = card;
  return body;
}

/// Key-order-independent stringify, so a semantically identical card with
/// reordered members compares equal.
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (typeof value === "object" && value !== null) {
    const entries = Object.keys(value)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${canonical(value[k])}`);
    return `{${entries.join(",")}}`;
  }
  return JSON.stringify(value);
}

function divergentFields(live, golden) {
  const fields = [];
  for (const key of new Set([...Object.keys(live), ...Object.keys(golden)])) {
    if (canonical(live[key]) !== canonical(golden[key])) fields.push(key);
  }
  return fields;
}

function readJson(path, label) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (e) {
    console.error(`validate-agentcard-conformance: ${label} is unreadable or not JSON: ${path}: ${e.message}`);
    process.exit(1);
  }
}

// --- self-test -------------------------------------------------------------

function selfTest(golden) {
  const failures = [];
  let assertions = 0;
  const expect = (name, cond) => {
    assertions += 1;
    if (!cond) failures.push(name);
  };

  expect("golden fixture is conformant", checkCard(golden).length === 0);

  // The shape served before this gate existed: pubkey string as agentId, a
  // network-name registry, skills inside service entries, and no A2A fields.
  const legacy = {
    type: REGISTRATION_TYPE,
    name: golden.name,
    description: golden.description,
    image: golden.image,
    active: true,
    registrations: [{ agentId: FOUNDATION_PUBKEY, agentRegistry: "solana:101:metaplex" }],
    services: [
      { name: "web", endpoint: "https://opencovenant.org", skills: ["identity"] },
    ],
  };
  const legacyProblems = checkCard(legacy);
  expect(
    "legacy pubkey-string agentId is caught",
    legacyProblems.some((p) => p.includes("not a u64 JSON integer")),
  );
  expect(
    "legacy solana:101 registry form is caught",
    legacyProblems.some((p) => p.includes("network-name forms")),
  );
  expect(
    "legacy services[].skills is caught",
    legacyProblems.some((p) => p.includes("services[0] has unknown field skills")),
  );
  expect(
    "legacy missing A2A fields are caught",
    ["protocolVersion", "url", "version", "capabilities", "skills"].every((f) =>
      legacyProblems.some((p) => p === `missing required field ${f}`),
    ),
  );

  const signed = {
    ...golden,
    signatures: [{ protected: "eyJhbGciOiJFZERTQSJ9", signature: "QUJD" }],
  };
  expect("signatures field stays conformant", checkCard(signed).length === 0);
  expect(
    "signature stripping makes the signed card golden-equal",
    JSON.stringify(withoutSignatures(signed)) === JSON.stringify(golden),
  );

  const injected = { ...golden, evilEndpoint: "https://attacker.example" };
  expect(
    "unknown top-level field is caught",
    checkCard(injected).some((p) => p.includes("unknown top-level field evilEndpoint")),
  );

  const renamed = { ...golden, name: "impostor" };
  expect("conformant-but-divergent card reports the field", divergentFields(renamed, golden).includes("name"));

  const reordered = Object.fromEntries(Object.entries(golden).reverse());
  expect(
    "key order does not count as divergence",
    canonical(reordered) === canonical(golden) && divergentFields(reordered, golden).length === 0,
  );

  const wrongId = {
    ...golden,
    registrations: [golden.registrations[0], { agentId: 1, agentRegistry: BASE_REGISTRY }],
  };
  expect(
    "missing Base agentId binding is caught",
    checkCard(wrongId).some((p) => p.includes(`agentId ${BASE_AGENT_ID}`)),
  );

  const repointedHome = {
    ...golden,
    registrations: [
      { agentId: 0, agentRegistry: `${SOLANA_CAIP2}:AttackerProgram1111111111111111111111111111` },
      golden.registrations[1],
    ],
  };
  expect(
    "re-pointed home registry is caught",
    checkCard(repointedHome).some((p) => p.includes("must be the Solana home registry")),
  );

  const skillInjected = {
    ...golden,
    skills: [{ ...golden.skills[0], endpoint: "https://attacker.example" }, ...golden.skills.slice(1)],
  };
  expect(
    "unknown field inside a skill entry is caught",
    checkCard(skillInjected).some((p) => p.includes("skills[0] has unknown field endpoint")),
  );

  const { supportedTrust, ...trustless } = golden;
  expect(
    "missing supportedTrust is caught",
    checkCard(trustless).some((p) => p === "missing required field supportedTrust"),
  );

  if (failures.length > 0) {
    for (const f of failures) console.error(`self-test FAILED: ${f}`);
    process.exit(1);
  }
  return assertions;
}

// --- main ------------------------------------------------------------------

const goldenPath = join(
  agentOsRoot,
  "crates/covenant-identity/tests/fixtures/covenant-foundation.unsigned.json",
);
const livePath = join(repoRoot, "landing/public/agents/covenant-foundation.json");

if (!existsSync(goldenPath)) {
  console.error(`missing tracked golden fixture: ${goldenPath}`);
  process.exit(1);
}
const golden = readJson(goldenPath, "golden fixture");

const fixtures = selfTest(golden);
if (selfTestOnly) {
  console.log(`validate-agentcard-conformance self-test ok (${fixtures} assertions)`);
  process.exit(0);
}

const goldenProblems = checkCard(golden);
if (goldenProblems.length > 0) {
  for (const p of goldenProblems) console.error(`validate-agentcard-conformance: golden: ${p}`);
  process.exit(1);
}

if (!existsSync(livePath)) {
  console.error(`missing tracked live card: ${livePath}`);
  process.exit(1);
}
const live = readJson(livePath, "live card");
const liveBody = withoutSignatures(live);

if (canonical(liveBody) === canonical(golden)) {
  console.log(
    `validate-agentcard-conformance ok (self-test ${fixtures} assertions; golden conformant; live card matches the golden${Array.isArray(live.signatures) && live.signatures.length > 0 ? ", signed" : ""})`,
  );
  process.exit(0);
}

// Replacing the served card is operator-gated: the Base registration's
// agentURI points at it, so this reports loudly without vetoing the commit
// gate. The crate golden test still hard-fails if the generator drifts.
const liveProblems = checkCard(liveBody);
console.error(
  `warning: live card ${livePath} diverges from the golden fixture (operator-reviewed swap pending)`,
);
for (const field of divergentFields(liveBody, golden)) {
  console.error(`warning:   divergent field: ${field}`);
}
for (const p of liveProblems) {
  console.error(`warning:   live card non-conformance: ${p}`);
}
console.log(
  `validate-agentcard-conformance ok (self-test ${fixtures} assertions; golden conformant; live card DIVERGES: ${liveProblems.length} non-conformance finding(s), swap is operator-gated)`,
);
