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
      // walletconnect/web3modal serve wallet logos over https for the AppKit modal
      "img-src 'self' data: blob: https://api.web3modal.org https://*.walletconnect.com https://*.qwerti.ai",
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
        source: "/hero-bg.(jpg|webp|avif)",
        headers: [{ key: "Cache-Control", value: "public, max-age=31536000, immutable" }],
      },
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
    ];
  },
};

export default config;
