"use client";

import { useEffect, useState } from "react";

const RESET_AT = process.env.NEXT_PUBLIC_DEMO_RESET_AT_UTC ?? "";
const RESET_INTERVAL_HOURS = Number(process.env.NEXT_PUBLIC_DEMO_RESET_INTERVAL_HOURS ?? "");

function nextResetMs(now: number): number | null {
  if (RESET_AT) {
    const parsed = Date.parse(RESET_AT);
    if (!Number.isNaN(parsed)) return parsed;
  }
  if (Number.isFinite(RESET_INTERVAL_HOURS) && RESET_INTERVAL_HOURS > 0) {
    const intervalMs = RESET_INTERVAL_HOURS * 3_600_000;
    return Math.ceil(now / intervalMs) * intervalMs;
  }
  return null;
}

function formatCountdown(ms: number): string {
  if (ms <= 0) return "resetting now";
  const totalMinutes = Math.floor(ms / 60_000);
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m`;
  return "<1m";
}

function ResetCountdown() {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(id);
  }, []);

  const target = nextResetMs(now);
  if (target === null) return <>Shared state, resets periodically.</>;
  return <>Shared state, resets in {formatCountdown(target - now)}.</>;
}

export function DemoBanner() {
  if (process.env.NEXT_PUBLIC_DEMO_MODE !== "1") return null;
  return (
    <div className="demo-banner" role="status">
      <span>
        <strong>Public sandbox.</strong> <ResetCountdown /> Destructive actions are disabled.
      </span>
      <a
        className="demo-banner-link"
        href="https://github.com/open-covenant/covenant"
        target="_blank"
        rel="noreferrer"
      >
        Run a real instance →
      </a>
    </div>
  );
}
