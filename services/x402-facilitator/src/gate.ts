import { createHash } from 'node:crypto';
import { extractDiscoveryInfo } from '@x402/extensions/bazaar';
import type { PaymentPayload, PaymentRequirements, SettleResponse, VerifyResponse } from '@x402/core/types';
import { USDC, type FacilitatorConfig } from './config.js';
import { sameAddress, utcDay, type CatalogItem, type Store } from './store.js';

/** The x402 facilitator this gate stands in front of. */
export interface FacilitatorCore {
  verify(payload: PaymentPayload, requirements: PaymentRequirements): Promise<VerifyResponse>;
  settle(payload: PaymentPayload, requirements: PaymentRequirements): Promise<SettleResponse>;
}

export interface BazaarOutcome {
  status: 'success' | 'rejected';
  rejectedReason?: string;
}

export interface SettleOutcome {
  response: SettleResponse;
  bazaar?: BazaarOutcome;
}

type Log = (event: Record<string, unknown>) => void;

const VERIFIED_TTL_MS = 5 * 60_000;
const MAX_VERIFIED = 10_000;
const MAX_CATALOG_ITEM_BYTES = 32 * 1024;

/**
 * The checks a public facilitator needs before it spends its own gas: the
 * x402 libraries verify that a payment is real, not that settling it is worth
 * paying for. Anyone can call /settle, so a payment to oneself, a dust amount
 * or a flood of settlements would otherwise be paid for by the fee payer.
 */
export class Gate {
  private readonly verified = new Map<string, { payer: string; atMs: number }>();

  constructor(
    private readonly core: FacilitatorCore,
    private readonly store: Store,
    private readonly config: Pick<
      FacilitatorConfig,
      'minAmount' | 'dailySettlements' | 'dailySettlementsPerPayTo' | 'unmeteredPayTo'
    >,
    private readonly networks: ReadonlySet<string>,
    private readonly log: Log = () => {},
    private readonly now: () => number = Date.now,
  ) {}

  async verify(payload: PaymentPayload, requirements: PaymentRequirements): Promise<VerifyResponse> {
    const refused = this.refuse(payload, requirements);
    if (refused) return { isValid: false, invalidReason: refused };
    const result = await this.core.verify(payload, requirements);
    if (!result.isValid) return result;
    if (sameAddress(result.payer, requirements.payTo)) {
      return { isValid: false, invalidReason: 'self_send_not_allowed', payer: result.payer };
    }
    if (result.payer) this.remember(payload, result.payer);
    return result;
  }

  async settle(payload: PaymentPayload, requirements: PaymentRequirements): Promise<SettleOutcome> {
    const network = requirements?.network;
    const fail = (errorReason: string, payer?: string): SettleOutcome => ({
      response: { success: false, errorReason, transaction: '', network, ...(payer ? { payer } : {}) },
    });
    const refused = this.refuse(payload, requirements);
    if (refused) return fail(refused);

    // Settling re-verifies inside the scheme, but only after this gate has
    // decided to pay for it, so the payer must be known first.
    let payer = this.recall(payload);
    if (!payer) {
      const checked = await this.core.verify(payload, requirements);
      if (!checked.isValid || !checked.payer) return fail(checked.invalidReason ?? 'invalid_payment', checked.payer);
      payer = checked.payer;
    }
    if (sameAddress(payer, requirements.payTo)) return fail('self_send_not_allowed', payer);

    const metered = !this.config.unmeteredPayTo.some((address) => sameAddress(address, requirements.payTo));
    if (metered) {
      const day = utcDay(this.now());
      if (this.store.meteredSettlements(network, day) >= this.config.dailySettlements) {
        return fail('facilitator_daily_limit_reached', payer);
      }
      if (this.store.meteredSettlements(network, day, requirements.payTo) >= this.config.dailySettlementsPerPayTo) {
        return fail('pay_to_daily_limit_reached', payer);
      }
    }

    const response = await this.core.settle(payload, requirements);
    if (!response.success) return { response };
    this.store.recordSettlement({
      network,
      transaction: response.transaction,
      payTo: requirements.payTo,
      payer: response.payer ?? payer,
      amount: requirements.amount,
      metered,
      settledAtMs: this.now(),
    });
    this.log({ event: 'facilitator_settled', network, transaction: response.transaction, metered });
    return { response, bazaar: this.catalog(payload, requirements) };
  }

