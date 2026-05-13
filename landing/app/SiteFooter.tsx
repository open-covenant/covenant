import Image from "next/image";
import { GITHUB_URL, LICENSE, RELEASE_DATE, REPO_SLUG, TAGLINE } from "./_brand";

type SiteFooterProps = {
  /** Extra classes for the outer <footer>. Pages own positioning. */
  className?: string;
  /** Inline styles for the outer <footer>. Useful for safe-area insets. */
  style?: React.CSSProperties;
};

/**
 * Site-wide footer. Single source of truth — update the tagline, license,
 * repo, or release date in `_brand.tsx` and every page that mounts
 * <SiteFooter /> picks it up. Pages control positioning via `className`
 * (e.g. add `border-t mt-24 pt-6` for the docs layout, or `absolute
 * inset-x-0 bottom-6` for the landing hero).
 */
export function SiteFooter({ className, style }: SiteFooterProps) {
  return (
    <footer
      className={[
        "flex flex-col items-center gap-2 px-4 text-center text-[10px] uppercase tracking-widest text-neutral-500 sm:text-[11px]",
        className ?? "",
      ]
        .filter(Boolean)
        .join(" ")}
      style={style}
    >
      <div className="flex items-center gap-2.5">
        <Image
          src="/logomark.svg"
          alt="covenant"
          width={30}
          height={15}
          className="h-auto w-[30px] opacity-70"
        />
        <span>
          Covenant · {TAGLINE} · {RELEASE_DATE.toLowerCase()}
        </span>
      </div>
      <div className="flex items-center gap-3 text-neutral-600">
        <span>{LICENSE}</span>
        <span aria-hidden="true">·</span>
        <a
          href={GITHUB_URL}
          target="_blank"
          rel="noopener noreferrer"
          className="transition-colors hover:text-neutral-200"
        >
          {REPO_SLUG} ↗
        </a>
      </div>
    </footer>
  );
}
