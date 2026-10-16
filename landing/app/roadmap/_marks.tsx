// The roadmap's state glyphs, drawn from the mark: a module with its core.
// Live is the solid module; in progress is the core forming in the Covenant
// gradient inside its module; next is the module's outline in blue; planned
// is a faint outline. The glyph is decoration: every use sits beside its label.

import { STATE_LABEL, type State } from "./_roadmap";

const SIZE = { sm: 8, md: 11, lg: 15 } as const;

export function StateMark({
  state,
  size = "md",
  className = "",
}: {
  state: State;
  size?: keyof typeof SIZE;
  className?: string;
}) {
  return (
    <span
      aria-hidden
      data-state={state}
      className={`rm-mark ${className}`}
      style={{ "--rm-size": `${SIZE[size]}px` } as React.CSSProperties}
    />
  );
}

const CHIP: Record<State, string> = {
  live: "border-mint/35 bg-mint/[0.07] text-mint",
  progress: "rm-chip-ramp text-fg",
  next: "border-blue/45 bg-blue/[0.09] text-blue",
  planned: "border-line-strong text-muted",
};

export function StateChip({ state, className = "" }: { state: State; className?: string }) {
  return (
    <span
      className={`inline-flex w-fit items-center gap-2 rounded-full border px-2.5 py-[5px] font-mono text-[9.5px] font-medium uppercase leading-none tracking-[0.2em] ${CHIP[state]} ${className}`}
    >
      <StateMark state={state} size="sm" />
      {STATE_LABEL[state]}
    </span>
  );
}
