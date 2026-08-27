import { createPrivateKey } from 'node:crypto';
import { isIP } from 'node:net';
import { getDomain } from 'tldts';
import { z } from 'zod';

const base58 = z.string().regex(/^[1-9A-HJ-NP-Za-km-z]{32,44}$/);
const MAINNET_USDC_MINT = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';

const envSchema = z
  .object({
    NODE_ENV: z.enum(['development', 'test', 'production']).default('development'),
    MIZUKI_SIGNER_HOST: z.string().default('127.0.0.1'),
    MIZUKI_SIGNER_PORT: z.coerce.number().int().min(1).max(65535).default(8792),
    MIZUKI_SIGNER_AUTH_TOKEN: z.string().min(32),
    MIZUKI_SIGNER_DATABASE_URL: z.string().url().optional(),
    MIZUKI_SIGNER_RPC_URL: z.string().url().optional(),
    MIZUKI_SIGNER_SECONDARY_RPC_URL: z.string().url().optional(),
    MIZUKI_SIGNER_RPC_TIMEOUT_MS: z.coerce.number().int().min(1_000).max(10_000).default(5_000),
    MIZUKI_REFUND_PRIVATE_KEY_JSON: z.string().optional(),
    MIZUKI_ESCROW_PRIVATE_KEY_JSON: z.string().optional(),
    MIZUKI_SIGNER_GITHUB_APP_ID: z
      .string()
      .regex(/^[1-9]\d{0,15}$/)
      .refine((value) => Number.isSafeInteger(Number(value)))
      .optional(),
    MIZUKI_SIGNER_GITHUB_PRIVATE_KEY: z.string().min(100).optional(),
    MIZUKI_SIGNER_REVIEW_BASE_URL: z.string().url().optional(),
    MIZUKI_SIGNER_REVIEW_API_KEY: z.string().min(16).optional(),
    MIZUKI_SIGNER_REVIEW_MODEL: z
      .string()
      .min(1)
      .max(256)
      .regex(/^\S(?:.*\S)?$/)
      .optional(),
    MIZUKI_SIGNER_REVIEW_MIN_BALANCE: z
      .string()
      .regex(/^[1-9]\d{0,15}$/)
      .default('4000000'),
    MIZUKI_SIGNER_REVIEW_MAX_INPUT_PRICE_MICROUNITS: z.coerce
      .number()
      .int()
      .positive()
      .default(200_000),
    MIZUKI_SIGNER_REVIEW_MAX_OUTPUT_PRICE_MICROUNITS: z.coerce
      .number()
      .int()
      .positive()
      .default(400_000),
    MIZUKI_SIGNER_REVIEW_MAX_COST_MICROUNITS: z.coerce
      .number()
      .int()
      .positive()
      .max(1_000_000)
      .default(1_000_000),
    MIZUKI_JOB_AUTHORITY_PUBLIC_KEY: base58.optional(),
    MIZUKI_REFUND_TREASURY: base58.optional(),
    MIZUKI_ESCROW_AUTHORITY: base58.optional(),
    MIZUKI_REFUND_MINT: base58.optional(),
    MIZUKI_REFUND_DECIMALS: z.coerce.number().int().min(0).max(18).default(6),
    MIZUKI_REFUND_TOKEN_PROGRAM: z.literal('spl-token').default('spl-token'),
    MIZUKI_ESCROW_PROGRAM_ID: base58.optional(),
    MIZUKI_ESCROW_PROGRAM_DATA_SHA256: z
      .string()
      .regex(/^[a-f0-9]{64}$/)
      .optional(),
    MIZUKI_SOL_USD_PRICE_URL: z.string().url().optional(),
    MIZUKI_SOL_USD_PRICE_TOKEN: z.string().min(16).optional(),
    MIZUKI_SOL_USD_SECONDARY_PRICE_URL: z.string().url().optional(),
    MIZUKI_SOL_USD_SECONDARY_PRICE_TOKEN: z.string().min(16).optional(),
    MIZUKI_SOL_USD_MAX_DIVERGENCE_BPS: z.coerce.number().int().min(1).max(1_000).default(500),
    MIZUKI_SOL_USD_MAX_AGE_MS: z.coerce.number().int().min(60_000).max(300_000).default(300_000),
    MIZUKI_SOL_USD_MIN_MICROS: z.coerce.number().int().positive().default(1_000_000),
    MIZUKI_SOL_USD_MAX_MICROS: z.coerce.number().int().positive().default(1_000_000_000),
    MIZUKI_OPERATION_LIMIT_USD_CENTS: z.coerce.number().int().positive().default(2_500),
    MIZUKI_REFUND_DAILY_LIMIT_USD_CENTS: z.coerce.number().int().positive().default(10_000),
    MIZUKI_REFUND_AUTH_MAX_TTL_SECONDS: z.coerce.number().int().min(60).max(3_600).default(900),
    MIZUKI_ESCROW_DAILY_LIMIT_USD_CENTS: z.coerce.number().int().positive().default(10_000),
    MIZUKI_MAX_ESCROW_LAMPORTS: z.coerce.number().int().positive().default(1_000_000_000),
    MIZUKI_SOL_FEE_RESERVE_LAMPORTS: z.coerce.number().int().positive().default(1_000_000),
    MIZUKI_REFUND_FEE_RESERVE_LAMPORTS: z.coerce.number().int().positive().default(10_000),
    MIZUKI_BIND_CHALLENGE_TTL_SECONDS: z.coerce.number().int().min(60).max(900).default(600),
    MIZUKI_GITHUB_GRANT_TTL_SECONDS: z.coerce.number().int().min(60).max(900).default(600),
    MIZUKI_CLAIM_TTL_SECONDS: z.coerce.number().int().min(172_800).max(604_800).default(172_800),
    MIZUKI_SIGNER_MOCK_MODE: z
      .enum(['true', 'false'])
      .default('false')
      .transform((value) => value === 'true'),
  })
  .passthrough();

