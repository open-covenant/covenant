"use client";

import { api } from "@/lib/api";
import { dateTime, short, time } from "@/lib/audit";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "../components/PageHeader";

async function loadSettlement() {
  const [receipts, debits] = await Promise.all([
    api.recentReceipts(40),
    api.recentDebits(40),
  ]);
  return { receipts: receipts.receipts, debits: debits.debits };
}

export default function SettlementPage() {
  const { data, error, lastSyncMs } = usePoll(loadSettlement, 4000);
  const receipts = data?.receipts ?? [];
  const debits = data?.debits ?? [];
  const totalCredits = receipts.reduce((s, r) => s + r.credits_consumed, 0);
  const onChain = receipts.filter((r) => r.onchain_sig).length;

  return (
    <>
      <PageHeader
        eyebrow="resource accounting"
        title="Settlement"
        subhead="Receipts and debits from agent activity. On-chain settlement is in development; for now, everything stays local."
        syncMs={lastSyncMs}
        error={error}
      />

      <section className="metric-row">
        <article className="metric">
          <span className="eyebrow">receipts</span>
          <span className="value">{receipts.length}</span>
          <span className="caption">{totalCredits} credits consumed</span>
        </article>
        <article className="metric">
          <span className="eyebrow">debits</span>
          <span className="value">{debits.length}</span>
          <span className="caption">paired against receipts</span>
        </article>
        <article className="metric">
          <span className="eyebrow">on-chain</span>
          <span className="value">{onChain}</span>
          <span className="caption">{receipts.length - onChain} pending settlement</span>
        </article>
        <article className="metric">
          <span className="eyebrow">window</span>
          <span className="value small">40</span>
          <span className="caption">latest receipts</span>
        </article>
      </section>

      <section className="split-2">
        <div className="panel">
          <div className="panel-head">
            <div>
              <p className="eyebrow">debits</p>
              <h2>
                Budget consumption <span className="count">{debits.length}</span>
              </h2>
            </div>
          </div>
          {debits.length === 0 ? (
            <p className="empty">No debits yet.</p>
          ) : (
            <div className="records">
              {debits.map((d) => (
                <article key={d.paired_receipt} className="record fade-up">
                  <div className="ts">
                    {time(d.at_ms)}
                    <em>debit</em>
                  </div>
                  <div className="body">
                    <strong>{d.agent.display}</strong>
                    <p>
                      {d.credits} credit(s) · receipt {short(d.paired_receipt, 14)}
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
              <p className="eyebrow">receipts</p>
              <h2>
                Settlement receipts <span className="count">{receipts.length}</span>
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
                    {dateTime(r.settled_at)}
                    <em>{r.resource}</em>
                  </div>
                  <div className="body">
                    <strong>{r.payer.display}</strong>
                    <p>
                      {r.credits_consumed} credit(s) ·{" "}
                      {r.onchain_sig ? `on-chain ${short(r.onchain_sig, 16)}` : "local-only"}
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
