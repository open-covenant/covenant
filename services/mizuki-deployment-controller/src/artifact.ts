import { createHash } from 'node:crypto';
import { z } from 'zod';
import { ControllerError, ociDigest } from './domain.js';

const releaseOrigin = 'https://github.com';
const releaseAssetOrigin = 'https://release-assets.githubusercontent.com';
const releaseAssetRepositoryId = '1219904470';
const releasePath = new RegExp(
  '^/open-covenant/covenant/releases/download/mizuki-image-[0-9a-f]{40}/manifest\\.oci\\.json$',
);
const releaseAssetPath = new RegExp(
  `^/github-production-release-asset/${releaseAssetRepositoryId}/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`,
);
const redirectStatuses = new Set([301, 302, 303, 307, 308]);

const descriptor = z
  .object({
    mediaType: z.string().min(1).max(200).optional(),
    digest: ociDigest,
    size: z.number().int().nonnegative(),
    annotations: z.record(z.string(), z.string()).optional(),
    urls: z.array(z.string().url()).optional(),
  })
  .passthrough();
const manifest = z
  .object({
    schemaVersion: z.literal(2),
    mediaType: z
      .enum([
        'application/vnd.oci.image.manifest.v1+json',
        'application/vnd.docker.distribution.manifest.v2+json',
      ])
      .optional(),
    config: descriptor,
    layers: z.array(descriptor).min(1).max(10_000),
    annotations: z.record(z.string(), z.string()).optional(),
  })
  .strict();

export interface ArtifactReceipt {
  sha256: string;
  sizeBytes: number;
  mediaType: string;
}

export interface ArtifactGateway {
  verify(url: string, expectedSha256: string, expectedSizeBytes: number): Promise<ArtifactReceipt>;
}

export class HttpArtifactGateway implements ArtifactGateway {
  constructor(
    private readonly allowedOrigins: Set<string>,
    private readonly timeoutMs: number,
  ) {
    assertReleaseOrigins(allowedOrigins);
  }

  async verify(
    inputUrl: string,
    expectedSha256: string,
    expectedSizeBytes: number,
  ): Promise<ArtifactReceipt> {
    const releaseUrl = this.releaseUrl(inputUrl);
    const redirect = await this.request(releaseUrl);
    if (!redirectStatuses.has(redirect.status)) {
      await redirect.body?.cancel();
      this.throwRequestFailure(redirect, 'Artifact endpoint did not return the required redirect');
    }
    const location = redirect.headers.get('location');
    await redirect.body?.cancel();
    if (!location) {
      throw new ControllerError('artifact_redirect_invalid', 'Artifact redirect is invalid', 422);
    }
    const assetUrl = this.releaseAssetUrl(location);
    const response = await this.request(assetUrl);
    if (redirectStatuses.has(response.status)) {
      await response.body?.cancel();
      throw new ControllerError(
        'artifact_redirect_limit',
        'Artifact may redirect exactly once',
        422,
      );
    }
    if (!response.ok || !response.body) {
      this.throwRequestFailure(response, `Artifact endpoint returned ${response.status}`);
    }
    const contentLength = response.headers.get('content-length');
    if (contentLength !== null && Number(contentLength) !== expectedSizeBytes) {
      await response.body.cancel();
      throw new ControllerError('artifact_size_mismatch', 'Artifact length does not match', 422);
    }

    const reader = response.body.getReader();
    const hash = createHash('sha256');
    const chunks: Uint8Array[] = [];
    let sizeBytes = 0;
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      sizeBytes += value.byteLength;
      if (sizeBytes > expectedSizeBytes) {
        await reader.cancel();
        throw new ControllerError('artifact_size_mismatch', 'Artifact length does not match', 422);
      }
      hash.update(value);
      chunks.push(value);
    }
    if (sizeBytes !== expectedSizeBytes) {
      throw new ControllerError('artifact_size_mismatch', 'Artifact length does not match', 422);
    }
    const sha256 = hash.digest('hex');
    if (sha256 !== expectedSha256) {
      throw new ControllerError('artifact_hash_mismatch', 'Artifact SHA-256 does not match', 422);
    }

