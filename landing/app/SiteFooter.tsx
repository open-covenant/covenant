import Image from "next/image";
import Link from "next/link";
import { CopyAddress } from "./CopyAddress";
import {
  CVNT_MINT,
  FOOTER_COLUMNS,
  FOOTER_LINKS,
  FOOTER_PROTOCOLS,
  type FooterColumnLink,
  TAGLINE,
} from "./_brand";

type SiteFooterProps = {
  /** Extra classes for the outer <footer>. Pages own positioning. */
  className?: string;
  /** Inline styles for the outer <footer>. Useful for safe-area insets. */
  style?: React.CSSProperties;
  /**
   * `full` (default) is the multi-column site footer. `minimal` is the single
   * centered row, for the hero and full-screen dashboard views where a tall
   * footer doesn't fit.
   */
  variant?: "full" | "minimal";
};

function FooterLink({ item, className }: { item: FooterColumnLink; className?: string }) {
  const cls = ["transition-colors hover:text-neutral-200", className ?? ""].filter(Boolean).join(" ");
  return item.external ? (
    <a href={item.href} target="_blank" rel="noopener noreferrer" className={cls}>
      {item.label}
    </a>
  ) : (
    <Link href={item.href} className={cls}>
      {item.label}
    </Link>
  );
}

/**
 * Site-wide footer. The link columns, tagline, protocol chips, and $CVNT
 * contract come from `_brand.tsx`, so every page that mounts the footer stays
 * in sync. Pages control positioning via `className`.
 */
export function SiteFooter({ className, style, variant = "full" }: SiteFooterProps) {
  if (variant === "minimal") {
    return (
      <footer
        className={[
          "flex flex-wrap items-center justify-center gap-x-6 gap-y-2 px-4 text-center text-[12px] text-neutral-400 sm:text-[13px]",
          className ?? "",
        ]
          .filter(Boolean)
          .join(" ")}
        style={style}
      >
        <Image src="/covenant-logomark.png" alt="Covenant" width={28} height={28} className="h-7 w-7 object-contain opacity-70" />
        <nav aria-label="Footer" className="contents">
          {FOOTER_LINKS.map((item) => (
            <FooterLink key={item.href} item={item} />
          ))}
        </nav>
      </footer>
    );
  }

  return (
    <footer
      className={["w-full text-[13px] text-neutral-400", className ?? ""].filter(Boolean).join(" ")}
      style={style}
    >
      <div className="mx-auto w-full max-w-7xl px-5 sm:px-8">
        <div className="grid gap-10 border-t border-neutral-800/80 pt-12 md:grid-cols-[1.5fr_repeat(5,1fr)]">
          <div className="max-w-xs">
            <Image src="/covenant-logomark.png" alt="Covenant" width={26} height={26} className="h-6 w-6 object-contain" />
            <p className="mt-4 text-[12.5px] leading-relaxed text-neutral-500">{TAGLINE}.</p>
            <p className="mt-4 font-mono text-[11px] tracking-wide text-neutral-600">
              {FOOTER_PROTOCOLS.join("  ·  ")}
            </p>
          </div>

          {FOOTER_COLUMNS.map((col) => (
            <div key={col.title}>
              <h3 className="font-mono text-[11px] uppercase tracking-[0.25em] text-neutral-200">{col.title}</h3>
              <ul className="mt-4 space-y-2.5 text-[13px] text-neutral-400">
                {col.links.map((item) => (
                  <li key={item.href}>
                    <FooterLink item={item} />
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>

        <div className="mt-12 flex flex-col items-center gap-3 border-t border-neutral-800/80 py-8 text-center">
          <span className="font-mono text-[11px] uppercase tracking-[0.25em] text-neutral-500">$CVNT contract</span>
          <CopyAddress address={CVNT_MINT} label="Copy the $CVNT contract address" />
        </div>

        <div className="pb-8 text-center font-mono text-[11px] text-neutral-600">
          © 2026 Covenant · Apache-2.0
        </div>
      </div>
    </footer>
  );
}
