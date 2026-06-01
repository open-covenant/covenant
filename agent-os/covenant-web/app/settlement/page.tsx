"use client";

import { api } from "@/lib/api";
import { solscanTxUrl } from "@/lib/explorer";
import { formatDateTime, formatTimestamp, shortHash } from "@/lib/format";
import { resolveSynapseStatus } from "@/lib/synapse";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "../components/PageHeader";

// The receipt set the daemon publishes is per-network. Use the same
// cluster the rest of the daemon advertises so the Solscan link
// always lands on the right explorer view (devnet sandbox today;
// flips to mainnet automatically when the operator switches).
const CLUSTER = resolveSynapseStatus().clusterAlias;

const DEMO_MODE = process.env.NEXT_PUBLIC_DEMO_MODE === "1";

async function loadSettlement() {
  const [receipts, debits] = await Promise.all([
    api.recentReceipts(40),
    api.recentDebits(40),
  ]);
  return { receipts: receipts.receipts, debits: debits.debits };
}

export default function SpendingPage() {
  const { data, error, lastSyncMs } = usePoll(loadSettlement, 4000);
  const receipts = data?.receipts ?? [];
  const debits = data?.debits ?? [];
  const totalCredits = receipts.reduce((s, r) => s + r.credits_consumed, 0);
  const onChain = receipts.filter((r) => r.onchain_sig).length;

  return (
    <>
      <PageHeader
        eyebrow="credits consumed · anchored on Solana"
        title="Settlement"
        subhead={
          DEMO_MODE
            ? "Every task on this sandbox burns credits against the deployed settlement program on devnet. Each receipt links to its on-chain transaction so the activity can be independently verified. Shared tally; resets periodically."
            : "Every task burns credits against the deployed settlement program. Receipts that have been anchored on-chain show their tx signature; the rest are local-only until the chain publisher runs."
        }
        syncMs={lastSyncMs}
        error={error}
      />

      <section className="metric-row">
        <article className="metric">
          <span className="eyebrow">charged</span>
          <span className="value">{receipts.length}</span>
          <span className="caption">{totalCredits} credits used</span>
        </article>
        <article className="metric">
          <span className="eyebrow">spent</span>
          <span className="value">{debits.length}</span>
          <span className="caption">matched to charges</span>
        </article>
        <article className="metric">
          <span className="eyebrow">on-chain</span>
          <span className="value">{onChain}</span>
          <span className="caption">{receipts.length - onChain} still local-only</span>
        </article>
        <article className="metric">
          <span className="eyebrow">window</span>
          <span className="value small">40</span>
          <span className="caption">most recent charges</span>
        </article>
      </section>

      <section className="split-2">
        <div className="panel">
          <div className="panel-head">
            <div>
              <p className="eyebrow">spent</p>
              <h2>
                What your agents spent <span className="count">{debits.length}</span>
              </h2>
            </div>
          </div>
          {debits.length === 0 ? (
            <p className="empty">Nothing spent yet.</p>
          ) : (
            <div className="records">
              {debits.map((d) => (
                <article key={d.paired_receipt} className="record fade-up">
                  <div className="ts">
                    {formatTimestamp(d.at_ms)}
                    <em>spent</em>
                  </div>
                  <div className="body">
                    <strong>{d.agent.display}</strong>
                    <p>
                      Used {d.credits} {d.credits === 1 ? "credit" : "credits"} · receipt{" "}
                      {shortHash(d.paired_receipt, 14)}
                    </p>
                  </div>
                </article>
              ))}
            </div>
          )}
        </div>

        <div className="panel">
          <div className="panel-head">
            <div>
              <p className="eyebrow">charged</p>
              <h2>
                Settled charges <span className="count">{receipts.length}</span>
              </h2>
            </div>
          </div>
          {receipts.length === 0 ? (
            <p className="empty">No receipts yet.</p>
          ) : (
            <div className="records">
              {receipts.map((r) => (
                <article key={r.id} className="record fade-up">
                  <div className="ts">
                    {formatDateTime(r.settled_at)}
                    <em>{r.resource}</em>
                  </div>
                  <div className="body">
                    <strong>{r.payer.display}</strong>
                    <p>
                      {r.credits_consumed} {r.credits_consumed === 1 ? "credit" : "credits"} ·{" "}
                      {r.onchain_sig ? (
                        <>
                          on-chain{" "}
                          <a
                            href={solscanTxUrl(r.onchain_sig, CLUSTER)}
                            target="_blank"
                            rel="noopener noreferrer"
                            className="onchain-link"
                            title="Open this transaction on Solscan"
                          >
                            {shortHash(r.onchain_sig, 16)} ↗
                          </a>
                        </>
                      ) : (
                        "local only"
                      )}
                    </p>
                  </div>
                </article>
              ))}
            </div>
          )}
        </div>
      </section>
    </>
  );
}
