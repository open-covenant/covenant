import { createHash } from 'node:crypto';
import { extractDiscoveryInfo, validateDiscoveryExtensionSpec } from '@x402/extensions/bazaar';
import type { PaymentPayload, PaymentRequirements, SettleResponse, VerifyResponse } from '@x402/core/types';
import { USDC, type FacilitatorConfig } from './config.js';
import { normalAddress, sameAddress, utcDay, type CatalogItem, type Store } from './store.js';

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
const REQUIREMENT_FIELDS = ['scheme', 'network', 'asset', 'amount', 'payTo', 'maxTimeoutSeconds'] as const;
/**
 * The parts of `extra` a buyer needs to pay again. Sellers also put per-quote
 * data there (quote ids, request hashes, expiry), which has no place in a
 * public catalog.
 */
const CATALOG_EXTRA_FIELDS = ['feePayer', 'name', 'version'] as const;

/**
 * The checks a public facilitator needs before it spends its own gas: the
 * x402 libraries verify that a payment is real, not that settling it is worth
 * paying for. Anyone can call /settle, so a payment to oneself, a dust amount,
 * a flood of settlements or a burst of conflicting payments from one wallet
 * would otherwise be paid for by the fee payer.
 */
export class Gate {
  private readonly verified = new Map<string, { payer: string; atMs: number }>();
  /** Settles between their cap check and their ledger write, counted against the caps. */
  private readonly reserved = new Map<string, number>();
  /** One settle per payer at a time: concurrent payments from one balance can all simulate clean and all but one fail on chain. */
  private readonly settlingPayers = new Set<string>();

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
    const result = this.withoutMessages(await this.core.verify(payload, requirements));
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

    const payerKey = `${network} ${normalAddress(payer)}`;
    if (this.settlingPayers.has(payerKey)) return fail('payer_settlement_in_progress', payer);

    // Check and reserve with no await in between, so concurrent settles cannot overshoot a cap.
    const metered = !this.config.unmeteredPayTo.some((address) => sameAddress(address, requirements.payTo));
    const day = utcDay(this.now());
    const networkKey = `${day} ${network}`;
    const payToKey = `${networkKey} ${normalAddress(requirements.payTo)}`;
    if (metered) {
      if (this.count(networkKey, network, day) >= this.config.dailySettlements) {
        return fail('facilitator_daily_limit_reached', payer);
      }
      if (this.count(payToKey, network, day, requirements.payTo) >= this.config.dailySettlementsPerPayTo) {
        return fail('pay_to_daily_limit_reached', payer);
      }
      this.adjust(networkKey, 1);
      this.adjust(payToKey, 1);
    }
    this.settlingPayers.add(payerKey);

