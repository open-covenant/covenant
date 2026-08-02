#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const agentOsRoot = resolve(here, "..");

function usage() {
  console.log(`usage: validate-reputation-staging [--self-test] [--submission]

Decode-validate the staged EAS reputation attest transaction
(autonomy/multichain/staging/reputation-attest-base-sepolia.json and its
-dryrun evidence) against the covenant-evm-signer crate's own constants, so
hand-assembled or drifted calldata can never sit awaiting operator submission.

Asserts, word by word: the derived attest selector, the nested-tuple offsets,
the pinned reputation schema UID, expirationTime mirrored between envelope and
schema data, source_chain equal to the crate's SOLANA_MAINNET_CAIP2 (read out
of reputation.rs, so the two cannot drift), the relay payload bound, and a
solana_attestation_pda policy mirroring the crate's projection validation:
an all-zero PDA and any 32-repeats-of-one-byte pattern (including the
retired 0xab..ab staging placeholder) always fail, in every mode.
--submission is the operator pre-sign gate: a superseded dry-run fails.

The staged artifacts are local staging state (not in a clean clone); when the
staging directory is absent the artifact checks are skipped. The embedded
self-test always runs: the good fixture is the crate encoder's own output, and
each detector must fire on a known-bad fixture, including the exact corrupt
source_chain bytes this validator exists to catch. Pass --self-test to run
only the self-test.`);
}

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  usage();
  process.exit(0);
}
const selfTestOnly = args.includes("--self-test");
const submission = args.includes("--submission");
const unknown = args.filter((arg) => arg !== "--self-test" && arg !== "--submission");
if (unknown.length > 0) {
  usage();
  process.exit(2);
}

// Crate-source truths, read out of reputation.rs so the validator cannot
// drift from the encoder. A failed match is a hard error, never a default.
const reputationSourcePath = join(
  agentOsRoot,
  "crates/covenant-evm-signer/src/reputation.rs",
);
const reputationSource = readFileSync(reputationSourcePath, "utf8");
function sourceConst(name, re) {
  const m = reputationSource.match(re);
  if (!m) {
    console.error(`cannot find ${name} in ${reputationSourcePath}; refusing to guess`);
    process.exit(1);
  }
  return m[1];
}
const SOURCE_CHAIN = sourceConst(
  "SOLANA_MAINNET_CAIP2",
  /pub const SOLANA_MAINNET_CAIP2: &str =\s*"([^"]+)";/,
);
const ATTEST_SIGNATURE = sourceConst(
  "ATTEST_SIGNATURE",
  /pub const ATTEST_SIGNATURE: &str =\s*"([^"]+)";/,
);
const RELAY_MAX_DATA_BYTES = Number(
  sourceConst("RELAY_MAX_DATA_BYTES", /pub const RELAY_MAX_DATA_BYTES: usize =\s*(\d+);/),
);

// keccak256(ATTEST_SIGNATURE)[..4] — derived by the crate's attest_selector()
// and pinned by its attest_selector_is_derived_and_pinned test.
const ATTEST_SELECTOR = "f17325e7";
// Pinned by the crate's schema_uid_is_pinned test.
const SCHEMA_UID =
  "0x84738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc39";
const EAS_PREDEPLOY = "0x4200000000000000000000000000000000000021";
const ZERO_ADDRESS = "0x0000000000000000000000000000000000000000";
const BASE_SEPOLIA_CHAIN_ID = 84532;
// The live anchor's bytes: base58 7PEd79CG1hFUU9qeBnAKmyA77YWzckd572qsYdq3W3GH
// (docs/metaplex-integration.md), cross-generated via @solana/web3.js and
// pinned by the crate's solana_account_bytes test.
const REAL_PDA_HEX = "5ed84d69180c43cbb5a3fbc022dddb666b30155ecc0acad29a2e8941d522c8e6";

const ceil32 = (n) => Math.ceil(n / 32) * 32;

