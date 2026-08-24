import type { Metadata } from 'next';

type PageMetadata = {
  title: string;
  description: string;
  path: string;
};

const image = {
  url: '/mizuki-og.png',
  width: 1200,
  height: 630,
  alt: 'Mizuki the Mech — fixed-price GitHub maintenance',
};

export function pageMetadata({ title, description, path }: PageMetadata): Metadata {
  return {
    title,
    description,
    alternates: { canonical: path },
    openGraph: {
      type: 'website',
      siteName: 'Mizuki the Mech',
      url: path,
      title,
      description,
      images: [image],
    },
    twitter: {
      card: 'summary_large_image',
      title,
      description,
      images: [image],
    },
  };
}
