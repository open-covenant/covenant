/**
 * Swap encoding and execution through the UniversalRouter.
 *
 * The router takes a string of command bytes and one input per command. A v4
 * swap is a single command, `V4_SWAP`, whose input is a second string of action
 * bytes and one parameter per action. The router on 4663 runs the v4 periphery whose single-swap struct still carries `sqrtPriceLimitX96` (sent as 0, no limit). The desk sends three actions every time:
 * the swap, then `SETTLE_ALL` to pay the pool from the caller's Permit2
 * allowance, then `TAKE_ALL` to collect the output. `TAKE_ALL` carries the
 * minimum, so a route that fills worse than the quote reverts rather than
 * settling.
 *
 * Encoding is pure and is covered by a fixed vector in the tests. Sending is
 * separate, refuses unless the desk is live and the order asked for it, and is
 * never exercised by a test.
 */

import {
  concatHex,
  encodeAbiParameters,
  encodeFunctionData,
  maxUint160,
  maxUint256,
  toHex,
} from 'viem';
import type { Config } from '../core/config.js';
import { liveBlockedReason } from '../core/config.js';
import type { Logger } from '../core/logger.js';
import { KeystoreError, LiveDisabledError, UpstreamError } from '../core/errors.js';
import { quantity, type Address, type Hex32, type Quantity } from '../core/types.js';
import { erc20Abi, permit2Abi, swapActionAbi, universalRouterAbi } from './abis.js';
import type { DeskChainClient } from './client.js';
import { effectivePriceOf } from './price.js';
import { poolKeyOf } from './pools.js';
import {
  isNative,
  pathKeyFor,
  sameAddress,
  type DeskQuoter,
  type QuoteRequest,
  type RouteHop,
} from './quoter.js';

/** UniversalRouter command: run a Uniswap v4 action list. */
export const V4_SWAP = 0x10;
/** v4 router actions the desk encodes. */
export const SWAP_EXACT_IN_SINGLE = 0x06;
export const SWAP_EXACT_IN = 0x07;
export const SETTLE_ALL = 0x0c;
export const TAKE_ALL = 0x0f;

/** How long a Permit2 allowance is granted for, seconds. */
const PERMIT2_EXPIRATION_SEC = 30 * 24 * 60 * 60;

export interface SwapRequest extends QuoteRequest {
  /** Smallest units of `tokenOut`. Below this the swap reverts. */
  readonly minAmountOut: bigint;
  /** Seconds from now. */
  readonly deadlineSec: number;
  /** True signs and sends. False simulates and returns the same shape. */
  readonly live: boolean;
}

export interface SwapResult {
  readonly txHash?: Hex32;
  readonly amountIn: bigint;
  readonly amountOut: bigint;
  /** `tokenOut` per `tokenIn`, decimals applied. */
  readonly effectivePrice: Quantity;
  readonly gasUsed?: bigint;
  readonly blockNumber?: bigint;
  readonly simulated: boolean;
  /** Plain reason a request that asked to go live stayed a simulation. */
  readonly reason?: string;
}

/** Everything the encoder needs, with no clock and no network. */
export interface EncodeInput {
  readonly hops: readonly RouteHop[];
  readonly tokenIn: Address;
  readonly tokenOut: Address;
  readonly amountIn: bigint;
  readonly minAmountOut: bigint;
  /** Absolute deadline, seconds since the Unix epoch. */
  readonly deadline: bigint;
}

export interface EncodedSwap {
  readonly to: Address;
  readonly data: `0x${string}`;
  readonly value: bigint;
  readonly commands: `0x${string}`;
  readonly inputs: readonly `0x${string}`[];
  readonly actions: `0x${string}`;
}

export interface DeskSwap {
  encode(request: SwapRequest): Promise<{ to: Address; data: `0x${string}`; value: bigint }>;
  ensureApprovals(
    token: Address,
    amount: bigint,
  ): Promise<{ approved: boolean; txHashes: Hex32[] }>;
  execute(request: SwapRequest): Promise<SwapResult>;
}

export interface SwapDeps {
  readonly client: DeskChainClient;
  readonly config: Config;
  readonly logger: Logger;
  readonly quoter: DeskQuoter;
  readonly routerAddress: Address;
  readonly permit2Address: Address;
}

/**
 * Build the UniversalRouter calldata for a v4 swap.
 *
 * Layout of the single input, which is `abi.encode(bytes actions, bytes[]
 * params)`:
 *
 *   actions   0x06 0x0c 0x0f  for one pool
 *             0x07 0x0c 0x0f  for two or more
 *   params[0] the swap: the pool key and direction for a single hop, or the
 *             input currency and the path for a multi-hop route
 *   params[1] SETTLE_ALL (currencyIn, amountIn), the most the router may pull
 *   params[2] TAKE_ALL (currencyOut, minAmountOut), the least it must deliver
 */
