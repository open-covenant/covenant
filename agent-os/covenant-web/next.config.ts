import type { NextConfig } from "next";

// `script-src`/`style-src` keep 'unsafe-inline': Next's App Router emits
// inline bootstrap/flight scripts and styled-jsx injects inline <style>
// tags, neither of which carry a nonce here. The directives that actually
// close the report's findings — frame-ancestors, object-src, base-uri —
// don't depend on inline allowances. challenges.cloudflare.com is whitelisted
// for the Turnstile script + iframe.
const csp = [
  "default-src 'self'",
  "base-uri 'self'",
  "object-src 'none'",
  "frame-ancestors 'none'",
  "form-action 'self'",
  "img-src 'self' data: https:",
  "font-src 'self' data:",
  "style-src 'self' 'unsafe-inline'",
  "script-src 'self' 'unsafe-inline' https://challenges.cloudflare.com",
  "connect-src 'self' https://challenges.cloudflare.com",
  "frame-src https://challenges.cloudflare.com",
  "worker-src 'self' blob:",
  "upgrade-insecure-requests",
].join("; ");

const securityHeaders = [
  { key: "Content-Security-Policy", value: csp },
  { key: "Strict-Transport-Security", value: "max-age=63072000; includeSubDomains; preload" },
  { key: "X-Frame-Options", value: "DENY" },
  { key: "X-Content-Type-Options", value: "nosniff" },
  { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
  { key: "Permissions-Policy", value: "camera=(), microphone=(), geolocation=(), browsing-topics=()" },
];

const config: NextConfig = {
  reactStrictMode: true,
  async headers() {
    return [{ source: "/:path*", headers: securityHeaders }];
  },
};

export default config;