/// Strict word-by-word decode of attest((bytes32,(address,uint64,bool,
/// bytes32,bytes,uint256))) calldata carrying the reputation schema tuple.
/// Throws on the first structural violation; nothing is tolerated or skipped.
function decodeAttest(dataHex) {
  if (!/^0x(?:[0-9a-f]{2})+$/.test(dataHex)) {
    throw new Error("not lowercase 0x-prefixed hex of whole bytes");
  }
  const bytes = Buffer.from(dataHex.slice(2), "hex");
  if (bytes.length < 4 + 10 * 32) {
    throw new Error(`calldata is ${bytes.length} bytes, below the fixed 324-byte head`);
  }
  const selector = bytes.subarray(0, 4).toString("hex");
  if (selector !== ATTEST_SELECTOR) {
    throw new Error(`selector ${selector} != ${ATTEST_SELECTOR} (${ATTEST_SIGNATURE})`);
  }
  const body = bytes.subarray(4);
  if (body.length % 32 !== 0) {
    throw new Error("calldata body is not 32-byte word aligned");
  }
  const word = (i) => body.subarray(i * 32, (i + 1) * 32);
  const wordHex = (i) => word(i).toString("hex");
  const expectWord = (i, value, name) => {
    const expected = value.toString(16).padStart(64, "0");
    if (wordHex(i) !== expected) {
      throw new Error(`word ${i} (${name}) = 0x${wordHex(i)}, expected 0x${expected}`);
    }
  };
  const uint = (i, name, byteWidth) => {
    const w = word(i);
    if (w.subarray(0, 32 - byteWidth).some((b) => b !== 0)) {
      throw new Error(`word ${i} (${name}) overflows uint${byteWidth * 8}`);
    }
    return BigInt("0x" + w.toString("hex"));
  };

  expectWord(0, 0x20n, "AttestationRequest offset");
  const schemaUid = "0x" + wordHex(1);
  expectWord(2, 0x40n, "AttestationRequestData offset");
  expectWord(3, 0n, "recipient");
  const expirationTime = uint(4, "expirationTime", 8);
  expectWord(5, 1n, "revocable");
  expectWord(6, 0n, "refUID");
  expectWord(7, 0xc0n, "bytes data offset");
  expectWord(8, 0n, "value");
  const dataLen = Number(uint(9, "data length", 4));

  if (body.length !== 10 * 32 + ceil32(dataLen)) {
    throw new Error(
      `calldata body is ${body.length} bytes, expected ${10 * 32 + ceil32(dataLen)} for a ${dataLen}-byte payload`,
    );
  }
  const data = body.subarray(10 * 32, 10 * 32 + dataLen);
  if (body.subarray(10 * 32 + dataLen).some((b) => b !== 0)) {
    throw new Error("nonzero padding after the schema data");
  }

  if (dataLen < 6 * 32) {
    throw new Error(`schema data is ${dataLen} bytes, below the 5-word head + length word`);
  }
  const inner = (i) => data.subarray(i * 32, (i + 1) * 32);
  const innerUint = (i, name, byteWidth) => {
    const w = inner(i);
    if (w.subarray(0, 32 - byteWidth).some((b) => b !== 0)) {
      throw new Error(`schema word ${i} (${name}) overflows uint${byteWidth * 8}`);
    }
    return BigInt("0x" + w.toString("hex"));
  };
  const score = Number(innerUint(0, "score", 4));
  const decimals = Number(innerUint(1, "score_decimals", 1));
  const expiry = innerUint(2, "expiry", 8);
  if (inner(3).toString("hex") !== "a0".padStart(64, "0")) {
    throw new Error(`schema word 3 (source_chain offset) = 0x${inner(3).toString("hex")}, expected 0xa0`);
  }
  const pdaHex = inner(4).toString("hex");
  const sourceLen = Number(innerUint(5, "source_chain length", 4));
  if (dataLen !== 6 * 32 + ceil32(sourceLen)) {
    throw new Error(
      `schema data is ${dataLen} bytes, expected ${6 * 32 + ceil32(sourceLen)} for a ${sourceLen}-byte source_chain`,
    );
  }
  const sourceBytes = data.subarray(6 * 32, 6 * 32 + sourceLen);
  if (data.subarray(6 * 32 + sourceLen).some((b) => b !== 0)) {
    throw new Error("nonzero padding after source_chain");
  }
  const sourceChain = new TextDecoder("utf-8", { fatal: true }).decode(sourceBytes);

  return { schemaUid, expirationTime, dataLen, score, decimals, expiry, pdaHex, sourceChain };
}

