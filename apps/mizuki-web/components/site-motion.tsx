'use client';

import { useEffect } from 'react';

// Splits a heading into words wrapped in masks so they can be lifted into view
// one at a time. Walks text nodes in place so inline wrappers (the gradient
// span) survive the split. Runs on the client, so the markup stays plain HTML
// for crawlers and for anyone whose JS never executes.
function splitTextNode(node: Text, counter: { i: number }) {
  const words = (node.textContent ?? '').split(/(\s+)/);
  const frag = document.createDocumentFragment();
  for (const part of words) {
    if (!part) continue;
    if (/^\s+$/.test(part)) {
      frag.append(document.createTextNode(part));
      continue;
    }
    const mask = document.createElement('span');
    mask.className = 'word-mask';
    const inner = document.createElement('span');
    inner.className = 'word';
    inner.textContent = part;
    inner.style.setProperty('--word-delay', `${(counter.i += 1) * 0.055 - 0.055}s`);
    mask.append(inner);
    frag.append(mask);
  }
  node.replaceWith(frag);
}

function prepareWordReveal(el: HTMLElement) {
  if (el.dataset.split === 'true') return;
  const counter = { i: 0 };
  const walk = (parent: Node) => {
    for (const child of [...parent.childNodes]) {
      if (child.nodeType === Node.TEXT_NODE) {
        splitTextNode(child as Text, counter);
        continue;
      }
      if (child.nodeType !== Node.ELEMENT_NODE) continue;
      const el = child as HTMLElement;
      // background-clip:text cannot paint into nested spans, so a gradient
      // word is lifted whole rather than split.
      if (el.classList.contains('ramp-text')) {
        const mask = document.createElement('span');
        mask.className = 'word-mask';
        el.classList.add('word');
        el.style.setProperty('--word-delay', `${(counter.i += 1) * 0.055 - 0.055}s`);
        el.replaceWith(mask);
        mask.append(el);
        continue;
      }
      walk(el);
    }
  };
  walk(el);
  el.dataset.split = 'true';
}

// Mizuki's pipeline, read out around the cursor: how a scoped issue becomes a
// patch, passes the repository's checks, and clears a separate AI reviewer.
const READOUTS = [
  { slot: 'tl', label: 'SCOPE', speed: 1.0, cross: 0.6, phase: 0.0 },
  { slot: 'tr', label: 'PATCH', speed: 1.6, cross: -0.9, phase: 1.7 },
  { slot: 'bl', label: 'CHECKS', speed: -1.3, cross: 0.7, phase: 3.1 },
  { slot: 'br', label: 'REVIEW', speed: 2.1, cross: 1.2, phase: 4.6 },
];

export function SiteMotion() {
  useEffect(() => {
    const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    const topbar = document.querySelector<HTMLElement>('.site-header');
    let last = window.scrollY;

    const onScroll = () => {
      const y = window.scrollY;
      if (topbar) topbar.classList.toggle('is-scrolled', y > 40);

      last = y;
    };

    // Only hide-then-reveal once JS is running; without this the sections
    // stay at opacity 0 for anyone whose JS never executes.
    document.documentElement.classList.add('site-motion');

    if (!reduced) {
      document.querySelectorAll<HTMLElement>('[data-reveal-words]').forEach(prepareWordReveal);
    }

    const targets = document.querySelectorAll<HTMLElement>('.reveal, [data-reveal-words]');
    const io = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          entry.target.classList.add('is-in');
          io.unobserve(entry.target);
        }
      },
      { rootMargin: '0px 0px -12% 0px' },
    );
    targets.forEach((el) => io.observe(el));

    // Hero cursor: a difference-blended dot with a label, alive only while the
    // pointer is over the stage.
    const stage = document.querySelector<HTMLElement>('.hero-stage');
    let orb: HTMLElement | null = null;
    let orbFrame = 0;
    let ox = 0;
    let oy = 0;
    let tx = 0;
    let ty = 0;

    const onMove = (event: PointerEvent) => {
      tx = event.clientX;
      ty = event.clientY;
    };
    const onEnter = () => orb?.classList.add('is-visible');
    const onLeave = () => orb?.classList.remove('is-visible');

    const values = new Map<string, HTMLElement>();
    const shown = new Map<string, number>();

    const trackOrb = () => {
      ox += (tx - ox) * 0.16;
      oy += (ty - oy) * 0.16;
      // Exponential easing never actually arrives, which left the readouts
      // creeping forever after the pointer stopped. Snap once close.
      if (Math.abs(tx - ox) < 0.05) ox = tx;
      if (Math.abs(ty - oy) < 0.05) oy = ty;
      if (orb && stage) {
        orb.style.transform = `translate3d(${ox}px, ${oy}px, 0) translate(-50%, -50%)`;
        // The readouts track the pointer, not the clock: each eases toward a
        // value derived from where the cursor is, so they move while you move
        // and hold still the moment you stop.
        const nx = ox / Math.max(window.innerWidth, 1);
        const ny = oy / Math.max(window.innerHeight, 1);

        for (const r of READOUTS) {
          const el = values.get(r.slot);
          if (!el) continue;
          const target =
            0.5 + 0.5 * Math.sin((nx * r.speed + ny * r.cross) * Math.PI * 2 + r.phase);
          const current = shown.get(r.slot) ?? target;
          const delta = target - current;
          const next = Math.abs(delta) < 0.0005 ? target : current + delta * 0.13;
          shown.set(r.slot, next);
          const text = next.toFixed(2);
          if (el.textContent !== text) el.textContent = text;
        }
      }
      orbFrame = requestAnimationFrame(trackOrb);
    };

    const fine = window.matchMedia('(pointer: fine)').matches;
    if (!reduced && fine && stage) {
      orb = document.createElement('div');
      orb.className = 'hero-cursor';
      orb.setAttribute('aria-hidden', 'true');
      orb.innerHTML =
        '<span class="hero-cursor-cross-h"></span>' +
        '<span class="hero-cursor-cross-v"></span>' +
        READOUTS.map(
          (r) =>
            `<span class="hero-cursor-readout is-${r.slot}">${r.label} <b data-readout="${r.slot}">0.00</b></span>`,
        ).join('');
      document.body.append(orb);
      orb.querySelectorAll<HTMLElement>('[data-readout]').forEach((el) => {
        values.set(el.dataset.readout ?? '', el);
      });
      stage.addEventListener('pointermove', onMove, { passive: true });
      stage.addEventListener('pointerenter', onEnter);
      stage.addEventListener('pointerleave', onLeave);
      orbFrame = requestAnimationFrame(trackOrb);
    }

    window.addEventListener('scroll', onScroll, { passive: true });
    return () => {
      window.removeEventListener('scroll', onScroll);
      stage?.removeEventListener('pointermove', onMove);
      stage?.removeEventListener('pointerenter', onEnter);
      stage?.removeEventListener('pointerleave', onLeave);
      if (orbFrame) cancelAnimationFrame(orbFrame);
      orb?.remove();
      io.disconnect();
      document.documentElement.classList.remove('site-motion');
    };
  }, []);

  return null;
}