export function encodeV4Swap(input: EncodeInput, router: Address): EncodedSwap {
  const firstHop = input.hops[0];
  if (!firstHop) throw new UpstreamError('swap', 'a swap needs at least one hop');

  const single = input.hops.length === 1;
  const actions = concatHex([
    toHex(single ? SWAP_EXACT_IN_SINGLE : SWAP_EXACT_IN, { size: 1 }),
    toHex(SETTLE_ALL, { size: 1 }),
    toHex(TAKE_ALL, { size: 1 }),
  ]);

  const swapParams = single
    ? encodeAbiParameters(swapActionAbi.exactInputSingle, [
        {
          poolKey: poolKeyOf(firstHop.pool),
          zeroForOne: sameAddress(firstHop.pool.currency0, input.tokenIn),
          amountIn: input.amountIn,
          amountOutMinimum: input.minAmountOut,
          sqrtPriceLimitX96: 0n,
          hookData: '0x',
        },
      ])
    : encodeAbiParameters(swapActionAbi.exactInput, [
        {
          currencyIn: input.tokenIn,
          path: input.hops.map((hop) => pathKeyFor(hop)),
          amountIn: input.amountIn,
          amountOutMinimum: input.minAmountOut,
        },
      ]);

  const settleParams = encodeAbiParameters(swapActionAbi.currencyAndAmount, [
    input.tokenIn,
    input.amountIn,
  ]);
  const takeParams = encodeAbiParameters(swapActionAbi.currencyAndAmount, [
    input.tokenOut,
    input.minAmountOut,
  ]);

  const routerInput = encodeAbiParameters(
    [{ type: 'bytes' }, { type: 'bytes[]' }],
    [actions, [swapParams, settleParams, takeParams]],
  );
  const commands = toHex(V4_SWAP, { size: 1 });

  return {
    to: router,
    data: encodeFunctionData({
      abi: universalRouterAbi,
      functionName: 'execute',
      args: [commands, [routerInput], input.deadline],
    }),
    value: isNative(input.tokenIn) ? input.amountIn : 0n,
    commands,
    inputs: [routerInput],
    actions,
  };
}

