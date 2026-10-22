import type { ComputeCleanupReport } from './compute.js';
import type { TokenUsage } from './types.js';

/** Captured before provider work; never reconstruct old tariffs from current config. */
export interface RunAccountingBasis {
  sandbox: {
    provider: 'e2b' | 'local';
    attempted: boolean;
    /** Worst-case reservation the spend ledger holds. */
    maximumUsd: number;
    usdPerSec: number;
    maximumSeconds: number;
    /** List price per second from verified tariff evidence; absent when unverified. */
    listUsdPerSec?: number;
    tariffRef?: string;
    /** Taken just before the create request and after the sandbox was confirmed destroyed. */
    startedAtMs?: number;
    endedAtMs?: number;
  };
  model: {
    method: 'unknown' | 'usage_estimate' | 'provider_receipt' | 'no_requests';
    estimatedUsd?: number;
    name?: string;
    /** USD per million tokens the usage was priced at. */
    prices?: { input: number; output: number; cacheRead: number; cacheWrite: number };
  };
}

type Basis<T extends string> = { usd: number | null; basis: T };

/**
 * The receipt a run reports. `final` means every line is an amount charged,
 * not an estimate or a reservation; the lines then add up to `totalUsd`.
 * Otherwise `totalUsd` is the conservative amount held for the run.
 */
export interface RunAccounting {
  version: 2;
  final: boolean;
  totalUsd: number;
  gpu: (Basis<'compute_receipt'> & { runtimeSecs: number; jobs: string[] }) | null;
  model: Basis<
    | 'usage_at_list_price'
    | 'provider_price_ceiling'
    | 'no_requests'
    | 'conservative_hold'
    | 'unknown'
  > & {
    name?: string;
    usage: TokenUsage | null;
    usdPerMillionTokens?: RunAccountingBasis['model']['prices'];
  };
  sandbox: Basis<'metered_at_list_price' | 'maximum_reservation' | 'not_started' | 'unknown'> & {
    seconds: number | null;
    usdPerSec: number | null;
    tariff?: string;
  };
  /** GPU start-up time before the machine was ready. Covenant pays it; it is not in the total. */
  coveredByCovenant: { gpuProvisioningUsd: number; gpuProvisioningSecs: number } | null;
}

const money = (value: unknown): value is number =>
  typeof value === 'number' && Number.isFinite(value) && value >= 0;
const instant = (value: unknown): boolean =>
  value === undefined || (Number.isSafeInteger(value) && (value as number) > 0);
const usd = (value: number): number => Number(value.toFixed(9));

export function validAccountingBasis(value: unknown): value is RunAccountingBasis {
  const basis = value as RunAccountingBasis | null;
  const prices = basis?.model?.prices;
  return (
    !!basis &&
    typeof basis === 'object' &&
    !!basis.sandbox &&
    ['e2b', 'local'].includes(basis.sandbox.provider) &&
    typeof basis.sandbox.attempted === 'boolean' &&
    money(basis.sandbox.maximumUsd) &&
    money(basis.sandbox.usdPerSec) &&
    money(basis.sandbox.maximumSeconds) &&
    (basis.sandbox.listUsdPerSec === undefined || money(basis.sandbox.listUsdPerSec)) &&
    (basis.sandbox.tariffRef === undefined ||
      (typeof basis.sandbox.tariffRef === 'string' && basis.sandbox.tariffRef.length <= 512)) &&
    instant(basis.sandbox.startedAtMs) &&
    instant(basis.sandbox.endedAtMs) &&
    !!basis.model &&
    ['unknown', 'usage_estimate', 'provider_receipt', 'no_requests'].includes(basis.model.method) &&
    (basis.model.estimatedUsd === undefined || money(basis.model.estimatedUsd)) &&
    (basis.model.name === undefined ||
      (typeof basis.model.name === 'string' && basis.model.name.length <= 128)) &&
    (prices === undefined ||
      (!!prices &&
        typeof prices === 'object' &&
        [prices.input, prices.output, prices.cacheRead, prices.cacheWrite].every(money)))
  );
}

function safeUsage(usage: TokenUsage | undefined): TokenUsage | null {
  if (
    !usage ||
    !['inputTokens', 'outputTokens', 'cacheReadTokens', 'cacheCreationTokens'].every(
      (field) =>
        Number.isSafeInteger(usage[field as keyof TokenUsage]) &&
        usage[field as keyof TokenUsage] >= 0,
    )
  )
    return null;
  return {
    inputTokens: usage.inputTokens,
    outputTokens: usage.outputTokens,
    cacheReadTokens: usage.cacheReadTokens,
    cacheCreationTokens: usage.cacheCreationTokens,
  };
}

