"use client";

import { useEffect, useState } from "react";

type Stats = {
  commits: number | null;
  head: string | null;
  alphaSince: string;
  tests: string | null;
  live: string | null;
  crates: string | null;
};

function uptime(sinceISO: string): string {
  const ms = Date.now() - new Date(sinceISO).getTime();
  if (!Number.isFinite(ms) || ms < 0) return "—";
  const d = Math.floor(ms / 86_400_000);
  const h = Math.floor(ms / 3_600_000) % 24;
  const m = Math.floor(ms / 60_000) % 60;
  return `${d}d ${h}h ${m}m`;
}

function Cell({
  label,
  value,
  accent,
  href,
}: {
  label: string;
  value: string;
  accent?: boolean;
  href?: string;
}) {
  return (
    <div className="flex flex-col items-end leading-none">
      <span className="text-[9px] uppercase tracking-[0.18em] text-neutral-400">{label}</span>
      {href ? (
        <a
          href={href}
          target="_blank"
          rel="noopener noreferrer"
          className="mt-1 text-[12px] tabular-nums text-[#7f9f78] no-underline hover:text-white hover:no-underline"
        >
          {value}
        </a>
      ) : (
        <span
          className="mt-1 text-[12px] tabular-nums"
          style={{ color: accent ? "#7f9f78" : "#e0e0e0" }}
        >
          {value}
        </span>
      )}
    </div>
  );
}

export function HeaderStats() {
  const [s, setS] = useState<Stats | null>(null);
  const [, tick] = useState(0);

  useEffect(() => {
    let alive = true;
    const load = () =>
      fetch("/api/stats")
        .then((r) => r.json())
        .then((d: Stats) => {
          if (alive) setS(d);
        })
        .catch(() => {});
    load();
    const poll = setInterval(load, 60_000);
    const clock = setInterval(() => {
      if (alive) tick((n) => n + 1);
    }, 1000);
    return () => {
      alive = false;
      clearInterval(poll);
      clearInterval(clock);
    };
  }, []);

  if (!s) return null;
  const num = (n: number | null) => (n == null ? "—" : n.toLocaleString("en-US"));

  return (
    <div className="flex items-center gap-4">
      <Cell label="commits" value={num(s.commits)} />
      {s.tests && <Cell label="tests" value={s.tests} />}
      {s.live && <Cell label="live" value={s.live} />}
      {s.crates && <Cell label="crates" value={s.crates} />}
      <Cell label="up" value={uptime(s.alphaSince)} />
      <Cell
        label="committed"
        value={s.head ?? "—"}
        href={s.head ? `https://github.com/open-covenant/covenant/commit/${s.head}` : undefined}
      />
    </div>
  );
}
