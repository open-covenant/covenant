#!/usr/bin/env node
// Check an eyebrow scan against what was anchored on Solana.
//
// This is the half that matters. Anyone can publish a hash; the claim is only
// worth something if a stranger holding the lockfile can recompute the same
// numbers and read the chain for themselves. This tool needs nothing from us:
// a lockfile, a batch id, and a public RPC endpoint.
//
//   node verify-anchor.mjs --lockfile eyebrowlock.json --batch-id <hex>
//   node verify-anchor.mjs --lockfile eyebrowlock.json --batch-id <hex> --prove <artifact id>
//
// Exit 0 when every check passes, 1 when any of them does not.

import { readFileSync } from "node:fs";
import { deriveFromLockfile, hex, inclusionProof, verifyInclusion } from "./derive.mjs";
import {
  CLUSTERS,
  Connection,
  PROGRAM_ID,
  batchPda,
  explorerAddress,
  readReceiptBatch,
} from "./chain.mjs";

const USAGE = `verify-anchor: check an eyebrow lockfile against its on-chain anchor

  --lockfile <path>            the lockfile to check (required)
  --batch-id <hex>             the anchored batch id (default: the one this lockfile derives)
  --cluster <mainnet|devnet>   which Solana cluster to read (default mainnet)
  --rpc <url>                  override the cluster's RPC endpoint
  --prove <artifact id>        also print and check an inclusion proof for one artifact
  -h, --help                   this text
`;

