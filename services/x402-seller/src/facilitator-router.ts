/**
 * Sending each asset to the facilitator that will actually settle it.
 *
 * Coinbase's facilitator settles USDC on Solana and refuses anything else with
 * `preflight_validation_failed`, which is what a MIZUKI payment gets. Our own
 * facilitator settles any SPL or Token-2022 transfer, but a resource that
 * settles there is invisible in the Bazaar, because that catalog is built from
 * what Coinbase settles.
 *
 * Neither one alone works, so each payment goes to whichever facilitator can
 * complete it: USDC to Coinbase, so the resource stays discoverable, and
 * everything else to ours, so the token is genuinely spendable.
 *
 * The fee payer differs between them, so the challenge has to advertise the
 * sponsor belonging to the facilitator that will settle that particular asset.
 * Getting that wrong produces a transaction nobody will sign.
 */

import type { FacilitatorClient } from '@x402/core/server';
import type {
  PaymentPayload,
  PaymentRequirements,
  SettleResponse,
  SupportedResponse,
  VerifyResponse,
} from '@x402/core/types';

export interface AssetRoutedFacilitatorConfig {
  /** Settles the default asset and keeps the resource in the Bazaar. */
  primary: FacilitatorClient;
  /** Settles assets the primary refuses. */
  fallback: FacilitatorClient;
  /** Assets the primary will settle. Anything else goes to the fallback. */
  primaryAssets: readonly string[];
}

export class AssetRoutedFacilitator implements FacilitatorClient {
  private readonly primary: FacilitatorClient;
  private readonly fallback: FacilitatorClient;
  private readonly primaryAssets: Set<string>;

  constructor(config: AssetRoutedFacilitatorConfig) {
    this.primary = config.primary;
    this.fallback = config.fallback;
    this.primaryAssets = new Set(config.primaryAssets);
  }

  /**
   * Which facilitator settles this asset.
   *
   * @param requirements - The requirements the payer chose
   * @returns The facilitator that can settle them
   */
  private route(requirements: PaymentRequirements): FacilitatorClient {
    const asset = (requirements as { asset?: unknown }).asset;
    return typeof asset === 'string' && this.primaryAssets.has(asset)
      ? this.primary
      : this.fallback;
  }

  async verify(
    payload: PaymentPayload,
    requirements: PaymentRequirements,
  ): Promise<VerifyResponse> {
    return this.route(requirements).verify(payload, requirements);
  }

  async settle(
    payload: PaymentPayload,
    requirements: PaymentRequirements,
  ): Promise<SettleResponse> {
    // Verification and settlement must reach the same facilitator, because only
    // the one that verified holds the fee payer the payer signed against.
    return this.route(requirements).settle(payload, requirements);
  }

  /**
   * What the resource server advertises it can take.
   *
   * Reported by the primary, because that is the answer the Bazaar reads. A
   * fallback outage should not silently unpublish the resource.
   */
  async getSupported(): Promise<SupportedResponse> {
    return this.primary.getSupported();
  }
}
