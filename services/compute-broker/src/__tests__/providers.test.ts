import { afterEach, describe, expect, it, vi } from 'vitest';
import { createProviders, selectProvider } from '../providers.js';

const cfg = (over: Record<string, unknown> = {}) => ({
  ionetApiUrl: 'https://io.example',
  akashRpcUrl: 'https://akash.example',
  ...over,
});
const req = { gpuHours: 1, durationSecs: 60 };

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('createProviders fail-closed credential gate', () => {
  it('hard-fails every io.net lease op with an actionable hint when IONET_API_KEY is absent', async () => {
    const { ionet } = createProviders(cfg());
    await expect(ionet.reserve(req)).rejects.toThrow('io.net provider not configured: set IONET_API_KEY');
    await expect(ionet.cancel('lease-1')).rejects.toThrow('IONET_API_KEY');
  });

  it('hard-fails every akash lease op with an actionable hint when AKASH_WALLET is absent', async () => {
    const { akash } = createProviders(cfg());
    await expect(akash.reserve(req)).rejects.toThrow('akash provider not configured: set AKASH_WALLET');
    await expect(akash.reclaim('lease-1')).rejects.toThrow('AKASH_WALLET');
  });

  it('builds a live io.net provider that reaches the network once its credential is set', async () => {
    const fetchMock = vi.fn(async () => {
      throw new Error('NET_SENTINEL');
    });
    vi.stubGlobal('fetch', fetchMock);

    const { ionet } = createProviders(cfg({ ionetApiKey: 'secret-key' }));
    // The real provider reaches fetch (and surfaces the network error), the
    // fail-closed brick would reject with 'not configured' before any fetch.
    await expect(ionet.reserve(req)).rejects.toThrow('NET_SENTINEL');
    expect(fetchMock).toHaveBeenCalledOnce();
  });

  it('builds a live akash provider that reaches the network once its credential is set', async () => {
    const fetchMock = vi.fn(async () => {
      throw new Error('NET_SENTINEL');
    });
    vi.stubGlobal('fetch', fetchMock);

    const { akash } = createProviders(cfg({ akashWallet: 'wallet-1' }));
    await expect(akash.reserve(req)).rejects.toThrow('NET_SENTINEL');
    expect(fetchMock).toHaveBeenCalledOnce();
  });
});

describe('selectProvider routing', () => {
  it('routes a requested provider name to its own backend, never the other', () => {
    const providers = createProviders(cfg({ ionetApiKey: 'k', akashWallet: 'w' }));
    expect(selectProvider('ionet', providers)).toBe(providers.ionet);
    expect(selectProvider('akash', providers)).toBe(providers.akash);
  });
});
