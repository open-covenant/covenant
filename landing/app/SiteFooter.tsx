import Image from "next/image";
import Link from "next/link";
import { FOOTER_LINKS } from "./_brand";

type SiteFooterProps = {
  /** Extra classes for the outer <footer>. Pages own positioning. */
  className?: string;
  /** Inline styles for the outer <footer>. Useful for safe-area insets. */
  style?: React.CSSProperties;
};

/**
 * Site-wide footer. Single source of truth — update the tagline or
 * release status in `_brand.tsx` and every page that mounts <SiteFooter />
 * picks it up. Pages control positioning via `className` (e.g. add
 * `border-t mt-24 pt-6` for the docs layout, or `absolute inset-x-0
 * bottom-6` for the landing hero).
 */
export function SiteFooter({ className, style }: SiteFooterProps) {
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
      <Image
        src="/logomark.svg"
        alt="Covenant"
        width={30}
        height={15}
        className="h-auto w-[30px] opacity-70"
      />
      {/* `contents` lets the links flow in the footer's own wrap so the logo
          shares a line with them instead of wrapping onto a line of its own */}
      <nav aria-label="Footer" className="contents">
        {FOOTER_LINKS.map((item) =>
          item.external ? (
            <a
              key={item.href}
              href={item.href}
              target="_blank"
              rel="noopener noreferrer"
              className="transition-colors hover:text-neutral-200"
            >
              {item.label}
            </a>
          ) : (
            <Link
              key={item.href}
              href={item.href}
              className="transition-colors hover:text-neutral-200"
            >
              {item.label}
            </Link>
          ),
        )}
      </nav>
    </footer>
  );
}
