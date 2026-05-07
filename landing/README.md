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

- `public/hero.png` — full-bleed hero (currently a 1x1 black placeholder).
- `public/logo.svg` — top-centered wordmark (currently a typographic fallback).

The page reads them by these exact paths; no rewiring needed.

## Release date

`app/page.tsx` → `RELEASE_DATE` constant. Single line edit.