export interface SignerConfig {
  environment: 'development' | 'test' | 'production';
  host: string;
  port: number;
  authToken: string;
  databaseUrl?: string;
  rpcUrl?: string;
  secondaryRpcUrl?: string;
  rpcTimeoutMs: number;
  refundPrivateKeyJson?: string;
  escrowPrivateKeyJson?: string;
  githubAppId?: string;
  githubPrivateKey?: string;
  reviewBaseUrl?: string;
  reviewApiKey?: string;
  reviewModel?: string;
  reviewMinimumBalance: string;
  reviewMaxInputPriceMicrounits: number;
  reviewMaxOutputPriceMicrounits: number;
  reviewMaxCostMicrounits: number;
  jobAuthorityPublicKey?: string;
  refundTreasury?: string;
  escrowAuthority?: string;
  refundMint?: string;
  refundDecimals: number;
  refundTokenProgram: 'spl-token';
  escrowProgramId?: string;
  escrowProgramDataSha256?: string;
  priceUrl?: string;
  priceToken?: string;
  secondaryPriceUrl?: string;
  secondaryPriceToken?: string;
  maxPriceDivergenceBps: number;
  maxPriceAgeMs: number;
  minSolUsdMicros: number;
  maxSolUsdMicros: number;
  operationLimitUsdCents: number;
  refundDailyLimitUsdCents: number;
  refundAuthMaxTtlSeconds: number;
  escrowDailyLimitUsdCents: number;
  maxEscrowLamports: number;
  solFeeReserveLamports: number;
  refundFeeReserveLamports: number;
  bindChallengeTtlSeconds: number;
  githubGrantTtlSeconds: number;
  claimTtlSeconds: number;
  mockMode: boolean;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): SignerConfig {
  const parsed = envSchema.parse(env);
  if (
    parsed.MIZUKI_OPERATION_LIMIT_USD_CENTS > parsed.MIZUKI_REFUND_DAILY_LIMIT_USD_CENTS ||
    parsed.MIZUKI_OPERATION_LIMIT_USD_CENTS > parsed.MIZUKI_ESCROW_DAILY_LIMIT_USD_CENTS
  ) {
    throw new Error('Per-operation limit cannot exceed either rolling daily limit');
  }
  if (parsed.MIZUKI_SOL_USD_MIN_MICROS >= parsed.MIZUKI_SOL_USD_MAX_MICROS) {
    throw new Error('SOL/USD price bounds are invalid');
  }
  if (parsed.MIZUKI_SIGNER_MOCK_MODE) {
    if (parsed.NODE_ENV === 'production') throw new Error('Mock mode is disabled in production');
    if (!isLoopback(parsed.MIZUKI_SIGNER_HOST)) {
      throw new Error('Mock mode must bind to a loopback address');
    }
  } else {
    const required = [
      ['MIZUKI_SIGNER_DATABASE_URL', parsed.MIZUKI_SIGNER_DATABASE_URL],
      ['MIZUKI_SIGNER_RPC_URL', parsed.MIZUKI_SIGNER_RPC_URL],
      ['MIZUKI_SIGNER_SECONDARY_RPC_URL', parsed.MIZUKI_SIGNER_SECONDARY_RPC_URL],
      ['MIZUKI_REFUND_PRIVATE_KEY_JSON', parsed.MIZUKI_REFUND_PRIVATE_KEY_JSON],
      ['MIZUKI_ESCROW_PRIVATE_KEY_JSON', parsed.MIZUKI_ESCROW_PRIVATE_KEY_JSON],
      ['MIZUKI_SIGNER_GITHUB_APP_ID', parsed.MIZUKI_SIGNER_GITHUB_APP_ID],
      ['MIZUKI_SIGNER_GITHUB_PRIVATE_KEY', parsed.MIZUKI_SIGNER_GITHUB_PRIVATE_KEY],
      ['MIZUKI_SIGNER_REVIEW_BASE_URL', parsed.MIZUKI_SIGNER_REVIEW_BASE_URL],
      ['MIZUKI_SIGNER_REVIEW_API_KEY', parsed.MIZUKI_SIGNER_REVIEW_API_KEY],
      ['MIZUKI_SIGNER_REVIEW_MODEL', parsed.MIZUKI_SIGNER_REVIEW_MODEL],
      ['MIZUKI_JOB_AUTHORITY_PUBLIC_KEY', parsed.MIZUKI_JOB_AUTHORITY_PUBLIC_KEY],
      ['MIZUKI_REFUND_TREASURY', parsed.MIZUKI_REFUND_TREASURY],
      ['MIZUKI_ESCROW_AUTHORITY', parsed.MIZUKI_ESCROW_AUTHORITY],
      ['MIZUKI_REFUND_MINT', parsed.MIZUKI_REFUND_MINT],
      ['MIZUKI_ESCROW_PROGRAM_ID', parsed.MIZUKI_ESCROW_PROGRAM_ID],
      ['MIZUKI_ESCROW_PROGRAM_DATA_SHA256', parsed.MIZUKI_ESCROW_PROGRAM_DATA_SHA256],
      ['MIZUKI_SOL_USD_PRICE_URL', parsed.MIZUKI_SOL_USD_PRICE_URL],
      ['MIZUKI_SOL_USD_SECONDARY_PRICE_URL', parsed.MIZUKI_SOL_USD_SECONDARY_PRICE_URL],
    ] as const;
    const missing = required.filter(([, value]) => !value).map(([name]) => name);
    if (missing.length > 0)
      throw new Error(`Missing production signer settings: ${missing.join(', ')}`);
    assertRsaPrivateKey(parsed.MIZUKI_SIGNER_GITHUB_PRIVATE_KEY!);
    if (
      parsed.NODE_ENV === 'production' &&
      parsed.MIZUKI_SIGNER_REVIEW_BASE_URL !== 'https://api.usepod.ai'
    ) {
      throw new Error('Production independent review must use the pinned provider origin');
    }
    assertHttpsOrLoopback('MIZUKI_SIGNER_REVIEW_BASE_URL', parsed.MIZUKI_SIGNER_REVIEW_BASE_URL!);
    assertEncryptedDatabase(parsed.MIZUKI_SIGNER_DATABASE_URL!);
    assertHttpsOrLoopback('MIZUKI_SIGNER_RPC_URL', parsed.MIZUKI_SIGNER_RPC_URL!);
    assertHttpsOrLoopback(
      'MIZUKI_SIGNER_SECONDARY_RPC_URL',
      parsed.MIZUKI_SIGNER_SECONDARY_RPC_URL!,
    );
    assertIndependentProviders(
      'RPC',
      parsed.MIZUKI_SIGNER_RPC_URL!,
      parsed.MIZUKI_SIGNER_SECONDARY_RPC_URL!,
      parsed.NODE_ENV,
    );
    assertHttpsOrLoopback('MIZUKI_SOL_USD_PRICE_URL', parsed.MIZUKI_SOL_USD_PRICE_URL!);
    assertHttpsOrLoopback(
      'MIZUKI_SOL_USD_SECONDARY_PRICE_URL',
      parsed.MIZUKI_SOL_USD_SECONDARY_PRICE_URL!,
    );
    assertIndependentProviders(
      'price',
      parsed.MIZUKI_SOL_USD_PRICE_URL!,
      parsed.MIZUKI_SOL_USD_SECONDARY_PRICE_URL!,
      parsed.NODE_ENV,
    );
    if (
      parsed.NODE_ENV === 'production' &&
      (parsed.MIZUKI_REFUND_MINT !== MAINNET_USDC_MINT || parsed.MIZUKI_REFUND_DECIMALS !== 6)
    ) {
      throw new Error('Production refunds must use canonical mainnet USDC with six decimals');
    }
  }

  return {
    environment: parsed.NODE_ENV,
    host: parsed.MIZUKI_SIGNER_HOST,
    port: parsed.MIZUKI_SIGNER_PORT,
    authToken: parsed.MIZUKI_SIGNER_AUTH_TOKEN,
    databaseUrl: parsed.MIZUKI_SIGNER_DATABASE_URL,
    rpcUrl: parsed.MIZUKI_SIGNER_RPC_URL,
    secondaryRpcUrl: parsed.MIZUKI_SIGNER_SECONDARY_RPC_URL,
    rpcTimeoutMs: parsed.MIZUKI_SIGNER_RPC_TIMEOUT_MS,
    refundPrivateKeyJson: parsed.MIZUKI_REFUND_PRIVATE_KEY_JSON,
    escrowPrivateKeyJson: parsed.MIZUKI_ESCROW_PRIVATE_KEY_JSON,
    githubAppId: parsed.MIZUKI_SIGNER_GITHUB_APP_ID,
    githubPrivateKey: parsed.MIZUKI_SIGNER_GITHUB_PRIVATE_KEY,
    reviewBaseUrl: parsed.MIZUKI_SIGNER_REVIEW_BASE_URL,
    reviewApiKey: parsed.MIZUKI_SIGNER_REVIEW_API_KEY,
    reviewModel: parsed.MIZUKI_SIGNER_REVIEW_MODEL,
    reviewMinimumBalance: parsed.MIZUKI_SIGNER_REVIEW_MIN_BALANCE,
    reviewMaxInputPriceMicrounits: parsed.MIZUKI_SIGNER_REVIEW_MAX_INPUT_PRICE_MICROUNITS,
    reviewMaxOutputPriceMicrounits: parsed.MIZUKI_SIGNER_REVIEW_MAX_OUTPUT_PRICE_MICROUNITS,
    reviewMaxCostMicrounits: parsed.MIZUKI_SIGNER_REVIEW_MAX_COST_MICROUNITS,
    jobAuthorityPublicKey: parsed.MIZUKI_JOB_AUTHORITY_PUBLIC_KEY,
    refundTreasury: parsed.MIZUKI_REFUND_TREASURY,
    escrowAuthority: parsed.MIZUKI_ESCROW_AUTHORITY,
    refundMint: parsed.MIZUKI_REFUND_MINT,
    refundDecimals: parsed.MIZUKI_REFUND_DECIMALS,
    refundTokenProgram: parsed.MIZUKI_REFUND_TOKEN_PROGRAM,
    escrowProgramId: parsed.MIZUKI_ESCROW_PROGRAM_ID,
    escrowProgramDataSha256: parsed.MIZUKI_ESCROW_PROGRAM_DATA_SHA256,
    priceUrl: parsed.MIZUKI_SOL_USD_PRICE_URL,
    priceToken: parsed.MIZUKI_SOL_USD_PRICE_TOKEN,
    secondaryPriceUrl: parsed.MIZUKI_SOL_USD_SECONDARY_PRICE_URL,
    secondaryPriceToken: parsed.MIZUKI_SOL_USD_SECONDARY_PRICE_TOKEN,
    maxPriceDivergenceBps: parsed.MIZUKI_SOL_USD_MAX_DIVERGENCE_BPS,
    maxPriceAgeMs: parsed.MIZUKI_SOL_USD_MAX_AGE_MS,
    minSolUsdMicros: parsed.MIZUKI_SOL_USD_MIN_MICROS,
    maxSolUsdMicros: parsed.MIZUKI_SOL_USD_MAX_MICROS,
    operationLimitUsdCents: parsed.MIZUKI_OPERATION_LIMIT_USD_CENTS,
    refundDailyLimitUsdCents: parsed.MIZUKI_REFUND_DAILY_LIMIT_USD_CENTS,
    refundAuthMaxTtlSeconds: parsed.MIZUKI_REFUND_AUTH_MAX_TTL_SECONDS,
    escrowDailyLimitUsdCents: parsed.MIZUKI_ESCROW_DAILY_LIMIT_USD_CENTS,
    maxEscrowLamports: parsed.MIZUKI_MAX_ESCROW_LAMPORTS,
    solFeeReserveLamports: parsed.MIZUKI_SOL_FEE_RESERVE_LAMPORTS,
    refundFeeReserveLamports: parsed.MIZUKI_REFUND_FEE_RESERVE_LAMPORTS,
    bindChallengeTtlSeconds: parsed.MIZUKI_BIND_CHALLENGE_TTL_SECONDS,
    githubGrantTtlSeconds: parsed.MIZUKI_GITHUB_GRANT_TTL_SECONDS,
    claimTtlSeconds: parsed.MIZUKI_CLAIM_TTL_SECONDS,
    mockMode: parsed.MIZUKI_SIGNER_MOCK_MODE,
  };
}