    let payload: unknown;
    try {
      payload = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    } catch {
      throw new ControllerError(
        'artifact_not_oci_manifest',
        'Artifact is not an OCI image manifest',
        422,
      );
    }
    const parsed = manifest.safeParse(payload);
    if (!parsed.success) {
      throw new ControllerError(
        'artifact_not_oci_manifest',
        'Artifact is not an OCI image manifest',
        422,
      );
    }
    return {
      sha256,
      sizeBytes,
      mediaType: parsed.data.mediaType ?? 'application/vnd.oci.image.manifest.v1+json',
    };
  }

  private async request(url: URL): Promise<Response> {
    try {
      return await fetch(url, {
        method: 'GET',
        headers: {
          accept: [
            'application/vnd.oci.image.manifest.v1+json',
            'application/vnd.docker.distribution.manifest.v2+json',
            'application/octet-stream',
          ].join(', '),
        },
        redirect: 'manual',
        signal: AbortSignal.timeout(this.timeoutMs),
      });
    } catch {
      throw new ControllerError('artifact_unavailable', 'Artifact request failed', 503, true, 5);
    }
  }

  private releaseUrl(value: string): URL {
    const url = parseUrl(value, 'artifact_origin_denied');
    if (
      url.origin !== releaseOrigin ||
      !this.allowedOrigins.has(releaseOrigin) ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      !releasePath.test(url.pathname)
    ) {
      throw new ControllerError('artifact_origin_denied', 'Artifact URL is not allowed', 422);
    }
    return url;
  }

  private releaseAssetUrl(value: string): URL {
    const url = parseUrl(value, 'artifact_redirect_invalid');
    if (
      url.origin !== releaseAssetOrigin ||
      !this.allowedOrigins.has(releaseAssetOrigin) ||
      url.username ||
      url.password ||
      url.hash ||
      !releaseAssetPath.test(url.pathname) ||
      !validReleaseAssetQuery(url.searchParams)
    ) {
      throw new ControllerError('artifact_redirect_invalid', 'Artifact redirect is invalid', 422);
    }
    return url;
  }

  private throwRequestFailure(response: Response, message: string): never {
    const retryable =
      response.status === 408 ||
      response.status === 409 ||
      response.status === 429 ||
      response.status >= 500;
    throw new ControllerError(
      'artifact_request_failed',
      message,
      retryable ? 503 : 422,
      retryable,
      retryable ? 5 : undefined,
    );
  }
}

function parseUrl(value: string, code: string): URL {
  try {
    const url = new URL(value);
    if (url.protocol !== 'https:') throw new Error('HTTPS required');
    return url;
  } catch {
    throw new ControllerError(code, 'Artifact URL is invalid', 422);
  }
}

function validReleaseAssetQuery(query: URLSearchParams): boolean {
  return (
    one(query, 'sp') === 'r' &&
    one(query, 'sr') === 'b' &&
    one(query, 'spr') === 'https' &&
    one(query, 'sv') !== undefined &&
    one(query, 'se') !== undefined &&
    one(query, 'sig') !== undefined &&
    one(query, 'rscd') === 'attachment; filename=manifest.oci.json' &&
    one(query, 'rsct') === 'application/octet-stream' &&
    one(query, 'response-content-disposition') === 'attachment; filename=manifest.oci.json' &&
    one(query, 'response-content-type') === 'application/octet-stream'
  );
}

function one(query: URLSearchParams, key: string): string | undefined {
  const values = query.getAll(key);
  if (values.length !== 1 || values[0] === '') return undefined;
  return values[0];
}

function assertReleaseOrigins(origins: Set<string>): void {
  if (origins.size !== 2 || !origins.has(releaseOrigin) || !origins.has(releaseAssetOrigin)) {
    throw new Error('Artifact origins must be the GitHub release and release asset origins');
  }
}
