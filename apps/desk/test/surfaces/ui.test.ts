import { describe, expect, it } from 'vitest';
import { UI_REFRESH_MS, createUiSurface } from '../../src/surfaces/ui/index.js';

const ui = createUiSurface();

describe('the page', () => {
  it('loads its own script and stylesheet, and nothing from elsewhere', () => {
    const html = ui.html();
    expect(html).toContain('<link rel="stylesheet" href="/app.css">');
    expect(html).toContain('<script src="/app.js"');
    expect(html).not.toMatch(/https?:\/\/(?!www\.w3\.org)/);
  });

  it('carries every section the desk needs', () => {
    const html = ui.html();
    for (const id of [
      'fact-session',
      'fact-block',
      'fact-mode',
      'fact-uptime',
      'premium-rows',
      'quote-form',
      'orders-rows',
      'hedge-form',
      'sparkline',
      'connect',
    ]) {
      expect(html).toContain(`id="${id}"`);
    }
  });

  it('refreshes every fifteen seconds', () => {
    expect(UI_REFRESH_MS).toBe(15_000);
    expect(ui.script()).toContain('const REFRESH_MS = 15000;');
  });

  it('reads the desk through the same API as everything else, with a bearer token', () => {
    const script = ui.script();
    for (const route of ['/v1/status', '/v1/premium', '/v1/orders', '/v1/hedge', '/v1/recorder']) {
      expect(script).toContain(route);
    }
    expect(script).toContain("'Bearer '");
    expect(script).toContain('DELETE');
  });

  it('draws the premium chart as inline SVG', () => {
    const script = ui.script();
    expect(script).toContain("createElementNS(ns, 'svg')");
    expect(script).toContain("createElementNS(ns, 'path')");
    expect(script).toContain('DAY_MS');
  });

  it('gives buttons a pointer cursor', () => {
    const styles = ui.styles();
    expect(styles).toMatch(/button\s*\{[^}]*cursor: pointer/);
  });

  it('shows a sub-cent price instead of rounding it to zero', () => {
    const script = ui.script();
    const source = script.slice(script.indexOf('function num('), script.indexOf('function bps('));
    const usd = new Function(`${source} return usd;`)() as (quantity: { value: number }) => string;

    expect(usd({ value: 5.882169420526356e-8 })).toBe('$0.00000005882');
    expect(usd({ value: 0.25 })).toBe('$0.2500');
    expect(usd({ value: 230.68 })).toBe('$230.68');
  });

  it('reads as product copy', () => {
    const copy = ui.html().replace(/<[^>]+>/g, ' ');
    expect(copy).not.toMatch(/—/);
    expect(copy).not.toMatch(/\b(canary|rollout|feature flag|shadow mode|promotion gate|backfill|refactor)\b/i);
    expect(copy).not.toMatch(/\b(institutional-grade|enterprise-grade|risk-free|guaranteed|seamless|robust)\b/i);
    expect(copy).toContain('dry runs until live execution is turned on');
  });
});
