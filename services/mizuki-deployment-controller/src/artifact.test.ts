import { createHash } from 'node:crypto';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { HttpArtifactGateway } from './artifact.js';

const mediaType = 'application/vnd.oci.image.manifest.v1+json';
const commit = 'a'.repeat(40);
const releaseUrl =
  `https://github.com/open-covenant/covenant/releases/download/mizuki-image-${commit}` +
  '/manifest.oci.json';
const releaseAssetOrigin = 'https://release-assets.githubusercontent.com';

describe('artifact verification', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('requires exactly the GitHub release and release asset origins', () => {
    expect(() => new HttpArtifactGateway(new Set(['https://github.com']), 1_000)).toThrow(
      'Artifact origins must be the GitHub release and release asset origins',
    );
    expect(
      () =>
        new HttpArtifactGateway(
          new Set([
            'https://github.com',
            releaseAssetOrigin,
            'https://objects.githubusercontent.com',
          ]),
          1_000,
        ),
    ).toThrow('Artifact origins must be the GitHub release and release asset origins');
  });

  it('follows the exact release redirect and verifies an OCI manifest identity', async () => {
    const body = manifestBytes();
    const fetch = releaseFetch(
      releaseAssetUrl(),
      new Response(new Uint8Array(body), {
        headers: { 'content-length': String(body.length) },
      }),
    );
    vi.stubGlobal('fetch', fetch);

    const sha256 = hash(body);
    await expect(createVerifier().verify(releaseUrl, sha256, body.length)).resolves.toEqual({
      sha256,
      sizeBytes: body.length,
      mediaType,
    });
    expect(fetch).toHaveBeenCalledTimes(2);
    for (const [, init] of fetch.mock.calls) {
      expect(new Headers(init?.headers).has('authorization')).toBe(false);
      expect(init?.redirect).toBe('manual');
    }
  });

  it('rejects bytes that are not an OCI image manifest', async () => {
    const body = Buffer.from('reviewed archive');
    vi.stubGlobal('fetch', releaseFetch(releaseAssetUrl(), new Response(new Uint8Array(body))));
    await expect(
      createVerifier().verify(releaseUrl, hash(body), body.length),
    ).rejects.toMatchObject({ code: 'artifact_not_oci_manifest' });
  });

  it('rejects incorrect size and hash before parsing the manifest', async () => {
    const body = manifestBytes();
    vi.stubGlobal('fetch', releaseFetch(releaseAssetUrl(), new Response(new Uint8Array(body))));
    await expect(
      createVerifier().verify(releaseUrl, hash(body), body.length + 1),
    ).rejects.toMatchObject({ code: 'artifact_size_mismatch' });

    vi.stubGlobal('fetch', releaseFetch(releaseAssetUrl(), new Response(new Uint8Array(body))));
    await expect(
      createVerifier().verify(releaseUrl, '0'.repeat(64), body.length),
    ).rejects.toMatchObject({ code: 'artifact_hash_mismatch' });
  });

  it('rejects malformed release inputs before making a request', async () => {
    const fetch = vi.fn();
    vi.stubGlobal('fetch', fetch);
    const invalid = [
      releaseUrl.replace('open-covenant/covenant', 'open-covenant/elsewhere'),
      releaseUrl.replace(`mizuki-image-${commit}`, 'mizuki-image-v1'),
      releaseUrl.replace('manifest.oci.json', 'promotion-input.json'),
      `${releaseUrl}?download=1`,
      `${releaseUrl}#manifest`,
      releaseUrl.replace('https://', 'https://token@'),
      releaseUrl.replace('https://github.com', 'https://objects.githubusercontent.com'),
    ];

    for (const value of invalid) {
      await expect(createVerifier().verify(value, 'a'.repeat(64), 10)).rejects.toMatchObject({
        code: 'artifact_origin_denied',
      });
    }
    expect(fetch).not.toHaveBeenCalled();
  });

  it('rejects cross-origin, cross-repository, and cross-asset redirects', async () => {
    const invalid = [
      releaseAssetUrl().replace(releaseAssetOrigin, 'https://objects.githubusercontent.com'),
      releaseAssetUrl().replace('/1219904470/', '/212613049/'),
      releaseAssetUrl().replace(
        'attachment%3B+filename%3Dmanifest.oci.json',
        'attachment%3B+filename%3Dpromotion-input.json',
      ),
      releaseAssetUrl().replace('sp=r&', ''),
      `${releaseAssetUrl()}#fragment`,
      releaseAssetUrl().replace('https://', 'https://token@'),
    ];
    const fetch = vi.fn();
    for (const location of invalid) {
      fetch.mockResolvedValueOnce(new Response(null, { status: 302, headers: { location } }));
    }
    vi.stubGlobal('fetch', fetch);

    for (const _location of invalid) {
      await expect(createVerifier().verify(releaseUrl, 'a'.repeat(64), 10)).rejects.toMatchObject({
        code: 'artifact_redirect_invalid',
      });
    }
    expect(fetch).toHaveBeenCalledTimes(invalid.length);
  });

  it('rejects a missing redirect and a second redirect', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response('not redirected')),
    );
    await expect(createVerifier().verify(releaseUrl, 'a'.repeat(64), 10)).rejects.toMatchObject({
      code: 'artifact_request_failed',
      retryable: false,
    });

    const fetch = releaseFetch(
      releaseAssetUrl(),
      new Response(null, { status: 302, headers: { location: releaseAssetUrl() } }),
    );
    vi.stubGlobal('fetch', fetch);
    await expect(createVerifier().verify(releaseUrl, 'a'.repeat(64), 10)).rejects.toMatchObject({
      code: 'artifact_redirect_limit',
      retryable: false,
    });
  });

  it('preserves retryable classification for transient failures', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(null, { status: 503 })),
    );
    await expect(createVerifier().verify(releaseUrl, 'a'.repeat(64), 10)).rejects.toMatchObject({
      code: 'artifact_request_failed',
      retryable: true,
      status: 503,
    });

    vi.stubGlobal('fetch', releaseFetch(releaseAssetUrl(), new Response(null, { status: 503 })));
    await expect(createVerifier().verify(releaseUrl, 'a'.repeat(64), 10)).rejects.toMatchObject({
      code: 'artifact_request_failed',
      retryable: true,
      status: 503,
    });
  });
});

