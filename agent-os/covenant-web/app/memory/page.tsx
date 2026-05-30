"use client";

import { useCallback, useState, type FormEvent } from "react";
import { api, type Memory } from "@/lib/api";
import { truncate } from "@/lib/format";
import { memoryTierLabel } from "@/lib/labels";
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
        eyebrow="what your agents remember"
        title="Memory"
        subhead="Three kinds — what's currently top of mind, this session's notes, and the long-term store. Agents read and write here when you give them permission."
        syncMs={lastSyncMs}
        error={error}
      />

      <section className="metric-row">
        <article className="metric">
          <span className="eyebrow">{memoryTierLabel("working").toLowerCase()}</span>
          <span className="value">{counts.working}</span>
          <span className="caption">currently on the agent&apos;s mind</span>
        </article>
        <article className="metric">
          <span className="eyebrow">{memoryTierLabel("episodic").toLowerCase()}</span>
          <span className="value">{counts.episodic}</span>
          <span className="caption">tied to a task or chat</span>
        </article>
        <article className="metric">
          <span className="eyebrow">{memoryTierLabel("longterm").toLowerCase()}</span>
          <span className="value">{counts.longterm}</span>
          <span className="caption">kept across sessions</span>
        </article>
        <article className="metric">
          <span className="eyebrow">total</span>
          <span className="value">{records.length}</span>
          <span className="caption">showing {records.length} most recent</span>
        </article>
      </section>

      <section className="search-card">
        <form onSubmit={onSearch}>
          <div className="row">
            <p className="eyebrow">search memory</p>
            <select value={tier} onChange={(e) => setTier(e.target.value as Tier)}>
              <option value="">all</option>
              <option value="working">{memoryTierLabel("working")}</option>
              <option value="episodic">{memoryTierLabel("episodic")}</option>
              <option value="longterm">{memoryTierLabel("longterm")}</option>
            </select>
          </div>
          <div className="row controls">
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="What are you looking for?"
            />
            <button type="submit" className="btn primary" disabled={!query || searching}>
              {searching ? "Searching" : "Search"}
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
            <button type="button" className="btn ghost" onClick={() => setHits(null)}>
              clear
            </button>
          </div>
          {hits.length === 0 ? (
            <p className="empty">Nothing matched your search.</p>
          ) : (
            <div className="records">
              {hits.map((m) => (
                <article key={m.id} className="record">
                  <div className="ts">
                    <span>{memoryTierLabel(m.tier)}</span>
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
            <p className="eyebrow">recently saved</p>
            <h2>
              All memories <span className="count">{records.length}</span>
            </h2>
          </div>
        </div>
        {records.length === 0 ? (
          <p className="empty">Nothing saved yet. Memories appear here as your agents make notes.</p>
        ) : (
          <div className="records">
            {records.map((m) => (
              <article key={m.id} className="record fade-up">
                <div className="ts">
                  <span>{memoryTierLabel(m.tier)}</span>
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
          grid-template-columns: minmax(0, 1fr) auto;
        }
      `}</style>
    </>
  );
}
