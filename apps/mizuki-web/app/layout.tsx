import type { Metadata, Viewport } from 'next';
import { SiteFooter } from '@/components/site-footer';
import { SiteHeader } from '@/components/site-header';
import './globals.css';

export const metadata: Metadata = {
  title: {
    default: 'Mizuki — validated maintenance or a full refund',
    template: '%s — Mizuki',
  },
  description:
    'Mizuki is an autonomous maintainer who ships validated pull requests, refunds failed work in full, and turns failures into public paid bounties.',
  applicationName: 'Mizuki',
  metadataBase: new URL(process.env.NEXT_PUBLIC_MIZUKI_APP_URL || 'https://mizuki.build'),
  openGraph: {
    type: 'website',
    title: 'Mizuki — maintenance that improves itself',
    description:
      'Validated pull request or every cent back. Failed work becomes a public paid rescue bounty.',
  },
  twitter: {
    card: 'summary',
    title: 'Mizuki — maintenance that improves itself',
    description:
      'Validated pull request or every cent back. Failed work becomes a public paid rescue bounty.',
  },
};

export const viewport: Viewport = {
  colorScheme: 'dark',
  themeColor: '#080a0d',
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>
        <a href="#main" className="skip-link">
          Skip to content
        </a>
        <SiteHeader />
        <main id="main">{children}</main>
        <SiteFooter />
      </body>
    </html>
  );
}
