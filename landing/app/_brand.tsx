// Brand constants and inline icons. Centralised so /, /roadmap, /docs/*
// (and any future top-level page) share the same nav, X handle, GitHub
// repo, release status, and tagline without copy-paste drift.

import { useId } from "react";
import { workspaceRelease } from "./compute-workspaces/_release";

// The mark: a C built from the 210-unit module (a top bar, a stem and a bottom
// bar meeting at filleted corners) around a detached core. 840 × 1050, 4:5.
export const MARK_PATHS = [
  "M248 0h554a38 38 0 0 1 38 38v134a38 38 0 0 1-38 38H248a38 38 0 0 0-38 38v554a38 38 0 0 0 38 38h554a38 38 0 0 1 38 38v134a38 38 0 0 1-38 38H248a38 38 0 0 1-38-38V878a38 38 0 0 0-38-38H38a38 38 0 0 1-38-38V248a38 38 0 0 1 38-38h134a38 38 0 0 0 38-38V38a38 38 0 0 1 38-38Z",
  "M432 385h186a47 47 0 0 1 47 47v186a47 47 0 0 1-47 47H432a47 47 0 0 1-47-47V432a47 47 0 0 1 47-47Z",
];

// The ramp, violet through blue to mint, in the order the mark carries it.
export const RAMP = ["#A03BF0", "#4C7FDE", "#1DE3AE"] as const;

/**
 * The Covenant mark, inline so it can take the ramp or the current text colour.
 * Size it by height; the width follows the 4:5 viewBox.
 */
export function CovenantMark({
  tone = "ramp",
  className,
  title,
}: {
  tone?: "ramp" | "mono";
  className?: string;
  title?: string;
}) {
  const gradientId = `covenant-ramp-${useId().replace(/[^\w-]/g, "")}`;
  return (
    <svg
      viewBox="0 0 840 1050"
      className={className}
      role={title ? "img" : undefined}
      aria-label={title}
      aria-hidden={title ? undefined : true}
      focusable="false"
    >
      {tone === "ramp" && (
        <defs>
          <linearGradient id={gradientId} x1="0" y1="0" x2="840" y2="1050" gradientUnits="userSpaceOnUse">
            <stop offset="0" stopColor={RAMP[0]} />
            <stop offset="0.5" stopColor={RAMP[1]} />
            <stop offset="1" stopColor={RAMP[2]} />
          </linearGradient>
        </defs>
      )}
      <g fill={tone === "ramp" ? `url(#${gradientId})` : "currentColor"}>
        {MARK_PATHS.map((d) => (
          <path key={d} d={d} />
        ))}
      </g>
    </svg>
  );
}

/** The lockup: the mark beside the name, set in the mono. */
export function CovenantLockup({ className, markClassName }: { className?: string; markClassName?: string }) {
  return (
    <span className={["inline-flex items-center gap-2.5", className ?? ""].join(" ")}>
      <CovenantMark className={markClassName ?? "h-6 w-auto"} />
      <span className="font-mono text-[11px] font-medium uppercase leading-none tracking-[0.24em]">Covenant</span>
    </span>
  );
}

export const X_URL = "https://x.com/OpenCovenant";
export const GITHUB_URL = "https://github.com/open-covenant";
// Deep links (files, commits, releases) resolve against the repo, not the org.
export const GITHUB_REPO_URL = "https://github.com/open-covenant/covenant";
export const TELEGRAM_URL = "https://t.me/+4zEexbN94E1hOWY0";
export const TAGLINE = "Open infrastructure for agent-native computing";
export const RELEASE_STATUS = "ALPHA LIVE";

// Stable sitemap lastmod. Bump on a content/release change rather than
// emitting a fresh request-time clock (which churns lastmod on every crawl).
export const SITE_BUILD_DATE = "2026-05-31";

// Docs went live with the 0.1.0 release. Feeds TechArticle datePublished;
// dateModified tracks SITE_BUILD_DATE — only bump that on real content change.
export const DOCS_PUBLISHED_DATE = "2026-05-13";

// The side menu's primary nav: the most relevant surfaces only (products,
// learn, buy), set large. The rest of the site follows as MENU_LINKS, and
// everything is also in the footer.
export const NAV_LINKS = [
  { label: "compute", href: "/compute", external: false as const },
  { label: "guard", href: "/guard", external: false as const },
  { label: "trading", href: "/trading", external: false as const },
  { label: "docs", href: "https://docs.opencovenant.org", external: true as const },
  { label: "buy", href: "/token", external: false as const },
];

export const FOOTER_LINKS = [
  { label: "FAQ", href: "/faq", external: false as const },
  { label: "Changelog", href: "/changelog", external: false as const },
  { label: "Partners", href: "/partners", external: false as const },
  { label: "Brand", href: "/brand", external: false as const },
  { label: "Treasury", href: "/treasury", external: false as const },
  { label: "Paper", href: "https://doi.org/10.5281/zenodo.20134416", external: true as const },
  { label: "Terms of Service", href: "/terms", external: false as const },
  { label: "Privacy Policy", href: "/privacy", external: false as const },
  { label: "Contact", href: "/contact", external: false as const },
];

