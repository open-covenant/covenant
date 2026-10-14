import { useId } from "react";

// The Covenant mark: a C built from the 210-unit module around a detached
// core, 840 × 1050. Carries the ramp, violet to blue to mint.
const MARK_PATHS = [
  "M248 0h554a38 38 0 0 1 38 38v134a38 38 0 0 1-38 38H248a38 38 0 0 0-38 38v554a38 38 0 0 0 38 38h554a38 38 0 0 1 38 38v134a38 38 0 0 1-38 38H248a38 38 0 0 1-38-38V878a38 38 0 0 0-38-38H38a38 38 0 0 1-38-38V248a38 38 0 0 1 38-38h134a38 38 0 0 0 38-38V38a38 38 0 0 1 38-38Z",
  "M432 385h186a47 47 0 0 1 47 47v186a47 47 0 0 1-47 47H432a47 47 0 0 1-47-47V432a47 47 0 0 1 47-47Z",
];

export function CovenantMark({ className, height = 28 }: { className?: string; height?: number }) {
  const id = `covenant-ramp-${useId().replace(/[^\w-]/g, "")}`;
  return (
    <svg
      viewBox="0 0 840 1050"
      height={height}
      width={(height * 840) / 1050}
      className={className}
      aria-hidden
      focusable="false"
    >
      <defs>
        <linearGradient id={id} x1="0" y1="0" x2="840" y2="1050" gradientUnits="userSpaceOnUse">
          <stop offset="0" stopColor="#A03BF0" />
          <stop offset="0.5" stopColor="#4C7FDE" />
          <stop offset="1" stopColor="#1DE3AE" />
        </linearGradient>
      </defs>
      <g fill={`url(#${id})`}>
        {MARK_PATHS.map((d) => (
          <path key={d} d={d} />
        ))}
      </g>
    </svg>
  );
}
