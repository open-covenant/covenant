# Partner logos

Logos for the `/partners` page. Until a file exists here, each card falls back
to a clean monospace wordmark, so the page is complete without any assets.

## How to add a logo

1. Drop the file here named exactly `<slug>.svg` (SVG preferred; transparent
   PNG also works). The site is always dark (`#000000`) and never shows a
   white ground, so provide a **light / monochrome-on-dark** version of the mark.
2. In `app/_partners.ts`, set `logo: "/partners/<slug>.svg"` on that entry.

Marks are displayed at ~28px tall, `object-contain`, max width 160px — a
horizontal lockup or standalone wordmark reads best.

## Slugs

Protocols & standards:

- `solana`
- `x402`
- `mcp` (Model Context Protocol)
- `a2a` (Agent-to-Agent)

Integrations & partners:

- `synapse` (Synapse Agent Protocol)
- `hyre`
- `fairscale`
- `hermes` (Nous Research)
- `acedata` (Ace Data Cloud)
- `metaplex`
- `magicblock`
- `gitlawb`
- `xona` (Xona Agent)
- `orbserv`
