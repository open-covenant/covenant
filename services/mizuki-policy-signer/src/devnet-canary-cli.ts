import { DevnetCanaryError, parseDevnetCanaryArgs, runDevnetCanary } from './devnet-canary.js';

const USAGE = `Usage: node dist/devnet-canary-cli.js \\
  --rpc-url-file PATH \\
  --program-id PUBKEY \\
  --artifact PATH \\
  --artifact-sha256 HEX \\
  --artifact-commit GIT_SHA \\
  --authority-keypair PATH \\
  --claimant-keypair PATH \\
  --adversary-keypair PATH \\
  --output PATH \\
  [--amount-lamports 1000000] \\
  [--expiry-seconds 90] \\
  [--execute]

The default is a read-only dry run. --execute authorizes devnet transactions only.`;

async function main(): Promise<void> {
  if (process.argv.slice(2).includes('--help')) {
    process.stdout.write(`${USAGE}\n`);
    return;
  }
  const options = parseDevnetCanaryArgs(process.argv.slice(2));
  const receipt = await runDevnetCanary(options);
  process.stdout.write(`${receipt.status} ${receipt.payloadSha256} ${receipt.artifact.sha256}\n`);
}

main().catch((error: unknown) => {
  const code = error instanceof DevnetCanaryError ? error.code : 'unexpected_failure';
  process.stderr.write(`devnet canary failed: ${code}\n`);
  process.exitCode = 1;
});
