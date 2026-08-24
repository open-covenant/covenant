import { afterEach, describe, expect, it, vi } from 'vitest';
import { RenderClient } from './render.js';

const digest = `sha256:${'a'.repeat(64)}`;
const imageRef = `ghcr.io/open-covenant/mizuki-api@${digest}`;

describe('Render API client', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('deploys only an exact OCI digest to an allowlisted service', async () => {
    const request = vi.fn(async () =>
      Response.json(deploy('dep-candidate', imageRef), { status: 201 }),
    );
    vi.stubGlobal('fetch', request);
    const client = createClient();

    await expect(client.deployImage('srv-shadow123', imageRef)).resolves.toMatchObject({
      id: 'dep-candidate',
      image: { ref: imageRef, sha: digest },
    });
    expect(request).toHaveBeenCalledWith(
      'https://api.render.com/v1/services/srv-shadow123/deploys',
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({ imageUrl: imageRef }),
      }),
    );
  });

  it('treats queued acceptance and conflicts as uncertain mutations', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(null, { status: 202 })),
    );
    await expect(createClient().deployImage('srv-shadow123', imageRef)).resolves.toBeNull();

    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(null, { status: 409, headers: { 'retry-after': '9' } })),
    );
    await expect(createClient().deployImage('srv-shadow123', imageRef)).rejects.toMatchObject({
      code: 'render_mutation_conflict',
      status: 503,
      retryable: true,
      retryAfterSeconds: 9,
    });
  });

  it('never sends a request for an unlisted service', async () => {
    const request = vi.fn();
    vi.stubGlobal('fetch', request);
    await expect(createClient().deployImage('srv-other123', imageRef)).rejects.toMatchObject({
      code: 'render_service_denied',
    });
    expect(request).not.toHaveBeenCalled();
  });

  it('does not expose the API key in upstream failures', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response('credential echoed', { status: 500 })),
    );
    const error = await createClient()
      .service('srv-shadow123')
      .catch((value) => value);
    expect(error.message).toBe('Render API returned 500');
    expect(JSON.stringify(error)).not.toContain('render-secret');
  });
});

function createClient(): RenderClient {
  return new RenderClient({
    apiUrl: 'https://api.render.com/v1',
    apiKey: 'render-secret',
    allowedServiceIds: new Set(['srv-shadow123', 'srv-production123']),
    timeoutMs: 1_000,
  });
}

function deploy(id: string, ref: string) {
  return {
    id,
    commit: null,
    image: { ref, sha: ref.slice(ref.indexOf('@') + 1) },
    status: 'created',
    trigger: 'api',
    createdAt: '2026-08-23T12:00:00.000Z',
    finishedAt: null,
  };
}
