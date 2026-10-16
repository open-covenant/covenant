"use client";

import { useState } from "react";
import { StateChip, StateMark } from "./_marks";
import { STATES, TRACKS, countByState, itemId, orderedItems, tally, type State } from "./_roadmap";

type Hit = { code: string; n: number; name: string; state: State };

// The whole roadmap at a glance: the four states with their counts, then one
// row per track with a module for each item, in its state's glyph. Pointing at
// a module reads its item out along the bottom edge; selecting it jumps to the
// item. The modules grow into place, row by row, as the map comes into view.
export function RoadmapMap() {
  const [hit, setHit] = useState<Hit | null>(null);
  const counts = countByState(TRACKS);
  const total = TRACKS.reduce((sum, t) => sum + t.items.length, 0);

  return (
    <div className="rm-map reveal mt-12 overflow-hidden rounded-[6px] border border-line bg-sunken md:mt-16">
      <dl className="grid grid-cols-2 border-b border-line lg:grid-cols-4">
        {STATES.map((s, i) => (
          <div
            key={s.id}
            className={`flex flex-col p-5 sm:p-6 ${i % 2 === 0 ? "border-r" : ""} ${i < 2 ? "border-b lg:border-b-0" : ""} ${
              i === 1 ? "lg:border-r" : ""
            } border-line`}
          >
            <dt className="flex items-center gap-2.5 font-mono text-[10px] font-medium uppercase tracking-[0.22em] text-fg-2">
              <StateMark state={s.id} size="md" />
              {s.label}
            </dt>
            <dd
              className={`type-display mt-5 text-[clamp(2.2rem,4vw,3.2rem)] tabular-nums ${
                s.id === "live" ? "text-mint" : s.id === "progress" ? "text-ramp" : s.id === "next" ? "text-blue" : "text-muted-2"
              }`}
            >
              {String(counts[s.id]).padStart(2, "0")}
            </dd>
            <dd className="mt-3 text-[11.5px] leading-relaxed text-muted">{s.definition}</dd>
          </div>
        ))}
      </dl>

      <div className="flex h-[3px] gap-[3px] bg-sunken" aria-hidden>
        {STATES.map((s, i) => (
          <span
            key={s.id}
            data-state={s.id}
            className="rm-bar"
            style={{ flexGrow: counts[s.id], "--rm-delay": `${0.15 + i * 0.12}s` } as React.CSSProperties}
          />
        ))}
      </div>

      <ol className="divide-y divide-line-soft" onPointerLeave={() => setHit(null)}>
        {TRACKS.map((t, row) => (
          <li
            key={t.id}
            className="grid gap-3 px-5 py-4 transition-colors hover:bg-raised/40 sm:px-6 md:grid-cols-[17rem_minmax(0,1fr)_auto] md:items-center md:gap-6"
          >
            <div className="flex items-start justify-between gap-4">
              <a
                href={`#${t.id}`}
                className="flex min-w-0 items-baseline gap-3 font-mono text-[11px] uppercase leading-snug tracking-[0.14em] text-fg-2 transition-colors hover:text-fg"
              >
                <span className="w-6 shrink-0 text-[10px] tabular-nums text-faint">{t.code ?? ""}</span>
                <span>{t.title}</span>
              </a>
              <StateChip state={t.state} className="shrink-0 md:hidden" />
            </div>
            <div className="flex flex-col gap-2.5 pl-9 md:pl-0">
            <div className="flex flex-wrap gap-[6px]">
              {orderedItems(t).map(({ item, n, state }, col) => {
                const label = STATES.find((s) => s.id === state)?.label ?? "";
                const show = () => setHit({ code: t.code ?? t.title, n, name: item.name, state });
                return (
                  <a
                    key={item.name}
                    href={`#${itemId(t, item)}`}
                    aria-label={`${item.name}: ${label}`}
                    onPointerEnter={show}
                    onFocus={show}
                    onBlur={() => setHit(null)}
                    className="rm-cell grid place-items-center rounded-[3px] p-[2px]"
                    style={{ "--rm-delay": `${(row * 0.07 + col * 0.022).toFixed(3)}s` } as React.CSSProperties}
                  >
                    <StateMark state={state} size="lg" />
                  </a>
                );
              })}
            </div>
            <span className="font-mono text-[10px] uppercase tracking-[0.2em] text-muted-2 md:hidden">{tally(t)}</span>
            </div>
            <div className="hidden items-center justify-end gap-5 md:flex">
              <span className="font-mono text-[10px] uppercase tracking-[0.2em] text-muted-2">{tally(t)}</span>
              <StateChip state={t.state} />
            </div>
          </li>
        ))}
      </ol>

      <div
        className="flex min-h-[52px] flex-wrap items-center gap-x-4 gap-y-2 border-t border-line px-5 py-3 font-mono text-[10.5px] uppercase tracking-[0.16em] sm:px-6"
      >
        {hit ? (
          <>
            <span className="tabular-nums text-faint">
              {hit.code} · {String(hit.n).padStart(2, "0")}
            </span>
            <span className="min-w-0 flex-1 truncate text-fg">{hit.name}</span>
            <StateChip state={hit.state} />
          </>
        ) : (
          <span className="text-muted-2">
            {total} items across {TRACKS.length} tracks
          </span>
        )}
      </div>
    </div>
  );
}