function createVerifier(): HttpArtifactGateway {
  return new HttpArtifactGateway(new Set(['https://github.com', releaseAssetOrigin]), 1_000);
}

function releaseFetch(location: string, asset: Response) {
  return vi
    .fn<typeof fetch>()
    .mockResolvedValueOnce(new Response(null, { status: 302, headers: { location } }))
    .mockResolvedValueOnce(asset);
}

function releaseAssetUrl(): string {
  const query = new URLSearchParams({
    sp: 'r',
    sv: '2018-11-09',
    sr: 'b',
    spr: 'https',
    se: '2026-08-23T20:00:00Z',
    rscd: 'attachment; filename=manifest.oci.json',
    rsct: 'application/octet-stream',
    sig: 'signed-value',
    'response-content-disposition': 'attachment; filename=manifest.oci.json',
    'response-content-type': 'application/octet-stream',
  });
  return (
    `${releaseAssetOrigin}/github-production-release-asset/1219904470/` +
    `123e4567-e89b-12d3-a456-426614174000?${query}`
  );
}

function manifestBytes(): Buffer {
  return Buffer.from(
    JSON.stringify({
      schemaVersion: 2,
      mediaType,
      config: { digest: `sha256:${'1'.repeat(64)}`, size: 100 },
      layers: [{ digest: `sha256:${'2'.repeat(64)}`, size: 200 }],
    }),
  );
}

function hash(value: Buffer): string {
  return createHash('sha256').update(value).digest('hex');
}
