import Image from "next/image";
import Link from "next/link";
import { FOOTER_LINKS, RELEASE_STATUS, TAGLINE } from "./_brand";

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
        "flex flex-wrap items-center justify-center gap-x-3 gap-y-1.5 px-4 text-center text-[10px] uppercase tracking-widest text-neutral-500 sm:text-[11px]",
        className ?? "",
      ]
        .filter(Boolean)
        .join(" ")}
      style={style}
    >
      <span className="inline-flex items-center gap-2.5">
        <Image
          src="/logomark.svg"
          alt="covenant"
          width={30}
          height={15}
          className="h-auto w-[30px] opacity-70"
        />
        <span>
          Covenant · {TAGLINE} · {RELEASE_STATUS.toLowerCase()}
        </span>
      </span>
      {FOOTER_LINKS.map((item) =>
        item.external ? (
          <a
            key={item.href}
            href={item.href}
            target="_blank"
            rel="noopener noreferrer"
            className="text-neutral-500 transition-colors hover:text-neutral-200"
          >
            {item.label}
          </a>
        ) : (
          <Link
            key={item.href}
            href={item.href}
            className="text-neutral-500 transition-colors hover:text-neutral-200"
          >
            {item.label}
          </Link>
        ),
      )}
    </footer>
  );
}
