"use client";

import Image from "next/image";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { type ReactNode } from "react";
import { CommandPalette } from "./CommandPalette";

const NAV: ReadonlyArray<{ href: string; label: string; section: string }> = [
  { href: "/", label: "overview", section: "operate" },
  { href: "/audit", label: "audit", section: "operate" },
  { href: "/intents", label: "intents", section: "operate" },
  { href: "/peers", label: "peers", section: "trust" },
  { href: "/capabilities", label: "capabilities", section: "trust" },
  { href: "/memory", label: "memory", section: "state" },
  { href: "/settlement", label: "settlement", section: "state" },
  { href: "/queues", label: "queues", section: "state" },
];

function isActive(pathname: string, href: string): boolean {
  if (href === "/") return pathname === "/";
  return pathname === href || pathname.startsWith(`${href}/`);
}

export function Shell({ children }: { children: ReactNode }) {
  const pathname = usePathname() ?? "/";

  const sections = ["operate", "trust", "state"] as const;
  return (
    <div className="shell">
      <aside className="sidebar" aria-label="navigation">
        <Link href="/" className="brand">
          <span className="mark" aria-hidden>
            <Image src="/logo.png" alt="" width={32} height={32} priority />
          </span>
          <span>
            <strong>COVENANT</strong>
            <em>operator console</em>
          </span>
        </Link>

        <nav>
          {sections.map((section) => (
            <div key={section} className="nav-group">
              <p className="eyebrow">{section}</p>
              {NAV.filter((item) => item.section === section).map((item) => (
                <Link
                  key={item.href}
                  href={item.href}
                  className={isActive(pathname, item.href) ? "nav-link active" : "nav-link"}
                  prefetch={false}
                >
                  {item.label}
                </Link>
              ))}
            </div>
          ))}
        </nav>

        <div className="sidebar-foot">
          <p className="eyebrow">shortcuts</p>
          <dl>
            <div>
              <dt>⌘K</dt>
              <dd>command palette</dd>
            </div>
            <div>
              <dt>g h</dt>
              <dd>overview</dd>
            </div>
            <div>
              <dt>g a</dt>
              <dd>audit</dd>
            </div>
          </dl>
          <p className="hint">daemon · 127.0.0.1:8421</p>
        </div>
      </aside>

      <main className="surface">{children}</main>

      <CommandPalette />

      <style jsx global>{`
        .shell {
          display: grid;
          grid-template-columns: 248px minmax(0, 1fr);
          min-height: 100vh;
          background: var(--bg);
        }

        .sidebar {
          position: sticky;
          top: 0;
          align-self: start;
          display: flex;
          flex-direction: column;
          gap: 36px;
          height: 100vh;
          padding: 36px 22px 28px;
          border-right: 1px solid var(--border);
          background: var(--bg);
          overflow: auto;
        }

        .brand {
          display: flex;
          align-items: center;
          gap: 12px;
          color: var(--fg);
          text-decoration: none;
        }

        .brand .mark {
          display: grid;
          place-items: center;
          width: 32px;
          height: 32px;
          border-radius: 6px;
          overflow: hidden;
          background: #000;
        }

        .brand .mark :global(img) {
          display: block;
          width: 32px;
          height: 32px;
        }

        .brand strong {
          display: block;
          color: var(--fg);
          font-size: 11px;
          font-weight: 500;
          letter-spacing: 0.32em;
          line-height: 1.1;
        }

        .brand em {
          display: block;
          margin-top: 4px;
          color: var(--muted);
          font-size: 11px;
          font-style: normal;
          letter-spacing: 0.12em;
        }

        .sidebar nav {
          display: grid;
          gap: 22px;
        }

        .nav-group {
          display: grid;
          gap: 4px;
        }

        .nav-group .eyebrow {
          margin-bottom: 6px;
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 10px;
          font-weight: 500;
          letter-spacing: 0.32em;
          text-transform: uppercase;
        }

        .nav-link {
          position: relative;
          display: block;
          padding: 6px 0 6px 12px;
          color: var(--dim);
          font-size: 13px;
          letter-spacing: 0.02em;
          border-left: 2px solid transparent;
          text-decoration: none;
          transition: color 120ms ease, border-color 120ms ease;
        }

        .nav-link:hover {
          color: var(--fg);
          border-left-color: var(--faint);
        }

        .nav-link.active {
          color: var(--fg);
          border-left-color: var(--fg);
        }

        .sidebar-foot {
          margin-top: auto;
          padding-top: 22px;
          border-top: 1px solid var(--border);
          display: grid;
          gap: 12px;
        }

        .sidebar-foot .eyebrow {
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.32em;
          text-transform: uppercase;
        }

        .sidebar-foot dl {
          display: grid;
          gap: 6px;
          margin: 0;
        }

        .sidebar-foot dl div {
          display: flex;
          align-items: center;
          gap: 10px;
          font-size: 11px;
          color: var(--dim);
        }

        .sidebar-foot dt {
          flex: 0 0 auto;
          padding: 2px 6px;
          border: 1px solid var(--border);
          border-radius: 4px;
          font-family: var(--font-mono);
          font-size: 10px;
          color: var(--fg);
        }

        .sidebar-foot dd {
          margin: 0;
          font-family: var(--font-mono);
          font-size: 11px;
        }

        .sidebar-foot .hint {
          margin-top: 4px;
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 10px;
          letter-spacing: 0.08em;
        }

        .surface {
          min-width: 0;
          padding: 40px 36px 96px;
        }

        @media (max-width: 900px) {
          .shell {
            grid-template-columns: 1fr;
          }

          .sidebar {
            position: static;
            height: auto;
            flex-direction: row;
            gap: 16px;
            flex-wrap: wrap;
            padding: 18px 22px;
            border-right: 0;
            border-bottom: 1px solid var(--border);
          }

          .sidebar nav {
            grid-auto-flow: column;
            grid-auto-columns: auto;
            grid-template-rows: 1fr;
            gap: 16px;
            overflow-x: auto;
          }

          .sidebar-foot {
            display: none;
          }

          .surface {
            padding: 24px 22px 64px;
          }
        }
      `}</style>
    </div>
  );
}
