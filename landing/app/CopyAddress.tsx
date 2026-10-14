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
      className="group inline-flex max-w-full items-center gap-2 rounded-md border border-line bg-raised px-3 py-2 font-mono text-[12px] text-fg-soft transition-colors hover:border-line-strong hover:text-fg"
    >
      <span className="truncate">{address}</span>
      {copied ? (
        <Check className="h-3.5 w-3.5 shrink-0 text-ok" />
      ) : (
        <Copy className="h-3.5 w-3.5 shrink-0 text-muted-2 transition-colors group-hover:text-fg" />
      )}
    </button>
  );
}
