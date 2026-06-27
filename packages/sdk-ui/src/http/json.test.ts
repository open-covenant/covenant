import { afterEach, describe, expect, it, vi } from 'vitest';
import { getJson } from './json.js';

// getJson only reads response.ok and response.text(), so a minimal Response
// stub drives every arm without a real network or DOM.
const resp = (ok: boolean, body: string): Response =>
  ({ ok, text: async () => body }) as unknown as Response;

const stubFetch = (r: Response) => {
  const fn = vi.fn(async () => r);
  vi.stubGlobal('fetch', fn);
  return fn;
};

afterEach(() => vi.unstubAllGlobals());

describe('getJson', () => {
  it('returns the parsed payload on a 2xx JSON response', async () => {
    stubFetch(resp(true, JSON.stringify({ value: 7 })));
    await expect(getJson('/x')).resolves.toEqual({ value: 7 });
  });

  it('sends credentials and a json content-type, merging caller headers', async () => {
    const fn = stubFetch(resp(true, '{}'));
    await getJson('/x', { headers: { 'x-test': '1' } });
    expect(fn).toHaveBeenCalledWith(
      '/x',
      expect.objectContaining({
        credentials: 'include',
        headers: expect.objectContaining({ 'content-type': 'application/json', 'x-test': '1' }),
      }),
    );
  });

  it('surfaces the server error field on a non-ok response', async () => {
    stubFetch(resp(false, JSON.stringify({ error: 'nope' })));
    await expect(getJson('/x')).rejects.toThrow('nope');
  });

  it('falls back to message, then to request failed, when no error field is present', async () => {
    stubFetch(resp(false, JSON.stringify({ message: 'bad' })));
    await expect(getJson('/x')).rejects.toThrow('bad');
    stubFetch(resp(false, JSON.stringify({ detail: 'x' })));
    await expect(getJson('/x')).rejects.toThrow('request failed');
  });

  it('throws invalid_json_response instead of silently coercing a 2xx non-JSON body', async () => {
    stubFetch(resp(true, '<html>not json</html>'));
    await expect(getJson('/x')).rejects.toThrow('invalid_json_response');
  });

  it('throws empty_response when a 2xx body is empty', async () => {
    stubFetch(resp(true, ''));
    await expect(getJson('/x')).rejects.toThrow('empty_response');
  });
});
