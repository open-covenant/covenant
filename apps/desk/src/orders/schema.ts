/**
 * Order validation.
 *
 * An order that reaches the store has already been checked for a token pair
 * that makes sense, an amount above zero, a trigger that can fire, and bounds
 * inside the configured caps. Refusals name the field and the value.
 */

import { z } from 'zod';
import type { Address, OrderBounds, OrderKind, OrderSide, OrderTrigger } from '../core/types.js';

/** 20-byte address, `0x`-prefixed. Normalised to lowercase, as the store keeps it. */
export const AddressSchema = z
  .string()
  .regex(/^0x[0-9a-fA-F]{40}$/, 'must be a 20-byte address, 0x followed by 40 hex characters')
  .transform((value) => value.toLowerCase() as Address);

/**
 * Amount in smallest units. Accepts a bigint, a decimal string, or a safe
 * integer, because JSON has no bigint.
 */
export const RawAmountSchema = z
  .union([
    z.bigint(),
    z.string().regex(/^\d+$/, 'must be a whole number of smallest units'),
    z.number().int().nonnegative(),
  ])
  .transform((value) => BigInt(value))
  .refine((value) => value > 0n, 'must be above zero');

export const OrderKindSchema = z.enum(['limit', 'stop', 'takeProfit', 'oco', 'atOpen', 'premium']);
export const OrderSideSchema = z.enum(['buy', 'sell']);

/** Highest offset accepted after the next open: thirty days. */
const MAX_OPEN_OFFSET_SEC = 30 * 24 * 60 * 60;

export const OrderTriggerSchema = z
  .strictObject({
    priceLte: z.number().positive().finite().optional(),
    priceGte: z.number().positive().finite().optional(),
    priceBasis: z.enum(['usdOnchain', 'usdFair']).optional(),
    premiumLteBps: z.number().finite().optional(),
    premiumGteBps: z.number().finite().optional(),
    atNextOpenOffsetSec: z.number().int().min(0).max(MAX_OPEN_OFFSET_SEC).optional(),
    at: z.number().int().positive().optional(),
  })
  .superRefine((trigger, ctx) => {
    if (trigger.priceGte !== undefined && trigger.priceLte !== undefined && trigger.priceGte > trigger.priceLte) {
      ctx.addIssue({
        code: 'custom',
        path: ['priceGte'],
        message: `priceGte ${trigger.priceGte} is above priceLte ${trigger.priceLte}, so no price can satisfy both.`,
      });
    }
    if (
      trigger.premiumGteBps !== undefined &&
      trigger.premiumLteBps !== undefined &&
      trigger.premiumGteBps > trigger.premiumLteBps
    ) {
      ctx.addIssue({
        code: 'custom',
        path: ['premiumGteBps'],
        message: `premiumGteBps ${trigger.premiumGteBps} is above premiumLteBps ${trigger.premiumLteBps}, so no premium can satisfy both.`,
      });
    }
  });

export const OrderBoundsOverrideSchema = z.strictObject({
  maxSlippageBps: z.number().int().min(0).max(10_000).optional(),
  maxOrderNotionalUsd: z.number().positive().finite().optional(),
  maxBuyPremiumBps: z.number().int().min(0).max(100_000).optional(),
});

const LegShape = {
  side: OrderSideSchema,
  tokenIn: AddressSchema,
  tokenOut: AddressSchema,
  amountIn: RawAmountSchema,
  trigger: OrderTriggerSchema,
  bounds: OrderBoundsOverrideSchema.optional(),
  live: z.boolean().default(false),
  expiresAt: z.number().int().positive().nullish().transform((value) => value ?? null),
};

const LegSchema = z.strictObject(LegShape).superRefine(sameTokenCheck);

/** One branch of an OCO pair, or a standalone order once `kind` is applied. */
export type ParsedLeg = z.infer<typeof LegSchema>;

