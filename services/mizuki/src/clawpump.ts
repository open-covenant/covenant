import { z } from 'zod';
import type { Config } from './config.js';
import type { MizukiStore } from './store.js';

// ClawPump serves the agent's own earnings and does not echo the agent id
// back, so it is carried from configuration and only cross-checked when the
// response does include one.
const earningsSchema = z.object({
  agentId: z.string().optional(),
  totalEarned: z.number().nonnegative(),
  totalSent: z.number().nonnegative(),
  totalPending: z.number().nonnegative(),
  totalHeld: z.number().nonnegative(),
  totalFailed: z.number().nonnegative().optional(),
  recentDistributions: z.array(z.unknown()).default([]),
});

export type ClawPumpEarnings = z.infer<typeof earningsSchema> & { agentId: string };

export class ClawPumpClient {
  constructor(
    private readonly config: Pick<Config, 'clawPumpBaseUrl' | 'clawPumpApiKey' | 'clawPumpAgentId'>,
    private readonly request: typeof fetch = fetch,
  ) {}

  configured(): boolean {
    return Boolean(this.config.clawPumpAgentId);
  }

  async earnings(): Promise<ClawPumpEarnings> {
    if (!this.config.clawPumpAgentId) throw new Error('CLAWPUMP_AGENT_ID is not configured');
    const agentId = this.config.clawPumpAgentId;
    const value = await this.get(`/api/agents/${encodeURIComponent(agentId)}/earnings`);
    const earnings = earningsSchema.parse(value);
    if (earnings.agentId !== undefined && earnings.agentId !== agentId) {
      throw new Error('ClawPump earnings response does not match the configured agent');
    }
    return { ...earnings, agentId };
  }

  private async get(path: string): Promise<Record<string, unknown>> {
    const response = await this.request(
      `${this.config.clawPumpBaseUrl.replace(/\/$/, '')}${path}`,
      {
        headers: this.config.clawPumpApiKey
          ? { authorization: `Bearer ${this.config.clawPumpApiKey}` }
          : undefined,
        signal: AbortSignal.timeout(15_000),
      },
    );
    if (!response.ok) throw new Error(`ClawPump ${path} failed: ${response.status}`);
    return (await response.json()) as Record<string, unknown>;
  }
}

export class EarningsReconciler {
  constructor(
    private readonly store: MizukiStore,
    private readonly client: ClawPumpClient,
  ) {}

  async reconcile(): Promise<ClawPumpEarnings | undefined> {
    if (!this.client.configured()) return undefined;
    const earnings = await this.client.earnings();
    const ledger = await this.store.ledgerEntries();
    const referencePrefix = `clawpump:${earnings.agentId}:`;
    const recorded = ledger
      .filter(
        (entry) =>
          entry.kind === 'creator_fee' &&
          entry.asset === 'SOL' &&
          entry.referenceId.startsWith(referencePrefix),
      )
      .reduce((total, entry) => total + BigInt(entry.amountAtomic), 0n);
    const sent = solToLamports(earnings.totalSent);
    if (sent > recorded) {
      await this.store.appendLedger({
        kind: 'creator_fee',
        referenceId: `${referencePrefix}${sent}`,
        asset: 'SOL',
        amountAtomic: String(sent - recorded),
        amountUsd: 0,
      });
    }
    return earnings;
  }
}

/**
 * ClawPump reports earnings as a SOL float derived from a revenue split, so the
 * value carries precision below one lamport (1.16282653808594 SOL is
 * 1162826538.086 lamports). Round to the nearest whole lamport: the residue is
 * an artefact of the split, not a balance we can credit. Deltas are cumulative,
 * so a rounding step never compounds.
 */
function solToLamports(sol: number): bigint {
  if (!Number.isFinite(sol) || sol < 0) throw new Error('invalid SOL amount');
  const rounded = Math.round(sol * 1_000_000_000);
  if (!Number.isSafeInteger(rounded)) {
    throw new Error('SOL amount is out of range');
  }
  return BigInt(rounded);
}
