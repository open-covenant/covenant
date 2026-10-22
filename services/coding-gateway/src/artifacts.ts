import { createHash, randomBytes } from 'node:crypto';
import {
  closeSync,
  existsSync,
  fsyncSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
  writeSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join } from 'node:path';
import { boundedInteger } from './config.js';
import { ConfigError } from './config-error.js';

/**
 * Files a GPU job produced, kept on the gateway's disk for a limited time and
 * served by an unguessable token. The gateway is private: the public link
 * points at whichever service proxies GET /artifacts/<token>/<name> to it.
 */

export const ARTIFACT_TOKEN = /^[A-Za-z0-9_-]{32}$/;
export const ARTIFACT_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const HOUR_MS = 3_600_000;

export interface ArtifactMeta {
  token: string;
  name: string;
  bytes: number;
  sha256: string;
  createdAt: string;
  expiresAt: string;
}

export interface ArtifactConfig {
  dir: string;
  publicBaseUrl: string | null;
  ttlMs: number;
  maxTotalBytes: number;
}

export function artifactConfigFromEnv(env: NodeJS.ProcessEnv = process.env): ArtifactConfig {
  const runStore = env.RUN_STORE_PATH;
  const dir =
    env.COMPUTE_ARTIFACT_DIR ||
    (runStore && isAbsolute(runStore)
      ? join(dirname(runStore), 'artifacts')
      : join(tmpdir(), 'covenant-gateway-artifacts'));
  if (!isAbsolute(dir))
    throw new ConfigError(`COMPUTE_ARTIFACT_DIR=${JSON.stringify(dir)} must be an absolute path`);
  return {
    dir,
    publicBaseUrl: publicBaseUrl(env.COMPUTE_ARTIFACT_BASE_URL),
    ttlMs:
      boundedInteger(env.COMPUTE_ARTIFACT_TTL_HOURS, 168, 'COMPUTE_ARTIFACT_TTL_HOURS', 1, 720) *
      HOUR_MS,
    maxTotalBytes:
      boundedInteger(env.COMPUTE_ARTIFACT_MAX_MB, 512, 'COMPUTE_ARTIFACT_MAX_MB', 1, 4_096) *
      1024 *
      1024,
  };
}

function publicBaseUrl(raw: string | undefined): string | null {
  if (!raw?.trim()) return null;
  const value = raw.trim();
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new ConfigError(`COMPUTE_ARTIFACT_BASE_URL=${JSON.stringify(value)} is not a URL`);
  }
  if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash)
    throw new ConfigError(
      `COMPUTE_ARTIFACT_BASE_URL=${JSON.stringify(value)} must be a plain https URL such as https://connector.example/artifacts`,
    );
  return `${url.origin}${url.pathname}`.replace(/\/+$/, '');
}

export class ArtifactStore {
  constructor(readonly config: ArtifactConfig) {}

  url(meta: Pick<ArtifactMeta, 'token' | 'name'>): string | null {
    return this.config.publicBaseUrl
      ? `${this.config.publicBaseUrl}/${meta.token}/${encodeURIComponent(meta.name)}`
      : null;
  }

  save(name: string, bytes: Buffer, now = Date.now()): ArtifactMeta {
    if (!ARTIFACT_NAME.test(name)) throw new Error(`invalid artifact name ${JSON.stringify(name)}`);
    this.prune(now);
    if (this.totalBytes() + bytes.length > this.config.maxTotalBytes)
      throw new Error('artifact storage is full; retry after older results expire');
    const token = randomBytes(24).toString('base64url');
    const meta: ArtifactMeta = {
      token,
      name,
      bytes: bytes.length,
      sha256: createHash('sha256').update(bytes).digest('hex'),
      createdAt: new Date(now).toISOString(),
      expiresAt: new Date(now + this.config.ttlMs).toISOString(),
    };
    const directory = join(this.config.dir, token);
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    durableWrite(join(directory, 'data'), bytes);
    // meta.json is written last: its presence marks a complete artifact.
    durableWrite(join(directory, 'meta.json'), Buffer.from(JSON.stringify(meta)));
    return meta;
  }

  open(token: string, name: string, now = Date.now()): { file: string; meta: ArtifactMeta } | null {
    if (!ARTIFACT_TOKEN.test(token) || !ARTIFACT_NAME.test(name)) return null;
    const meta = this.read(token);
    if (!meta || meta.name !== name || Date.parse(meta.expiresAt) <= now) return null;
    const file = join(this.config.dir, token, 'data');
    return existsSync(file) && statSync(file).size === meta.bytes ? { file, meta } : null;
  }

  prune(now = Date.now()): void {
    for (const token of this.tokens()) {
      const meta = this.read(token);
      if (!meta || Date.parse(meta.expiresAt) <= now)
        rmSync(join(this.config.dir, token), { recursive: true, force: true });
    }
  }

  private totalBytes(): number {
    return this.tokens().reduce((sum, token) => sum + (this.read(token)?.bytes ?? 0), 0);
  }

  private tokens(): string[] {
    try {
      return readdirSync(this.config.dir).filter((entry) => ARTIFACT_TOKEN.test(entry));
    } catch {
      return [];
    }
  }

  private read(token: string): ArtifactMeta | null {
    try {
      const meta = JSON.parse(readFileSync(join(this.config.dir, token, 'meta.json'), 'utf8'));
      return meta?.token === token &&
        typeof meta.name === 'string' &&
        Number.isSafeInteger(meta.bytes) &&
        typeof meta.sha256 === 'string' &&
        Number.isFinite(Date.parse(meta.expiresAt))
        ? (meta as ArtifactMeta)
        : null;
    } catch {
      return null;
    }
  }
}

function durableWrite(path: string, bytes: Buffer): void {
  const temporary = `${path}.tmp`;
  const fd = openSync(temporary, 'w', 0o600);
  try {
    for (let offset = 0; offset < bytes.length; )
      offset += writeSync(fd, bytes, offset, bytes.length - offset);
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
  renameSync(temporary, path);
}
