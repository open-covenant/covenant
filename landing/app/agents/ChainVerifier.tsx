"use client";

// In-browser witness-chain verification. Fetches the published audit
// event log + hash-chain sidecar from /audit/, recomputes every link with
// WebCrypto (event_hash = SHA-256 of the exact event line, chain_hash =
// SHA-256 of previous + "\n" + event_hash), and reports whether the
// on-chain attested root reproduces from public data. The server is never
// asked for a verdict — the proof runs on the reader's machine.

import { useCallback, useEffect, useRef, useState } from "react";

type Phase = "idle" | "fetching" | "computing" | "matched" | "absent" | "broken" | "error";

type Progress = {
  index: number;
  total: number;
  chainHash: string;
};

async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

export function ChainVerifier({ rootHex }: { rootHex: string }) {
  const [phase, setPhase] = useState<Phase>("idle");
  const [progress, setProgress] = useState<Progress | null>(null);
  const [matchIndex, setMatchIndex] = useState<number | null>(null);
  const [detail, setDetail] = useState<string>("");
  const runId = useRef(0);

  const verify = useCallback(async () => {
    const id = ++runId.current;
    setPhase("fetching");
    setProgress(null);
    setMatchIndex(null);
    setDetail("");

    let events: string[];
    let sidecar: Array<{ event_hash_hex: string; chain_hash_hex: string }>;
    try {
      const [eventsRes, chainRes] = await Promise.all([
        fetch("/audit/events.jsonl", { cache: "no-store" }),
        fetch("/audit/events.chain.jsonl", { cache: "no-store" }),
      ]);
      if (!eventsRes.ok || !chainRes.ok) throw new Error("audit files unavailable");
      events = (await eventsRes.text()).split("\n").filter(Boolean);
      sidecar = (await chainRes.text())
        .split("\n")
        .filter(Boolean)
        .map((l) => JSON.parse(l) as { event_hash_hex: string; chain_hash_hex: string });
    } catch {
      if (runId.current === id) {
        setPhase("error");
        setDetail("could not fetch the published audit chain");
      }
      return;
    }
    if (runId.current !== id) return;

    setPhase("computing");
    const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const total = Math.min(events.length, sidecar.length);
    let prev = "0".repeat(64);
    let found: number | null = null;

    for (let i = 0; i < total; i++) {
      const eventHash = await sha256Hex(events[i]);
      if (eventHash !== sidecar[i].event_hash_hex) {
        if (runId.current === id) {
          setPhase("broken");
          setDetail(`event ${i} does not hash to its sidecar entry — published chain is inconsistent`);
        }
        return;
      }
      const chainHash = await sha256Hex(`${prev}\n${eventHash}`);
      if (chainHash !== sidecar[i].chain_hash_hex) {
        if (runId.current === id) {
          setPhase("broken");
          setDetail(`chain link ${i} does not recompute — published chain is inconsistent`);
        }
        return;
      }
      prev = chainHash;
      if (chainHash === rootHex && found === null) found = i;

      if (runId.current !== id) return;
      setProgress({ index: i + 1, total, chainHash });
      if (!reduceMotion) {
        // Let the reader watch each link close. Cap the ceremony so long
        // chains still finish promptly.
        await new Promise((r) => setTimeout(r, Math.min(220, 1800 / total)));
      }
    }

    if (runId.current !== id) return;
    if (found !== null) {
      setMatchIndex(found);
      setPhase("matched");
      setDetail(
        `the attested root is link ${found + 1} of ${total} in the published witness chain`,
      );
    } else {
      setPhase("absent");
      setDetail(
        "every published link recomputes, but none equals this root — it may predate or postdate the published window, or come from a different Covenant home",
      );
    }
  }, [rootHex]);

  useEffect(() => {
    void verify();
  }, [verify]);

  const dot =
    phase === "matched"
      ? "bg-emerald-400"
      : phase === "broken" || phase === "error"
        ? "bg-rose-400"
        : phase === "absent"
          ? "bg-amber-400"
          : "bg-neutral-600 animate-pulse";

  return (
    <div
      className={`flex flex-col gap-3 border p-5 transition-colors duration-700 ${
        phase === "matched"
          ? "border-emerald-900/70 bg-emerald-950/20"
          : "border-neutral-800 bg-neutral-950/60"
      }`}
    >
      <div className="flex items-center justify-between gap-2.5">
        <div className="flex items-center gap-2.5">
          <span aria-hidden="true" className={`inline-block h-2.5 w-2.5 rounded-full ${dot} shrink-0`} />
          <span className="text-[11px] font-light uppercase tracking-[2px] text-neutral-300">
            Witness chain · recomputed in your browser
          </span>
        </div>
        <button
          type="button"
          onClick={() => void verify()}
          className="text-[10px] uppercase tracking-[1.5px] text-neutral-500 hover:text-neutral-300"
        >
          re-verify
        </button>
      </div>

      <div className="break-all font-mono text-[12px] text-neutral-400">
        root <span className={phase === "matched" ? "text-emerald-300" : "text-neutral-300"}>{rootHex}</span>
      </div>

      {phase === "fetching" && (
        <p className="text-[13px] font-light text-neutral-500">
          fetching the published audit chain from /audit/…
        </p>
      )}

      {(phase === "computing" || phase === "matched" || phase === "absent") && progress && (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-center justify-between text-[11px] uppercase tracking-[1.5px] text-neutral-500">
            <span>
              link {progress.index} / {progress.total}
            </span>
            <span className="font-mono normal-case tracking-normal text-neutral-600">
              {progress.chainHash.slice(0, 16)}…
            </span>
          </div>
          <div className="h-px w-full bg-neutral-800">
            <div
              className={`h-px transition-all duration-200 ${
                phase === "matched" ? "bg-emerald-400" : "bg-neutral-400"
              }`}
              style={{ width: `${(progress.index / progress.total) * 100}%` }}
            />
          </div>
        </div>
      )}

      {detail && (
        <p
          className={`text-[13px] font-light leading-relaxed ${
            phase === "matched"
              ? "text-emerald-300"
              : phase === "broken" || phase === "error"
                ? "text-rose-300"
                : "text-neutral-400"
          }`}
        >
          {phase === "matched" && matchIndex !== null ? "Root reproduces. " : null}
          {detail}
        </p>
      )}

      <p className="text-[11px] font-light leading-relaxed text-neutral-600">
        SHA-256 over the published event log, link by link — no server verdict involved.{" "}
        <a
          href="/audit/events.chain.jsonl"
          target="_blank"
          rel="noopener noreferrer"
          className="underline-offset-4 hover:text-neutral-400 hover:underline"
        >
          inspect the chain →
        </a>
      </p>
    </div>
  );
}
