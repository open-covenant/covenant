import { z } from 'zod';

const SOLANA_MAINNET = 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp';

/**
 * A Solana secret key as `solana-keygen` writes it: 64 bytes, secret half
 * followed by public half. Validating the shape here rather than at signer
 * construction names the offending variable when the value is wrong.
 */
const keypairJsonSchema = z
  .string()
  .min(1)
  .transform((value, ctx) => {
    let parsed: unknown;
    try {
      parsed = JSON.parse(value);
    } catch {
      ctx.addIssue({ code: 'custom', message: 'must be a JSON array of 64 bytes' });
      return z.NEVER;
    }
    const bytes = z.array(z.number().int().min(0).max(255)).length(64).safeParse(parsed);
    if (!bytes.success) {
      ctx.addIssue({ code: 'custom', message: 'must be a JSON array of exactly 64 bytes 0-255' });
      return z.NEVER;
    }
    return Uint8Array.from(bytes.data);
  });

const base58 = z.string().regex(/^[1-9A-HJ-NP-Za-km-z]{32,44}$/, 'must be a base58 address');

const schema = z.object({
  MIZUKI_FACILITATOR_HOST: z.string().min(1).default('0.0.0.0'),
  MIZUKI_FACILITATOR_PORT: z.coerce.number().int().min(1).max(65_535).default(8402),
  MIZUKI_FACILITATOR_NETWORK: z.literal(SOLANA_MAINNET).default(SOLANA_MAINNET),
  MIZUKI_FACILITATOR_RPC_URL: z.string().url(),
  MIZUKI_FACILITATOR_FEE_PAYER_PRIVATE_KEY_JSON: keypairJsonSchema,
  MIZUKI_FACILITATOR_FEE_PAYER_PUBLIC_KEY: base58,
  MIZUKI_FACILITATOR_TOKEN: z.string().min(32),
  MIZUKI_FACILITATOR_REQUEST_BYTES: z.coerce.number().int().min(1_024).max(262_144).default(65_536),
});

export interface FacilitatorConfig {
  host: string;
  port: number;
  network: typeof SOLANA_MAINNET;
  rpcUrl: string;
  feePayerSecretKey: Uint8Array;
  feePayerPublicKey: string;
  token: string;
  maxRequestBytes: number;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): FacilitatorConfig {
  const parsed = schema.safeParse(env);
  if (!parsed.success) {
    const detail = parsed.error.issues
      .map((issue) => `${issue.path.join('.') || 'config'}: ${issue.message}`)
      .join('; ');
    throw new Error(`Invalid facilitator configuration: ${detail}`);
  }
  const value = parsed.data;
  if (!value.MIZUKI_FACILITATOR_RPC_URL.startsWith('https://')) {
    throw new Error('Invalid facilitator configuration: MIZUKI_FACILITATOR_RPC_URL must be https');
  }
  return {
    host: value.MIZUKI_FACILITATOR_HOST,
    port: value.MIZUKI_FACILITATOR_PORT,
    network: value.MIZUKI_FACILITATOR_NETWORK,
    rpcUrl: value.MIZUKI_FACILITATOR_RPC_URL,
    feePayerSecretKey: value.MIZUKI_FACILITATOR_FEE_PAYER_PRIVATE_KEY_JSON,
    feePayerPublicKey: value.MIZUKI_FACILITATOR_FEE_PAYER_PUBLIC_KEY,
    token: value.MIZUKI_FACILITATOR_TOKEN,
    maxRequestBytes: value.MIZUKI_FACILITATOR_REQUEST_BYTES,
  };
}

export { SOLANA_MAINNET };
