import Link from "next/link";
import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = ["settlement", "Settlement", 'Local receipts, credit accounting, and the on-chain settlement program, live on mainnet.'] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

export default function SettlementPage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
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
  resource:         "memory" | "compute" | "tool" | "message" | "registration",
  credits_consumed: u64,
  settled_at:       u64,                // unix milliseconds
  memory_record_id: uuid | null,        // set when resource == "memory"
  chain:            string | null,      // populated when anchored on-chain
  cluster:          string | null,      // Solana cluster identifier when anchored on-chain
  batch_id:         string | null,      // identifier of the receipt batch
  merkle_root:      string | null,      // batch merkle root committed on-chain
  tx_sig:           string | null,      // signature of anchor_receipt_batch tx
  slot:             u64 | null,         // chain slot where the batch confirmed
  confirmed_at:     u64 | null,         // unix milliseconds at confirmation
  onchain_sig:      string | null       // populated when flushed on-chain
}`}</code>
      </pre>

      <p>
        Receipts accumulate in{" "}
        <code>$COVENANT_HOME/receipts/working.jsonl</code>. The daemon
        writes one receipt per resource event. For example, every
        memory write produces a receipt with{" "}
        <code>{'resource = "memory"'}</code>,{" "}
        <code>memory_record_id</code>, and <code>credits_consumed</code>{" "}
        proportional to bytes written.
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
        The planned on-chain side is an Anchor program for Solana. Three
        of its instruction shapes describe the credit flow:
      </p>

      <ul>
        <li>
          <code>initialize</code>: one-shot setup of a{" "}
          <code>Config</code> PDA under seed{" "}
          <code>b&quot;settlement-config&quot;</code>; records the
          authority, mints, and rates.
        </li>
        <li>
          <code>buy_credits(amount_covnt)</code>: transfer{" "}
          <code>$COVNT</code> into the treasury in exchange for
          credits at the configured rate.
        </li>
        <li>
          <code>consume_credits(amount, receipt_hash)</code>:
          destroy credits at the point of consumption (memory write,
          tool call, etc.). <code>receipt_hash</code> binds the
          on-chain consumption to a specific local receipt batch for
          reconciliation.
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
curl -s '127.0.0.1:8421/receipts/recent?limit=20&since_ms=1714938000000' | jq`}</code>
      </pre>

      <h2>Verification</h2>
      <p>
        <code>covenant verify --json</code> cross-checks memory writes against
        settlement receipts by <code>memory_record_id</code> when the
        receipt carries one, with owner/resource count fallback for older
        rows. Missing, duplicate, orphaned, or wrong-payer correlations
        surface as drift. The daemon is fail-soft on receipt write (a
        failed receipt does not cancel the memory write), so drift in this
        dimension is the principal operator-visible indicator of a
        settlement-side fault.
      </p>

      <h2>Backfill</h2>
      <p>
        When <code>covenant verify</code> reports legacy receipts without a{" "}
        <code>memory_record_id</code> correlation, the operator can repair
        them with the{" "}
        <code>covenant settlement backfill-receipts --json</code> CLI verb
        (HTTP equivalent: <code>POST /settlement/receipts/backfill</code>).
        Verify remains read-only; the backfill mutator runs only when
        explicitly invoked. The verb applies by default; pass{" "}
        <code>--dry-run</code> to report the <code>row_count</code> an
        apply would change without writing. Dry-run requires the{" "}
        <code>settlement.backfill.dry_run</code> capability; apply requires{" "}
        <code>settlement.backfill.apply</code>. See{" "}
        <Link href="/docs/capabilities">capabilities</Link> for the scope
        contract. An apply path writes a rollback checkpoint sibling file
        before mutation, fsyncs both the rewrite and the rename, then emits
        an operator-issued <code>SettlementReceiptBackfillApplied</code>{" "}
        audit row carrying <code>row_count</code>,{" "}
        <code>rollback_path</code>, and <code>dry_run</code>.
      </p>
      <pre>
        <code>{`$ covenant settlement backfill-receipts --dry-run --json
{"schema":"covenant.settlement.backfill.v1","row_count":12,"rollback_path":null,"dry_run":true}`}</code>
      </pre>

      <h2>x402 payment rail</h2>
      <p>
        Distinct from the internal receipt flush above, the agent-to-service
        payment rail over HTTP 402 (x402) is live. The daemon can pay for
        metered x402 resources outbound, and Covenant operates a deployed
        x402 seller that settles in USDC on Solana mainnet &mdash; selling
        Covenant-Verified attestations, on-chain identity passports, and
        reputation reads, with the seller&rsquo;s signing key published at
        <code>/.well-known/x402</code> &mdash; alongside an escrow service and
        an Ephemeral-Rollup credit facilitator. This is a
        separate concern from anchoring internal resource receipts: the
        payment rail being live does not make the receipt flush production.
      </p>

      <h2>Release</h2>
      <p>
        Local receipts and recent-receipt reads are implemented. The Solana
        settlement program is deployed and live on mainnet — $CVNT-to-credits,
        staking, slashing, and on-chain receipt anchoring transact on-chain.
        The daemon-driven per-intent settlement lifecycle that anchors internal
        resource receipts is not yet production.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/architecture">Architecture</Link>: the
          settlement program in the broader system map.
        </li>
        <li>
          <Link href="/identity">Identity and keys</Link>: the
          same local identity signs capability grants today and is the
          planned signer boundary for future settlement transactions.
        </li>
        <li>
          <Link href="/audit">Audit log</Link>: settlement
          receipts pair 1:1 with memory writes; drift shows up here.
        </li>
      </ul>
    </>
  );
}
