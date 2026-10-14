"use client";

import { useState } from "react";
import { Check, Copy } from "lucide-react";

/**
 * Copies `value` to the clipboard. With children it is an inline control (a
 * colour value, a snippet); without, a small icon button for a panel corner.
 */
export function CopyButton({
  value,
  label,
  className = "",
  children,
}: {
  value: string;
  label: string;
  className?: string;
  children?: React.ReactNode;
}) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch {}
  };

  if (children) {
    return (
      <button
        type="button"
        onClick={copy}
        aria-label={label}
        title={copied ? "Copied" : label}
        className={`group inline-flex items-center gap-2 text-left transition-colors ${className}`}
      >
        {children}
        {copied ? (
          <Check aria-hidden className="h-3 w-3 shrink-0 text-ok" />
        ) : (
          <Copy aria-hidden className="h-3 w-3 shrink-0 opacity-0 transition-opacity group-hover:opacity-70 group-focus-visible:opacity-70" />
        )}
      </button>
    );
  }

  return (
    <button
      type="button"
      onClick={copy}
      aria-label={label}
      title={copied ? "Copied" : label}
      className={`inline-flex h-8 w-8 items-center justify-center rounded-md border border-line bg-ground/80 text-muted transition-colors hover:border-line-strong hover:text-fg ${className}`}
    >
      {copied ? <Check aria-hidden className="h-3.5 w-3.5 text-ok" /> : <Copy aria-hidden className="h-3.5 w-3.5" />}
    </button>
  );
}

/** Copies the text of a published file (an SVG) without inlining it in the page. */
export function CopyFileButton({ src, label }: { src: string; label: string }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      // A promised ClipboardItem keeps the user's click as the activation
      // while the file loads (Safari refuses a write after an await).
      const text = fetch(src).then((r) => r.text());
      if (typeof ClipboardItem !== "undefined" && navigator.clipboard.write) {
        await navigator.clipboard.write([
          new ClipboardItem({ "text/plain": text.then((t) => new Blob([t], { type: "text/plain" })) }),
        ]);
      } else {
        await navigator.clipboard.writeText(await text);
      }
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch {}
  };

  return (
    <button
      type="button"
      onClick={copy}
      aria-label={label}
      title={copied ? "Copied" : label}
      className="inline-flex h-8 items-center gap-1.5 rounded-md border border-line bg-ground/80 px-2.5 font-mono text-[9.5px] uppercase tracking-[0.16em] text-muted transition-colors hover:border-line-strong hover:text-fg"
    >
      {copied ? <Check aria-hidden className="h-3.5 w-3.5 text-ok" /> : <Copy aria-hidden className="h-3.5 w-3.5" />}
      SVG
    </button>
  );
}
