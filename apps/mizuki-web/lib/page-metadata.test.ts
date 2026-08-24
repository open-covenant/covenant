import { describe, expect, it } from 'vitest';
import { pageMetadata } from './page-metadata';

describe('pageMetadata', () => {
  it('publishes an absolute-ready canonical URL and large social card', () => {
    const metadata = pageMetadata({
      title: 'Submit an issue',
      description: 'Submit a scoped issue.',
      path: '/work',
    });

    expect(metadata.alternates).toEqual({ canonical: '/work' });
    expect(metadata.openGraph).toMatchObject({
      url: '/work',
      title: 'Submit an issue',
      images: [{ url: '/mizuki-og.png', width: 1200, height: 630 }],
    });
    expect(metadata.twitter).toMatchObject({
      card: 'summary_large_image',
      title: 'Submit an issue',
      images: [{ url: '/mizuki-og.png', width: 1200, height: 630 }],
    });
  });
});
