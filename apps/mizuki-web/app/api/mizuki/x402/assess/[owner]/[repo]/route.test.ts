import { afterEach, describe, expect, it, vi } from 'vitest';
import { GET } from './route';

const params = (owner: string, repo: string) => ({ params: Promise.resolve({ owner, repo }) });

afterEach(() => {
  vi.restoreAllMocks();
});

describe('priced assessment route', () => {
  it('passes the payment challenge back unchanged so a caller can settle it', async () => {
    const challenge = 'eyJ4NDAyVmVyc2lvbiI6Mn0=';
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response('{}', {
        status: 402,
        headers: { 'content-type': 'application/json', 'payment-required': challenge },
      }),
    );

    const response = await GET(
      new Request('https://mizuki.test/'),
      params('open-covenant', 'covenant'),
    );

    expect(response.status).toBe(402);
    expect(response.headers.get('payment-required')).toBe(challenge);
  });

  it('forwards the payment signature to the service that issued the challenge', async () => {
    const fetchMock = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValue(new Response('{}', { status: 200 }));

    await GET(
      new Request('https://mizuki.test/', { headers: { 'payment-signature': 'signed' } }),
      params('open-covenant', 'covenant'),
    );

    const [target, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(target).toBe(
      'https://covenant-x402-seller.onrender.com/x402/mizuki/assess/open-covenant/covenant',
    );
    expect(new Headers(init.headers).get('payment-signature')).toBe('signed');
  });

  it('refuses path segments that are not GitHub names', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch');

    const response = await GET(new Request('https://mizuki.test/'), params('..', 'covenant'));

    expect(response.status).toBe(400);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('reports an unreachable assessment service as a gateway failure', async () => {
    vi.spyOn(globalThis, 'fetch').mockRejectedValue(new Error('connect ECONNREFUSED'));

    const response = await GET(
      new Request('https://mizuki.test/'),
      params('open-covenant', 'covenant'),
    );

    expect(response.status).toBe(502);
  });
});
