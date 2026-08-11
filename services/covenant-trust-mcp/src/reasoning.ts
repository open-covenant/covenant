// Providers hand a model's chain-of-thought back to the client as an opaque
// AEAD envelope: Anthropic signs a `thinking` block with `signature`, OpenAI
// returns `encrypted_content` on a reasoning item, Gemini uses
// `thoughtSignature`. The envelopes replay across sessions, users and sibling
// models within a provider family, which cuts both ways. Publishing one leaks
// everything the model reasoned over, including secrets that never appeared in
// the visible turn and that redacting the plaintext therefore cannot remove.
// Ingesting one lets an author who is not you plant instructions a resuming
// model treats as its own prior reasoning, in a channel no plaintext monitor
// reads.
//
// Nobody outside the provider can decrypt an envelope to see what is in it, so
// there is no sanitizing it — detection is the whole remedy, and the fix is
// always to strip the block rather than scrub its contents.

/** Shortest payload worth reporting under a field name only a provider uses.
 *  Real envelopes run to tens of thousands of base64 characters; the floor keeps
 *  prose and elided fixtures (`"signature": "..."`) from reading as findings. */
const MIN_ENVELOPE_LEN = 64;

/** `signature` is the one field name that collides with ordinary payloads, so it
 *  needs a floor no real signature can reach: ed25519 base58 is 88 characters,
 *  an EVM signature 132, while the Anthropic envelope this catches runs past
 *  36,000. Anything between the two is far likelier to be a key than a thought. */
const MIN_BARE_SIGNATURE_LEN = 256;

const B64 = `[A-Za-z0-9+/=]{${MIN_ENVELOPE_LEN},}`;
const B64_LONG = `[A-Za-z0-9+/=]{${MIN_BARE_SIGNATURE_LEN},}`;

const ENVELOPE_FIELDS = [
  {field: 'encrypted_content', provider: 'OpenAI'},
  {field: 'thoughtSignature', provider: 'Gemini'},
  {field: 'thought_signature', provider: 'Gemini'},
  {field: 'reasoning_signature', provider: 'generic'},
] as const;

export interface EnvelopeFinding {
  field: string;
  provider: string;
  count: number;
}

export interface ScanResult {
  clean: boolean;
  total: number;
  findings: EnvelopeFinding[];
}

/** Hex is not an envelope: a 65-byte EVM signature quoted in a transcript is
 *  132 characters of base64 alphabet and would otherwise read as one. */
function isHex(value: string): boolean {
  const body = value.startsWith('0x') || value.startsWith('0X') ? value.slice(2) : value;
  return body.length > 0 && /^[0-9a-fA-F]+$/.test(body);
}

function countField(content: string, field: string, b64: string = B64): number {
  const re = new RegExp(`"${field}"\\s*:\\s*"(${b64})"`, 'g');
  let count = 0;
  for (const m of content.matchAll(re)) {
    if (!isHex(m[1])) count += 1;
  }
  return count;
}

/**
 * Scan an artifact — an agent transcript, a rollout, a shared session, a bug
 * report — for opaque reasoning envelopes. Pure and local: no network, no
 * decryption, nothing retained.
 */
export function scanReasoning(content: string): ScanResult {
  const findings: EnvelopeFinding[] = [];

  for (const {field, provider} of ENVELOPE_FIELDS) {
    const count = countField(content, field);
    if (count > 0) findings.push({field, provider, count});
  }

  // A bare `signature` is ambiguous — on-chain payloads carry them constantly —
  // so it counts only alongside a thinking block, and only at envelope length.
  if (/"thinking"/.test(content)) {
    const count = countField(content, 'signature', B64_LONG);
    if (count > 0) findings.push({field: 'signature', provider: 'Anthropic', count});
  }

  const redacted = content.match(/redacted_thinking/g)?.length ?? 0;
  if (redacted > 0) findings.push({field: 'redacted_thinking', provider: 'Anthropic', count: redacted});

  const total = findings.reduce((n, f) => n + f.count, 0);
  return {clean: total === 0, total, findings};
}

export function scanText(result: ScanResult): string {
  if (result.clean) {
    return (
      'CLEAN · no reasoning envelopes found\n' +
      'Nothing here replays into a model as hidden prior reasoning.'
    );
  }
  const lines = result.findings.map((f) => `  ${f.count}x ${f.field} (${f.provider})`);
  return (
    `FOUND · ${result.total} reasoning envelope${result.total === 1 ? '' : 's'}\n` +
    lines.join('\n') +
    '\n\nDo not publish this artifact and do not replay it into a model. The blocks\n' +
    'carry whatever the model reasoned over, which may include secrets absent from\n' +
    'the visible text, and they can carry instructions a model treats as its own\n' +
    'prior reasoning. They cannot be decrypted to check or sanitized in place —\n' +
    'strip the blocks entirely.'
  );
}