  private refuse(payload: PaymentPayload, requirements: PaymentRequirements): string | undefined {
    if (payload?.x402Version !== 2 || typeof requirements !== 'object' || requirements === null) {
      return 'invalid_x402_version';
    }
    if (requirements.scheme !== 'exact') return 'unsupported_scheme';
    if (!this.networks.has(requirements.network)) return 'invalid_network';
    if (!sameAddress(requirements.asset, USDC[requirements.network])) return 'unsupported_asset';
    if (typeof requirements.amount !== 'string' || !/^[0-9]{1,30}$/.test(requirements.amount)) {
      return 'invalid_amount';
    }
    if (BigInt(requirements.amount) < this.config.minAmount) return 'amount_below_minimum';
    if (typeof requirements.payTo !== 'string' || requirements.payTo.length === 0) return 'invalid_pay_to';
    return undefined;
  }

  private key(payload: PaymentPayload): string {
    return createHash('sha256').update(JSON.stringify(payload)).digest('hex');
  }

  private remember(payload: PaymentPayload, payer: string): void {
    const now = this.now();
    if (this.verified.size >= MAX_VERIFIED) {
      for (const [key, entry] of this.verified) {
        if (now - entry.atMs > VERIFIED_TTL_MS || this.verified.size >= MAX_VERIFIED) this.verified.delete(key);
        if (this.verified.size < MAX_VERIFIED / 2) break;
      }
    }
    this.verified.set(this.key(payload), { payer, atMs: now });
  }

  private recall(payload: PaymentPayload): string | undefined {
    const key = this.key(payload);
    const entry = this.verified.get(key);
    if (!entry) return undefined;
    this.verified.delete(key);
    return this.now() - entry.atMs <= VERIFIED_TTL_MS ? entry.payer : undefined;
  }

  /** Bazaar: catalog the resource behind a settled payment that declared one. */
  private catalog(payload: PaymentPayload, requirements: PaymentRequirements): BazaarOutcome | undefined {
    const declared = payload.extensions?.bazaar;
    if (declared === undefined) return undefined;
    let found: ReturnType<typeof extractDiscoveryInfo> = null;
    try {
      found = extractDiscoveryInfo(payload, requirements, true);
    } catch {
      found = null;
    }
    if (!found) return { status: 'rejected', rejectedReason: 'discovery info failed schema validation' };
    const url = new URL(found.resourceUrl);
    if (url.protocol !== 'https:' && url.protocol !== 'http:') {
      return { status: 'rejected', rejectedReason: 'resource URL must be http or https' };
    }
    const input = found.discoveryInfo.input as { type: string; method?: string; toolName?: string };
    const item: CatalogItem = {
      resource: found.resourceUrl,
      type: input.type,
      x402Version: found.x402Version,
      accepts: [requirements as unknown as Record<string, unknown>],
      lastUpdated: new Date(this.now()).toISOString(),
      ...(found.description ? { description: found.description.slice(0, 500) } : {}),
      ...(found.mimeType ? { mimeType: found.mimeType.slice(0, 100) } : {}),
      ...(found.serviceName ? { serviceName: found.serviceName } : {}),
      ...(found.tags?.length ? { tags: found.tags } : {}),
      ...(found.iconUrl ? { iconUrl: found.iconUrl } : {}),
      extensions: { bazaar: declared },
    };
    if (Buffer.byteLength(JSON.stringify(item)) > MAX_CATALOG_ITEM_BYTES) {
      return { status: 'rejected', rejectedReason: 'discovery info is too large' };
    }
    const key = input.type === 'mcp' ? `mcp ${found.resourceUrl} ${input.toolName}` : `http ${input.method} ${found.resourceUrl}`;
    this.store.upsertCatalogItem(key, item, this.now());
    return { status: 'success' };
  }
}
