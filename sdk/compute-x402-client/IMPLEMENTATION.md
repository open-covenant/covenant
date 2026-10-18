# x402 AgentKit client qualification

Validated locally on 2026-10-18 using Node 24.20.0. This file records implementation evidence, not deployment or funded execution.

The direct dependency versions are Coinbase AgentKit 0.10.4, official x402 core/SVM 2.28.0, Solana Kit 8.5.0 and web3.js 1.98.4. `npm-shrinkwrap.json` pins the full graph and is included in the published tarball. No `--legacy-peer-deps`, `--force`, private package mirrors or modified dependency source was needed.

18 focused tests passed from source. The same 18 passed against a packed tarball installed in `/tmp/covenant-x402-packed-check`, outside the source checkout. The installed tests import the public package exports for the client and AgentKit action. Both runs exercised:

- AgentKit registration and real `SolanaKeypairWalletProvider.signTransaction` invocation;
- official x402 client transaction construction and actual gateway facilitator verification, with simulation mocked and broadcasting prohibited;
- exact standard memo/request binding and a real payer signature;
- real Rust `BuyerExecutor.prepare` and `SignedWorkReceipt` fixture verification;
- invalid network, asset, recipient, price, digest, memo and expiry rejected before wallet signing or paid dispatch;
- exact authorization reuse after unknown network outcome and client restart;
- completed result reuse, changed-request refusal and persistent cumulative spend limits;
- simultaneous independent Node processes sharing one SQLite ceiling;
- launch limits of 32 messages and 4,096 UTF-8 JSON bytes enforced before quoting, reserving or signing;
- duplicate JSON keys, changed signed output and invalid receipt signatures rejected.

The cross-process test found an initialization race: SQLite's busy timeout must be set before changing the journal mode. The implementation and test now cover that ordering.

AgentKit 0.10.4's initialization analytics hook starts an unawaited request that produced an unhandled HTTP-400 rejection during the initial test. `CovenantSolanaKeypairWalletProvider` suppresses that hook only; signing and custody inherit Coinbase's original wallet implementation.

Reproduce source and cross-component checks from this directory:

```sh
npm ci --ignore-scripts --no-audit --no-fund
COVENANT_X402_BRIDGE_PATH="$(pwd)/../../services/compute-x402-facilitator/bridge.mjs" npm test
```

The package tarball tested on this date is `/tmp/covenant-x402-packed-check/covenant-compute-x402-client-0.1.0.tgz`, SHA-256 `815cc93d7861447730c695c00f5ad0825e609f16f958ec8ef5cf117cc2bfdcbd`. This is a local artifact, not a publication URL.

No mainnet payment, production inference request, external provider charge, deployment or public package publication was performed during this client qualification. Those gates belong in the release's production evidence after the funded run.
