# covenant — landing

Single-page teaser site. Black background, centered logo on top, hero image,
release date.

## Dev

    pnpm install
    pnpm dev          # http://localhost:3001

## Build

    pnpm build
    pnpm start

## Assets

Drop the real artwork over the placeholders in `public/`:

- `public/hero-bg.jpg` — full-bleed hero artwork. JPG keeps the file inline
  with the source asset (the AI/cyborg composite). For a smaller payload,
  regenerate AVIF + WebP siblings via sharp's `.avif({ quality: 50, effort: 6 })`
  and `.webp({ quality: 72, effort: 6 })` and update the preload `type=` in
  `app/layout.tsx`.
- `public/logo.svg` — top-centered wordmark.

The page reads them by these exact paths; no rewiring needed.

## Release date

`app/page.tsx` → `RELEASE_DATE` constant. Single line edit.