// $CVNT is a fair-launch pump.fun token, no pre-mine and no team allocation.
export const CVNT_MINT = "2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump";
export const CVNT_PUMP_URL = `https://pump.fun/coin/${CVNT_MINT}`;

// The open protocols Covenant speaks, shown as chips in the footer brand block.
export const FOOTER_PROTOCOLS = ["Solana", "x402", "MCP", "A2A"];

export type FooterColumnLink = { label: string; href: string; external?: boolean };

// The structured, multi-column footer. Single source of truth so every page
// that mounts the full <SiteFooter /> stays in sync. Every href resolves to a
// real route or an external surface we own.
export const FOOTER_COLUMNS: { title: string; links: FooterColumnLink[] }[] = [
  {
    title: "Product",
    links: [
      { label: "Guard", href: "/guard" },
      { label: "Trading", href: "/trading" },
      { label: "Compute", href: "/compute" },
      { label: "Sandbox", href: "https://sandbox.opencovenant.org", external: true },
      { label: "Agents", href: "/agents" },
      { label: "Arena", href: "/arena" },
      { label: "Stake", href: "https://stake.opencovenant.org", external: true },
      { label: "Token", href: "/token" },
    ],
  },
  {
    title: "Ecosystem",
    links: [
      { label: "Partners", href: "/partners" },
      {
        label: "MCP Registry",
        href: "https://registry.modelcontextprotocol.io/v0.1/servers?search=org.opencovenant",
        external: true,
      },
    ],
  },
  {
    title: "Developers",
    links: [
      { label: "Docs", href: "https://docs.opencovenant.org", external: true },
      { label: "API Reference", href: "/docs/http-api" },
      { label: "Compute status", href: "/compute-status" },
      ...(workspaceRelease.accepted ? [{ label: "Compute workspaces", href: "/compute-workspaces" }] : []),
      { label: "SDK", href: "/sdk" },
      { label: "GitHub", href: GITHUB_URL, external: true },
    ],
  },
  {
    title: "Legal",
    links: [
      { label: "Terms", href: "/terms" },
      { label: "Privacy", href: "/privacy" },
      { label: "Security", href: "/security" },
    ],
  },
  {
    title: "Company",
    links: [
      { label: "About", href: "/about" },
      { label: "Roadmap", href: "/roadmap" },
      { label: "Blog", href: "/blog" },
      { label: "Paper", href: "https://doi.org/10.5281/zenodo.20134416", external: true },
      { label: "Contact", href: "/contact" },
      { label: "Follow on X", href: X_URL, external: true },
    ],
  },
];

// The side menu's second tier: the rest of the site, beneath the primary nav.
export const MENU_LINKS = [
  { label: "About", href: "/about", external: false as const },
  { label: "Blog", href: "/blog", external: false as const },
  { label: "Roadmap", href: "/roadmap", external: false as const },
  { label: "Partners", href: "/partners", external: false as const },
  { label: "Agents", href: "/agents", external: false as const },
  { label: "Arena", href: "/arena", external: false as const },
  { label: "Brand", href: "/brand", external: false as const },
  { label: "Contact", href: "/contact", external: false as const },
];

// Socials sit on the right of the header and at the foot of the side menu.
export const SOCIAL_LINKS = [
  { label: "X", href: X_URL, Icon: XIcon },
  { label: "Telegram", href: TELEGRAM_URL, Icon: TelegramIcon },
  { label: "GitHub", href: GITHUB_URL, Icon: GithubIcon },
];

export function XIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      aria-hidden="true"
      className={className}
      fill="currentColor"
    >
      <path d="M17.53 3H20.5l-6.49 7.41L21.75 21h-6.18l-4.84-6.34L5.16 21H2.18l6.94-7.93L1.75 3h6.34l4.38 5.79L17.53 3Zm-1.08 16.2h1.71L7.66 4.7H5.83l10.62 14.5Z" />
    </svg>
  );
}

export function TelegramIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      aria-hidden="true"
      className={className}
      fill="currentColor"
    >
      <path d="M21.94 4.51 18.9 19.2c-.23 1.02-.84 1.27-1.7.79l-4.7-3.47-2.27 2.18c-.25.25-.46.46-.94.46l.34-4.78 8.7-7.86c.38-.34-.08-.53-.59-.19L6.98 13.2 2.34 11.74c-1.01-.32-1.03-1.01.21-1.5l18.13-6.99c.84-.31 1.58.19 1.26 1.26Z" />
    </svg>
  );
}

export function GithubIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      aria-hidden="true"
      className={className}
      fill="currentColor"
    >
      <path
        fillRule="evenodd"
        clipRule="evenodd"
        d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0 1 12 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.203 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.31.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.02 10.02 0 0 0 22 12.017C22 6.484 17.522 2 12 2Z"
      />
    </svg>
  );
}