/// Validate the staged unsigned-transaction artifact. Returns problems (veto)
/// and warnings (loud, non-veto) separately.
function validateArtifact(artifact, { submission }) {
  const problems = [];
  const warnings = [];

  let decoded;
  try {
    decoded = decodeAttest(String(artifact.data ?? ""));
  } catch (e) {
    problems.push(`data: ${e.message}`);
    return { problems, warnings };
  }

  if (decoded.schemaUid !== SCHEMA_UID) {
    problems.push(`data: schema UID ${decoded.schemaUid} != pinned ${SCHEMA_UID}`);
  }
  if (artifact.schemaUID !== SCHEMA_UID) {
    problems.push(`schemaUID field ${artifact.schemaUID} != pinned ${SCHEMA_UID}`);
  }
  if (BigInt(artifact.expirationTime ?? -1) !== decoded.expirationTime) {
    problems.push(
      `expirationTime field ${artifact.expirationTime} != calldata ${decoded.expirationTime}`,
    );
  }
  if (decoded.expirationTime === 0n) {
    problems.push(
      "data: expirationTime is 0; EAS treats 0 as never-expiring, which the crate's projection validation refuses",
    );
  }
  if (decoded.expiry !== decoded.expirationTime) {
    problems.push(
      `schema-data expiry ${decoded.expiry} != envelope expirationTime ${decoded.expirationTime}`,
    );
  }
  if (decoded.sourceChain !== SOURCE_CHAIN) {
    problems.push(
      `data: source_chain "${decoded.sourceChain}" (${Buffer.byteLength(decoded.sourceChain)} bytes) != crate SOLANA_MAINNET_CAIP2 "${SOURCE_CHAIN}"`,
    );
  }
  if (decoded.decimals > 18) {
    problems.push(`data: score_decimals ${decoded.decimals} exceeds 18`);
  }
  const maxDataBytes = artifact.policy?.maxDataBytes;
  if (maxDataBytes !== RELAY_MAX_DATA_BYTES) {
    problems.push(
      `policy.maxDataBytes ${maxDataBytes} != crate RELAY_MAX_DATA_BYTES ${RELAY_MAX_DATA_BYTES}`,
    );
  }
  if (decoded.dataLen > RELAY_MAX_DATA_BYTES) {
    problems.push(
      `data: schema data is ${decoded.dataLen} bytes, exceeds policy.maxDataBytes ${RELAY_MAX_DATA_BYTES}`,
    );
  }
  if (artifact.function !== ATTEST_SIGNATURE) {
    problems.push(`function field "${artifact.function}" != crate ATTEST_SIGNATURE`);
  }
  if (artifact.to !== EAS_PREDEPLOY) {
    problems.push(`to ${artifact.to} != EAS predeploy ${EAS_PREDEPLOY}`);
  }
  if (artifact.chainId !== BASE_SEPOLIA_CHAIN_ID) {
    problems.push(`chainId ${artifact.chainId} != Base Sepolia ${BASE_SEPOLIA_CHAIN_ID}`);
  }
  if (artifact.recipient !== ZERO_ADDRESS) {
    problems.push(`recipient ${artifact.recipient} != zero address`);
  }
  if (artifact.revocable !== true) {
    problems.push(`revocable ${artifact.revocable} != true`);
  }
  if (artifact.value !== "0x0") {
    problems.push(`value ${artifact.value} != "0x0"`);
  }

  if (decoded.pdaHex === "00".repeat(32)) {
    problems.push("data: solana_attestation_pda is all-zero; the score must reference its Solana anchor");
  } else if (/^([0-9a-f]{2})\1{31}$/.test(decoded.pdaHex)) {
    // Mirrors the crate's projection validation: no real Solana account
    // is 32 repeats of one byte. The 0xab..ab staging placeholder's
    // declared-note allowance was retired when the real anchor was
    // staged (multichain-attestation-pda-producer); the whole class now
    // fails in every mode.
    problems.push(
      `data: solana_attestation_pda is 32 repeats of 0x${decoded.pdaHex.slice(0, 2)} — a placeholder pattern, not a real Solana account`,
    );
  }

  return { problems, warnings, decoded };
}

