import { z } from 'zod';

export const SOLANA_MAINNET = 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp';
export const BASE_MAINNET = 'eip155:8453';
export const BASE_SEPOLIA = 'eip155:84532';

/** The only asset settled on each network. All are 6-decimal USDC. */
export const USDC: Record<string, string> = {
  [SOLANA_MAINNET]: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
  [BASE_MAINNET]: '0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913',
  [BASE_SEPOLIA]: '0x036CbD53842c5426634e7929541eC2318f3dCF7e',
};

const httpsUrl = z
  .string()
  .url()
  .refine((value) => new URL(value).protocol === 'https:', 'must be an https URL');

const secretKeyJson = z.string().transform((value, ctx) => {
  try {
    const bytes = JSON.parse(value) as unknown;
    if (
      Array.isArray(bytes) &&
      bytes.length === 64 &&
      bytes.every((b) => Number.isInteger(b) && b >= 0 && b <= 255)
    ) {
      return Uint8Array.from(bytes as number[]);
    }
  } catch {
    // fall through to the issue below
  }
  ctx.addIssue({ code: 'custom', message: 'must be a JSON array of 64 bytes' });
  return z.NEVER;
});

const addressList = z
  .string()
  .default('')
  .transform((value) =>
    value
      .split(',')
      .map((entry) => entry.trim())
      .filter(Boolean),
  );

const schema = z.object({
  PORT: z.coerce.number().int().min(1).max(65_535).default(8402),
  COVENANT_FACILITATOR_HOST: z.string().min(1).default('0.0.0.0'),
  COVENANT_FACILITATOR_PUBLIC_URL: httpsUrl,
  COVENANT_FACILITATOR_DATA_DIR: z.string().min(1).default('data'),
  COVENANT_FACILITATOR_REQUEST_BYTES: z.coerce.number().int().min(1_024).max(262_144).default(65_536),

  COVENANT_FACILITATOR_SOLANA_RPC_URL: httpsUrl.optional(),
  COVENANT_FACILITATOR_SOLANA_FEE_PAYER_PRIVATE_KEY_JSON: secretKeyJson.optional(),
  COVENANT_FACILITATOR_SOLANA_FEE_PAYER_PUBLIC_KEY: z.string().min(32).max(44).optional(),

  /** Base Sepolia is for test deployments only. */
  COVENANT_FACILITATOR_BASE_NETWORK: z.enum([BASE_MAINNET, BASE_SEPOLIA]).default(BASE_MAINNET),
  COVENANT_FACILITATOR_BASE_RPC_URL: httpsUrl.optional(),
  COVENANT_FACILITATOR_BASE_SIGNER_PRIVATE_KEY: z
    .string()
    .regex(/^0x[0-9a-fA-F]{64}$/, 'must be a 0x-prefixed 32-byte hex key')
    .optional(),
  COVENANT_FACILITATOR_BASE_SIGNER_ADDRESS: z
    .string()
    .regex(/^0x[0-9a-fA-F]{40}$/, 'must be a 0x-prefixed address')
    .optional(),

  /** Smallest payment settled, in USDC atomic units (1000 = $0.001). */
  COVENANT_FACILITATOR_MIN_AMOUNT: z.coerce.bigint().min(1n).default(1_000n),
  /** Settlements per UTC day per network for metered receivers. */
  COVENANT_FACILITATOR_DAILY_SETTLEMENTS: z.coerce.number().int().min(0).default(2_000),
  /** Settlements per UTC day for any one metered receiver on one network. */
  COVENANT_FACILITATOR_DAILY_SETTLEMENTS_PER_PAY_TO: z.coerce.number().int().min(0).default(500),
  /** Receivers outside the daily caps, such as Covenant's own sellers. */
  COVENANT_FACILITATOR_UNMETERED_PAY_TO: addressList,
});

export interface SolanaConfig {
  rpcUrl: string;
  feePayerSecretKey: Uint8Array;
  feePayerPublicKey: string;
}

export interface BaseConfig {
  network: typeof BASE_MAINNET | typeof BASE_SEPOLIA;
  rpcUrl: string;
  signerPrivateKey: `0x${string}`;
  signerAddress: `0x${string}`;
}

export interface FacilitatorConfig {
  host: string;
  port: number;
  publicUrl: string;
  dataDir: string;
  maxRequestBytes: number;
  solana?: SolanaConfig;
  base?: BaseConfig;
  minAmount: bigint;
  dailySettlements: number;
  dailySettlementsPerPayTo: number;
  unmeteredPayTo: string[];
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): FacilitatorConfig {
  const parsed = schema.parse(env);

  const solanaParts = [
    parsed.COVENANT_FACILITATOR_SOLANA_RPC_URL,
    parsed.COVENANT_FACILITATOR_SOLANA_FEE_PAYER_PRIVATE_KEY_JSON,
    parsed.COVENANT_FACILITATOR_SOLANA_FEE_PAYER_PUBLIC_KEY,
  ];
  const baseParts = [
    parsed.COVENANT_FACILITATOR_BASE_RPC_URL,
    parsed.COVENANT_FACILITATOR_BASE_SIGNER_PRIVATE_KEY,
    parsed.COVENANT_FACILITATOR_BASE_SIGNER_ADDRESS,
  ];
  // A half-configured network would advertise a signer it cannot settle with.
  for (const [name, parts] of [
    ['Solana', solanaParts],
    ['Base', baseParts],
  ] as const) {
    const set = parts.filter((part) => part !== undefined).length;
    if (set !== 0 && set !== parts.length) {
      throw new Error(`${name} needs its RPC URL, signing key and address set together`);
    }
  }

  const solana = parsed.COVENANT_FACILITATOR_SOLANA_RPC_URL
    ? {
        rpcUrl: parsed.COVENANT_FACILITATOR_SOLANA_RPC_URL,
        feePayerSecretKey: parsed.COVENANT_FACILITATOR_SOLANA_FEE_PAYER_PRIVATE_KEY_JSON!,
        feePayerPublicKey: parsed.COVENANT_FACILITATOR_SOLANA_FEE_PAYER_PUBLIC_KEY!,
      }
    : undefined;
  const base = parsed.COVENANT_FACILITATOR_BASE_RPC_URL
    ? {
        network: parsed.COVENANT_FACILITATOR_BASE_NETWORK,
        rpcUrl: parsed.COVENANT_FACILITATOR_BASE_RPC_URL,
        signerPrivateKey: parsed.COVENANT_FACILITATOR_BASE_SIGNER_PRIVATE_KEY as `0x${string}`,
        signerAddress: parsed.COVENANT_FACILITATOR_BASE_SIGNER_ADDRESS as `0x${string}`,
      }
    : undefined;
  if (!solana && !base) throw new Error('Configure at least one network');

  return {
    host: parsed.COVENANT_FACILITATOR_HOST,
    port: parsed.PORT,
    publicUrl: parsed.COVENANT_FACILITATOR_PUBLIC_URL.replace(/\/+$/, ''),
    dataDir: parsed.COVENANT_FACILITATOR_DATA_DIR,
    maxRequestBytes: parsed.COVENANT_FACILITATOR_REQUEST_BYTES,
    solana,
    base,
    minAmount: parsed.COVENANT_FACILITATOR_MIN_AMOUNT,
    dailySettlements: parsed.COVENANT_FACILITATOR_DAILY_SETTLEMENTS,
    dailySettlementsPerPayTo: parsed.COVENANT_FACILITATOR_DAILY_SETTLEMENTS_PER_PAY_TO,
    unmeteredPayTo: parsed.COVENANT_FACILITATOR_UNMETERED_PAY_TO,
  };
}
