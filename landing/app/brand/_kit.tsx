// Building blocks for the brand guidelines pages: the page head, the part row
// (words on the left, the visual on the right), panels, chips, do/don't tiles,
// spec lists, code and the previous/next links.

import Link from "next/link";
import type { ReactNode } from "react";
import { CopyButton } from "./CopyButton";
import { brandNeighbours, brandSection } from "./_sections";

export function PageHead({ slug, title, intro, children }: { slug: string; title?: string; intro?: ReactNode; children?: ReactNode }) {
  const s = brandSection(slug);
  return (
    <header className="pb-14 md:pb-20">
      <p className="font-mono text-[10px] font-medium uppercase tracking-[0.28em] text-muted">
        <span className="tabular-nums text-faint">{s.n}</span>
        <span aria-hidden> · </span>Brand guidelines
      </p>
      <h1 className="page-title mt-5 text-fg">{title ?? s.title}</h1>
      {intro ? (
        <div className="mt-7 max-w-[46rem] space-y-4 text-[15px] leading-[1.7] text-fg-soft [&_strong]:font-medium [&_strong]:text-fg">
          {intro}
        </div>
      ) : null}
      {children ? <div className={intro ? "mt-12" : "mt-10"}>{children}</div> : null}
    </header>
  );
}

/**
 * One part of a page. By default the words sit in a narrow left column and the
 * visual fills the right; `wide` puts the words above a full-width visual.
 */
export function Part({
  id,
  kicker,
  title,
  text,
  children,
  wide = false,
}: {
  id: string;
  kicker: string;
  title: string;
  text?: ReactNode;
  children?: ReactNode;
  wide?: boolean;
}) {
  const heading = (
    <div>
      <p className="font-mono text-[10px] uppercase tracking-[0.22em] text-muted-2">{kicker}</p>
      <h2 className="type-display mt-3 text-[1.3rem] text-fg">{title}</h2>
    </div>
  );
  const prose = text ? (
    <div className="space-y-3 text-[12.5px] leading-[1.75] text-fg-soft [&_strong]:font-medium [&_strong]:text-fg">{text}</div>
  ) : null;

  return (
    <section
      id={id}
      data-part={id}
      className="scroll-mt-[calc(var(--header-h)+80px)] border-t border-line py-10 md:py-14 lg:scroll-mt-[calc(var(--header-h)+32px)]"
    >
      {wide ? (
        <>
          <div className="grid gap-5 lg:grid-cols-[minmax(0,16.5rem)_minmax(0,1fr)] lg:gap-12">
            {heading}
            <div className="max-w-[42rem]">{prose}</div>
          </div>
          {children ? <div className="mt-8">{children}</div> : null}
        </>
      ) : (
        <div className="grid gap-8 lg:grid-cols-[minmax(0,16.5rem)_minmax(0,1fr)] lg:gap-12">
          <div>
            {heading}
            {prose ? <div className="mt-4">{prose}</div> : null}
          </div>
          <div className="min-w-0">{children}</div>
        </div>
      )}
    </section>
  );
}

export function Chip({ children, className = "" }: { children: ReactNode; className?: string }) {
  return (
    <span
      className={`inline-flex w-fit items-center rounded-full border border-line px-2.5 py-1 font-mono text-[9.5px] uppercase leading-none tracking-[0.2em] text-muted ${className}`}
    >
      {children}
    </span>
  );
}

const TONES = { ground: "bg-ground", raised: "bg-raised", sunken: "bg-sunken" } as const;

/** A framed visual, with an optional chip above, corner actions and a caption. */
export function Panel({
  label,
  tone = "raised",
  className = "",
  outerClassName = "",
  actions,
  caption,
  children,
}: {
  label?: string;
  tone?: keyof typeof TONES;
  /** Classes for the frame itself. */
  className?: string;
  /** Classes for the whole figure, chip and caption included (spacing). */
  outerClassName?: string;
  actions?: ReactNode;
  caption?: ReactNode;
  children: ReactNode;
}) {
  return (
    <figure className={`flex min-w-0 flex-col gap-2.5 ${outerClassName}`}>
      {label ? <Chip>{label}</Chip> : null}
      <div className={`relative overflow-hidden rounded-[6px] border border-line ${TONES[tone]} ${className}`}>
        {children}
        {actions ? <div className="absolute right-2.5 top-2.5 z-10 flex gap-1.5">{actions}</div> : null}
      </div>
      {caption ? <figcaption className="text-[11.5px] leading-relaxed text-muted">{caption}</figcaption> : null}
    </figure>
  );
}