function parseArgs(argv) {
  const opts = { lockfile: null, batchId: null, cluster: "mainnet", rpc: null, prove: null };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = () => {
      const value = argv[i + 1];
      if (value === undefined) throw new Error(`${arg} needs a value`);
      i += 1;
      return value;
    };
    switch (arg) {
      case "--lockfile": opts.lockfile = next(); break;
      case "--batch-id": opts.batchId = next(); break;
      case "--cluster": opts.cluster = next(); break;
      case "--rpc": opts.rpc = next(); break;
      case "--prove": opts.prove = next(); break;
      case "-h":
      case "--help": opts.help = true; break;
      default: throw new Error(`unknown flag ${arg}`);
    }
  }
  if (!opts.help) {
    if (!opts.lockfile) throw new Error("--lockfile is required");
    if (!CLUSTERS[opts.cluster]) {
      throw new Error(`unknown cluster ${opts.cluster}; expected mainnet or devnet`);
    }
    if (opts.batchId !== null && !/^[0-9a-fA-F]{64}$/.test(opts.batchId)) {
      throw new Error("--batch-id must be 64 hex characters");
    }
  }
  return opts;
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.help) {
    process.stdout.write(USAGE);
    return 0;
  }

  const cluster = CLUSTERS[opts.cluster];
  const lockfile = JSON.parse(readFileSync(opts.lockfile, "utf8"));
  const derived = deriveFromLockfile(lockfile);
  const claimedBatchId = opts.batchId
    ? Buffer.from(opts.batchId.toLowerCase(), "hex")
    : derived.batchId;

  const checks = [];
  const record = (label, ok, detail) => {
    checks.push({ label, ok, detail });
    process.stdout.write(`  ${ok ? "PASS" : "FAIL"}  ${label}\n`);
    if (detail) process.stdout.write(`        ${detail}\n`);
  };

  process.stdout.write(`lockfile   ${opts.lockfile}\n`);
  process.stdout.write(`  generator      ${derived.generator}\n`);
  process.stdout.write(`  generated at   ${derived.generatedAt}\n`);
  process.stdout.write(`  artifacts      ${derived.receiptCount}\n`);
  process.stdout.write(`  content hash   ${hex(derived.contentHash)}\n`);
  process.stdout.write(`  merkle root    ${hex(derived.merkleRoot)}\n`);
  process.stdout.write(`  batch id       ${hex(derived.batchId)}\n\n`);

  const pda = batchPda(claimedBatchId);
  process.stdout.write(`chain      ${PROGRAM_ID.toBase58()} on ${cluster.key}\n`);
  process.stdout.write(`  batch id       ${hex(claimedBatchId)}\n`);
  process.stdout.write(`  batch account  ${pda.toBase58()}\n`);
  process.stdout.write(`  explorer       ${explorerAddress(pda.toBase58(), cluster)}\n\n`);

  const connection = new Connection(opts.rpc ?? cluster.rpcUrl, "confirmed");
  let onchain = null;
  try {
    onchain = (await readReceiptBatch(connection, claimedBatchId)).account;
  } catch (error) {
    process.stdout.write(`  read error: ${error.message}\n`);
  }

  process.stdout.write("checks\n");

  record(
    "lockfile derives the claimed batch id",
    derived.batchId.equals(claimedBatchId),
    derived.batchId.equals(claimedBatchId)
      ? null
      : `derived ${hex(derived.batchId)}, claimed ${hex(claimedBatchId)}`,
  );

  record("a ReceiptBatch is anchored at that batch id", onchain !== null, onchain ? null : "no account at the batch PDA");

  if (onchain) {
    record(
      "on-chain batch id matches its PDA seed",
      onchain.batchId.equals(claimedBatchId),
      onchain.batchId.equals(claimedBatchId) ? null : `account holds ${hex(onchain.batchId)}`,
    );
    record(
      "on-chain merkle root matches the lockfile",
      onchain.merkleRoot.equals(derived.merkleRoot),
      onchain.merkleRoot.equals(derived.merkleRoot)
        ? null
        : `on chain ${hex(onchain.merkleRoot)}, lockfile ${hex(derived.merkleRoot)}`,
    );
    record(
      "on-chain receipt count matches the artifact count",
      onchain.receiptCount === derived.receiptCount,
      onchain.receiptCount === derived.receiptCount
        ? null
        : `on chain ${onchain.receiptCount}, lockfile ${derived.receiptCount}`,
    );
    process.stdout.write("\nanchored record\n");
    process.stdout.write(`  anchored at    ${new Date(onchain.createdAt * 1000).toISOString()}\n`);
    process.stdout.write(`  anchored by    ${onchain.authority}\n`);
    process.stdout.write(`  merkle root    ${hex(onchain.merkleRoot)}\n`);
    process.stdout.write(`  receipt count  ${onchain.receiptCount}\n`);
  }

  if (opts.prove) {
    const index = derived.leaves.findIndex((leaf) => leaf.id === opts.prove);
    process.stdout.write("\ninclusion proof\n");
    if (index === -1) {
      record(`artifact ${opts.prove} is in the lockfile`, false, "no artifact with that id");
    } else {
      const leaf = derived.leaves[index];
      const path = inclusionProof(derived.digests, index);
      const root = onchain ? onchain.merkleRoot : derived.merkleRoot;
      const ok = verifyInclusion({
        digest: leaf.digest,
        index,
        leafCount: derived.receiptCount,
        path,
        root,
      });
      process.stdout.write(`  artifact       ${leaf.tool}/${leaf.type}/${leaf.name}\n`);
      process.stdout.write(`  leaf index     ${index} of ${derived.receiptCount}\n`);
      process.stdout.write(`  leaf digest    ${hex(leaf.digest)}\n`);
      process.stdout.write(`  audit path     ${path.length} sibling hash${path.length === 1 ? "" : "es"}\n`);
      for (const sibling of path) process.stdout.write(`                 ${hex(sibling)}\n`);
      record(
        `audit path carries ${opts.prove} to the ${onchain ? "on-chain" : "derived"} root`,
        ok,
      );
    }
  }

  const failed = checks.filter((check) => !check.ok);
  process.stdout.write(
    `\n${failed.length === 0 ? "VERIFIED" : "NOT VERIFIED"}: ${checks.length - failed.length}/${checks.length} checks passed\n`,
  );
  return failed.length === 0 ? 0 : 1;
}

main()
  .then((code) => process.exit(code))
  .catch((error) => {
    process.stderr.write(`verify-anchor: ${error.message}\n`);
    process.exit(1);
  });
