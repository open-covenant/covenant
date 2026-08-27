import type { MetadataRoute } from 'next';

export default function manifest(): MetadataRoute.Manifest {
  return {
    name: 'Mizuki the Mech',
    short_name: 'Mizuki',
    description:
      'Fixed-price maintenance for authorized public GitHub issues, with a qualifying pull request or refund of the quoted USDC payment.',
    start_url: '/',
    display: 'standalone',
    background_color: '#07060b',
    theme_color: '#07060b',
    icons: [
      {
        src: '/mizuki-icon-192.png',
        sizes: '192x192',
        type: 'image/png',
      },
      {
        src: '/mizuki-icon-512.png',
        sizes: '512x512',
        type: 'image/png',
      },
    ],
  };
}
