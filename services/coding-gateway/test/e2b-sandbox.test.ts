import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { SandboxSpec } from '../src/types.js';

const { createMock, listMock, ALL_TRAFFIC } = vi.hoisted(() => ({
  createMock: vi.fn(),
  listMock: vi.fn(),
  ALL_TRAFFIC: 'all-traffic-sentinel',
}));

vi.mock('e2b', () => ({
  Sandbox: { create: createMock, list: listMock },
  ALL_TRAFFIC,
}));

import { E2bSandboxProvider } from '../src/sandbox/e2b.js';

const SPEC: SandboxSpec = {
  runId: 'run-1',
  egressAllowlist: [],
  cpuMs: 60_000,
  memoryMb: 512,
  diskMb: 512,
  wallMs: 90_000,
};

// create(opts) | create(template, opts) — opts is always the last argument.
function lastOpts(): any {
  return createMock.mock.calls.at(-1)!.at(-1);
}

describe('E2bSandboxProvider egress firewall', () => {
  beforeEach(() => {
    createMock.mockReset();
    listMock.mockReset();
    listMock.mockReturnValue({ nextItems: vi.fn(async () => []) });
    createMock.mockResolvedValue({});
    delete process.env.E2B_TEMPLATE;
  });
  afterEach(() => {
    delete process.env.E2B_EGRESS_ALLOW;
    delete process.env.E2B_TEMPLATE;
  });

  it('denies all egress for an empty run allowlist and threads the lifecycle options', async () => {
    await new E2bSandboxProvider('key-abc').create(SPEC);
    const opts = lastOpts();
    expect(opts.network).toEqual({ denyOut: [ALL_TRAFFIC] });
    expect(opts.apiKey).toBe('key-abc');
    expect(opts.timeoutMs).toBe(SPEC.wallMs);
  });

  it('checks credentials through the non-billable list control plane', async () => {
    const nextItems = vi.fn(async () => []);
    listMock.mockReturnValue({ nextItems });

    await new E2bSandboxProvider('key-abc').check();

    expect(listMock).toHaveBeenCalledWith({ apiKey: 'key-abc', limit: 1 });
    expect(nextItems).toHaveBeenCalledWith({ requestTimeoutMs: 15_000 });
    expect(createMock).not.toHaveBeenCalled();
  });

  it('allows only the explicit run subset of the captured operator policy', async () => {
    const provider = new E2bSandboxProvider('key-abc', undefined, [
      'registry.npmjs.org',
      'github.com',
    ]);
    await provider.create({ ...SPEC, egressAllowlist: ['registry.npmjs.org'] });
    expect(lastOpts().network).toEqual({
      denyOut: [ALL_TRAFFIC],
      allowOut: ['registry.npmjs.org'],
    });
  });

  it('cannot be broadened by an environment change after construction', async () => {
    const provider = new E2bSandboxProvider('key-abc', undefined, ['registry.npmjs.org']);
    process.env.E2B_EGRESS_ALLOW = 'registry.npmjs.org,attacker.example';
    await provider.create({ ...SPEC, egressAllowlist: ['registry.npmjs.org'] });

    expect(lastOpts().network.allowOut).toEqual(['registry.npmjs.org']);
  });

  it('rejects a run host outside the captured policy before provisioning', async () => {
    const provider = new E2bSandboxProvider('key-abc', undefined, ['registry.npmjs.org']);

    await expect(
      provider.create({
        ...SPEC,
        egressAllowlist: ['registry.npmjs.org', 'attacker.example'],
      }),
    ).rejects.toThrow(/attacker\.example.*outside the operator policy/);
    expect(createMock).not.toHaveBeenCalled();
  });

  it('rejects an unsupported operator policy at construction', () => {
    expect(() => new E2bSandboxProvider('key-abc', undefined, ['attacker.example'])).toThrow(
      /unsupported host attacker\.example/,
    );
    expect(createMock).not.toHaveBeenCalled();
  });

  it('rejects malformed and duplicate run hosts before provisioning', async () => {
    const provider = new E2bSandboxProvider('key-abc', undefined, ['registry.npmjs.org']);

    await expect(
      provider.create({ ...SPEC, egressAllowlist: ['https://registry.npmjs.org'] }),
    ).rejects.toThrow(/invalid hostname/);
    await expect(
      provider.create({
        ...SPEC,
        egressAllowlist: ['registry.npmjs.org', 'registry.npmjs.org'],
      }),
    ).rejects.toThrow(/duplicate host/);
    expect(createMock).not.toHaveBeenCalled();
  });

  it('opts into a trimmed E2B_TEMPLATE and falls back to the base sandbox otherwise', async () => {
    process.env.E2B_TEMPLATE = '  covenant-coder  ';
    await new E2bSandboxProvider('k').create(SPEC);
    expect(createMock.mock.calls.at(-1)).toEqual([
      'covenant-coder',
      expect.objectContaining({ apiKey: 'k' }),
    ]);

    createMock.mockClear();
    process.env.E2B_TEMPLATE = '   ';
    await new E2bSandboxProvider('k').create(SPEC);
    expect(createMock.mock.calls.at(-1)).toHaveLength(1);
  });

  it('accepts only the exact pinned template ID and resource dimensions', async () => {
    const kill = vi.fn(async () => undefined);
    const getInfo = vi.fn(async () => ({
      templateId: 'tpl_immutable_123',
      cpuCount: 4,
      memoryMB: 4096,
    }));
    createMock.mockResolvedValue({ getInfo, kill });
    const expected = { templateId: 'tpl_immutable_123', cpuCount: 4, memoryMb: 4096 };

    await expect(new E2bSandboxProvider('k', expected).create(SPEC)).resolves.toBeDefined();
    expect(createMock).toHaveBeenCalledWith(
      expected.templateId,
      expect.objectContaining({ apiKey: 'k' }),
    );
    expect(kill).not.toHaveBeenCalled();

    getInfo.mockResolvedValueOnce({
      templateId: expected.templateId,
      cpuCount: 8,
      memoryMB: 8192,
    });
    await expect(new E2bSandboxProvider('k', expected).create(SPEC)).rejects.toThrow(
      /sandbox identity mismatch/,
    );
    expect(kill).toHaveBeenCalledTimes(1);
  });

  it('kills the sandbox when identity evidence cannot be read', async () => {
    const kill = vi.fn(async () => undefined);
    createMock.mockResolvedValue({
      getInfo: vi.fn(async () => {
        throw new Error('identity unavailable');
      }),
      kill,
    });

    await expect(
      new E2bSandboxProvider('k', {
        templateId: 'tpl_immutable_123',
        cpuCount: 4,
        memoryMb: 4096,
      }).create(SPEC),
    ).rejects.toThrow(/identity unavailable/);
    expect(kill).toHaveBeenCalledTimes(1);
  });
});

