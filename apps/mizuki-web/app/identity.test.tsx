import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it, vi } from 'vitest';
import { SiteFooter } from '@/components/site-footer';
import { SiteHeader } from '@/components/site-header';
import { metadata } from './layout';
import manifest from './manifest';

vi.mock('server-only', () => ({}));

const avatarPath = '/mizuki-avatar.jpg';

describe('Mizuki public identity', () => {
  it('keeps the verified profile image unchanged', () => {
    const avatar = readFileSync(new URL('../public/mizuki-avatar.jpg', import.meta.url));

    expect(createHash('sha256').update(avatar).digest('hex')).toBe(
      'f01397753222adc7ab4aebc784cd3ae7faec557f73a4d4a6f2c1f1b7f0423505',
    );
  });

  it('uses the profile image for visible identity and social previews', () => {
    expect(renderToStaticMarkup(<SiteHeader intakeOpen />)).toContain('mizuki-avatar.jpg');
    expect(renderToStaticMarkup(<SiteFooter intakeOpen />)).toContain('mizuki-avatar.jpg');
    expect(metadata.openGraph).toMatchObject({
      images: [{ url: avatarPath, width: 400, height: 400, alt: 'Mizuki the Mech' }],
    });
    expect(metadata.twitter).toMatchObject({
      card: 'summary',
      images: [avatarPath],
    });
  });

  it('links to the official X profile', () => {
    const footer = renderToStaticMarkup(<SiteFooter intakeOpen />);

    expect(footer).toContain('href="https://x.com/MizukiMech"');
    expect(footer).toContain('Follow on X');
  });

  it('links to the official ClawPump marketplace profile', () => {
    const footer = renderToStaticMarkup(<SiteFooter intakeOpen />);

    expect(footer).toContain(
      'href="https://clawpump.tech/marketplace/agents/711fa8b1-5f37-4451-b7a7-bfcb9a021f6d"',
    );
    expect(footer).toContain('>ClawPump</a>');
  });

  it('replaces transactional calls to action when paid intake is closed', () => {
    const header = renderToStaticMarkup(<SiteHeader intakeOpen={false} />);
    const footer = renderToStaticMarkup(<SiteFooter intakeOpen={false} />);

    expect(header).toContain('View service status');
    expect(header).not.toContain('>Service status</a>');
    expect(header).not.toContain('Request a quote');
    expect(footer).toContain('Service status');
    expect(footer).not.toContain('Submit an issue');
  });

  it('publishes browser, Apple, and installable app icons', () => {
    expect(metadata.icons).toEqual({
      icon: [{ url: '/mizuki-icon-64.png', type: 'image/png', sizes: '64x64' }],
      shortcut: '/mizuki-icon-64.png',
      apple: [{ url: '/mizuki-icon-180.png', type: 'image/png', sizes: '180x180' }],
    });
    expect(manifest().icons).toEqual([
      { src: '/mizuki-icon-192.png', sizes: '192x192', type: 'image/png' },
      { src: '/mizuki-icon-512.png', sizes: '512x512', type: 'image/png' },
    ]);
  });
});