export function createSwap(deps: SwapDeps): DeskSwap {
  const logger = deps.logger.child({ component: 'chain.swap' });

  const build = async (
    request: SwapRequest,
  ): Promise<{ hops: RouteHop[]; encoded: EncodedSwap }> => {
    const hops = await deps.quoter.route(
      request.tokenIn,
      request.tokenOut,
      request.poolId,
      request.route,
    );
    const deadline = BigInt(Math.floor(Date.now() / 1000) + request.deadlineSec);
    return {
      hops,
      encoded: encodeV4Swap(
        {
          hops,
          tokenIn: request.tokenIn,
          tokenOut: request.tokenOut,
          amountIn: request.amountIn,
          minAmountOut: request.minAmountOut,
          deadline,
        },
        deps.routerAddress,
      ),
    };
  };

  const decimalsFor = (hop: RouteHop, token: Address): number =>
    sameAddress(hop.pool.currency0, token) ? hop.pool.decimals0 : hop.pool.decimals1;

  const priceOf = (
    request: SwapRequest,
    hops: readonly RouteHop[],
    amountOut: bigint,
  ): Quantity => {
    const first = hops[0];
    const last = hops[hops.length - 1];
    if (!first || !last) throw new UpstreamError('swap', 'a swap needs at least one hop');
    const value = effectivePriceOf(
      request.amountIn,
      decimalsFor(first, request.tokenIn),
      amountOut,
      decimalsFor(last, request.tokenOut),
    );
    return quantity(value, 'token', 'pool');
  };

  const ensureApprovals = async (
    token: Address,
    amount: bigint,
  ): Promise<{ approved: boolean; txHashes: Hex32[] }> => {
    {
      const wallet = deps.client.walletClient();
      const account = deps.client.account();
      if (!wallet || !account) {
        throw new KeystoreError('No signing key is loaded, so approvals cannot be set.', {
          key: 'DESK_EVM_PRIVATE_KEY',
        });
      }
      const txHashes: Hex32[] = [];

      const erc20Allowance = (await deps.client.publicClient.readContract({
        address: token,
        abi: erc20Abi,
        functionName: 'allowance',
        args: [account.address, deps.permit2Address],
      })) as bigint;
      if (erc20Allowance < amount) {
        const hash = await wallet.writeContract({
          address: token,
          abi: erc20Abi,
          functionName: 'approve',
          args: [deps.permit2Address, maxUint256],
          chain: null,
          account,
        });
        await deps.client.publicClient.waitForTransactionReceipt({ hash });
        txHashes.push(hash as Hex32);
        logger.info('token approved to Permit2', { token });
      }

      const [permitAmount, expiration] = (await deps.client.publicClient.readContract({
        address: deps.permit2Address,
        abi: permit2Abi,
        functionName: 'allowance',
        args: [account.address, token, deps.routerAddress],
      })) as readonly [bigint, number, number];
      const nowSec = Math.floor(Date.now() / 1000);
      if (permitAmount < amount || Number(expiration) <= nowSec) {
        const hash = await wallet.writeContract({
          address: deps.permit2Address,
          abi: permit2Abi,
          functionName: 'approve',
          args: [token, deps.routerAddress, maxUint160, nowSec + PERMIT2_EXPIRATION_SEC],
          chain: null,
          account,
        });
        await deps.client.publicClient.waitForTransactionReceipt({ hash });
        txHashes.push(hash as Hex32);
        logger.info('router approved through Permit2', { token });
      }

      return { approved: txHashes.length > 0, txHashes };
    }
  };

  const execute = async (request: SwapRequest): Promise<SwapResult> => {
    {
      const quote = await deps.quoter.quoteExactInput(request);
      const { hops, encoded } = await build(request);

      const blocked = request.live
        ? liveBlockedReason(deps.config)
        : 'The order was created as a dry run.';
      const account = deps.client.account();

      if (blocked || !account) {
        const reason = blocked ?? 'No signing key is loaded.';
        // A dry run still checks the route against the chain, so the recorded
        // fill is a real quote rather than an estimate.
        logger.info('swap simulated', {
          reason,
          amountIn: request.amountIn,
          amountOut: quote.amountOut,
        });
        return {
          amountIn: request.amountIn,
          amountOut: quote.amountOut,
          effectivePrice: priceOf(request, hops, quote.amountOut),
          simulated: true,
          blockNumber: quote.blockNumber,
          reason,
        };
      }

      const wallet = deps.client.walletClient();
      if (!wallet) throw new LiveDisabledError('No signing key is loaded, so nothing can be sent.');

      // The router pulls the input through Permit2, so both allowances have to
      // exist before anything is simulated. Simulating first would revert on
      // the transfer and report the route as broken.
      if (!isNative(request.tokenIn)) await ensureApprovals(request.tokenIn, request.amountIn);

      try {
        await deps.client.publicClient.call({
          account: account.address,
          to: encoded.to,
          data: encoded.data,
          value: encoded.value,
        });
      } catch (error) {
        throw new UpstreamError('universal-router', `the swap would revert: ${messageOf(error)}`, {
          tokenIn: request.tokenIn,
          tokenOut: request.tokenOut,
        });
      }

      const before = await balanceOf(deps, request.tokenOut, account.address);
      const hash = await wallet.sendTransaction({
        to: encoded.to,
        data: encoded.data,
        value: encoded.value,
        chain: null,
        account,
      });
      const receipt = await deps.client.publicClient.waitForTransactionReceipt({ hash });
      const after = await balanceOf(deps, request.tokenOut, account.address);
      // Gas comes out of the same balance as a swap into native ether, so the
      // fee is added back to leave the amount the pool actually paid.
      const gasPaid = isNative(request.tokenOut)
        ? receipt.gasUsed * (receipt.effectiveGasPrice ?? 0n)
        : 0n;
      const amountOut = after - before + gasPaid;

      logger.info('swap sent', { txHash: hash, amountIn: request.amountIn, amountOut });
      return {
        txHash: hash as Hex32,
        amountIn: request.amountIn,
        amountOut,
        effectivePrice: priceOf(request, hops, amountOut),
        gasUsed: receipt.gasUsed,
        blockNumber: receipt.blockNumber,
        simulated: false,
      };
    }
  };

  return {
    async encode(request) {
      const { encoded } = await build(request);
      return { to: encoded.to, data: encoded.data, value: encoded.value };
    },
    ensureApprovals,
    execute,
  };
}

async function balanceOf(deps: SwapDeps, token: Address, holder: Address): Promise<bigint> {
  if (isNative(token)) return deps.client.publicClient.getBalance({ address: holder });
  return (await deps.client.publicClient.readContract({
    address: token,
    abi: erc20Abi,
    functionName: 'balanceOf',
    args: [holder],
  })) as bigint;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
