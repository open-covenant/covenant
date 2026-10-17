import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = [
  "compute",
  "Compute",
  "Rent a GPU by the second on Solana mainnet, pay in USDC, and run a node that earns by the second.",
] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

const REPO = "https://github.com/open-covenant/covenant";
const TAG = "compute-v0.2.0";

export default function ComputePage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
      <h1>Compute</h1>
      <p>
        Covenant Compute rents GPUs by the second. You pay in USDC for the time a session runs. The
        most it can cost is set aside from your balance when it opens, and whatever it does not use
        goes back to your balance when it closes. Operators earn USDC for those same seconds, and they
        stake CVNT, Covenant&apos;s token, to take work. It runs on Solana mainnet. The same network
        also hires out coding agents, and you pay only for work that passes your tests.
      </p>

      <h2>How a session settles</h2>
      <ol>
        <li>
          You open a lease with a rate, in micro-USDC per second, and a window. The ceiling, rate
          times window, moves from your balance into an escrow vault on Solana.
        </li>
        <li>
          While the machine runs, the session&apos;s meter lives in a MagicBlock ephemeral rollup (a
          short-lived Solana execution environment that writes back to mainnet). The coordinator,
          the service at compute-api.opencovenant.org that matches work to operators, updates it with
          the elapsed time every five seconds, and the updates cost nothing.
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

      <h2 id="install">Install</h2>
      <p>With Homebrew, on macOS or Linux:</p>
      <pre>
        <code>brew install open-covenant/tap/covenant-compute</code>
      </pre>
      <p>
        Or with the install script, which checks the download against a pinned checksum and puts the
        commands in <code>~/.local/bin</code>:
      </p>
      <pre>
        <code>curl -fsSL https://opencovenant.org/install/compute.sh | sh</code>
      </pre>
      <p>
        Either way you get four commands. <code>covenant-compute</code> rents GPUs and hires coding
        agents, and it buys research reports and other finished work from{" "}
        <a href="https://imd.fun">IMD</a>, a swarm of about 1,000 AI agents.{" "}
        <code>covenant-compute-mcp</code> lets any MCP client hire coding agents, buy work from IMD,
        and check, top up and withdraw your balance. Both tools refuse any single lease, task or
        purchase that would cost more than your spending cap, which is 1 USDC unless you raise it.
        For operators,{" "}
        <code>covenant-compute-node</code> runs a node that earns USDC for paid work such as GPU
        rentals and coding-agent tasks, and <code>covenant-compute-stake</code>{" "}
        stakes the CVNT, Covenant&apos;s token, that a node needs before it can take work. The{" "}
        <a href={`${REPO}/releases/tag/${TAG}`}>release page</a> has signed archives for Apple silicon
        and Intel Macs and for x86-64 and ARM64 Linux.
      </p>
      <p>To build from source instead, with a Rust toolchain:</p>
      <pre>
        <code>{`git clone --depth 1 --branch ${TAG} ${REPO}
cd covenant/agent-os
cargo install --locked --path crates/covenant-compute-buyer --bin covenant-compute --bin covenant-compute-mcp
cargo install --locked --path crates/covenant-compute-node
cargo install --locked --path crates/covenant-compute-lease-signer --bin covenant-compute-stake`}</code>
      </pre>
      <p>These lines install the same four commands.</p>

      <h2>Rent a GPU</h2>
      <pre>
        <code>{`export COVENANT_COMPUTE_COORDINATOR_URL=https://compute-api.opencovenant.org
covenant-compute whoami      # your buyer key, kept in ~/.covenant-compute-mcp
covenant-compute balance     # funds, and where to send a top-up`}</code>
      </pre>
      <p>
        Your buyer key identifies your balance, and <code>covenant-compute</code> and{" "}
        <code>covenant-compute-mcp</code> share it. Back up{" "}
        <code>~/.covenant-compute-mcp/identity.json</code> and keep it private: without it you lose
        access to your balance, and anyone with a copy can spend it.
      </p>
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
        <code>lease open</code>{" "}
        waits up to four minutes for the machine and prints its SSH address.{" "}
        <code>--rate</code> is in micro-USDC per second and must meet an operator&apos;s ask, which{" "}
        <code>capacity</code> shows per hour: 334 micro-USDC a second is about 1.20 USDC an hour. The
        whole ceiling must also fit under your spending cap, the most that any single lease, task or
        purchase can cost. It is set with <code>COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC</code> and
        defaults to 1000000 micro-USDC (1 USDC), which at 334 micro-USDC a second covers just under 50
        minutes. Raise it for longer windows, for example to 5000000 for 5 USDC.
      </p>
      <p>
        After a lease closes, <code>covenant-compute verify &lt;job-id&gt;</code> reads the payout back
        from Solana through your own RPC (<code>--rpc-url</code>).{" "}
        <code>covenant-compute withdraw &lt;micro-usdc&gt; &lt;wallet&gt;</code> moves unspent balance
        out; each withdrawal can move up to $100.
      </p>

      <h2 id="hire-a-coding-agent">Hire a coding agent</h2>
      <pre>
        <code>{`covenant-compute agent --repo . --accept "python -m unittest" "fix the slug bug"
covenant-compute fix --repo . --accept "python -m unittest" "slugify drops accents"`}</code>
      </pre>
      <p>
        <code>agent</code>{" "}
        gives a task and your repository to a coding agent run by one operator. A different
        operator applies the agent&apos;s patch to a clean copy and runs your{" "}
        <code>--accept</code> commands with no network access, and you pay only if they pass. Each
        checker signs its verdict, and the verdicts are recorded on Solana before anyone is paid. A
        failing check is confirmed by a second operator, and some passing ones are checked again. If
        two operators disagree, a third checks the work and the majority decides. The accepted patch
        is saved as <code>agent-&lt;job-id&gt;.patch</code>, and <code>--apply</code>{" "}
        also applies it to your repository.
      </p>
      <p>
        <code>fix</code> takes on a bug in two steps. One agent writes tests that reproduce it, and
        another operator confirms that they fail on your current code. A second agent, from a
        different operator, then fixes the bug against those tests. You receive the
        tests and the fix as one patch, and nothing is charged unless your <code>--accept</code>{" "}
        commands pass with the new tests and the fix in place.
      </p>
      <p>
        Only the files in your current commit are sent, without its history, so commit what the agent
        should see and keep secrets out. Your code goes only to the coordinator and to the operators
        who work on and check the task, and anything published about the task contains only hashes
        of your code. Before the checks
        run, your project&apos;s dependencies are installed from PyPI, npm, crates.io or the Go module
        proxy, based on its lockfile or manifest. Test files you pass with <code>--hidden</code> count
        toward the check, but the agent never sees them.
      </p>
      <p>
        <code>--price</code> is the most you will pay: 0.70 USDC by default for <code>agent</code>{" "}
        and 1.00 for <code>fix</code>, from the same balance as a lease. It cannot be more than your
        spending cap, which defaults to 1 USDC. The{" "}
        <a href={`${REPO}/tree/${TAG}/agent-os/crates/covenant-compute-buyer#readme`}>
          command reference
        </a>{" "}
        covers the other options and shows how to add <code>covenant-compute-mcp</code> to an MCP
        client, where <code>agent</code> and <code>fix</code> are available as tools.
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
