import Link from "next/link";

export const metadata = {
  title: "Settlement",
  description:
    "Credits, receipts, off-chain accounting, and the on-chain settlement program.",
};

export default function SettlementPage() {
  return (
    <>
      <h1>Settlement</h1>
      <p>
        Settlement is how Covenant accounts for resource consumption.
        The model is intentionally split: every action that consumes
        resources writes a local receipt; receipts are batched and
        flushed to the on-chain settlement program; once flushed,
        receipts are reconcilable from chain state alone.
      </p>

      <h2>Receipt</h2>
      <pre>
        <code>{`SettlementReceipt {
  id:               uuid,
  payer:            AgentId,            // who consumed resources
  resource:         "memory" | "compute" | "tool" | "egress",
  credits_consumed: u64,
  settled_at:       u64,                // unix milliseconds
  onchain_sig:      string | null       // populated when flushed on-chain
}`}</code>
      </pre>

      <p>
        Receipts accumulate in{" "}
        <code>$COVENANT_HOME/receipts/working.jsonl</code>. The daemon
        writes one receipt per resource event — for example, every
        memory write produces a receipt with{" "}
        <code>resource = "memory"</code> and{" "}
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

      <h2>Flushing on-chain</h2>
      <p>
        The on-chain side is a single Anchor program for Solana. It
        exposes three instructions:
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
        The daemon batches off-chain receipts and submits them via{" "}
        <code>consume_credits</code>. Once the on-chain transaction
        confirms, the receipt&apos;s <code>onchain_sig</code> is
        populated with the signature; from that point the receipt is
        reconcilable from chain state alone.
      </p>

      <h2>Buyback</h2>
      <p>
        Credits are minted in exchange for burned tokens; the mint side
        burns, the consume side destroys. Net circulating supply
        contracts as the system is used. The on-chain program
        serializes mint and consume operations and binds credits to a
        single authority per cluster, ensuring buyback semantics are not
        subject to mint-versus-consume races.
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
              Off-chain receipts awaiting flush, plus historical
              flushed receipts (with <code>onchain_sig</code>{" "}
              populated).
            </td>
          </tr>
        </tbody>
      </table>

      <h2>Reading recent receipts</h2>
      <pre>
        <code>{`covenant receipts recent --limit 20
# Or via HTTP:
curl -s 127.0.0.1:8421/receipts/recent?limit=20 | jq`}</code>
      </pre>

      <h2>Verification</h2>
      <p>
        <code>covenant verify</code> cross-checks memory writes against
        settlement receipts: a memory write without a corresponding
        receipt, or the inverse, surfaces as drift. The daemon is
        fail-soft on receipt write — a failed receipt does not cancel
        the memory write — so drift in this dimension is the principal
        operator-visible indicator of a settlement-side fault.
      </p>

      <h2>Release</h2>
      <p>
        Off-chain receipts and the local credit accounting are stable.
        The on-chain settlement program is deployed to Solana mainnet
        from the alpha release; an external security audit follows on
        the M2 milestone. Refer to the public roadmap for the milestone
        schedule.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/architecture">Architecture</Link> — the
          on-chain program in the broader system map.
        </li>
        <li>
          <Link href="/identity">Identity and keys</Link> — the
          same key signs settlement transactions and capability
          grants.
        </li>
        <li>
          <Link href="/audit">Audit log</Link> — settlement
          receipts pair 1:1 with memory writes; drift shows up here.
        </li>
      </ul>
    </>
  );
}