/// Validate the dry-run evidence against the staged artifact: same bytes, same
/// target — or an explicit top-level "superseded" marker acknowledging the
/// evidence predates the current calldata and must be re-run before signing.
function validateDryrun(dryrun, mainDataHex, { submission }) {
  const problems = [];
  const warnings = [];

  if (typeof dryrun.superseded === "string" && dryrun.superseded.length > 0) {
    if (submission) {
      problems.push(`dry-run evidence is superseded ("${dryrun.superseded}"); re-run it before submission`);
    } else {
      warnings.push(
        `dry-run evidence is SUPERSEDED and does not cover the current calldata: ${dryrun.superseded}`,
      );
    }
    return { problems, warnings };
  }

  const call = dryrun.attestDryRun ?? {};
  if (call.data !== mainDataHex) {
    problems.push("attestDryRun.data differs from the staged artifact's data (evidence covers different bytes)");
  }
  if (call.to !== EAS_PREDEPLOY) {
    problems.push(`attestDryRun.to ${call.to} != EAS predeploy ${EAS_PREDEPLOY}`);
  }
  if (dryrun.chainId !== BASE_SEPOLIA_CHAIN_ID) {
    problems.push(`dry-run chainId ${dryrun.chainId} != Base Sepolia ${BASE_SEPOLIA_CHAIN_ID}`);
  }
  return { problems, warnings };
}

// --- self-test -------------------------------------------------------------
// The good fixture is attest_calldata()'s own output for the reviewed staging
// projection (score 9500 at 4 decimals, window 1700000000..1800000000,
// SOLANA_MAINNET_CAIP2) anchored to the live audit-root attestation asset
// (docs/metaplex-integration.md), pasted from
// `cargo run -p covenant-evm-signer --example stage_reputation_attest` — never
// hand-assembled here.
const GOLDEN_DATA_HEX =
  "0xf17325e7000000000000000000000000000000000000000000000000000000000000002084738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc3900000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000006b49d2000000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000251c0000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000006b49d20000000000000000000000000000000000000000000000000000000000000000a05ed84d69180c43cbb5a3fbc022dddb666b30155ecc0acad29a2e8941d522c8e60000000000000000000000000000000000000000000000000000000000000027736f6c616e613a3565796b7434557346763850384e4a64545245705931767a714b715a4b76647000000000000000000000000000000000000000000000000000";

// Frozen historical output: the pre-anchor artifact with the retired
// 0xab..ab staging placeholder. Kept only as the known-bad fixture
// proving the placeholder class now fails in every mode.
const RETIRED_PLACEHOLDER_DATA_HEX =
  "0xf17325e7000000000000000000000000000000000000000000000000000000000000002084738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc3900000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000006b49d2000000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000251c0000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000006b49d20000000000000000000000000000000000000000000000000000000000000000a0abababababababababababababababababababababababababababababababab0000000000000000000000000000000000000000000000000000000000000027736f6c616e613a3565796b7434557346763850384e4a64545245705931767a714b715a4b76647000000000000000000000000000000000000000000000000000";

// The exact corrupt calldata this validator exists to catch: staged on-disk
// until 2026-07-28, its source_chain is a 42-byte
// "solana:5eykt4UsFv8P8NJdTREpY1vzqKv3xpK6QqZ" no encoder can produce.
const HISTORICAL_CORRUPT_DATA_HEX =
  "0xf17325e7000000000000000000000000000000000000000000000000000000000000002084738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc3900000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000006b49d2000000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000251c0000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000006b49d20000000000000000000000000000000000000000000000000000000000000000a0abababababababababababababababababababababababababababababababab000000000000000000000000000000000000000000000000000000000000002a736f6c616e613a3565796b7434557346763850384e4a64545245705931767a714b763378704b3651715a00000000000000000000000000000000000000000000";

