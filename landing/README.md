# covenant — landing

opencovenant.org, docs.opencovenant.org (served from `app/docs`, rewritten by
`middleware.ts`), and the legacy stake host.

## Dev

    pnpm install
    pnpm dev          # http://localhost:3001

The docs host only resolves when the server listens on every interface (the
default for `next start`); bound to 127.0.0.1, the docs rewrite falls back to
`localhost` and loops into the apex `/docs` redirect.

## Build

    pnpm build
    pnpm check:cards  # every page has its own social card (CI runs this)
    pnpm start

## Social cards

Every page except the home page gets a generated share card at
`/og/<page path>.png` (for example `/og/brand/color.png`,
`/og/docs/concepts.png`). Pages export their metadata through `pageMetadata()`
in `app/_metadata.ts`; docs pages do it through `buildDocsMetadata()`. That
sets the canonical URL, the Open Graph and Twitter tags, the card image and its
alt text, and the card's headline and section label:

    export const metadata: Metadata = pageMetadata("/brand/color", { title: "Color", section: "Brand guidelines" }, {
      title: "Color: Covenant brand guidelines",
      description: "…",
    });

The headline defaults to the page title; pass the page's h1 when the title
carries a suffix. `app/og/[...path]/route.ts` draws the card on request: it
reads the page from this server (over the local port, with the Host header the
page is served under), takes the headline and section the page declares, and
sets them over a field background with the site's type (`app/og/_card.tsx`,
fonts and backgrounds in `app/og/_assets`). Cards are cached in memory and by
the CDN for a day.

`pnpm check:cards` fails when a page doesn't use the helper or a built page's
`og:image` doesn't point at its own card, so a new page can't ship without
one. The home page keeps its rendered share card (`app/opengraph-image.jpg`).

## Identity

The site has a dark theme only: light text on a black background
(`#000000`), with the Covenant gradient (violet → blue → mint) as the accent.
There is no light theme and no light surface anywhere — not a section, card,
button, modal or printout. The tokens live in `app/globals.css`:

- colors as `@theme` tokens: `ground` (page background), `sunken` (code,
  terminals), `raised` (cards, inputs), the text colors `fg`, `fg-2`,
  `fg-soft`, `muted`, `muted-2`, `faint`, the `line*` borders, and the gradient
  stops `violet`, `blue` and `mint`. `fg` is a text color only; nothing uses it
  as a background;
- `type-display` and `page-title` set Archivo Expanded (width 125, weight 600,
  −0.05em tracking, capitals); everything else is JetBrains Mono. Both load
  through `next/font` in `app/layout.tsx`;
- `.btn` with `btn-primary` (the gradient pill) and `btn-secondary` (outlined).

The header carries the lockup, live stats and a menu button; all navigation
lives in the side menu (`SiteMenu`), which opens from the right at every width.

The mark is inline (`CovenantMark` and `CovenantLockup` in `app/_brand.tsx`).
The hero animation (`ModuleField`) and the section dividers (`FieldSeam`) are
WebGL fields of small rounded squares drawn from the mark. They respond to
scroll and pointer, and the hero comes together into the mark as the page
scrolls. The intro transition (`ModuleCurtain`) and the reveal and cursor
script (`SiteMotion`) use the same shape. The shared GLSL and WebGL setup live
in `app/_field.ts`.

## Brand guidelines

`/brand` hosts the brand guidelines: messaging, logo, color, typography,
imagery, sub-brands, motion, partnerships and examples in use, ten pages under
`app/brand/` with a shared sidebar (`BrandNav`), layout components
(`_kit.tsx`), diagrams built from the mark's geometry (`_visuals.tsx`), live
demos (`_demos.tsx`) and the palette data (`_palette.ts`).
`public/brand/covenant-brand-assets.zip` is the downloadable kit; the In use
screenshots live in `public/brand/in-use/`.

## Assets

- `public/covenant-lockup.svg`, `covenant-lockup-white.svg` — mark and name,
  the name in light text (or both in white) and outlined from JetBrains Mono
  Medium so the files need no font.
- `public/covenant-mark.svg`, `covenant-mark-white.svg`, `covenant-mark.png` —
  the mark in the gradient, in white, and as a 1024px transparent PNG.
- `app/icon.png`, `app/apple-icon.png`, `app/favicon.ico`,
  `public/icon-maskable.png` — rendered from the mark, on the dark background
  where one is needed.
- `app/opengraph-image.jpg` / `app/twitter-image.jpg` — the home page's share
  card, rendered from the `ModuleField` shader with the site's type.
- `app/og/_assets/field-*.png` — the field alone on black, rendered from the
  same shader, behind the generated page cards. Archivo Expanded SemiBold and
  JetBrains Mono Medium sit beside them as static TTFs (SIL OFL) for the card
  renderer.

`public/logo.svg`, `logomark.svg`, `wordmark.png` and `covenant-logomark.png`
are the previous identity, kept only so existing outside links keep resolving.
Nothing on the site references them.
