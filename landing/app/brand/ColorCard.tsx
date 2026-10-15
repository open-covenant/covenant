import { CopyButton } from "./CopyButton";
import type { PaletteColor } from "./_palette";

// Relative luminance, to pick a legible label over a fill.
function luminance([r, g, b]: [number, number, number]) {
  const lin = (c: number) => {
    const v = c / 255;
    return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

/**
 * One color with every value a click away. Surfaces and accents show as a
 * fill; text colors show as text on the background, the only way they are used.
 */
export function ColorCard({ color, kind = "fill" }: { color: PaletteColor; kind?: "fill" | "type" }) {
  const [r, g, b] = color.rgb;
  const values: [string, string][] = [
    ["HEX", color.hex],
    ["RGB", `${r} ${g} ${b}`],
    ["CMYK", color.cmyk.join(" ")],
    ["OKLCH", color.oklch],
    ["CSS", color.token],
  ];
  // Whichever of the light text color or the background reads better over the fill.
  const L = luminance(color.rgb);
  const onFill = 1.05 / (L + 0.05) >= (L + 0.05) / 0.05 ? "#f3f1fa" : "#000000";

  return (
    <div className="flex min-w-0 flex-col overflow-hidden rounded-[6px] border border-line bg-raised">
      {kind === "type" ? (
        <div className="flex h-28 items-end justify-between border-b border-line bg-ground p-3">
          <span className="font-mono text-[2.4rem] font-medium leading-none" style={{ color: color.hex }}>
            Aa
          </span>
          <span className="font-mono text-[10px] tabular-nums text-muted">{color.contrast.toFixed(2)}:1</span>
        </div>
      ) : (
        <div
          className={`flex h-28 items-end justify-between p-3 ${L < 0.02 ? "border-b border-line" : ""}`}
          style={{ backgroundColor: color.hex, color: onFill }}
        >
          <span className="font-mono text-[10px] uppercase tracking-[0.18em]">{color.name}</span>
        </div>
      )}
      <div className="flex flex-col gap-0.5 px-3 py-2.5">
        {kind === "type" ? (
          <div className="mb-1.5 font-mono text-[10px] uppercase tracking-[0.18em] text-fg">{color.name}</div>
        ) : null}
        {values.map(([k, v]) => (
          <CopyButton
            key={k}
            value={v}
            label={`Copy ${color.name} ${k}`}
            className="-mx-1.5 justify-between rounded px-1.5 py-1 font-mono text-[10.5px] hover:bg-ground"
          >
            <span className="w-11 shrink-0 text-muted-2">{k}</span>
            <span className="min-w-0 flex-1 truncate text-fg-soft">{v}</span>
          </CopyButton>
        ))}
      </div>
      <p className="mt-auto border-t border-line-soft px-3 py-2.5 text-[11px] leading-relaxed text-muted">{color.role}</p>
    </div>
  );
}
