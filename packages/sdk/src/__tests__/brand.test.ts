import { describe, expect, it } from 'vitest';
import {
  covenantBrand,
  resolveBrandMetadata,
  resolveSiteUrl,
  type CovenantBrandMetadata,
} from '@covenant/config/brand';

// brand.mjs is the source of truth for site URLs, cookie names, the security
// contact, and COVNT token metadata. resolveSiteUrl is the only seam that
// normalizes URL-ish input and resolveBrandMetadata is the only override seam:
// both must fail closed (bad input -> trusted default) and a partial nested
// override must never silently drop the sibling fields next to it.

describe('resolveSiteUrl', () => {
  it('falls back to the brand site URL for missing input', () => {
    expect(resolveSiteUrl()).toBe(covenantBrand.siteUrl);
    expect(resolveSiteUrl('')).toBe(covenantBrand.siteUrl);
  });

  it('normalizes a valid URL and strips the trailing slash', () => {
    expect(resolveSiteUrl('https://example.com/')).toBe('https://example.com');
    expect(resolveSiteUrl('https://example.com')).toBe('https://example.com');
  });

  it('fails closed to the brand site URL on a malformed URL', () => {
    expect(resolveSiteUrl('not a url')).toBe(covenantBrand.siteUrl);
    expect(resolveSiteUrl('http://')).toBe(covenantBrand.siteUrl);
  });
});

describe('resolveBrandMetadata', () => {
  it('returns the full brand metadata when no overrides are given', () => {
    expect(resolveBrandMetadata()).toEqual(covenantBrand);
  });

  it('applies top-level overrides without dropping sibling fields', () => {
    const meta = resolveBrandMetadata({ siteUrl: 'https://override.test' });
    expect(meta.siteUrl).toBe('https://override.test');
    expect(meta.name).toBe(covenantBrand.name);
    expect(meta.securityEmail).toBe(covenantBrand.securityEmail);
  });

  it('merges a partial token override without dropping sibling token fields', () => {
    const meta = resolveBrandMetadata({
      token: { symbol: 'XYZ' } as CovenantBrandMetadata['token'],
    });
    expect(meta.token.symbol).toBe('XYZ');
    expect(meta.token.name).toBe(covenantBrand.token.name);
    expect(meta.token.decimals).toBe(covenantBrand.token.decimals);
  });

  it('merges a partial cookies override without dropping sibling cookie fields', () => {
    const meta = resolveBrandMetadata({
      cookies: { session: 'alt_session' } as CovenantBrandMetadata['cookies'],
    });
    expect(meta.cookies.session).toBe('alt_session');
    expect(meta.cookies.nonce).toBe(covenantBrand.cookies.nonce);
  });
});

describe('covenantBrand', () => {
  it('freezes the brand singleton and its nested config against runtime mutation', () => {
    expect(Object.isFrozen(covenantBrand)).toBe(true);
    expect(Object.isFrozen(covenantBrand.token)).toBe(true);
    expect(Object.isFrozen(covenantBrand.cookies)).toBe(true);
  });
});
