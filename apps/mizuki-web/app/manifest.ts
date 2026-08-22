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
  };
}
