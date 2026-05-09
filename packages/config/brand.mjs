const DEFAULT_SITE_URL = 'https://opencovenant.org';
const DEFAULT_DOCS_URL = 'https://docs.opencovenant.org';
const DEFAULT_ANALYTICS_URL = 'https://analytics.opencovenant.org';
const DEFAULT_REPO_URL = 'https://github.com/open-covenant/covenant';
const DEFAULT_SECURITY_EMAIL = 'security@opencovenant.org';

function toNumber(raw, fallback) {
  const value = Number(raw);
  return Number.isFinite(value) ? value : fallback;
}

export const covenantBrand = Object.freeze({
  name: 'Covenant Protocol',
  shortName: 'Covenant',
  slug: 'covenant',
  npmScope: '@covenant',
  tagline: 'Solana-native agent coordination, settlement, and provenance.',
  networkTagline: 'Agent-native operating infrastructure with COVNT credits, staking, and receipt anchoring on Solana.',
  siteUrl: process.env.NEXT_PUBLIC_SITE_URL ?? process.env.COVENANT_SITE_URL ?? DEFAULT_SITE_URL,
  portalUrl: process.env.NEXT_PUBLIC_PORTAL_URL ?? process.env.COVENANT_PORTAL_URL ?? DEFAULT_SITE_URL,
  docsUrl: process.env.NEXT_PUBLIC_DOCS_URL ?? process.env.COVENANT_DOCS_URL ?? DEFAULT_DOCS_URL,
  analyticsUrl:
    process.env.NEXT_PUBLIC_ANALYTICS_URL ?? process.env.COVENANT_ANALYTICS_URL ?? DEFAULT_ANALYTICS_URL,
  repoUrl: process.env.COVENANT_REPO_URL ?? DEFAULT_REPO_URL,
  securityEmail: process.env.COVENANT_SECURITY_EMAIL ?? DEFAULT_SECURITY_EMAIL,
  socialHandle: process.env.NEXT_PUBLIC_COVENANT_X_HANDLE ?? '@opencovenant',
  cookies: Object.freeze({
    session: 'covenant_session',
    nonce: 'covenant_nonce',
  }),
  token: Object.freeze({
    name: process.env.NEXT_PUBLIC_COVENANT_TOKEN_NAME ?? process.env.COVENANT_TOKEN_NAME ?? 'Covenant Token',
    symbol: process.env.NEXT_PUBLIC_COVENANT_TOKEN_SYMBOL ?? process.env.COVENANT_TOKEN_SYMBOL ?? 'COVNT',
    decimals: toNumber(
      process.env.NEXT_PUBLIC_COVENANT_TOKEN_DECIMALS ?? process.env.COVENANT_TOKEN_DECIMALS,
      6,
    ),
  }),
});

export function resolveSiteUrl(input) {
  if (!input) return covenantBrand.siteUrl;
  try {
    return new URL(input).toString().replace(/\/$/, '');
  } catch {
    return covenantBrand.siteUrl;
  }
}

export function resolveBrandMetadata(overrides = {}) {
  return {
    ...covenantBrand,
    ...overrides,
    token: {
      ...covenantBrand.token,
      ...(overrides.token ?? {}),
    },
    cookies: {
      ...covenantBrand.cookies,
      ...(overrides.cookies ?? {}),
    },
  };
}