function sandboxLine(basis: RunAccountingBasis['sandbox']): RunAccounting['sandbox'] {
  if (!basis.attempted) return { usd: 0, basis: 'not_started', seconds: 0, usdPerSec: null };
  const { listUsdPerSec, startedAtMs, endedAtMs } = basis;
  if (
    listUsdPerSec !== undefined &&
    startedAtMs !== undefined &&
    endedAtMs !== undefined &&
    endedAtMs >= startedAtMs
  ) {
    const seconds = Math.max(1, Math.ceil((endedAtMs - startedAtMs) / 1_000));
    return {
      usd: usd(seconds * listUsdPerSec),
      basis: 'metered_at_list_price',
      seconds,
      usdPerSec: listUsdPerSec,
      ...(basis.tariffRef ? { tariff: basis.tariffRef } : {}),
    };
  }
  // A sandbox whose end was never confirmed may run to its own deadline.
  return {
    usd: usd(basis.maximumUsd),
    basis: 'maximum_reservation',
    seconds: basis.maximumSeconds,
    usdPerSec: basis.usdPerSec,
  };
}

/** Fixed fields only: no prompts, provider errors, workspace URLs or credentials. */
export function runAccounting(input: {
  basis: RunAccountingBasis | undefined;
  usage: TokenUsage | undefined;
  /** Model plus the sandbox reservation, as held by the spend ledger. */
  nonComputeUsd: number | undefined;
  compute: ComputeCleanupReport | undefined;
  /** What the ledger charges the run; the total whenever any line is not final. */
  conservativeTotalUsd: number;
}): RunAccounting {
  const known = input.basis && validAccountingBasis(input.basis) ? input.basis : undefined;
  const settled = input.compute?.settled ?? [];
  const gpu: RunAccounting['gpu'] = input.compute
    ? {
        usd: usd(
          settled.reduce((sum, job) => sum + job.receipt.charged_usdc_micros, 0) / 1_000_000,
        ),
        basis: 'compute_receipt',
        runtimeSecs: settled.reduce((sum, job) => sum + job.receipt.runtime_secs, 0),
        jobs: settled.map((job) => job.job_id),
      }
    : null;
  const coveredByCovenant = input.compute
    ? {
        gpuProvisioningUsd: usd(
          settled.reduce((sum, job) => sum + (job.receipt.provisioning_usdc_micros ?? 0), 0) /
            1_000_000,
        ),
        gpuProvisioningSecs: settled.reduce(
          (sum, job) => sum + (job.receipt.provisioning_secs ?? 0),
          0,
        ),
      }
    : null;
  const usage = safeUsage(input.usage);

  if (!known) {
    return {
      version: 2,
      final: false,
      totalUsd: usd(input.conservativeTotalUsd),
      gpu,
      model: { usd: null, basis: 'unknown', usage },
      sandbox: { usd: null, basis: 'unknown', seconds: null, usdPerSec: null },
      coveredByCovenant,
    };
  }

  const sandbox = sandboxLine(known.sandbox);
  // The ledger always holds the sandbox at its reservation; what remains is model spend.
  const ledgerModelUsd =
    input.nonComputeUsd === undefined
      ? null
      : usd(
          Math.max(
            0,
            input.nonComputeUsd - (known.sandbox.attempted ? known.sandbox.maximumUsd : 0),
          ),
        );
  const priced = {
    ...(known.model.name ? { name: known.model.name } : {}),
    usage,
    ...(known.model.prices ? { usdPerMillionTokens: known.model.prices } : {}),
  };
  const model: RunAccounting['model'] =
    known.model.method === 'usage_estimate' && known.model.estimatedUsd !== undefined
      ? { usd: usd(known.model.estimatedUsd), basis: 'usage_at_list_price', ...priced }
      : known.model.method === 'no_requests'
        ? { usd: 0, basis: 'no_requests', usage }
        : known.model.method === 'provider_receipt'
          ? { usd: ledgerModelUsd, basis: 'provider_price_ceiling', usage }
          : { usd: ledgerModelUsd, basis: 'conservative_hold', usage };

  const final =
    (!input.compute || input.compute.complete) &&
    ['usage_at_list_price', 'no_requests'].includes(model.basis) &&
    ['metered_at_list_price', 'not_started'].includes(sandbox.basis);
  return {
    version: 2,
    final,
    totalUsd: final
      ? usd((gpu?.usd ?? 0) + (model.usd ?? 0) + (sandbox.usd ?? 0))
      : usd(input.conservativeTotalUsd),
    gpu,
    model,
    sandbox,
    coveredByCovenant,
  };
}
