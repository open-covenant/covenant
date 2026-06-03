"use client";

import { useState } from "react";
import { Check, Copy } from "lucide-react";

/** Monospace contract address with a click-to-copy button and brief feedback. */
export function CopyAddress({ address, label }: { address: string; label?: string }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(address);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {}
  };

  return (
    <button
      type="button"
      onClick={copy}
      aria-label={label ?? "Copy contract address"}
      title={copied ? "Copied" : "Copy"}
      className="group inline-flex max-w-full items-center gap-2 rounded-md border border-neutral-800/80 bg-[#0a0a0a]/60 px-3 py-2 font-mono text-[12px] text-neutral-300 transition-colors hover:border-neutral-700 hover:text-neutral-100"
    >
      <span className="truncate">{address}</span>
      {copied ? (
        <Check className="h-3.5 w-3.5 shrink-0 text-[#7f9f78]" />
      ) : (
        <Copy className="h-3.5 w-3.5 shrink-0 text-neutral-400 transition-colors group-hover:text-neutral-300" />
      )}
    </button>
  );
}
