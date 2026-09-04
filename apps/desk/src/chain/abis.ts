/**
 * Contract interfaces the desk reads and writes on chain 4663.
 *
 * Each ABI holds only the members the desk actually calls. Every one was
 * exercised against mainnet on 2026-09-04; the addresses live in
 * `FACTS.md` and in {@link CONTRACTS}.
 */

/** ERC-20 members the desk reads. */
export const erc20Abi = [
  {
    type: 'function',
    name: 'decimals',
    stateMutability: 'view',
    inputs: [],
    outputs: [{ type: 'uint8' }],
  },
  {
    type: 'function',
    name: 'symbol',
    stateMutability: 'view',
    inputs: [],
    outputs: [{ type: 'string' }],
  },
  {
    type: 'function',
    name: 'name',
    stateMutability: 'view',
    inputs: [],
    outputs: [{ type: 'string' }],
  },
  {
    type: 'function',
    name: 'totalSupply',
    stateMutability: 'view',
    inputs: [],
    outputs: [{ type: 'uint256' }],
  },
  {
    type: 'function',
    name: 'balanceOf',
    stateMutability: 'view',
    inputs: [{ name: 'account', type: 'address' }],
    outputs: [{ type: 'uint256' }],
  },
  {
    type: 'function',
    name: 'allowance',
    stateMutability: 'view',
    inputs: [
      { name: 'owner', type: 'address' },
      { name: 'spender', type: 'address' },
    ],
    outputs: [{ type: 'uint256' }],
  },
  {
    type: 'function',
    name: 'approve',
    stateMutability: 'nonpayable',
    inputs: [
      { name: 'spender', type: 'address' },
      { name: 'amount', type: 'uint256' },
    ],
    outputs: [{ type: 'bool' }],
  },
] as const;

/**
 * A Robinhood stock token: ERC-20 plus the ERC-8056 share fields and the two
 * issuer pause switches. There is no allowlist and no blocklist in this ABI.
 */
export const stockTokenAbi = [
  ...erc20Abi,
  {
    type: 'function',
    name: 'uiMultiplier',
    stateMutability: 'view',
    inputs: [],
    outputs: [{ type: 'uint256' }],
  },
  {
    type: 'function',
    name: 'newUIMultiplier',
    stateMutability: 'view',
    inputs: [],
    outputs: [{ type: 'uint256' }],
  },
  {
    type: 'function',
    name: 'effectiveAt',
    stateMutability: 'view',
    inputs: [],
    outputs: [{ type: 'uint256' }],
  },
  {
    type: 'function',
    name: 'balanceOfUI',
    stateMutability: 'view',
    inputs: [{ name: 'account', type: 'address' }],
    outputs: [{ type: 'uint256' }],
  },
  {
    type: 'function',
    name: 'tokenPaused',
    stateMutability: 'view',
    inputs: [],
    outputs: [{ type: 'bool' }],
  },
  {
    type: 'function',
    name: 'oraclePaused',
    stateMutability: 'view',
    inputs: [],
    outputs: [{ type: 'bool' }],
  },
] as const;

/** Chainlink `AggregatorV3Interface`. Equity feeds on 4663 publish 8 decimals. */
export const aggregatorV3Abi = [
  {
    type: 'function',
    name: 'decimals',
    stateMutability: 'view',
    inputs: [],
    outputs: [{ type: 'uint8' }],
  },
  {
    type: 'function',
    name: 'description',
    stateMutability: 'view',
    inputs: [],
    outputs: [{ type: 'string' }],
  },
  {
    type: 'function',
    name: 'latestRoundData',
    stateMutability: 'view',
    inputs: [],
    outputs: [
      { name: 'roundId', type: 'uint80' },
      { name: 'answer', type: 'int256' },
      { name: 'startedAt', type: 'uint256' },
      { name: 'updatedAt', type: 'uint256' },
      { name: 'answeredInRound', type: 'uint80' },
    ],
  },
] as const;

/** Uniswap v4 `StateView`: pool state without touching the PoolManager directly. */
export const stateViewAbi = [
  {
    type: 'function',
    name: 'getSlot0',
    stateMutability: 'view',
    inputs: [{ name: 'poolId', type: 'bytes32' }],
    outputs: [
      { name: 'sqrtPriceX96', type: 'uint160' },
      { name: 'tick', type: 'int24' },
      { name: 'protocolFee', type: 'uint24' },
      { name: 'lpFee', type: 'uint24' },
    ],
  },
  {
    type: 'function',
    name: 'getLiquidity',
    stateMutability: 'view',
    inputs: [{ name: 'poolId', type: 'bytes32' }],
    outputs: [{ name: 'liquidity', type: 'uint128' }],
  },
] as const;

/** `PoolKey`, the five fields whose hash is the pool id. */
const poolKeyComponents = [
  { name: 'currency0', type: 'address' },
  { name: 'currency1', type: 'address' },
  { name: 'fee', type: 'uint24' },
  { name: 'tickSpacing', type: 'int24' },
  { name: 'hooks', type: 'address' },
] as const;