/** A do or don't: the example on night, a badge, and a caption in tracked caps. */
export function Rule({
  ok,
  caption,
  className = "",
  children,
}: {
  ok: boolean;
  caption: string;
  className?: string;
  children: ReactNode;
}) {
  return (
    <figure className="flex min-w-0 flex-col gap-3">
      <div
        className={`relative flex aspect-[4/3] items-center justify-center overflow-hidden rounded-[6px] border border-line bg-ground ${className}`}
      >
        {children}
        <span
          aria-hidden
          className={`absolute left-2.5 top-2.5 inline-flex h-5 w-5 items-center justify-center rounded-full font-mono text-[10px] ${
            ok ? "bg-ok/15 text-ok" : "bg-bad/15 text-bad"
          }`}
        >
          {ok ? "✓" : "✕"}
        </span>
      </div>
      <figcaption className="font-mono text-[10px] uppercase leading-[1.65] tracking-[0.14em] text-muted">
        <span className={ok ? "text-ok" : "text-bad"}>{ok ? "Do" : "Don't"}</span>
        <span aria-hidden> · </span>
        {caption}
      </figcaption>
    </figure>
  );
}

export function RuleGrid({ children, cols = 4 }: { children: ReactNode; cols?: 2 | 3 | 4 }) {
  const grid = cols === 2 ? "sm:grid-cols-2" : cols === 3 ? "sm:grid-cols-2 lg:grid-cols-3" : "grid-cols-2 lg:grid-cols-4";
  return <div className={`grid gap-x-4 gap-y-7 ${grid}`}>{children}</div>;
}

/** Key/value rows in the mono, for specifications. */
export function Spec({ rows, className = "" }: { rows: [string, ReactNode][]; className?: string }) {
  return (
    <dl className={`divide-y divide-line-soft border-y border-line-soft ${className}`}>
      {rows.map(([k, v]) => (
        <div key={k} className="grid grid-cols-[minmax(0,8.5rem)_minmax(0,1fr)] gap-4 py-2.5">
          <dt className="pt-px font-mono text-[10px] uppercase tracking-[0.16em] text-muted-2">{k}</dt>
          <dd className="font-mono text-[11.5px] leading-relaxed text-fg-soft">{v}</dd>
        </div>
      ))}
    </dl>
  );
}

export function CodeBlock({ code, label }: { code: string; label: string }) {
  return (
    <div className="relative min-w-0 rounded-[6px] border border-line bg-sunken">
      <pre className="overflow-x-auto p-4 pr-14 font-mono text-[11.5px] leading-[1.75] text-fg-soft">
        <code>{code}</code>
      </pre>
      <div className="absolute right-2.5 top-2.5">
        <CopyButton value={code} label={`Copy ${label}`} />
      </div>
    </div>
  );
}

/** A numbered list of short rules, set like the site's spec lists. */
export function Points({ items }: { items: ReactNode[] }) {
  return (
    <ol className="flex flex-col">
      {items.map((item, i) => (
        <li key={i} className="flex gap-4 border-t border-line-soft py-3 text-[12.5px] leading-[1.7] text-fg-soft first:border-t-0 first:pt-0 [&_strong]:font-medium [&_strong]:text-fg">
          <span className="w-5 shrink-0 pt-px font-mono text-[10px] tabular-nums text-faint">{String(i + 1).padStart(2, "0")}</span>
          <span>{item}</span>
        </li>
      ))}
    </ol>
  );
}

export function MoreSections({ slug }: { slug: string }) {
  const { prev, next } = brandNeighbours(slug);
  const card = "group flex flex-col rounded-[6px] border border-line bg-raised p-5 transition-colors hover:border-line-strong";
  return (
    <nav aria-label="More sections" className="grid gap-3 border-t border-line pt-10 sm:grid-cols-2">
      {prev ? (
        <Link href={prev.href} className={card}>
          <span className="font-mono text-[10px] uppercase tracking-[0.22em] text-muted-2">
            <span aria-hidden>← </span>
            {prev.n}
          </span>
          <span className="type-display mt-3 text-[1.25rem] text-fg">{prev.title}</span>
          <span className="mt-2 text-[12px] leading-relaxed text-muted">{prev.summary}</span>
        </Link>
      ) : (
        <span className="hidden sm:block" />
      )}
      {next ? (
        <Link href={next.href} className={`${card} sm:items-end sm:text-right`}>
          <span className="font-mono text-[10px] uppercase tracking-[0.22em] text-muted-2">
            {next.n}
            <span aria-hidden> →</span>
          </span>
          <span className="type-display mt-3 text-[1.25rem] text-fg">{next.title}</span>
          <span className="mt-2 text-[12px] leading-relaxed text-muted">{next.summary}</span>
        </Link>
      ) : null}
    </nav>
  );
}

/** A download link styled as a quiet pill. */
export function DownloadLink({ href, file, children }: { href: string; file: string; children: ReactNode }) {
  return (
    <a href={href} download={file} className="btn btn-secondary min-h-9 px-4">
      {children} <span aria-hidden="true">↓</span>
    </a>
  );
}
