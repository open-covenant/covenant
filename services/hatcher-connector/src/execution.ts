import { createHash } from 'node:crypto';
import type { IntentExecution, GpuExecution } from './daemon.js';
import type { InboundFrame, MeshGrant } from './transport.js';

export const GPU_SCOPE = 'compute.gpu_workspace';
export const SMOKE_GPU: Readonly<GpuExecution> = Object.freeze({
  max_usdc_micros: 200_000, duration_secs: 300, max_launches: 1,
});

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

// No implicit defaults or wider limits for this narrowly authorized smoke test.
export function gpuGrant(grant: MeshGrant): GpuExecution {
  const c = grant.constraints;
  if (grant.scope !== GPU_SCOPE || !object(c) || Object.keys(c).length !== 3
    || Object.entries(SMOKE_GPU).some(([key, value]) => c[key] !== value)) {
    throw new Error('GPU grant must specify max_usdc_micros=200000, duration_secs=300, max_launches=1');
  }
  return { ...SMOKE_GPU };
}

export function executionFor(
  frame: Extract<InboundFrame, { type: 'dispatch' }>,
  configuredAgent: string | undefined,
  gpuEnabled: boolean,
): IntentExecution {
  if (typeof frame.agent_id !== 'string' || !frame.agent_id.trim()
    || typeof frame.dispatch_id !== 'string' || !frame.dispatch_id.trim()) {
    throw new Error('agent_id and dispatch_id are required');
  }
  if (configuredAgent && frame.agent_id !== configuredAgent) throw new Error('agent_id does not match the configured Hatcher agent');
  const context = frame.intent.context;
  if (context !== undefined && !object(context)) throw new Error('intent.context must be an object');
  const maxCost = context && 'max_cost_usd' in context ? context.max_cost_usd : 0.50;
  if (typeof maxCost !== 'number' || !Number.isFinite(maxCost) || maxCost <= 0 || maxCost > 1) {
    throw new Error('max_cost_usd must be a finite number greater than zero and at most 1');
  }
  if (frame.grants !== undefined && !Array.isArray(frame.grants)) throw new Error('grants must be an array');
  const grants = frame.grants ?? [];
  for (const grant of grants) {
    if (!object(grant) || typeof grant.scope !== 'string') throw new Error('invalid grant');
    if ((grant.scope.startsWith('compute.') || /gpu/i.test(grant.scope)) && grant.scope !== GPU_SCOPE) {
      throw new Error('unsupported GPU grant scope');
    }
  }
  const gpuGrants = grants.filter((g) => g.scope === GPU_SCOPE);
  let gpu: GpuExecution | undefined;
  if (gpuGrants.length) {
    if (!gpuEnabled) throw new Error('GPU dispatches are disabled');
    if (!configuredAgent || frame.agent_id !== configuredAgent) throw new Error('GPU dispatch requires the configured full Hatcher agent id');
    if (gpuGrants.length !== 1) throw new Error('GPU dispatch requires exactly one compute.gpu_workspace grant');
    if (context?.max_cost_usd !== 0.50) throw new Error('GPU smoke dispatch requires explicit max_cost_usd=0.50');
    gpu = gpuGrant(gpuGrants[0]!);
  }
  if (context?.gpu !== undefined) {
    if (!gpu) throw new Error('GPU context does not authorize a workspace; explicit grant required');
    if (!object(context.gpu) || Object.keys(context.gpu).length !== 1 || context.gpu.duration_secs !== 300) {
      throw new Error('GPU context may only specify duration_secs=300');
    }
  }
  return {
    // JSON tuple framing prevents ambiguous concatenation and includes the full id.
    run_key: createHash('sha256').update(JSON.stringify([frame.agent_id, frame.dispatch_id])).digest('hex'),
    max_cost_usd: maxCost,
    ...(gpu ? { gpu } : {}),
  };
}
