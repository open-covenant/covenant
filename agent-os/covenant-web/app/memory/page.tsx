"use client";

import { useCallback, useState, type FormEvent } from "react";
import { api, type Memory } from "@/lib/api";
import { truncate } from "@/lib/audit";
import { usePoll } from "@/lib/usePoll";
import { PageHeader } from "../components/PageHeader";

type Tier = "" | "working" | "episodic" | "longterm";

async function loadMemory(tier: Tier) {
  return api.recentMemory(40, tier || undefined);
}

export default function MemoryPage() {
  const [tier, setTier] = useState<Tier>("");
  const fetcher = useCallback(() => loadMemory(tier), [tier]);
  const { data, error, lastSyncMs } = usePoll(fetcher, 4000);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<Memory[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [errMsg, setErrMsg] = useState<string | null>(null);

  const onSearch = async (e: FormEvent) => {
    e.preventDefault();
    if (!query) return;
    setSearching(true);
    setErrMsg(null);
    try {
      const r = await api.searchMemory(query, 8);
      setHits(r.records);
    } catch (e) {
      setErrMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setSearching(false);
    }
  };

  const records = data?.records ?? [];
  const counts = {
    working: records.filter((r) => r.tier === "working").length,
    episodic: records.filter((r) => r.tier === "episodic").length,
    longterm: records.filter((r) => r.tier === "longterm").length,
  };

  return (
    <>
      <PageHeader
        eyebrow="agent state"
        title="Memory"
        subhead="Three tiers — working, episodic, and long-term. Agents read and write here under signed capabilities."
        syncMs={lastSyncMs}
        error={error}
      />

      <section className="metric-row">
        <article className="metric">
          <span className="eyebrow">working</span>
          <span className="value">{counts.working}</span>
          <span className="caption">active context</span>
        </article>
        <article className="metric">
          <span className="eyebrow">episodic</span>
          <span className="value">{counts.episodic}</span>
          <span className="caption">scoped to a task or session</span>
        </article>
        <article className="metric">
          <span className="eyebrow">longterm</span>
          <span className="value">{counts.longterm}</span>
          <span className="caption">persistent across runs</span>
        </article>
        <article className="metric">
          <span className="eyebrow">total</span>
          <span className="value">{records.length}</span>
          <span className="caption">showing {records.length} of {40}</span>
        </article>
      </section>

      <section className="search-card">
        <form onSubmit={onSearch}>
          <div className="row">
            <p className="eyebrow">semantic search</p>
            <select value={tier} onChange={(e) => setTier(e.target.value as Tier)}>
              <option value="">all tiers</option>
              <option value="working">working</option>
              <option value="episodic">episodic</option>
              <option value="longterm">longterm</option>
            </select>
          </div>
          <div className="row controls">
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search memory"
            />
            <button type="submit" className="btn primary" disabled={!query || searching}>
              {searching ? "searching" : "search"}
            </button>
          </div>
        </form>
      </section>

      {errMsg && <pre className="result compact error">{errMsg}</pre>}

      {hits != null && (
        <section className="panel">
          <div className="panel-head">
            <div>
              <p className="eyebrow">matches</p>
              <h2>
                Search results <span className="count">{hits.length}</span>
              </h2>
            </div>
            <button type="button" className="btn ghost" onClick={() => setHits(null)}>clear</button>
          </div>
          {hits.length === 0 ? (
            <p className="empty">No matches.</p>
          ) : (
            <div className="records">
              {hits.map((m) => (
                <article key={m.id} className="record">
                  <div className="ts">
                    <span>{m.tier}</span>
                  </div>
                  <div className="body">
                    <strong>{truncate(m.text, 80)}</strong>
                    <p>{truncate(m.text, 240)}</p>
                  </div>
                </article>
              ))}
            </div>
          )}
        </section>
      )}

      <section className="panel">
        <div className="panel-head">
          <div>
            <p className="eyebrow">recent</p>
            <h2>
              Records <span className="count">{records.length}</span>
            </h2>
          </div>
        </div>
        {records.length === 0 ? (
          <p className="empty">No records yet.</p>
        ) : (
          <div className="records">
            {records.map((m) => (
              <article key={m.id} className="record fade-up">
                <div className="ts">
                  <span>{m.tier}</span>
                </div>
                <div className="body">
                  <p>{truncate(m.text, 320)}</p>
                </div>
              </article>
            ))}
          </div>
        )}
      </section>

      <style jsx>{`
        .metric-row {
          margin-bottom: 22px;
        }

        .search-card {
          margin-bottom: 22px;
          padding: 22px;
          border: 1px solid var(--border);
          border-radius: 8px;
          background: var(--panel);
        }

        .search-card form {
          display: grid;
          gap: 12px;
        }

        .search-card .row {
          display: flex;
          align-items: center;
          justify-content: space-between;
          gap: 8px;
        }

        .search-card .row.controls {
          display: grid;
          grid-template-columns: 1fr auto;
        }
      `}</style>
    </>
  );
}
