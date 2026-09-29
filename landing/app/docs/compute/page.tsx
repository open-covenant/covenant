import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = [
  "compute",
  "Compute",
  "Rent a GPU by the second on Solana mainnet, pay in USDC, and run a node that earns by the second.",
] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

const REPO = "https://github.com/open-covenant/covenant";
const TAG = "compute-v0.1.0";

export default function ComputePage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
      <h1>Compute</h1>
      <p>
        Covenant Compute rents GPUs by the second. You pay in USDC for the time a session runs, and
        whatever you held back for it and did not use comes back when it closes. Operators earn USDC
        for the same seconds and stake CVNT to take work. It runs on Solana mainnet.
      </p>

      <h2>How a session settles</h2>
      <ol>
        <li>
          You open a lease with a rate, in micro-USDC per second, and a window. The ceiling, rate
          times window, moves from your balance into an escrow vault on Solana.
        </li>
        <li>
          While the machine runs, the session&apos;s meter lives in a MagicBlock ephemeral rollup (a
          short-lived Solana execution environment that writes back to mainnet). The coordinator
          ticks it with the elapsed time every five seconds, at no cost per tick.
        </li>
        <li>
          When you close the lease, the meter commits to Solana in one write and the vault pays the
          operator <code>ceil(rate × elapsed_ms / 1000)</code>. The rest of the ceiling returns to
          your balance.
        </li>
      </ol>
      <p>
        Elapsed time runs from the moment an operator accepts the lease. A lease no operator
        accepts, or whose machine never comes up, is refunded in full. A window can be at most 24
        hours.
      </p>
      <p>
        The first mainnet session held an NVIDIA L40S for 192,561 ms at 334 micro-USDC a second:
        64,316 micro-USDC to the operator, 35,884 back to the renter.
      </p>

      <h2>Endpoints and addresses</h2>
      <table>
        <thead>
          <tr>
            <th>What</th>
            <th>Value</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>Coordinator API</td>
            <td>
              <code>https://compute-api.opencovenant.org</code>
            </td>
          </tr>
          <tr>
            <td>Coordinator key</td>
            <td>
              <code>3Z27s9cCPNWYg7y86PhGwHpEMV4crrY5vAwTwzQNbjjh</code>
            </td>
          </tr>
          <tr>
            <td>Settlement program</td>
            <td>
              <code>3dTtXH7rah8YyWTsAfSg6qC3iqrJXWGEhAx57uUHXZff</code>
            </td>
          </tr>
          <tr>
            <td>Ephemeral rollup</td>
            <td>
              <code>https://eu.magicblock.app</code>, validator{" "}
              <code>MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e</code>
            </td>
          </tr>
          <tr>
            <td>Payments</td>
            <td>
              USDC, <code>EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v</code>
            </td>
          </tr>
          <tr>
            <td>Operator stake</td>
            <td>
              CVNT, <code>2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump</code>
            </td>
          </tr>
        </tbody>
      </table>
      <p>
        <code>GET /federation/capacity</code> on the coordinator lists what can be rented right now
        and at what price.
      </p>

      <h2>Install</h2>
      <p>The tools build from source with a Rust toolchain:</p>
      <pre>
        <code>{`git clone --depth 1 --branch ${TAG} ${REPO}
cd covenant/agent-os
cargo install --locked --path crates/covenant-compute-buyer
cargo install --locked --path crates/covenant-compute-node
cargo install --locked --path crates/covenant-compute-lease-signer --bin covenant-compute-stake`}</code>
      </pre>
      <p>
        <code>covenant-compute-buyer</code> installs the <code>covenant-compute</code> command. Operators
        also need <code>covenant-compute-node</code> and <code>covenant-compute-stake</code>.
      </p>

      <h2>Rent a GPU</h2>
      <pre>
        <code>{`export COVENANT_COMPUTE_COORDINATOR_URL=https://compute-api.opencovenant.org
covenant-compute whoami      # your buyer key, kept in ~/.covenant-compute-mcp
covenant-compute balance     # funds, and where to send a top-up`}</code>
      </pre>
      <p>
        To fund your balance, send USDC to the deposit address that <code>balance</code> prints, with
        the memo <code>compute-buyer:&lt;your buyer key&gt;</code>, then claim the transfer:
      </p>
      <pre>
        <code>{`covenant-compute deposit <transaction-signature>`}</code>
      </pre>
      <p>Open a lease and connect to it:</p>
      <pre>
        <code>{`covenant-compute capacity
covenant-compute lease open --minutes 10 --rate 334 --ssh-key ~/.ssh/id_ed25519.pub
covenant-compute lease view <job-id>
covenant-compute lease close <job-id>`}</code>
      </pre>
      <p>
        <code>lease open</code> waits up to four minutes for the machine and prints its SSH address.
        The rate must meet an operator&apos;s ask, which <code>capacity</code> shows per lease hour.
        The whole ceiling must also fit under your per-call spend cap,{" "}
        <code>COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC</code>, which defaults to $1: at 334 micro-USDC a
        second that is just under 50 minutes. Raise it for longer windows.
      </p>
      <p>
        After a lease closes, <code>covenant-compute verify &lt;job-id&gt;</code> reads the payout back
        from Solana through your own RPC (<code>--rpc-url</code>).{" "}
        <code>covenant-compute withdraw &lt;micro-usdc&gt; &lt;wallet&gt;</code> moves unspent balance
        out; each withdrawal can move up to $100.
      </p>

      <h2 id="run-a-node">Run a node</h2>
      <p>
        A node is a machine, or a broker for one, that registers with the coordinator and serves
        leases or jobs. It is paid in USDC from each session&apos;s escrow, one payout per job.
      </p>
      <pre>
        <code>{`covenant-compute-node setup     # coordinator URL and key, payout address, backend
covenant-compute-node           # register and serve
covenant-compute-node status    # why it is or is not winning work`}</code>
      </pre>
      <p>
        The{" "}
        <a href={`${REPO}/tree/${TAG}/agent-os/crates/covenant-compute-node`}>node README</a> lists the
        executors a node can serve with and their settings.
      </p>

      <h3>Stake</h3>
      <p>
        The coordinator matches a node only while at least 1,000,000 CVNT is staked for its identity
        in the settlement program. The stake belongs to the wallet that signs it, and only that wallet
        can withdraw it, once its lock ends.
      </p>
      <pre>
        <code>{`covenant-compute-stake stake <node> 1000000 --lock-days 30 --keypair <wallet.json> --rpc <mainnet RPC>
covenant-compute-stake status <node>
covenant-compute-stake unstake <node> --keypair <wallet.json>`}</code>
      </pre>
      <p>
        The program requires a lock of at least seven days. The coordinator counts a stake while it
        stays locked for at least one more day plus the dispute window, two days today, so a node
        stops matching shortly before its stake unlocks. To stay matched, stake again from another
        wallet before then: positions are kept per node and wallet.
      </p>

      <h3>Slashing</h3>
      <p>
        A fault the coordinator proves itself costs 50,000 CVNT of the node&apos;s stake, sent to the
        protocol treasury. Two kinds of fault count: a known-answer test job answered wrong, and the
        losing side of a check where the same job is run again on other nodes. A buyer&apos;s dispute is
        recorded against the node&apos;s reputation and never moves stake. Leases are not tested this
        way, so a node that only serves leases is not slashed by these checks.
      </p>
      <p>
        A node slashed below 1,000,000 CVNT stops matching until it is staked back up.
      </p>
    </>
  );
}
