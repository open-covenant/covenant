import Link from "next/link";
import { CopyAddress } from "./CopyAddress";
import { FieldSeam } from "./FieldSeam";
import {
  CovenantLockup,
  CovenantMark,
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
   * `full` (default) is the multi-column footer. `minimal` is the single
   * centered row, for the hero and full-screen dashboard views where a tall
   * footer doesn't fit.
   */
  variant?: "full" | "minimal";
};

function FooterLink({ item, className }: { item: FooterColumnLink; className?: string }) {
  const cls = ["transition-colors", className ?? ""].filter(Boolean).join(" ");
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
 * in sync. The full footer opens with a seam of the hero's field, like every
 * section boundary on the home page; pages only own their padding.
 */
export function SiteFooter({ className, style, variant = "full" }: SiteFooterProps) {
  if (variant === "minimal") {
    return (
      <footer
        className={[
          "flex flex-wrap items-center justify-center gap-x-6 gap-y-2 px-4 text-center font-mono text-[11px] uppercase tracking-[0.14em] text-muted",
          className ?? "",
        ]
          .filter(Boolean)
          .join(" ")}
        style={style}
      >
        <CovenantMark className="h-6 w-auto" title="Covenant" />
        <nav aria-label="Footer" className="contents">
          {FOOTER_LINKS.map((item) => (
            <FooterLink key={item.href} item={item} className="hover:text-fg" />
          ))}
        </nav>
      </footer>
    );
  }

  return (
    <footer
      className={["w-full text-[12px] text-muted", className ?? ""].filter(Boolean).join(" ")}
      style={style}
    >
      <FieldSeam seed={4.1} />
      <div className="w-full px-[clamp(20px,3.4vw,50px)] pt-10">
        <div className="grid gap-10 md:grid-cols-[minmax(250px,1.5fr)_repeat(5,minmax(0,1fr))] md:gap-[clamp(28px,4vw,64px)]">
          <div className="max-w-xs">
            <CovenantLockup className="text-fg" markClassName="h-7 w-auto" />
            <p className="mt-5 text-[12.5px] leading-relaxed text-muted">{TAGLINE}.</p>
            <p className="mt-4 font-mono text-[9.5px] uppercase tracking-[0.16em] text-muted-2">
              {FOOTER_PROTOCOLS.join("  ·  ")}
            </p>
          </div>

          {FOOTER_COLUMNS.map((col) => (
            <div key={col.title}>
              <h3 className="font-mono text-[10px] font-semibold uppercase tracking-[0.24em] text-fg">
                {col.title}
              </h3>
              <ul className="mt-5 space-y-3 text-[12px]">
                {col.links.map((item) => (
                  <li key={item.href}>
                    <FooterLink item={item} className="text-muted hover:text-fg" />
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>

        <div className="mt-12 flex flex-col items-start gap-3 border-t border-line pt-6 sm:flex-row sm:items-center sm:justify-between">
          <span className="font-mono text-[10px] uppercase tracking-[0.24em] text-muted-2">$CVNT contract</span>
          <CopyAddress address={CVNT_MINT} label="Copy the $CVNT contract address" />
        </div>

        <div className="mt-6 flex flex-wrap justify-between gap-x-6 gap-y-2 border-t border-line py-6 font-mono text-[10px] uppercase tracking-[0.12em] text-muted-2">
          <span>© 2026 Covenant</span>
          <a
            href="https://github.com/open-covenant/covenant/blob/main/LICENSE"
            target="_blank"
            rel="noopener noreferrer"
            className="transition-colors hover:text-fg"
          >
            Source code license: Apache-2.0 <span aria-hidden="true">→</span>
          </a>
        </div>
      </div>
    </footer>
  );
}
