#!/usr/bin/env node
import { readdirSync, readFileSync, existsSync } from "node:fs";
import { dirname, extname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const agentOsRoot = resolve(here, "..");

function usage() {
  console.log(`usage: validate-cvnt-solana-quarantine [--self-test]

Enforce the multichain epic's core value-capture invariant: $CVNT is
Solana-canonical and never leaves it. Concretely:

  (A) the canonical $CVNT mint never appears in a cross-chain surface, so no
      EVM crate can denominate, wrap, or re-issue the token;
  (B) no off-Solana token-bridge primitive (Wormhole NTT, LayerZero OFT,
      xERC20, a wrapped wCVNT) is wired into a cross-chain surface;
  (C) the per-call payment surfaces (agent bond, EVM x402) still denominate in
      chain-local USDC, never $CVNT.

A run also executes an embedded self-test proving each detector fires on a
known-bad fixture and stays quiet on the real prose these crates ship (which
legitimately says "no bridge" and names the Solana "sap-bridge"). Pass
--self-test to run only that self-test.`);
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

const CVNT_MINT = "2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump";

// Cross-chain surfaces $CVNT must never touch. The per-call payment crates
// (covenant-x402, covenant-x402-signer) are included whole: x402 fees are USDC
// on every chain, so the mint has no business anywhere in them. This is an
// allowlist: a new EVM/cross-chain crate is not scanned until it is added here,
// so extend this list whenever the trust layer reaches a new chain.
const QUARANTINE_ROOTS = [
  "crates/covenant-attestation",
  "crates/covenant-evm-signer",
  "crates/covenant-x402",
  "crates/covenant-x402-signer",
  "evm",
];

const SCAN_EXTENSIONS = new Set([".rs", ".toml", ".sol", ".md", ".json"]);

// Off-Solana token-bridge primitives. Deliberately narrow: the generic words
// "bridge" and "wrap" are load-bearing prose in these crates ("no bridge
// required", the Solana "sap-bridge", "Wraps an audit root as a VC"), so only
// distinctive product identifiers with no innocent meaning are banned. Do not
// broaden this to plain "bridge"/"wrap" — you will flag the docs that explain
// the invariant. A real bridge would also name the canonical mint, which
// check (A) already rejects.
const BRIDGE_TERMS = [
  { name: "Wormhole NTT (native token transfer)", re: /native[\s_-]?token[\s_-]?transfer/i },
  { name: "Wormhole NTT (NTT)", re: /\bNTT\b/ },
  { name: "LayerZero OFT (omnichain fungible token)", re: /omnichain[\s_-]?fungible/i },
  { name: "LayerZero OFT (OFT)", re: /\bOFT\b/ },
  { name: "crosschain mint/burn ERC-20 (xERC20)", re: /xERC20/i },
  { name: "wrapped $CVNT (wCVNT)", re: /\bwCVNT\b/i },
];

// Per-call payment surfaces that must keep denominating in USDC.
const USDC_SURFACES = [
  "crates/covenant-attestation/src/bond.rs",
  "crates/covenant-x402/src/evm.rs",
];

function scanText(label, text) {
  const violations = [];
  if (text.includes(CVNT_MINT)) {
    violations.push(
      `${label}: references the canonical $CVNT mint ${CVNT_MINT}; $CVNT is Solana-canonical and must not appear in a cross-chain surface`,
    );
  }
  for (const term of BRIDGE_TERMS) {
    if (term.re.test(text)) {
      violations.push(
        `${label}: references off-Solana token-bridge primitive "${term.name}"; $CVNT is never bridged, wrapped, or re-minted off Solana`,
      );
    }
  }
  return violations;
}

function usdcMissing(text) {
  return !/USDC/.test(text);
}

function collectFiles(absDir, acc) {
  for (const entry of readdirSync(absDir, { withFileTypes: true })) {
    if (entry.name === "target" || entry.name === "node_modules") continue;
    const abs = join(absDir, entry.name);
    if (entry.isDirectory()) {
      collectFiles(abs, acc);
      continue;
    }
    if (entry.isFile() && SCAN_EXTENSIONS.has(extname(entry.name))) {
      acc.push(abs);
    }
  }
  return acc;
}

const SELFTEST_BAD = [
  { label: "mint", text: `const M: &str = "${CVNT_MINT}";`, expect: /canonical \$CVNT mint/ },
  { label: "ntt-long", text: "use wormhole::NativeTokenTransfers;", expect: /token-bridge primitive/ },
  { label: "ntt-acr", text: "// bridged in via NTT and back", expect: /token-bridge primitive/ },
  { label: "oft-long", text: "adapter: OmnichainFungibleToken", expect: /token-bridge primitive/ },
  { label: "oft-acr", text: "contract CvntOFT is OFT {}", expect: /token-bridge primitive/ },
  { label: "xerc20", text: "an xERC20 lockbox for the token", expect: /token-bridge primitive/ },
  { label: "wrapped", text: "mint wCVNT on Base", expect: /token-bridge primitive/ },
];

// The genuine prose these crates ship. The detector must stay silent on all of
// it, or the invariant guard becomes noise the next author learns to ignore.
const SELFTEST_GOOD = [
  "//! A default agent bond is chain-local USDC on Base — never `$CVNT`.",
  "//! one ecrecover authenticates it: no bridge, light client, or Solana read.",
  "//! Wraps a Covenant audit-chain root as a portable, dual-signed VC.",
  "//! the same value the sap-bridge anchors on Solana, made verifiable off-chain.",
  "let usdc = base_usdc(); // per-call fees are always USDC via x402",
];

function runSelfTest() {
  const failures = [];
  for (const bad of SELFTEST_BAD) {
    const found = scanText(`selftest:${bad.label}`, bad.text);
    if (!found.some((v) => bad.expect.test(v))) {
      failures.push(`self-test: bad fixture "${bad.label}" was not caught (expected ${bad.expect})`);
    }
  }
  SELFTEST_GOOD.forEach((text, index) => {
    const found = scanText(`selftest:good-${index}`, text);
    if (found.length > 0) {
      failures.push(`self-test: good fixture #${index} was falsely flagged: ${found.join("; ")}`);
    }
  });
  if (usdcMissing("denominated in USDC")) {
    failures.push("self-test: USDC-present fixture reported as missing USDC");
  }
  if (!usdcMissing("denominated in $CVNT")) {
    failures.push("self-test: USDC-absent fixture not detected");
  }
  return failures;
}

const selfFailures = runSelfTest();

if (selfTestOnly) {
  if (selfFailures.length > 0) {
    console.error("validate-cvnt-solana-quarantine: failed (self-test)");
    for (const failure of selfFailures) console.error(`- ${failure}`);
    process.exit(1);
  }
  console.log(
    `validate-cvnt-solana-quarantine: ok (self-test: ${SELFTEST_BAD.length} caught, ${SELFTEST_GOOD.length} clean)`,
  );
  process.exit(0);
}

const errors = [...selfFailures];
let scanned = 0;

for (const root of QUARANTINE_ROOTS) {
  const absRoot = join(agentOsRoot, root);
  if (!existsSync(absRoot)) {
    errors.push(`${root}: quarantine root does not exist (rename or drop it from the guard)`);
    continue;
  }
  for (const abs of collectFiles(absRoot, [])) {
    scanned += 1;
    const rel = relative(agentOsRoot, abs);
    errors.push(...scanText(rel, readFileSync(abs, "utf8")));
  }
}

for (const surface of USDC_SURFACES) {
  const abs = join(agentOsRoot, surface);
  if (!existsSync(abs)) {
    errors.push(`${surface}: USDC payment surface not found (update the guard if it moved)`);
    continue;
  }
  if (usdcMissing(readFileSync(abs, "utf8"))) {
    errors.push(`${surface}: no longer references USDC; per-call payment surfaces must denominate in USDC, not $CVNT`);
  }
}

if (errors.length > 0) {
  console.error("validate-cvnt-solana-quarantine: failed");
  for (const error of errors) console.error(`- ${error}`);
  process.exit(1);
}

console.log(
  `validate-cvnt-solana-quarantine: ok (${scanned} cross-chain files, ${QUARANTINE_ROOTS.length} roots, ${USDC_SURFACES.length} USDC surfaces)`,
);
