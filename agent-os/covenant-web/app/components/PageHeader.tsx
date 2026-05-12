"use client";

import { type ReactNode } from "react";
import { relTime } from "@/lib/audit";

export function PageHeader({
  eyebrow,
  title,
  subhead,
  right,
  syncMs,
  error,
}: {
  eyebrow: string;
  title: string;
  subhead?: string;
  right?: ReactNode;
  syncMs?: number | null;
  error?: string | null;
}) {
  return (
    <header className="page-head">
      <div className="left">
        <p className="eyebrow">{eyebrow}</p>
        <h1>{title}</h1>
        {subhead && <p className="subhead">{subhead}</p>}
      </div>
      <div className="right">
        {syncMs != null || error ? (
          <span className={`sync-pill ${error ? "error" : "ok"}`}>
            <span className="dot" />
            {error ? "disconnected" : `synced ${relTime(syncMs ?? Date.now())}`}
          </span>
        ) : null}
        {right}
      </div>
    </header>
  );
}
