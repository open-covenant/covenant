import type { NextConfig } from 'next';

const deploymentId =
  process.env.MIZUKI_DEPLOYMENT_ID?.trim() ||
  process.env.RENDER_GIT_COMMIT?.trim() ||
  'development';

const nextConfig: NextConfig = {
  output: 'standalone',
  poweredByHeader: false,
  reactStrictMode: true,
  deploymentId,
  env: {
    NEXT_PUBLIC_MIZUKI_BUILD_ID: deploymentId,
  },
  async headers() {
    return [
      {
        source: '/app/:path*',
        headers: [{ key: 'Cache-Control', value: 'private, no-store, max-age=0' }],
      },
      {
        source: '/api/mizuki/v1/account/:path*',
        headers: [{ key: 'Cache-Control', value: 'private, no-store, max-age=0' }],
      },
      {
        source: '/api/mizuki/v1/jobs/:path*',
        headers: [{ key: 'Cache-Control', value: 'private, no-store, max-age=0' }],
      },
      {
        source: '/:path*',
        headers: [
          { key: 'X-Content-Type-Options', value: 'nosniff' },
          { key: 'X-Frame-Options', value: 'DENY' },
          { key: 'Referrer-Policy', value: 'strict-origin-when-cross-origin' },
          { key: 'Permissions-Policy', value: 'camera=(), microphone=(), geolocation=()' },
        ],
      },
    ];
  },
};

export default nextConfig;
