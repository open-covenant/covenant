"use client";

import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";

type Line = { k: string; t: string };
type AgentEvent =
  | { type: "transition"; taskId: string; from: string; to: string; actor: string; note: string }
  | { type: "commit"; hash: string; subject: string; stat: string };

const COLOR: Record<string, string> = {
  cmd: "#cacaca",
  write: "#9a9a9a",
  diff: "#6b6b6b",
  meta: "#585858",
  hunk: "#7c8a99",
  add: "#7f9f78",
  del: "#a8807f",
  ctx: "#707070",
  commit: "#b6b6b6",
  blank: "transparent",
};

const TICK_MS = 28;
const CHARS_PER_TICK = 6;
const BUFFER_MAX = 110;
const FALLBACK_MS = 4000;
const BEAT_TICKS: Record<string, number> = { cmd: 14, commit: 18, write: 3 };

const BOOT: Line[] = [{ k: "cmd", t: "$ tail -f agent-os/autonomy/events.jsonl" }];

function stateKind(to: string): string {
  if (to === "integrated") return "add";
  if (to === "ready") return "write";
  if (to === "blocked" || to === "repair") return "del";
  if (to === "validation" || to === "cross_review" || to === "self_review") return "hunk";
  return "meta";
}

function formatEvent(e: AgentEvent): Line[] {
  if (e.type === "commit") {
    const out: Line[] = [{ k: "commit", t: `commited ${e.hash}  # ${e.subject}` }];
    if (e.stat) out.push({ k: "meta", t: `  ${e.stat}` });
    out.push({ k: "blank", t: "" });
    return out;
  }
  if (e.type === "transition") {
    const out: Line[] = [{ k: stateKind(e.to), t: `[${e.to}] ${e.taskId}` }];
    if (e.note) out.push({ k: "ctx", t: `    ${e.note}` });
    out.push({ k: "blank", t: "" });
    return out;
  }
  return [];
}

function Row({ line }: { line: Line }) {
  return (
    <div className="whitespace-pre-wrap break-words" style={{ color: COLOR[line.k] ?? "#9a9a9a" }}>
      {line.t.length ? line.t : " "}
    </div>
  );
}

export function AgentTerminal({
  className,
  onClose,
}: {
  className?: string;
  onClose?: () => void;
}) {
  const [lines, setLines] = useState<Line[]>(BOOT);
  const [partial, setPartial] = useState<Line | null>(null);
  const [connected, setConnected] = useState(false);
  const src = useRef<Line[]>([]);
  const li = useRef(0);
  const ci = useRef(0);
  const beat = useRef(0);
  const live = useRef(false);
  const bodyRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
    let alive = true;
    let timer: ReturnType<typeof setInterval> | null = null;
    let fallback: ReturnType<typeof setTimeout> | null = null;
    let es: EventSource | null = null;
    let gotData = false;

    const commit = (line: Line) =>
      setLines((prev) => {
        const trimmed =
          prev.length >= BUFFER_MAX ? prev.slice(prev.length - BUFFER_MAX + 1) : prev;
        return [...trimmed, line];
      });

    const tick = () => {
      const s = src.current;
      if (!s.length) return;
      if (li.current >= s.length) {
        if (live.current) return; // idle: wait for the next live event
        li.current = 0;
        ci.current = 0;
      }
      if (beat.current > 0) {
        beat.current -= 1;
        return;
      }
      const line = s[li.current];
      const done = () => {
        ci.current = 0;
        li.current += 1;
      };
      if (!line.t.length) {
        commit(line);
        setPartial(null);
        done();
        return;
      }
      const step = reduced ? line.t.length : CHARS_PER_TICK;
      ci.current = Math.min(line.t.length, ci.current + step);
      setPartial({ k: line.k, t: line.t.slice(0, ci.current) });
      if (ci.current >= line.t.length) {
        commit(line);
        setPartial(null);
        beat.current = reduced ? 0 : BEAT_TICKS[line.k] ?? 0;
        done();
      }
    };

    const ensureTimer = () => {
      if (!timer && !reduced) timer = setInterval(tick, TICK_MS);
    };

    const startReplay = () => {
      fetch("/agent-stream", { cache: "no-store" })
        .then((r) => r.json())
        .then((data: { lines: Line[] }) => {
          if (!alive || !data?.lines?.length) return;
          live.current = false;
          src.current = data.lines;
          if (reduced) setLines(data.lines.slice(0, BUFFER_MAX));
          else ensureTimer();
        })
        .catch(() => {});
    };

    try {
      es = new EventSource("/agent-stream/live");
      es.onopen = () => {
        if (alive) setConnected(true);
      };
      es.onmessage = (m) => {
        if (!alive) return;
        let e: AgentEvent;
        try {
          e = JSON.parse(m.data);
        } catch {
          return;
        }
        const next = formatEvent(e);
        if (!next.length) return;
        gotData = true;
        if (fallback) {
          clearTimeout(fallback);
          fallback = null;
        }
        live.current = true;
        src.current = [...src.current, ...next];
        if (reduced) next.forEach(commit);
        else ensureTimer();
      };
      es.onerror = () => {
        if (alive) setConnected(false);
        if (!gotData && alive) {
          es?.close();
          es = null;
          startReplay();
        }
      };
      fallback = setTimeout(() => {
        if (!gotData && alive) {
          es?.close();
          es = null;
          startReplay();
        }
      }, FALLBACK_MS);
    } catch {
      startReplay();
    }

    return () => {
      alive = false;
      if (timer) clearInterval(timer);
      if (fallback) clearTimeout(fallback);
      es?.close();
    };
  }, []);

  // Keep the newest line in view. While content is shorter than the panel,
  // scrollTop clamps to 0, so the stream fills from the top; once it overflows
  // it tails the bottom and older lines scroll off the top.
  useEffect(() => {
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  });

  return (
    <div
      aria-hidden="true"
      className={[
        "flex flex-col overflow-hidden bg-[#050505]/85 font-mono text-[11px] leading-[1.55] backdrop-blur-md",
        className ?? "",
      ]
        .filter(Boolean)
        .join(" ")}
    >
      <div className="flex items-center gap-2 border-b border-neutral-800/70 px-3 py-2">
        <span className="text-[10px] uppercase tracking-[0.22em] text-neutral-500">
          Covenant build
        </span>
        <span className="ml-auto flex items-center gap-1.5">
          <span
            className={`h-1 w-1 rounded-full ${connected ? "animate-pulse bg-[#7f9f78]" : "bg-neutral-600"}`}
          />
          <span className="text-[10px] uppercase tracking-[0.22em] text-neutral-600">
            {connected ? "live" : "replay"}
          </span>
        </span>
        {onClose && (
          <button
            type="button"
            onClick={onClose}
            aria-label="Hide agent log"
            className="-mr-1 ml-1 rounded p-1 text-neutral-500 transition-colors hover:text-neutral-200 xl:hidden"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        )}
      </div>
      <div ref={bodyRef} className="min-w-0 flex-1 overflow-hidden px-3 py-2">
        {lines.map((line, i) => (
          <Row key={i} line={line} />
        ))}
        <div
          className="whitespace-pre-wrap break-words"
          style={{ color: COLOR[partial?.k ?? "cmd"] ?? "#cacaca" }}
        >
          {partial?.t ?? ""}
          <span className="term-cursor">▍</span>
        </div>
      </div>
    </div>
  );
}
