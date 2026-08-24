import type { Metadata, Viewport } from 'next';
import { SiteFooter } from '@/components/site-footer';
import { SiteHeader } from '@/components/site-header';
import { getAdmission } from '@/lib/api';
import './globals.css';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = {
  title: {
    default: 'Mizuki the Mech — fixed-price GitHub maintenance',
    template: '%s — Mizuki the Mech',
  },
  description:
    'Mizuki the Mech handles clearly scoped issues in public GitHub repositories. Every confirmed payment remains covered until Mizuki opens a validated pull request or the quoted USDC amount is refunded.',
  applicationName: 'Mizuki the Mech',
  metadataBase: new URL(
    process.env.NEXT_PUBLIC_MIZUKI_APP_URL || 'https://mizuki.opencovenant.org',
  ),
  alternates: { canonical: '/' },
  openGraph: {
    type: 'website',
    siteName: 'Mizuki the Mech',
    url: '/',
    title: 'Mizuki the Mech — fixed-price GitHub maintenance',
    description:
      'Submit a scoped issue from a public GitHub repository. Receive a validated pull request reviewed by a separate AI reviewer, or a full refund of the quoted USDC payment.',
    images: [
      {
        url: '/mizuki-og.png',
        width: 1200,
        height: 630,
        alt: 'Mizuki the Mech — fixed-price GitHub maintenance',
      },
    ],
  },
  twitter: {
    card: 'summary_large_image',
    title: 'Mizuki the Mech — fixed-price GitHub maintenance',
    description:
      'Submit a scoped public GitHub issue. Receive a validated pull request reviewed by a separate AI reviewer, or a full refund of the quoted USDC payment.',
    images: ['/mizuki-og.png'],
  },
  icons: {
    icon: [{ url: '/mizuki-icon-64.png', type: 'image/png', sizes: '64x64' }],
    shortcut: '/mizuki-icon-64.png',
    apple: [{ url: '/mizuki-icon-180.png', type: 'image/png', sizes: '180x180' }],
  },
};

export const viewport: Viewport = {
  colorScheme: 'dark',
  themeColor: '#030303',
};

export default async function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  const admission = await getAdmission();
  const intakeOpen = admission.status !== 'error' && admission.data.intakeEnabled;

  return (
    <html lang="en">
      <body>
        <a href="#main" className="skip-link">
          Skip to content
        </a>
        <SiteHeader intakeOpen={intakeOpen} />
        <main id="main">{children}</main>
        <SiteFooter intakeOpen={intakeOpen} />
      </body>
    </html>
  );
}
