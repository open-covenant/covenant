"use client";

import { useState } from "react";
import { Check, Copy } from "lucide-react";

export function ColorSwatch({
  name,
  hex,
  role,
  border = false,
}: {
  name: string;
  hex: string;
  role: string;
  border?: boolean;
}) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(hex);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {}
  };

  return (
    <button
      type="button"
      onClick={copy}
      aria-label={`Copy ${name} ${hex}`}
      title={copied ? "Copied" : "Copy hex"}
      className="group flex items-center gap-3 rounded-lg border border-neutral-800/80 bg-[#0a0a0a]/60 p-2.5 text-left transition-colors hover:border-neutral-700"
    >
      <span
        aria-hidden
        className={`h-10 w-10 shrink-0 rounded-md ${border ? "border border-neutral-700/70" : ""}`}
        style={{ backgroundColor: hex }}
      />
      <span className="flex min-w-0 flex-col">
        <span className="truncate text-[12px] font-light tracking-wide text-neutral-100">{name}</span>
        <span className="truncate text-[10px] uppercase tracking-[0.2em] text-neutral-500">{role}</span>
      </span>
      <span className="ml-auto flex shrink-0 items-center gap-1.5 pl-2 pr-1 font-mono text-[11px] text-neutral-400">
        {hex.toUpperCase()}
        {copied ? (
          <Check className="h-3.5 w-3.5 text-[#7f9f78]" />
        ) : (
          <Copy className="h-3.5 w-3.5 text-neutral-500 transition-colors group-hover:text-neutral-300" />
        )}
      </span>
    </button>
  );
}
