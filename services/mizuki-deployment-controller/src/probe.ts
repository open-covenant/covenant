import { z } from 'zod';
import { ControllerError } from './domain.js';

const productionProbeSchema = z
  .object({
    status: z.literal('ok'),
    service: z.literal('mizuki-api'),
    checks: z
      .object({
        database: z.literal('ok'),
        policySigner: z.literal('ok'),
        codingGateway: z.literal('ok'),
        settlement: z.literal('ok'),
      })
      .strict(),
  })
  .strict();

const shadowProbeSchema = z.object({ ok: z.literal(true) }).strict();

export interface ApplicationGateway {
  probe(serviceId: string): Promise<void>;
}

export interface HttpApplicationGatewayConfig {
  targets: Map<string, ApplicationProbeTarget>;
  timeoutMs: number;
}

export type ApplicationProbeTarget =
  | { role: 'shadow'; url: string }
  | { role: 'production'; url: string; token: string };

export class HttpApplicationGateway implements ApplicationGateway {
  constructor(private readonly config: HttpApplicationGatewayConfig) {}

  async probe(serviceId: string): Promise<void> {
    const target = this.config.targets.get(serviceId);
    if (!target) {
      throw new ControllerError(
        'probe_service_denied',
        'Application probe target is not allowed',
        403,
      );
    }
    let response: Response;
    try {
      response = await fetch(target.url, {
        method: 'GET',
        headers: probeHeaders(target),
        redirect: 'error',
        signal: AbortSignal.timeout(this.config.timeoutMs),
      });
    } catch {
      throw new ControllerError(
        'application_probe_unavailable',
        'Application readiness probe failed',
        503,
        true,
        5,
      );
    }
    if (!response.ok) {
      const retryable =
        response.status === 408 ||
        response.status === 409 ||
        response.status === 429 ||
        response.status >= 500;
      throw new ControllerError(
        'application_probe_failed',
        `Application readiness probe returned ${response.status}`,
        retryable ? 503 : 502,
        retryable,
        retryable ? 5 : undefined,
      );
    }
    const text = await readLimited(response, 64 * 1024);
    let payload: unknown;
    try {
      payload = JSON.parse(text);
    } catch {
      throw new ControllerError(
        'application_probe_invalid',
        'Application readiness probe returned invalid JSON',
        502,
      );
    }
    const schema = target.role === 'shadow' ? shadowProbeSchema : productionProbeSchema;
    const parsed = schema.safeParse(payload);
    if (!parsed.success) {
      throw new ControllerError(
        'application_probe_unhealthy',
        'Application readiness checks did not pass',
        502,
      );
    }
  }
}

function probeHeaders(target: ApplicationProbeTarget): Record<string, string> {
  const headers = { accept: 'application/json', 'cache-control': 'no-store' };
  if (target.role === 'shadow') return headers;
  return { ...headers, authorization: `Bearer ${target.token}` };
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
        'application_probe_invalid',
        'Application readiness response is too large',
        502,
      );
    }
    chunks.push(value);
  }
  return Buffer.concat(chunks).toString('utf8');
}
