import type { MetadataRoute } from 'next';

export default function manifest(): MetadataRoute.Manifest {
  return {
    name: 'Mizuki',
    short_name: 'Mizuki',
    description: 'Validated maintenance or a full refund.',
    start_url: '/',
    display: 'standalone',
    background_color: '#090b0e',
    theme_color: '#090b0e',
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
