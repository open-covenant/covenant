import Link from "next/link";

export const metadata = {
  title: "Settlement",
  description:
    "Local receipts, credit accounting, and the planned on-chain settlement path.",
};

export default function SettlementPage() {
  return (
    <>
      <h1>Settlement</h1>
      <p>
        Settlement is how Covenant accounts for resource consumption.
        Today the daemon writes local receipts. The future on-chain path
        batches those receipts into the Solana settlement program once
        the authority, mint, oracle, and reconciliation flows are ready.
      </p>

      <h2>Receipt</h2>
      <pre>
        <code>{`SettlementReceipt {
  id:               uuid,
  payer:            AgentId,            // who consumed resources
  resource:         "memory" | "compute" | "tool" | "egress",
  credits_consumed: u64,
  settled_at:       u64,                // unix milliseconds
  memory_record_id: uuid | null,        // set when resource == "memory"
  onchain_sig:      string | null       // populated when flushed on-chain
}`}</code>
      </pre>

      <p>
        Receipts accumulate in{" "}
        <code>$COVENANT_HOME/receipts/working.jsonl</code>. The daemon
        writes one receipt per resource event — for example, every
        memory write produces a receipt with{" "}
        <code>{'resource = "memory"'}</code> and{" "}
        <code>credits_consumed</code> proportional to bytes written.
      </p>

      <h2>Credit pricing</h2>
      <p>
        Each resource kind has a pricing function that maps the
        underlying unit (bytes, milliseconds, calls) to credits. The
        pricing functions are deliberately simple and live in one
        place, so operators can audit them at a glance and downstream
        integrations can replicate the math without a dependency.
      </p>

      <p>
        The default for memory writes is one credit per kilobyte (round
        up). Compute, tool calls, and egress have their own pricing
        functions that compose the same way.
      </p>

      <h2>Future on-chain flush</h2>
      <p>
        The planned on-chain side is an Anchor program for Solana. Its
        scaffold exposes three instruction shapes:
      </p>

      <ul>
        <li>
          <code>initialize</code> — one-shot setup of a{" "}
          <code>Config</code> PDA under seed{" "}
          <code>b&quot;settlement-config&quot;</code>; records the
          authority, mints, and rates.
        </li>
        <li>
          <code>mint_credits(amount_covnt)</code> — exchange burned
          tokens for credits at the configured rate.
        </li>
        <li>
          <code>consume_credits(amount)</code> — destroy credits at
          the point of consumption (memory write, tool call, etc.).
        </li>
      </ul>

      <p>
        The intended production flow is for the daemon to batch local
        receipts and submit consumption to the program. Once an on-chain
        transaction confirms, the receipt&apos;s <code>onchain_sig</code>{" "}
        can be populated with the signature. This is a planned
        reconciliation path, not deployed behavior.
      </p>

      <h2>Planned economic model</h2>
      <p>
        The target model is credits minted against an external payment
        input, credits consumed at resource-use time, and treasury policy
        that can route a portion of inflows toward buyback or provider
        payout. Those mechanics require oracle integration, mint
        authority decisions, and deployment review before they should be
        treated as operational.
      </p>

      <h2>Storage layout</h2>
      <table>
        <thead>
          <tr>
            <th>Path</th>
            <th>Format</th>
            <th>Purpose</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <code>receipts/working.jsonl</code>
            </td>
            <td>JSONL, append-only</td>
            <td>
              Local receipts. Future flushed receipts can populate{" "}
              <code>onchain_sig</code>.
            </td>
          </tr>
        </tbody>
      </table>

      <h2>Reading recent receipts</h2>
      <pre>
        <code>{`covenant chain status --json
covenant receipts recent --limit 20 --json
covenant chain flush-receipts --limit 20 --json
covenant chain receipt-batches --limit 20 --json
# Or via HTTP:
curl -s 127.0.0.1:8421/receipts/recent?limit=20 | jq`}</code>
      </pre>

      <h2>Verification</h2>
      <p>
        <code>covenant verify --json</code> cross-checks memory writes against
        settlement receipts: a memory write without a corresponding
        receipt, or the inverse, surfaces as drift. The daemon is
        fail-soft on receipt write — a failed receipt does not cancel
        the memory write — so drift in this dimension is the principal
        operator-visible indicator of a settlement-side fault.
      </p>

      <h2>Release</h2>
      <p>
        Local receipts and recent-receipt reads are implemented. On-chain
        settlement is planned and scaffolded; it is not production and
        should not be described as deployed.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/architecture">Architecture</Link> — the
          settlement scaffold in the broader system map.
        </li>
        <li>
          <Link href="/identity">Identity and keys</Link> — the
          same local identity signs capability grants today and is the
          planned signer boundary for future settlement transactions.
        </li>
        <li>
          <Link href="/audit">Audit log</Link> — settlement
          receipts pair 1:1 with memory writes; drift shows up here.
        </li>
      </ul>
    </>
  );
}
