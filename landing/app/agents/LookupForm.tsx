"use client";

import { useRouter } from "next/navigation";
import { useState } from "react";

export function LookupForm() {
  const router = useRouter();
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);

  return (
    <form
      className="flex flex-col gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        const v = value.trim();
        if (!/^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(v)) {
          setError("that does not look like a Solana address");
          return;
        }
        setError(null);
        router.push(`/agents/${v}`);
      }}
    >
      <div className="flex gap-2">
        <input
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="paste any 014 Registry asset address"
          spellCheck={false}
          className="w-full border border-line bg-raised/60 px-3 py-2.5 font-mono text-[13px] text-fg-2 placeholder:text-faint focus:border-faint focus:outline-none"
        />
        <button
          type="submit"
          className="shrink-0 border border-line-strong px-4 py-2.5 text-[11px] uppercase tracking-[2px] text-fg-soft transition-colors hover:border-muted hover:text-fg"
        >
          Verify
        </button>
      </div>
      {error && <p className="text-[12px] text-bad">{error}</p>}
    </form>
  );
}
