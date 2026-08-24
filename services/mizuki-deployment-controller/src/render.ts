import { z } from 'zod';
import { ControllerError, ociDigest, sha256 } from './domain.js';

const deployStatus = z.enum([
  'created',
  'queued',
  'build_in_progress',
  'pre_deploy_in_progress',
  'update_in_progress',
  'live',
  'deactivated',
  'build_failed',
  'pre_deploy_failed',
  'update_failed',
  'canceled',
]);
const imageSchema = z
  .object({
    ref: z.string().min(1).max(500),
    sha: z.union([ociDigest, sha256]),
    registryCredential: z.string().optional(),
  })
  .passthrough();
const deploySchema = z
  .object({
    id: z.string().min(1).max(128),
    commit: z
      .object({
        id: z.string().min(1),
        message: z.string().optional(),
        createdAt: z.string().datetime({ offset: true }).optional(),
      })
      .passthrough()
      .nullable()
      .optional(),
    image: imageSchema.optional(),
    status: deployStatus,
    trigger: z.enum([
      'api',
      'blueprint_sync',
      'deploy_hook',
      'deployed_by_render',
      'manual',
      'other',
      'new_commit',
      'rollback',
      'service_resumed',
      'service_updated',
    ]),
    createdAt: z.string().datetime({ offset: true }),
    updatedAt: z.string().datetime({ offset: true }).optional(),
    finishedAt: z.string().datetime({ offset: true }).nullable().optional(),
  })
  .passthrough();
const deployListSchema = z.array(
  z.object({ deploy: deploySchema, cursor: z.string().optional() }).passthrough(),
);
const serviceSchema = z
  .object({
    id: z.string().min(1),
    name: z.string().min(1),
    autoDeploy: z.literal('no'),
    imagePath: z.string().min(1).max(500),
    registryCredential: z
      .object({ id: z.string().optional(), name: z.string().optional() })
      .passthrough()
      .nullable()
      .optional(),
    repo: z.string().nullable().optional(),
    suspended: z.literal('not_suspended'),
    type: z.enum(['web_service', 'private_service']),
    serviceDetails: z
      .object({
        runtime: z.literal('image'),
        region: z.string().min(1),
        numInstances: z.number().int().positive().optional(),
        url: z.string().optional(),
      })
      .passthrough(),
  })
  .passthrough();

export type RenderDeploy = z.infer<typeof deploySchema>;
export type RenderService = z.infer<typeof serviceSchema>;

export interface RenderGateway {
  service(serviceId: string): Promise<RenderService>;
  listDeploys(serviceId: string, createdAfter?: Date): Promise<RenderDeploy[]>;
  deployImage(serviceId: string, imageRef: string): Promise<RenderDeploy | null>;
  rollback(serviceId: string, deployId: string): Promise<RenderDeploy>;
  deployment(serviceId: string, deployId: string): Promise<RenderDeploy>;
}

export interface RenderClientConfig {
  apiUrl: string;
  apiKey: string;
  allowedServiceIds: Set<string>;
  timeoutMs: number;
}

export class RenderClient implements RenderGateway {
  constructor(private readonly config: RenderClientConfig) {}

  async service(serviceId: string): Promise<RenderService> {
    return serviceSchema.parse(await this.request(serviceId, '', { method: 'GET' }));
  }

  async listDeploys(serviceId: string, createdAfter?: Date): Promise<RenderDeploy[]> {
    const query = new URLSearchParams({ limit: '100' });
    if (createdAfter) query.set('createdAfter', createdAfter.toISOString());
    const payload = await this.request(serviceId, `/deploys?${query}`, { method: 'GET' });
    return deployListSchema.parse(payload).map(({ deploy }) => deploy);
  }

  async deployImage(serviceId: string, imageRef: string): Promise<RenderDeploy | null> {
    const payload = await this.request(serviceId, '/deploys', {
      method: 'POST',
      body: { imageUrl: exactImageRef(imageRef) },
      accepted: [201, 202],
      mutation: true,
    });
    return payload === null ? null : deploySchema.parse(payload);
  }

  async rollback(serviceId: string, deployId: string): Promise<RenderDeploy> {
    const payload = await this.request(serviceId, '/rollback', {
      method: 'POST',
      body: { deployId: externalDeployId(deployId) },
      accepted: [201],
      mutation: true,
    });
    return deploySchema.parse(payload);
  }

  async deployment(serviceId: string, deployId: string): Promise<RenderDeploy> {
    return deploySchema.parse(
      await this.request(serviceId, `/deploys/${encodeURIComponent(externalDeployId(deployId))}`, {
        method: 'GET',
      }),
    );
  }

