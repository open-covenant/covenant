import { describe, expect, it, vi } from 'vitest';
import { ClawPumpClient, EarningsReconciler } from './clawpump.js';
import { MemoryStore } from './store.js';

const config = {
  clawPumpBaseUrl: 'https://clawpump.tech',
  clawPumpApiKey: 'cpk_test',
  clawPumpAgentId: 'mizuki-agent',
};

function earnings(totalSent: number) {
  return {
    agentId: 'mizuki-agent',
    totalEarned: totalSent,
    totalSent,
    totalPending: 0,
    totalHeld: 0,
    recentDistributions: [],
  };
}

describe('ClawPump earnings', () => {
  it('uses the documented agent earnings endpoint with bearer authentication', async () => {
    const request = vi.fn(async () => Response.json(earnings(1.25)));
    const client = new ClawPumpClient(config, request);

    await expect(client.earnings()).resolves.toMatchObject({
      agentId: 'mizuki-agent',
      totalSent: 1.25,
    });
    expect(request).toHaveBeenCalledOnce();
    expect(request.mock.calls[0]?.[0]).toBe(
      'https://clawpump.tech/api/fees/earnings?agentId=mizuki-agent',
    );
    expect(request.mock.calls[0]?.[1]?.headers).toEqual({
      authorization: 'Bearer cpk_test',
    });
  });

  it('records only the exact cumulative SOL delta and is idempotent', async () => {
    const store = new MemoryStore();
    const request = vi
      .fn<() => Promise<Response>>()
      .mockResolvedValueOnce(Response.json(earnings(1.25)))
      .mockResolvedValueOnce(Response.json(earnings(1.25)))
      .mockResolvedValueOnce(Response.json(earnings(1.5)));
    const reconciler = new EarningsReconciler(store, new ClawPumpClient(config, request));

    await reconciler.reconcile();
    await reconciler.reconcile();
    await reconciler.reconcile();

    expect(await store.ledgerEntries()).toEqual([
      expect.objectContaining({
        kind: 'creator_fee',
        referenceId: 'clawpump:mizuki-agent:1250000000',
        asset: 'SOL',
        amountAtomic: '1250000000',
        amountUsd: 0,
      }),
      expect.objectContaining({
        kind: 'creator_fee',
        referenceId: 'clawpump:mizuki-agent:1500000000',
        asset: 'SOL',
        amountAtomic: '250000000',
        amountUsd: 0,
      }),
    ]);
  });

  it('rejects earnings attributed to another agent', async () => {
    const client = new ClawPumpClient(config, async () =>
      Response.json({ ...earnings(1), agentId: 'other-agent' }),
    );

    await expect(client.earnings()).rejects.toThrow('does not match the configured agent');
  });

  it('rejects a cumulative amount with fractional lamports', async () => {
    const client = new ClawPumpClient(config, async () => Response.json(earnings(0.0000000005)));

    await expect(new EarningsReconciler(new MemoryStore(), client).reconcile()).rejects.toThrow(
      'whole lamports',
    );
  });

  it("does not reconcile one agent against another agent's reported total", async () => {
    const store = new MemoryStore();
    await store.appendLedger({
      kind: 'creator_fee',
      referenceId: 'clawpump:other-agent:5000000000',
      asset: 'SOL',
      amountAtomic: '5000000000',
      amountUsd: 0,
    });
    const client = new ClawPumpClient(config, async () => Response.json(earnings(1)));

    await new EarningsReconciler(store, client).reconcile();

    expect(await store.ledgerEntries()).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          referenceId: 'clawpump:mizuki-agent:1000000000',
          amountAtomic: '1000000000',
        }),
      ]),
    );
  });
});