// Fixture builder for known-bad shapes only; the good path is never blessed
// by this, only by the crate encoder's pasted output.
function synthCalldata({ sourceChain, pdaHex = REAL_PDA_HEX, expiry = 0x6b49d200 }) {
  const w = (n) => n.toString(16).padStart(64, "0");
  const src = Buffer.from(sourceChain, "utf8");
  const data =
    w(0x251c) +
    w(4) +
    w(expiry) +
    w(0xa0) +
    pdaHex +
    w(src.length) +
    src.toString("hex").padEnd(ceil32(src.length) * 2, "0");
  const dataLen = data.length / 2;
  return (
    "0x" +
    ATTEST_SELECTOR +
    w(0x20) +
    SCHEMA_UID.slice(2) +
    w(0x40) +
    w(0) +
    w(expiry) +
    w(1) +
    w(0) +
    w(0xc0) +
    w(0) +
    w(dataLen) +
    data
  );
}

function goodArtifact() {
  return {
    abiSourceCommit: "0c51c77cccd68e19ddbfeb832f153e75fac1af19",
    chainId: BASE_SEPOLIA_CHAIN_ID,
    data: GOLDEN_DATA_HEX,
    expirationTime: 1800000000,
    function: ATTEST_SIGNATURE,
    network: "Base Sepolia",
    notes: [
      "solanaAttestationPda is the live audit-root attestation asset (docs/metaplex-integration.md).",
    ],
    policy: { maxDataBytes: RELAY_MAX_DATA_BYTES, maxWritesPerDay: 24 },
    recipient: ZERO_ADDRESS,
    revocable: true,
    schemaUID: SCHEMA_UID,
    to: EAS_PREDEPLOY,
    value: "0x0",
  };
}

function selfTest() {
  const failures = [];
  let assertions = 0;
  const expect = (name, cond) => {
    assertions += 1;
    if (!cond) failures.push(name);
  };
  const run = (artifact, opts = { submission: false }) => validateArtifact(artifact, opts);

  const good = run(goodArtifact());
  expect(
    "good fixture passes with no problems and no warnings",
    good.problems.length === 0 && good.warnings.length === 0,
  );
  expect("good fixture decodes the crate source_chain", good.decoded?.sourceChain === SOURCE_CHAIN);
  expect("good fixture decodes score 9500/4", good.decoded?.score === 9500 && good.decoded?.decimals === 4);
  expect("good fixture carries the live anchor", good.decoded?.pdaHex === REAL_PDA_HEX);

  const goodSubmission = run(goodArtifact(), { submission: true });
  expect("good fixture passes the submission gate", goodSubmission.problems.length === 0);

  const retired = run({ ...goodArtifact(), data: RETIRED_PLACEHOLDER_DATA_HEX });
  expect(
    "the retired 0xab..ab placeholder fails in default mode",
    retired.problems.some((p) => /placeholder pattern/.test(p)),
  );

  const corrupt = run({ ...goodArtifact(), data: HISTORICAL_CORRUPT_DATA_HEX });
  expect(
    "historical corrupt source_chain is caught",
    corrupt.problems.some((p) => /source_chain/.test(p) && /v3xpK6QqZ/.test(p)),
  );

  const zeroPda = run({
    ...goodArtifact(),
    data: GOLDEN_DATA_HEX.replace(REAL_PDA_HEX, "00".repeat(32)),
  });
  expect("all-zero PDA is refused", zeroPda.problems.some((p) => /all-zero/.test(p)));

  const repeatedPda = run({
    ...goodArtifact(),
    data: GOLDEN_DATA_HEX.replace(REAL_PDA_HEX, "cd".repeat(32)),
  });
  expect(
    "a repeated-byte PDA is refused",
    repeatedPda.problems.some((p) => /placeholder pattern/.test(p)),
  );

  const wrongUid = run({
    ...goodArtifact(),
    data: GOLDEN_DATA_HEX.replace(SCHEMA_UID.slice(2), "11".repeat(32)),
  });
  expect("wrong schema UID is refused", wrongUid.problems.some((p) => /schema UID/.test(p)));

  const oversize = run({
    ...goodArtifact(),
    data: synthCalldata({ sourceChain: "solana:" + "x".repeat(340) }),
  });
  expect(
    "oversize payload is refused",
    oversize.problems.some((p) => /exceeds policy.maxDataBytes/.test(p)),
  );

  const neverExpiring = run({
    ...goodArtifact(),
    data: synthCalldata({ sourceChain: SOURCE_CHAIN, expiry: 0 }),
    expirationTime: 0,
  });
  expect(
    "never-expiring attestation is refused",
    neverExpiring.problems.some((p) => /never-expiring/.test(p)),
  );

  const truncated = run({ ...goodArtifact(), data: GOLDEN_DATA_HEX.slice(0, -64) });
  expect("truncated calldata is refused", truncated.problems.some((p) => /^data: /.test(p)));

  const staleDryrun = validateDryrun(
    { chainId: BASE_SEPOLIA_CHAIN_ID, attestDryRun: { to: EAS_PREDEPLOY, data: HISTORICAL_CORRUPT_DATA_HEX } },
    GOLDEN_DATA_HEX,
    { submission: false },
  );
  expect(
    "dry-run over different bytes is refused",
    staleDryrun.problems.some((p) => /differs from the staged artifact/.test(p)),
  );

  const superseded = validateDryrun(
    { superseded: "calldata regenerated", chainId: BASE_SEPOLIA_CHAIN_ID },
    GOLDEN_DATA_HEX,
    { submission: false },
  );
  expect("superseded dry-run warns, not vetoes", superseded.problems.length === 0 && superseded.warnings.length === 1);

  const supersededSubmission = validateDryrun(
    { superseded: "calldata regenerated", chainId: BASE_SEPOLIA_CHAIN_ID },
    GOLDEN_DATA_HEX,
    { submission: true },
  );
  expect(
    "submission gate refuses a superseded dry-run",
    supersededSubmission.problems.some((p) => /re-run it before submission/.test(p)),
  );

  if (failures.length > 0) {
    for (const f of failures) console.error(`self-test FAILED: ${f}`);
    process.exit(1);
  }
  return assertions;
}

