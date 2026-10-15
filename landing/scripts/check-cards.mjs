#!/usr/bin/env node
// Fails when a page would be shared without its own social card.
//
// 1. Source: every app/**/page.tsx exports its metadata through pageMetadata()
//    (docs pages: buildDocsMetadata()), directly or from a layout in the same
//    folder. Redirect-only pages and the home page are exempt. This covers
//    dynamic pages, which the build does not prerender.
// 2. Build output (after `next build`): every prerendered page's og:image and
//    twitter:image point at its own card, and the page declares a card title.
//
// usage: node scripts/check-cards.mjs   (from landing/, after next build)

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";

const ROOT = new URL("..", import.meta.url).pathname;
const APP = join(ROOT, "app");
const BUILD = join(ROOT, ".next/server/app");
const SITE_URL = "https://opencovenant.org";
const HOME_CARD = `${SITE_URL}/opengraph-image.jpg`;
const HELPERS = /\b(pageMetadata|buildDocsMetadata)\(/;

const failures = [];

function walk(dir, visit) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) {
      if (name !== "node_modules" && !name.startsWith(".")) walk(p, visit);
    } else visit(p);
  }
}

function routeOf(file) {
  const segments = relative(APP, dirname(file))
    .split(sep)
    .filter((s) => s && !/^\(.*\)$/.test(s) && !s.startsWith("@"));
  return `/${segments.join("/")}`;
}

// 1. Source
let pages = 0;
walk(APP, (file) => {
  if (!/[\\/]page\.(tsx|ts|jsx|js|mdx)$/.test(file)) return;
  pages++;
  const route = routeOf(file);
  if (route === "/") return;
  const src = readFileSync(file, "utf8");
  if (HELPERS.test(src)) return;
  const layout = ["layout.tsx", "layout.ts"].map((n) => join(dirname(file), n)).find(existsSync);
  if (layout && HELPERS.test(readFileSync(layout, "utf8"))) return;
  const redirectOnly = /\b(permanentRedirect|redirect)\(/.test(src) && !/<[A-Za-z]/.test(src);
  if (redirectOnly) return;
  failures.push(`${relative(ROOT, file)}: export the page's metadata through pageMetadata() from app/_metadata.ts`);
});

// 2. Build output
function meta(html, key) {
  for (const m of html.matchAll(/<meta\s+([^>]*?)\/?>/gi)) {
    const attrs = Object.fromEntries([...m[1].matchAll(/([a-zA-Z:-]+)="([^"]*)"/g)].map(([, k, v]) => [k.toLowerCase(), v]));
    if ((attrs.property ?? attrs.name) === key) return attrs.content?.replace(/&amp;/g, "&");
  }
  return undefined;
}

let checked = 0;
if (existsSync(BUILD)) {
  walk(BUILD, (file) => {
    if (!file.endsWith(".html")) return;
    const route = `/${relative(BUILD, file).replace(/\.html$/, "").split(sep).join("/")}`;
    if (route === "/_not-found" || route === "/_global-error" || route.startsWith("/_")) return;
    const metaFile = file.replace(/\.html$/, ".meta");
    const status = existsSync(metaFile) ? (JSON.parse(readFileSync(metaFile, "utf8")).status ?? 200) : 200;
    if (status >= 300 && status < 400) return;
    checked++;
    const html = readFileSync(file, "utf8");
    const page = route === "/index" ? "/" : route;
    const og = meta(html, "og:image");
    const tw = meta(html, "twitter:image");
    if (page === "/") {
      // The home page keeps its own share card (app/opengraph-image.jpg, app/twitter-image.jpg).
      if (!og?.startsWith(HOME_CARD)) failures.push(`/: og:image is ${og ?? "missing"}, expected ${HOME_CARD}`);
      if (!tw?.startsWith(`${SITE_URL}/twitter-image.jpg`)) failures.push(`/: twitter:image is ${tw ?? "missing"}`);
      return;
    }
    const expected = `${SITE_URL}/og${page}.png`;
    if (og !== expected) failures.push(`${page}: og:image is ${og ?? "missing"}, expected ${expected}`);
    if (tw !== expected) failures.push(`${page}: twitter:image is ${tw ?? "missing"}, expected ${expected}`);
    if (page !== "/" && !meta(html, "covenant:card-title")) failures.push(`${page}: no card title (covenant:card-title)`);
  });
} else {
  console.warn("check-cards: no build output in .next/server/app; checked sources only. Run next build first.");
}

if (failures.length) {
  console.error(`check-cards: ${failures.length} problem(s)\n${failures.map((f) => `  - ${f}`).join("\n")}`);
  console.error("\nEvery page needs its own social card. See app/_metadata.ts.");
  process.exit(1);
}
console.log(`check-cards: ${pages} pages use generated cards; ${checked} prerendered pages carry the right image tags.`);
