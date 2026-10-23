import { describe, it, expect } from 'vitest';
import { executionFor, gpuGrant, SMOKE_GPU } from './execution.js';
import type { InboundFrame } from './transport.js';

type Dispatch = Extract<InboundFrame, { type: 'dispatch' }>;
function frame(): Dispatch {
  return { v: 1, type: 'dispatch', agent_id: 'full-agent', dispatch_id: 'smoke-1', intent: { text: 'run smoke', context: { max_cost_usd: 0.50, gpu: { duration_secs: 300 } } }, grants: [{ scope: 'compute.gpu_workspace', constraints: { ...SMOKE_GPU } }] };
}

describe('GPU execution consent', () => {
  it('requires feature enablement, exact full agent identity, and one consent grant', () => {
    expect(() => executionFor(frame(), 'full-agent', false)).toThrow(/disabled/);
    expect(() => executionFor(frame(), undefined, true)).toThrow(/full Hatcher agent id/);
    expect(() => executionFor(frame(), 'different', true)).toThrow(/agent_id/);
    const duplicate = frame(); duplicate.grants!.push(duplicate.grants![0]!);
    expect(() => executionFor(duplicate, 'full-agent', true)).toThrow(/exactly one/);
    const missing = frame(); missing.grants = [];
    expect(() => executionFor(missing, 'full-agent', true)).toThrow(/explicit grant/);
  });

  it.each([
    {}, { ...SMOKE_GPU, max_usdc_micros: 200_001 }, { ...SMOKE_GPU, max_usdc_micros: '200000' },
    { ...SMOKE_GPU, duration_secs: 301 }, { ...SMOKE_GPU, duration_secs: NaN },
    { ...SMOKE_GPU, max_launches: 2 }, { ...SMOKE_GPU, arbitrary: true },
  ])('rejects incomplete, widened, malformed, or extra constraints %j', (constraints) => {
    expect(() => gpuGrant({ scope: 'compute.gpu_workspace', constraints })).toThrow(/GPU grant/);
  });

  it.each(['compute.gpu', 'compute.gpu_workspace.extra', 'gpu_workspace', 'mcp.gpu_workspace'])('rejects unsupported GPU scope %s', (scope) => {
    const f = frame(); f.grants![0]!.scope = scope;
    expect(() => executionFor(f, 'full-agent', true)).toThrow(/unsupported/);
  });

  it.each([undefined, null, NaN, Infinity, '0.50', 0, -1, 0.49, 0.51, 1.01])('requires explicit exact smoke total %s', (max_cost_usd) => {
    const f = frame(); f.intent.context!.max_cost_usd = max_cost_usd;
    expect(() => executionFor(f, 'full-agent', true)).toThrow();
  });

  it.each([{ duration_secs: 301 }, { duration_secs: 300, enabled: true }, true, null])('rejects GPU context overrides %j', (gpu) => {
    const f = frame(); f.intent.context!.gpu = gpu;
    expect(() => executionFor(f, 'full-agent', true)).toThrow(/GPU context/);
  });

  it('keeps ordinary dispatches GPU disabled and hashes the complete framed identity', () => {
    const f = frame(); f.grants = []; f.intent.context = {};
    const ordinary = executionFor(f, 'full-agent', false);
    expect(ordinary).toEqual({ run_key: expect.stringMatching(/^[a-f0-9]{64}$/), max_cost_usd: 0.50 });
    expect(executionFor(f, 'full-agent', false)).toEqual(ordinary);
    const other = { ...f, agent_id: 'full-agent-2' };
    expect(executionFor(other, undefined, false).run_key).not.toEqual(ordinary.run_key);
    expect(executionFor({ ...f, agent_id: 'ab', dispatch_id: 'c' }, undefined, false).run_key)
      .not.toEqual(executionFor({ ...f, agent_id: 'a', dispatch_id: 'bc' }, undefined, false).run_key);
  });
});