describe('E2bSandbox.exec result surfacing', () => {
  beforeEach(() => {
    createMock.mockReset();
    delete process.env.E2B_EGRESS_ALLOW;
    delete process.env.E2B_TEMPLATE;
  });

  // create() returns `new E2bSandbox(sbx)`, so a fake `sbx.commands.run`
  // drives the exec error-surfacing arm without a live microVM.
  async function sandboxWithRun(run: (cmd: string, o: { timeoutMs: number }) => Promise<unknown>) {
    createMock.mockResolvedValue({ commands: { run } });
    return new E2bSandboxProvider('k').create(SPEC);
  }

  it('returns the command result and defaults the per-command timeout backstop', async () => {
    const seen: { timeoutMs: number }[] = [];
    const sbx = await sandboxWithRun(async (_cmd, o) => {
      seen.push(o);
      return { stdout: 'ok', stderr: '', exitCode: 0 };
    });
    expect(await sbx.exec('npm test')).toEqual({ stdout: 'ok', stderr: '', exitCode: 0 });
    await sbx.exec('npm test', { timeoutMs: 5_000 });
    expect(seen.map((o) => o.timeoutMs)).toEqual([300_000, 5_000]);
  });

  it('surfaces an e2b non-zero exit as a result rather than throwing', async () => {
    const sbx = await sandboxWithRun(async () => {
      throw { stdout: 'build out', stderr: 'boom', exitCode: 2 };
    });
    expect(await sbx.exec('npm run build')).toEqual({
      stdout: 'build out',
      stderr: 'boom',
      exitCode: 2,
    });
  });

  it('defaults absent stdout/stderr to empty strings on a surfaced failure', async () => {
    const sbx = await sandboxWithRun(async () => {
      throw { exitCode: 1 };
    });
    expect(await sbx.exec('false')).toEqual({ stdout: '', stderr: '', exitCode: 1 });
  });

  it('rethrows a transport error that carries no exit code', async () => {
    const sbx = await sandboxWithRun(async () => {
      throw new Error('microVM unreachable');
    });
    await expect(sbx.exec('npm test')).rejects.toThrow(/microVM unreachable/);
  });
});
