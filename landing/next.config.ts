import type { NextConfig } from "next";

// Next.js dev (Turbopack) + React rely on eval() for HMR and debugging. Allow it
// only in development; production CSP stays strict (no unsafe-eval).
const isDev = process.env.NODE_ENV !== "production";

const securityHeaders = [
  { key: "X-Content-Type-Options", value: "nosniff" },
  { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
  { key: "X-Frame-Options", value: "DENY" },
  {
    key: "Permissions-Policy",
    value: "camera=(), microphone=(), geolocation=(), interest-cohort=()",
  },
  { key: "Strict-Transport-Security", value: "max-age=63072000; includeSubDomains; preload" },
  {
    key: "Content-Security-Policy",
    value: [
      "default-src 'self'",
      // Allow any https image: the qwerti buy widget pulls token logos, chain
      // icons (relay.link), and provider logos from many third-party CDNs across
      // 70+ chains. img-src is the safest directive to broaden (images can't
      // execute). Scripts/connections stay strictly allowlisted below.
      "img-src 'self' data: blob: https:",
      "font-src 'self' data:",
      "style-src 'self' 'unsafe-inline'",
      // qwerti buy widget (/token) loads its loader + core script from *.qwerti.ai
      `script-src 'self' 'unsafe-inline' https://*.qwerti.ai${isDev ? " 'unsafe-eval'" : ""}`,
      // Solana RPC + Jupiter price + Reown AppKit/WalletConnect (config, relay, analytics)
      "connect-src 'self' https://docs.opencovenant.org https://api.opencovenant.org https://*.helius-rpc.com https://api.mainnet-beta.solana.com https://api.devnet.solana.com https://lite-api.jup.ag https://api.web3modal.org https://*.walletconnect.org https://*.walletconnect.com wss://*.helius-rpc.com wss://api.mainnet-beta.solana.com wss://api.devnet.solana.com wss://*.walletconnect.org wss://*.walletconnect.com https://*.qwerti.ai",
      // qwerti checkout may render in an embedded frame
      "frame-src 'self' https://*.qwerti.ai",
      "frame-ancestors 'none'",
      "base-uri 'self'",
      "form-action 'self'",
    ].join("; "),
  },
];

const config: NextConfig = {
  reactStrictMode: true,
  experimental: {
    // Turbopack's on-disk dev cache grew to 2.2 GB within a day, and dev
    // servers started on it spawned PostCSS workers until the machine ran out
    // of memory. A cold start costs well under a second per page.
    turbopackFileSystemCacheForDev: false,
  },
  async redirects() {
    // The compute post was published at /blog/inference-receipts, then lived at
    // /covenant-compute, before settling at /blog/compute. Keep the old links alive.
    return [
      { source: "/blog/inference-receipts", destination: "/blog/compute", permanent: true },
      { source: "/covenant-compute", destination: "/blog/compute", permanent: true },
    ];
  },
  productionBrowserSourceMaps: false,
  images: {
    formats: ["image/avif", "image/webp"],
    deviceSizes: [320, 640, 1280],
    imageSizes: [16, 32, 48, 64, 96, 128, 256],
    minimumCacheTTL: 31536000,
  },
  async headers() {
    return [
      {
        source: "/(logo|logomark).svg",
        headers: [{ key: "Cache-Control", value: "public, max-age=31536000, immutable" }],
      },
      {
        source: "/(sitemap.xml|docs/sitemap.xml)",
        headers: [
          { key: "Cache-Control", value: "public, max-age=3600, stale-while-revalidate=86400" },
        ],
      },
      { source: "/:path*", headers: securityHeaders },
      {
        // This self-contained Compute page uses inline code/data fonts and
        // only the same-origin public price-data routes. Keep its stricter
        // source policy: Next's configured headers override handler headers.
        source: "/compute-prices",
        headers: [
          { key: "Content-Security-Policy", value: "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; font-src data:; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'" },
          { key: "Referrer-Policy", value: "no-referrer" },
        ],
      },
    ];
  },
};

export default config;