export function assertServerMode(config: SignerConfig): void {
  if (config.mockMode) {
    throw new Error('Mock adapters are test-only and cannot start the signer HTTP service');
  }
}

function isLoopback(host: string): boolean {
  return host === '127.0.0.1' || host === '::1' || host === '[::1]' || host === 'localhost';
}

function assertHttpsOrLoopback(name: string, value: string): void {
  const url = new URL(value);
  if (url.protocol !== 'https:' && !isLoopback(url.hostname)) {
    throw new Error(`${name} must use HTTPS unless it targets loopback`);
  }
}

function assertEncryptedDatabase(value: string): void {
  const url = new URL(value);
  if (isLoopback(url.hostname) || isRenderPrivateDatabase(url.hostname)) return;
  const sslMode = url.searchParams.get('sslmode');
  if (!['require', 'verify-ca', 'verify-full'].includes(sslMode ?? '')) {
    throw new Error(
      'MIZUKI_SIGNER_DATABASE_URL must require TLS unless it targets loopback or a Render private database',
    );
  }
}

function assertIndependentProviders(
  kind: 'RPC' | 'price',
  primary: string,
  secondary: string,
  environment: SignerConfig['environment'],
): void {
  if (endpointIdentity(primary) === endpointIdentity(secondary)) {
    throw new Error(`Primary and secondary ${kind} URLs must be different`);
  }
  if (environment !== 'production') return;

  const primaryProvider = providerDomain(primary);
  const secondaryProvider = providerDomain(secondary);
  if (primaryProvider === secondaryProvider) {
    throw new Error(`Primary and secondary ${kind} providers must use different domains`);
  }
}

