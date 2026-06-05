"use client";

import { useEffect, useState } from "react";

// Live Synapse-bridge RPC usage, read from the daemon's /sap/stats route
// (which reads the worker's counter file) via the operator proxy. The
// page itself is env-only and server-rendered; this card is the one
// piece that reflects on-chain activity, so it is a client component that
// polls. Degrades gracefully to em-dashes when the daemon is unreachable
// or no call has been recorded yet.
interface SapStats {
  calls: number;
  successes: number;
  failures: number;
  successRate: number;
  firstSeenUnix: number;
  lastCallUnix: number;
  uptimeSeconds: number;
}

function formatUptime(seconds: number): string {
  if (seconds <= 0) return "—";
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

export function SapStatsCard() {
  const [stats, setStats] = useState<SapStats | null>(null);
  const [error, setError] = useState(false);

  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const res = await fetch("/api/covenant/sap/stats", { cache: "no-store" });
        if (!res.ok) throw new Error(String(res.status));
        const data = (await res.json()) as SapStats;
        if (alive) {
          setStats(data);
          setError(false);
        }
      } catch {
        if (alive) setError(true);
      }
    };
    void load();
    const id = setInterval(() => void load(), 60_000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, []);

  const calls = stats?.calls ?? 0;
  const rate =
    stats && stats.calls > 0 ? `${Math.round(stats.successRate * 100)}%` : "—";
  const uptime = stats ? formatUptime(stats.uptimeSeconds) : "—";
  const callsCaption = error
    ? "daemon unreachable"
    : stats === null
      ? "loading…"
      : calls === 0
        ? "no calls recorded yet"
        : `${stats.successes.toLocaleString()} of ${stats.calls.toLocaleString()} ok`;

  return (
    <section className="metric-row">
      <article className="metric">
        <span className="eyebrow">uptime</span>
        <span className="value small">{uptime}</span>
        <span className="caption">since first RPC call</span>
      </article>
      <article className="metric">
        <span className="eyebrow">rpc calls</span>
        <span className="value">{stats ? calls.toLocaleString() : "—"}</span>
        <span className="caption">{callsCaption}</span>
      </article>
      <article className="metric">
        <span className="eyebrow">success rate</span>
        <span className="value small">{rate}</span>
        <span className="caption">confirmed / attempted</span>
      </article>
    </section>
  );
}
