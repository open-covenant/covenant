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
          className="w-full border border-neutral-800 bg-neutral-950/60 px-3 py-2.5 font-mono text-[13px] text-neutral-200 placeholder:text-neutral-600 focus:border-neutral-600 focus:outline-none"
        />
        <button
          type="submit"
          className="shrink-0 border border-neutral-700 px-4 py-2.5 text-[11px] uppercase tracking-[2px] text-neutral-300 transition-colors hover:border-neutral-400 hover:text-white"
        >
          Verify
        </button>
      </div>
      {error && <p className="text-[12px] font-light text-rose-300">{error}</p>}
    </form>
  );
}
