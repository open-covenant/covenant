// Brand constants and inline icons. Centralised so /, /roadmap, /docs/*
// (and any future top-level page) share the same nav, X handle, GitHub
// repo, release status, and tagline without copy-paste drift.

export const X_URL = "https://x.com/OpenCovenant";
export const GITHUB_URL = "https://github.com/open-covenant/covenant";
export const TAGLINE = "Open infrastructure for agent-native computing";
export const RELEASE_STATUS = "ALPHA LIVE";

export const NAV_LINKS = [
  { label: "stake", href: "/stake", external: false as const },
  { label: "sandbox", href: "https://sandbox.opencovenant.org", external: true as const },
  { label: "docs", href: "https://docs.opencovenant.org", external: true as const },
  { label: "paper", href: "https://doi.org/10.5281/zenodo.20134416", external: true as const },
  { label: "roadmap", href: "/roadmap", external: false as const },
  { label: "contact", href: "/contact", external: false as const },
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