// --- main ------------------------------------------------------------------

const fixtures = selfTest();
if (selfTestOnly) {
  console.log(`validate-reputation-staging self-test ok (${fixtures} assertions)`);
  process.exit(0);
}

const stagingDir = join(agentOsRoot, "autonomy/multichain/staging");
const mainPath = join(stagingDir, "reputation-attest-base-sepolia.json");
const dryrunPath = join(stagingDir, "reputation-attest-base-sepolia-dryrun.json");

if (!existsSync(mainPath) && !existsSync(dryrunPath)) {
  console.log(
    `validate-reputation-staging self-test ok (${fixtures} assertions); no staged reputation artifacts in this checkout, artifact checks skipped`,
  );
  process.exit(0);
}
if (!existsSync(mainPath) || !existsSync(dryrunPath)) {
  console.error(
    `staged reputation artifact pair is broken: expected both ${mainPath} and ${dryrunPath}`,
  );
  process.exit(1);
}

const artifact = JSON.parse(readFileSync(mainPath, "utf8"));
const dryrun = JSON.parse(readFileSync(dryrunPath, "utf8"));

const main = validateArtifact(artifact, { submission });
const evidence = validateDryrun(dryrun, artifact.data, { submission });
const problems = [...main.problems, ...evidence.problems];
const warnings = [...main.warnings, ...evidence.warnings];

for (const w of warnings) console.error(`warning: ${w}`);
if (problems.length > 0) {
  for (const p of problems) console.error(`validate-reputation-staging: ${p}`);
  process.exit(1);
}
console.log(
  `validate-reputation-staging ok (self-test ${fixtures} assertions; staged attest decoded: score ${main.decoded.score}/${main.decoded.decimals}, source_chain matches crate, ${main.decoded.dataLen}-byte payload within ${RELAY_MAX_DATA_BYTES}${warnings.length > 0 ? `; ${warnings.length} warning(s)` : ""})`,
);
