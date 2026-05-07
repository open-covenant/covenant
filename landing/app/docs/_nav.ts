export type DocsLink = { href: string; label: string };
export type DocsSection = { title: string; items: DocsLink[] };

export const DOCS_NAV: DocsSection[] = [
  {
    title: "Introduction",
    items: [
      { href: "/docs", label: "Overview" },
      { href: "/docs/getting-started", label: "Getting started" },
      { href: "/docs/concepts", label: "Concepts" },
    ],
  },
  {
    title: "Architecture",
    items: [
      { href: "/docs/architecture", label: "System architecture" },
      { href: "/docs/primitives", label: "The eight primitives" },
    ],
  },
  {
    title: "Reference",
    items: [
      { href: "/docs/cli", label: "Command-line interface" },
      { href: "/docs/http-api", label: "HTTP API" },
      { href: "/docs/ipc", label: "Local IPC" },
      { href: "/docs/agent-manifest", label: "Agent manifest" },
    ],
  },
  {
    title: "Protocols",
    items: [
      { href: "/docs/capabilities", label: "Capability tokens" },
      { href: "/docs/mcp", label: "MCP integration" },
      { href: "/docs/a2a", label: "Agent-to-agent" },
      { href: "/docs/audit", label: "Audit log" },
      { href: "/docs/settlement", label: "Settlement" },
    ],
  },
  {
    title: "Operations",
    items: [
      { href: "/docs/security", label: "Security model" },
      { href: "/docs/identity", label: "Identity and keys" },
      { href: "/docs/memory", label: "Memory tiers" },
    ],
  },
];