    let response: SettleResponse;
    try {
      response = this.withoutMessages(await this.core.settle(payload, requirements));
      if (response.transaction) {
        this.store.recordSettlement({
          network,
          transaction: response.transaction,
          payTo: requirements.payTo,
          payer: response.payer ?? payer,
          amount: requirements.amount,
          metered,
          succeeded: response.success,
          settledAtMs: this.now(),
        });
      }
    } finally {
      this.settlingPayers.delete(payerKey);
      if (metered) {
        this.adjust(networkKey, -1);
        this.adjust(payToKey, -1);
      }
    }
    if (!response.success) return { response };
    this.log({ event: 'facilitator_settled', network, transaction: response.transaction, metered });
    return { response, bazaar: this.catalog(payload, requirements) };
  }

  /** RPC error text can carry provider URLs and keys, so callers get reason codes only; the log keeps it redacted. */
  private withoutMessages<T extends object>(response: T): T {
    const { invalidMessage, errorMessage, ...rest } = response as T & { invalidMessage?: unknown; errorMessage?: unknown };
    const message = invalidMessage ?? errorMessage;
    if (message !== undefined) this.log({ event: 'facilitator_provider_message', message: redact(message) });
    return rest as T;
  }

  private count(key: string, network: string, day: string, payTo?: string): number {
    return this.store.meteredSettlements(network, day, payTo) + (this.reserved.get(key) ?? 0);
  }

  private adjust(key: string, delta: number): void {
    const next = (this.reserved.get(key) ?? 0) + delta;
    if (next > 0) this.reserved.set(key, next);
    else this.reserved.delete(key);
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
        if (now - entry.atMs > VERIFIED_TTL_MS) this.verified.delete(key);
      }
      // Still full of fresh entries: forget the oldest half rather than rescan on every insert.
      if (this.verified.size >= MAX_VERIFIED) {
        let drop = this.verified.size / 2;
        for (const key of this.verified.keys()) {
          if (drop-- <= 0) break;
          this.verified.delete(key);
        }
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

  /**
   * Bazaar: catalog the resource behind a settled payment that declared one.
   * The declaration is checked against the spec's own rules, never against
   * the schema it carries: compiling a caller's schema runs the caller's
   * regular expressions on this process's only thread.
   */
  private catalog(payload: PaymentPayload, requirements: PaymentRequirements): BazaarOutcome | undefined {
    const declared = payload.extensions?.bazaar;
    if (declared === undefined) return undefined;
    try {
      if (typeof declared !== 'object' || declared === null || !validateDiscoveryExtensionSpec(declared as never).valid) {
        return { status: 'rejected', rejectedReason: 'discovery info does not follow the bazaar spec' };
      }
      const found = extractDiscoveryInfo(payload, requirements, false);
      if (!found) return { status: 'rejected', rejectedReason: 'discovery info is missing' };
      const url = new URL(found.resourceUrl);
      if (url.protocol !== 'https:' && url.protocol !== 'http:') {
        return { status: 'rejected', rejectedReason: 'resource URL must be http or https' };
      }
      const input = found.discoveryInfo.input as { type: string; method?: unknown; toolName?: unknown };
      const accepted: Record<string, unknown> = Object.fromEntries(
        REQUIREMENT_FIELDS.filter((field) => requirements[field] !== undefined).map((field) => [field, requirements[field]]),
      );
      const extra = (requirements.extra ?? {}) as Record<string, unknown>;
      const kept = Object.fromEntries(CATALOG_EXTRA_FIELDS.filter((field) => typeof extra[field] === 'string').map((field) => [field, extra[field]]));
      if (Object.keys(kept).length > 0) accepted.extra = kept;
      const item: CatalogItem = {
        resource: found.resourceUrl,
        type: input.type,
        x402Version: found.x402Version,
        accepts: [accepted],
        lastUpdated: new Date(this.now()).toISOString(),
        ...(typeof found.description === 'string' ? { description: found.description.slice(0, 500) } : {}),
        ...(typeof found.mimeType === 'string' ? { mimeType: found.mimeType.slice(0, 100) } : {}),
        ...(found.serviceName ? { serviceName: found.serviceName } : {}),
        ...(found.tags?.length ? { tags: found.tags } : {}),
        ...(found.iconUrl ? { iconUrl: found.iconUrl } : {}),
        extensions: { bazaar: declared },
      };
      if (Buffer.byteLength(JSON.stringify(item)) > MAX_CATALOG_ITEM_BYTES) {
        return { status: 'rejected', rejectedReason: 'discovery info is too large' };
      }
      const method = String(input.type === 'mcp' ? input.toolName : input.method ?? '');
      this.store.upsertCatalogItem(item, method, this.now());
      return { status: 'success' };
    } catch (error) {
      // The payment has settled; a listing problem must not turn that into an error.
      this.log({ event: 'facilitator_catalog_failed', message: String(error) });
      return { status: 'rejected', rejectedReason: 'discovery info could not be cataloged' };
    }
  }
}

/** Provider URLs can carry API keys, so they never reach a log line. */
function redact(message: unknown): string {
  return String(message).replace(/[a-z]+:\/\/[^\s"']+/gi, '<url>').slice(0, 500);
}
