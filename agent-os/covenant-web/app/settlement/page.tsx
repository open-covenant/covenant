"use client";

import { api } from "@/lib/api";
import { solscanTxUrl } from "@/lib/explorer";
import { formatDateTime, formatTimestamp, shortHash } from "@/lib/format";
import {
  shortSig,
  solscanFor,
  useSettlementSigs,
} from "@/lib/settlementSigs";
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

export default function SettlementPage() {
  const { data, error, lastSyncMs } = usePoll(loadSettlement, 4000);
  const settlement = useSettlementSigs();
  const receipts = data?.receipts ?? [];
  const debits = data?.debits ?? [];
  const totalCredits = receipts.reduce((s, r) => s + r.credits_consumed, 0);
  const onChainReceipts = receipts.filter((r) => r.onchain_sig).length;

  // The publisher's sig table is the authoritative list of on-chain
  // anchors the sandbox has produced. Materialise it sorted newest
  // first so the page reads like a feed of recent verifiable activity.
  const anchors = Object.values(settlement.all)
    .slice()
    .sort((a, b) => b.settled_at_ms - a.settled_at_ms);

  return (
    <>
      <PageHeader
        eyebrow="credits consumed · anchored on Solana"
        title="Settlement"
        subhead={
          DEMO_MODE
            ? "Every task on this sandbox is settled on-chain on Solana devnet. Each row below is a real transaction you can verify in Solscan. Shared tally; resets periodically."
            : "Every task is settled on-chain on the configured Solana cluster. Each row below is a real transaction the publisher sidecar signed for a dispatched intent."
        }
        syncMs={lastSyncMs}
        error={error}
      />

      <section className="metric-row">
        <article className="metric">
          <span className="eyebrow">on-chain settlements</span>
          <span className="value">{anchors.length}</span>
          <span className="caption">{settlement.cluster}</span>
        </article>
        <article className="metric">
          <span className="eyebrow">local receipts</span>
          <span className="value">{receipts.length}</span>
          <span className="caption">{totalCredits} credits</span>
        </article>
        <article className="metric">
          <span className="eyebrow">debits</span>
          <span className="value">{debits.length}</span>
          <span className="caption">matched to charges</span>
        </article>
        <article className="metric">
          <span className="eyebrow">also-on-chain</span>
          <span className="value">{onChainReceipts}</span>
          <span className="caption">receipts with tx sig</span>
        </article>
      </section>

      <section className="panel flush">
        <div className="panel-head" style={{ padding: "22px 22px 0" }}>
          <div>
            <p className="eyebrow">verifiable on Solana</p>
            <h2>
              On-chain settlements <span className="count">{anchors.length}</span>
            </h2>
          </div>
          <span className="text-muted text-mono kbd-hint">
            published by the settlement sidecar
          </span>
        </div>
        {anchors.length === 0 ? (
          <p className="empty" style={{ padding: "30px 22px" }}>
            No settlements anchored yet. They land here once a task runs and the
            publisher posts the corresponding `consume_credits` transaction on
            devnet.
          </p>
        ) : (
          <div className="records" style={{ padding: "12px 0 0" }}>
            {anchors.map((a) => (
              <article key={a.tx_sig} className="record fade-up">
                <div className="ts">
                  {formatTimestamp(a.settled_at_ms)}
                  <em>settled</em>
                </div>
                <div className="body">
                  <strong>{a.intent_text ?? "(no text)"}</strong>
                  <p>
                    intent {shortHash(a.intent_id, 12)} ·{" "}
                    <a
                      className="onchain-link"
                      href={solscanFor(a)}
                      target="_blank"
                      rel="noopener noreferrer"
                      title="Open this transaction on Solscan"
                    >
                      {shortSig(a.tx_sig, 8)} ↗
                    </a>
                    {a.slot != null && (
                      <span className="text-muted text-mono"> · slot {a.slot}</span>
                    )}
                  </p>
                </div>
              </article>
            ))}
          </div>
        )}
      </section>

      <section className="split-2" style={{ marginTop: 22 }}>
        <div className="panel">
          <div className="panel-head">
            <div>
              <p className="eyebrow">local · operator-only</p>
              <h2>
                Daemon receipts <span className="count">{receipts.length}</span>
              </h2>
            </div>
          </div>
          {receipts.length === 0 ? (
            <p className="empty">
              The daemon&apos;s local receipt store is empty. Sandbox coding tasks
              are settled on-chain directly; daemon-issued receipts only land here
              if you wire a local payer.
            </p>
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

        <div className="panel">
          <div className="panel-head">
            <div>
              <p className="eyebrow">local · operator-only</p>
              <h2>
                Daemon debits <span className="count">{debits.length}</span>
              </h2>
            </div>
          </div>
          {debits.length === 0 ? (
            <p className="empty">
              Nothing here. The on-chain settlement above is the path the public
              sandbox uses.
            </p>
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
      </section>
    </>
  );
}
