/**
 * Decoding BlockRun's x402 402 challenge, carried base64 in the
 * `x-payment-required` header (mirrored in `www-authenticate: X402
 * requirements="…"`).
 */
import type { PaymentInfo } from "./receipt.js";

const USDC_DECIMALS = 6;

export interface Accept {
  scheme: string;
  network: string;
  amount: string;
  asset: string;
  payTo: string;
  maxTimeoutSeconds?: number;
  extra?: { name?: string; version?: string };
}

export interface Challenge {
  x402Version?: number;
  accepts: Accept[];
  resource?: { url?: string; description?: string };
}

export function decodeChallenge(headerValue: string): Challenge {
  const blob = extractRequirements(headerValue);
  const json = new TextDecoder().decode(base64Decode(blob.trim()));
  const parsed = JSON.parse(json) as Challenge;
  if (!Array.isArray(parsed.accepts)) parsed.accepts = [];
  return parsed;
}

/** The first option matching `network`, else the first offered. */
export function pickAccept(challenge: Challenge, network?: string): Accept | undefined {
  if (network) {
    return challenge.accepts.find((a) => a.network === network) ?? challenge.accepts[0];
  }
  return challenge.accepts[0];
}

export function acceptToPayment(accept: Accept, tx?: string): PaymentInfo {
  const amount = Number.parseInt(accept.amount, 10);
  return {
    network: accept.network,
    asset: accept.asset,
    amount: accept.amount,
    amountUsdc: Number.isFinite(amount) ? amount / 10 ** USDC_DECIMALS : 0,
    payTo: accept.payTo,
    tx,
  };
}

function extractRequirements(value: string): string {
  const idx = value.indexOf("requirements=");
  if (idx >= 0) {
    return value
      .slice(idx + "requirements=".length)
      .trim()
      .replace(/^"|"$/g, "");
  }
  return value.trim();
}

function base64Decode(b64: string): Uint8Array {
  if (typeof atob === "function") {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  return new Uint8Array(Buffer.from(b64, "base64"));
}
