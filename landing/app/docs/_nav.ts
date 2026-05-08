export type DocsLink = { href: string; label: string };
export type DocsSection = { title: string; items: DocsLink[] };

export const DOCS_NAV: DocsSection[] = [
  {
    title: "Introduction",
    items: [
      { href: "/", label: "Overview" },
      { href: "/getting-started", label: "Getting started" },
      { href: "/concepts", label: "Concepts" },
    ],
  },
  {
    title: "Architecture",
    items: [
      { href: "/architecture", label: "System architecture" },
      { href: "/primitives", label: "The eight primitives" },
    ],
  },
  {
    title: "Reference",
    items: [
      { href: "/cli", label: "Command-line interface" },
      { href: "/http-api", label: "HTTP API" },
      { href: "/ipc", label: "Local IPC" },
      { href: "/agent-manifest", label: "Agent manifest" },
    ],
  },
  {
    title: "Protocols",
    items: [
      { href: "/capabilities", label: "Capability tokens" },
      { href: "/mcp", label: "MCP integration" },
      { href: "/a2a", label: "Agent-to-agent" },
      { href: "/audit", label: "Audit log" },
      { href: "/settlement", label: "Settlement" },
    ],
  },
  {
    title: "Operations",
    items: [
      { href: "/security", label: "Security model" },
      { href: "/identity", label: "Identity and keys" },
      { href: "/memory", label: "Memory tiers" },
    ],
  },
];