function endpointIdentity(value: string): string {
  const url = new URL(value);
  const host = normalizedHost(url.hostname);
  const port =
    url.port || (url.protocol === 'https:' ? '443' : url.protocol === 'http:' ? '80' : '');
  const path = url.pathname.replace(/\/+$/, '') || '/';
  return `${url.protocol}//${host}:${port}${path}`;
}

function providerDomain(value: string): string {
  const host = normalizedHost(new URL(value).hostname).replace(/^\[|\]$/g, '');
  if (isIP(host)) {
    throw new Error('Production providers must use DNS hostnames');
  }
  const domain = getDomain(host, { allowPrivateDomains: false });
  if (!domain) {
    throw new Error('Production providers must use registrable DNS domains');
  }
  return domain;
}

function normalizedHost(host: string): string {
  const normalized = host.toLowerCase().replace(/\.$/, '');
  return isLoopback(normalized) ? 'loopback' : normalized;
}

function isRenderPrivateDatabase(host: string): boolean {
  return /^dpg-[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(host);
}

function assertRsaPrivateKey(value: string): void {
  try {
    const key = createPrivateKey(value);
    if (key.type !== 'private' || key.asymmetricKeyType !== 'rsa') throw new Error();
  } catch {
    throw new Error('MIZUKI_SIGNER_GITHUB_PRIVATE_KEY must be an RSA private key in PEM form');
  }
}