export const CreateOrderInputSchema = z
  .strictObject({
    ...LegShape,
    kind: OrderKindSchema,
    legs: z.tuple([LegSchema, LegSchema]).optional(),
  })
  .superRefine(sameTokenCheck)
  .superRefine((input, ctx) => {
    if (input.kind === 'oco') {
      if (!input.legs) {
        ctx.addIssue({
          code: 'custom',
          path: ['legs'],
          message: 'An OCO order needs two legs. The first one to fill cancels the other.',
        });
        return;
      }
      if (countConditions(input.trigger) > 0) {
        ctx.addIssue({
          code: 'custom',
          path: ['trigger'],
          message: 'An OCO order carries its conditions on each leg. Leave the top-level trigger empty.',
        });
      }
      for (const [index, leg] of input.legs.entries()) {
        if (countConditions(leg.trigger) === 0) {
          ctx.addIssue({
            code: 'custom',
            path: ['legs', index, 'trigger'],
            message: 'Each leg needs at least one condition.',
          });
        }
      }
      return;
    }

    if (input.legs) {
      ctx.addIssue({
        code: 'custom',
        path: ['legs'],
        message: `Legs belong to an OCO order. This one is a ${input.kind} order.`,
      });
    }
    if (countConditions(input.trigger) === 0) {
      ctx.addIssue({
        code: 'custom',
        path: ['trigger'],
        message: 'An order needs at least one condition: a price, a premium, or a time.',
      });
      return;
    }
    const missing = missingConditionFor(input.kind, input.trigger);
    if (missing) ctx.addIssue({ code: 'custom', path: ['trigger'], message: missing });
  });

export type ParsedCreateOrderInput = z.infer<typeof CreateOrderInputSchema>;

/** Minimal shape of the zod refinement context, so this helper works on any object schema. */
interface IssueSink {
  addIssue(issue: { code: 'custom'; path: (string | number)[]; message: string }): void;
}

function sameTokenCheck(input: { tokenIn: string; tokenOut: string }, ctx: IssueSink): void {
  if (input.tokenIn === input.tokenOut) {
    ctx.addIssue({
      code: 'custom',
      path: ['tokenOut'],
      message: `tokenIn and tokenOut are both ${input.tokenIn}. A swap needs two different tokens.`,
    });
  }
}

/** Number of conditions present on a trigger. `priceBasis` selects a price, it is not a condition. */
export function countConditions(trigger: OrderTrigger): number {
  let count = 0;
  if (trigger.priceLte !== undefined) count += 1;
  if (trigger.priceGte !== undefined) count += 1;
  if (trigger.premiumLteBps !== undefined) count += 1;
  if (trigger.premiumGteBps !== undefined) count += 1;
  if (trigger.atNextOpenOffsetSec !== undefined) count += 1;
  if (trigger.at !== undefined) count += 1;
  return count;
}

/** The condition a kind requires, when the trigger does not carry it. */
function missingConditionFor(kind: OrderKind, trigger: OrderTrigger): string | undefined {
  const hasPrice = trigger.priceLte !== undefined || trigger.priceGte !== undefined;
  const hasPremium = trigger.premiumLteBps !== undefined || trigger.premiumGteBps !== undefined;
  const hasTime = trigger.at !== undefined || trigger.atNextOpenOffsetSec !== undefined;
  switch (kind) {
    case 'limit':
    case 'stop':
    case 'takeProfit':
      return hasPrice ? undefined : `A ${kind} order needs priceLte or priceGte.`;
    case 'premium':
      return hasPremium ? undefined : 'A premium order needs premiumLteBps or premiumGteBps.';
    case 'atOpen':
      return hasTime ? undefined : 'An atOpen order needs atNextOpenOffsetSec or at.';
    default:
      return undefined;
  }
}

/** Bounds after config defaults are applied under the caller's overrides. */
export function resolveBounds(
  configured: { maxSlippageBps: number; maxOrderNotionalUsd: number; maxBuyPremiumBps: number },
  overrides: Partial<OrderBounds> | undefined,
): OrderBounds {
  return {
    maxSlippageBps: overrides?.maxSlippageBps ?? configured.maxSlippageBps,
    maxOrderNotionalUsd: overrides?.maxOrderNotionalUsd ?? configured.maxOrderNotionalUsd,
    maxBuyPremiumBps: overrides?.maxBuyPremiumBps ?? configured.maxBuyPremiumBps,
  };
}

/** Side and kind for the audit trail, typed for the store. */
export type { OrderKind, OrderSide };