  private async request(
    serviceId: string,
    suffix: string,
    options: {
      method: 'GET' | 'POST';
      body?: Record<string, unknown>;
      accepted?: number[];
      mutation?: boolean;
    },
  ): Promise<unknown> {
    this.assertService(serviceId);
    const url = `${this.config.apiUrl}/services/${encodeURIComponent(serviceId)}${suffix}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: options.method,
        headers: {
          accept: 'application/json',
          authorization: `Bearer ${this.config.apiKey}`,
          ...(options.body ? { 'content-type': 'application/json' } : {}),
        },
        body: options.body ? JSON.stringify(options.body) : undefined,
        redirect: 'error',
        signal: AbortSignal.timeout(this.config.timeoutMs),
      });
    } catch {
      throw new ControllerError('render_unavailable', 'Render API request failed', 503, true, 5);
    }
    const accepted = options.accepted ?? [200];
    if (!accepted.includes(response.status)) {
      const retryable =
        response.status === 408 ||
        response.status === 409 ||
        response.status === 429 ||
        response.status >= 500;
      const code =
        response.status === 409 && options.mutation
          ? 'render_mutation_conflict'
          : 'render_request_failed';
      throw new ControllerError(
        code,
        `Render API returned ${response.status}`,
        retryable ? 503 : 422,
        retryable,
        retryable ? retryAfterSeconds(response) : undefined,
      );
    }
    if (response.status === 202 || response.status === 204) return null;
    const text = await readLimited(response, 1024 * 1024);
    try {
      return JSON.parse(text);
    } catch {
      throw new ControllerError('render_invalid_response', 'Render API returned invalid JSON', 502);
    }
  }

  private assertService(serviceId: string): void {
    if (!this.config.allowedServiceIds.has(serviceId)) {
      throw new ControllerError('render_service_denied', 'Render service is not allowed', 403);
    }
  }
}

export function deploymentHealth(deploy: RenderDeploy): 'starting' | 'healthy' | 'unhealthy' {
  if (deploy.status === 'live') return 'healthy';
  if (
    deploy.status === 'created' ||
    deploy.status === 'queued' ||
    deploy.status === 'build_in_progress' ||
    deploy.status === 'pre_deploy_in_progress' ||
    deploy.status === 'update_in_progress'
  ) {
    return 'starting';
  }
  return 'unhealthy';
}

export function deployArtifactSha256(deploy: RenderDeploy): string {
  if (!deploy.image) {
    throw new ControllerError(
      'artifact_execution_unbound',
      'Render deploy does not attest an immutable image digest',
      503,
    );
  }
  return deploy.image.sha.startsWith('sha256:')
    ? deploy.image.sha.slice('sha256:'.length)
    : deploy.image.sha;
}

export function exactImageRef(value: string): string {
  if (!/^[a-z0-9.-]+(?::[0-9]{1,5})?\/[a-z0-9._/-]+@sha256:[a-f0-9]{64}$/.test(value)) {
    throw new ControllerError('invalid_image_reference', 'OCI image reference is invalid', 422);
  }
  return value;
}

export function imageRepository(value: string): string {
  const ref = exactImageRef(value);
  return ref.slice(0, ref.indexOf('@sha256:'));
}

export function serviceImageRepository(value: string): string {
  if (value !== value.toLowerCase() || value.includes('://')) {
    throw new ControllerError('invalid_image_reference', 'Service image path is invalid', 503);
  }
  const slash = value.indexOf('/');
  if (slash < 1 || slash === value.length - 1) {
    throw new ControllerError('invalid_image_reference', 'Service image path is invalid', 503);
  }
  const digest = value.indexOf('@sha256:');
  if (digest >= 0) return imageRepository(value);
  const tag = value.lastIndexOf(':');
  const repository = tag > slash ? value.slice(0, tag) : value;
  if (!/^[a-z0-9.-]+(?::[0-9]{1,5})?\/[a-z0-9._/-]+$/.test(repository)) {
    throw new ControllerError('invalid_image_reference', 'Service image path is invalid', 503);
  }
  return repository;
}

function externalDeployId(value: string): string {
  if (!/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value)) {
    throw new ControllerError('invalid_deploy_id', 'Deployment ID is invalid', 400);
  }
  return value;
}

function retryAfterSeconds(response: Response): number {
  const value = Number(response.headers.get('retry-after'));
  return Number.isInteger(value) && value > 0 && value <= 300 ? value : 5;
}

async function readLimited(response: Response, limit: number): Promise<string> {
  if (!response.body) return '';
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > limit) {
      await reader.cancel();
      throw new ControllerError(
        'render_response_too_large',
        'Render API response is too large',
        502,
      );
    }
    chunks.push(value);
  }
  return Buffer.concat(chunks).toString('utf8');
}