/** `PathKey`, one hop of a multi-hop v4 route. */
const pathKeyComponents = [
  { name: 'intermediateCurrency', type: 'address' },
  { name: 'fee', type: 'uint24' },
  { name: 'tickSpacing', type: 'int24' },
  { name: 'hooks', type: 'address' },
  { name: 'hookData', type: 'bytes' },
] as const;

/**
 * `V4Quoter`. Both entry points are marked `nonpayable` because they revert
 * with the answer and are read through `eth_call`, so no state changes.
 */
export const v4QuoterAbi = [
  {
    type: 'function',
    name: 'quoteExactInputSingle',
    stateMutability: 'nonpayable',
    inputs: [
      {
        name: 'params',
        type: 'tuple',
        components: [
          { name: 'poolKey', type: 'tuple', components: poolKeyComponents },
          { name: 'zeroForOne', type: 'bool' },
          { name: 'exactAmount', type: 'uint128' },
          { name: 'hookData', type: 'bytes' },
        ],
      },
    ],
    outputs: [
      { name: 'amountOut', type: 'uint256' },
      { name: 'gasEstimate', type: 'uint256' },
    ],
  },
  {
    type: 'function',
    name: 'quoteExactInput',
    stateMutability: 'nonpayable',
    inputs: [
      {
        name: 'params',
        type: 'tuple',
        components: [
          { name: 'exactCurrency', type: 'address' },
          { name: 'path', type: 'tuple[]', components: pathKeyComponents },
          { name: 'exactAmount', type: 'uint128' },
        ],
      },
    ],
    outputs: [
      { name: 'amountOut', type: 'uint256' },
      { name: 'gasEstimate', type: 'uint256' },
    ],
  },
] as const;

/** The UniversalRouter entry point every swap goes through. */
export const universalRouterAbi = [
  {
    type: 'function',
    name: 'execute',
    stateMutability: 'payable',
    inputs: [
      { name: 'commands', type: 'bytes' },
      { name: 'inputs', type: 'bytes[]' },
      { name: 'deadline', type: 'uint256' },
    ],
    outputs: [],
  },
  {
    type: 'function',
    name: 'poolManager',
    stateMutability: 'view',
    inputs: [],
    outputs: [{ type: 'address' }],
  },
] as const;

/** Permit2, where the UniversalRouter looks for the payer's allowance. */
export const permit2Abi = [
  {
    type: 'function',
    name: 'allowance',
    stateMutability: 'view',
    inputs: [
      { name: 'owner', type: 'address' },
      { name: 'token', type: 'address' },
      { name: 'spender', type: 'address' },
    ],
    outputs: [
      { name: 'amount', type: 'uint160' },
      { name: 'expiration', type: 'uint48' },
      { name: 'nonce', type: 'uint48' },
    ],
  },
  {
    type: 'function',
    name: 'approve',
    stateMutability: 'nonpayable',
    inputs: [
      { name: 'token', type: 'address' },
      { name: 'spender', type: 'address' },
      { name: 'amount', type: 'uint160' },
      { name: 'expiration', type: 'uint48' },
    ],
    outputs: [],
  },
] as const;

/** `Initialize`, the only PoolManager event pool discovery reads. */
export const poolManagerAbi = [
  {
    type: 'event',
    name: 'Initialize',
    inputs: [
      { name: 'id', type: 'bytes32', indexed: true },
      { name: 'currency0', type: 'address', indexed: true },
      { name: 'currency1', type: 'address', indexed: true },
      { name: 'fee', type: 'uint24', indexed: false },
      { name: 'tickSpacing', type: 'int24', indexed: false },
      { name: 'hooks', type: 'address', indexed: false },
      { name: 'sqrtPriceX96', type: 'uint160', indexed: false },
      { name: 'tick', type: 'int24', indexed: false },
    ],
  },
] as const;

/** ABI fragments for the two v4 router actions the desk encodes. */
export const swapActionAbi = {
  /** `SWAP_EXACT_IN_SINGLE` parameters. */
  exactInputSingle: [
    {
      type: 'tuple',
      components: [
        { name: 'poolKey', type: 'tuple', components: poolKeyComponents },
        { name: 'zeroForOne', type: 'bool' },
        { name: 'amountIn', type: 'uint128' },
        { name: 'amountOutMinimum', type: 'uint128' },
        { name: 'hookData', type: 'bytes' },
      ],
    },
  ],
  /** `SWAP_EXACT_IN` parameters. */
  exactInput: [
    {
      type: 'tuple',
      components: [
        { name: 'currencyIn', type: 'address' },
        { name: 'path', type: 'tuple[]', components: pathKeyComponents },
        { name: 'amountIn', type: 'uint128' },
        { name: 'amountOutMinimum', type: 'uint128' },
      ],
    },
  ],
  /** `SETTLE_ALL` and `TAKE_ALL` both take a currency and a bound. */
  currencyAndAmount: [{ type: 'address' }, { type: 'uint256' }],
} as const;

/** `PoolKey` field list, for hashing a pool id. */
export const poolKeyAbi = poolKeyComponents;
