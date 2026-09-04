/**
 * Key handling.
 *
 * Keys live in `keys.env` inside the desk home, mode 0600, and are read once at
 * start. A key is never logged, never written to the store, and never returned
 * by the HTTP, MCP, UI, or CLI surfaces. {@link redact} exists so that any
 * value which passes through a log line loses its secrets first.
 */

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { KeystoreError } from './errors.js';
import { deskPaths } from './config.js';

/** Names the desk reads. Nothing else is loaded from `keys.env`. */
export const KEY_NAMES = [
  'DESK_EVM_PRIVATE_KEY',
  'DESK_LIGHTER_RH_PRIVATE_KEY',
  'DESK_LIGHTER_RH_ACCOUNT_INDEX',
  'DESK_LIGHTER_RH_API_KEY_INDEX',
] as const;

export type KeyName = (typeof KEY_NAMES)[number];

/** macOS Keychain service names checked when a key is absent from the file and the environment. */
const KEYCHAIN_SERVICE: Record<KeyName, string> = {
  DESK_EVM_PRIVATE_KEY: 'covenant-desk-evm-private-key',
  DESK_LIGHTER_RH_PRIVATE_KEY: 'covenant-desk-lighter-rh-private-key',
  DESK_LIGHTER_RH_ACCOUNT_INDEX: 'covenant-desk-lighter-rh-account-index',
  DESK_LIGHTER_RH_API_KEY_INDEX: 'covenant-desk-lighter-rh-api-key-index',
};

/** Keys held in memory for the life of the process. */
export interface Keystore {
  /** Read a key. Returns undefined when it was not supplied. */
  get(name: KeyName): string | undefined;
  /** Read a key or throw a {@link KeystoreError} naming what to do about it. */
  require(name: KeyName): string;
  /** True when the key is present. */
  has(name: KeyName): boolean;
  /** Names that were supplied. Values are never included. */
  names(): KeyName[];
  /** Every secret value, for the log redactor. */
  secrets(): string[];
}

export interface LoadKeysOptions {
  /** Path to `keys.env`. Defaults to the desk home. */
  file?: string;
  env?: NodeJS.ProcessEnv;
  /** Read missing keys from the macOS Keychain. Default true on darwin. */
  useKeychain?: boolean;
  /** Refuse a key file that is readable by other users. Default true. */
  enforcePermissions?: boolean;
}

/** Parse `KEY=value` lines. Ignores blanks and `#` comments, strips quotes and a leading `export`. */
export function parseEnvFile(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (line === '' || line.startsWith('#')) continue;
    const body = line.startsWith('export ') ? line.slice(7).trim() : line;
    const eq = body.indexOf('=');
    if (eq <= 0) continue;
    const key = body.slice(0, eq).trim();
    let value = body.slice(eq + 1).trim();
    if (
      (value.startsWith('"') && value.endsWith('"') && value.length >= 2) ||
      (value.startsWith("'") && value.endsWith("'") && value.length >= 2)
    ) {
      value = value.slice(1, -1);
    }
    if (key !== '') out[key] = value;
  }
  return out;
}

/** Read a value from the macOS Keychain. Returns undefined when it is absent. */
export function readKeychain(service: string, account?: string): string | undefined {
  if (process.platform !== 'darwin') return undefined;
  const args = ['find-generic-password', '-s', service, '-w'];
  if (account) args.push('-a', account);
  try {
    const value = execFileSync('security', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] });
    const trimmed = value.trim();
    return trimmed === '' ? undefined : trimmed;
  } catch {
    return undefined;
  }
}

/**
 * Load keys from `keys.env`, then the process environment, then the macOS
 * Keychain. A missing file is not an error: the desk runs read-only without
 * keys and refuses execution with a named reason.
 */
export function loadKeystore(options: LoadKeysOptions = {}): Keystore {
  const env = options.env ?? process.env;
  const file = options.file ?? deskPaths(env).keys;
  const useKeychain = options.useKeychain ?? process.platform === 'darwin';
  const enforcePermissions = options.enforcePermissions ?? true;

  const values = new Map<KeyName, string>();

  if (existsSync(file)) {
    const mode = statSync(file).mode & 0o777;
    if (enforcePermissions && (mode & 0o077) !== 0) {
      throw new KeystoreError(
        `${file} is readable by other users (mode ${mode.toString(8)}). Run "chmod 600 ${file}" and start again.`,
        { path: file, mode: mode.toString(8) },
      );
    }
    const parsed = parseEnvFile(readFileSync(file, 'utf8'));
    for (const name of KEY_NAMES) {
      const value = parsed[name];
      if (value) values.set(name, value);
    }
  }

  for (const name of KEY_NAMES) {
    if (values.has(name)) continue;
    const fromEnv = env[name];
    if (fromEnv && fromEnv.trim() !== '') values.set(name, fromEnv.trim());
  }

  if (useKeychain) {
    for (const name of KEY_NAMES) {
      if (values.has(name)) continue;
      const fromKeychain = readKeychain(KEYCHAIN_SERVICE[name]);
      if (fromKeychain) values.set(name, fromKeychain);
    }
  }

  return {
    get: (name) => values.get(name),
    has: (name) => values.has(name),
    names: () => [...values.keys()],
    secrets: () =>
      [...values.entries()]
        .filter(([name]) => name.includes('PRIVATE_KEY'))
        .map(([, value]) => value)
        .filter((value) => value.length >= 8),
    require: (name) => {
      const value = values.get(name);
      if (!value) {
        throw new KeystoreError(`${name} is not set. Add it to ${file} (mode 600) and restart the desk.`, {
          name,
          path: file,
        });
      }
      return value;
    },
  };
}

/** Placeholder written in place of a secret. */
export const REDACTED = '[redacted]';

const SECRET_PATTERNS: RegExp[] = [
  // 32-byte hex private key, with or without the 0x prefix.
  /\b(?:0x)?[0-9a-fA-F]{64}\b/g,
  // Long bearer tokens and API keys.
  /\b[A-Za-z0-9_-]{40,}\b/g,
];

/**
 * Field names that hold a credential.
 *
 * Matched whole, not as a substring. `token` is deliberately absent: in this
 * product a token is an ERC-20 and its address is public, while the bearer
 * token is registered as a known secret value and masked wherever it appears.
 * A credential in a longer name is caught by the suffix list below, and by the
 * value patterns above.
 */
const SECRET_FIELD_NAMES = new Set([
  'bearer',
  'secret',
  'password',
  'mnemonic',
  'seed',
  'privatekey',
  'private_key',
  'apikey',
  'api_key',
  'authorization',
]);

const SECRET_FIELD_SUFFIXES = [
  'privatekey',
  'private_key',
  'apikey',
  'api_key',
  'secret',
  'password',
  'mnemonic',
];

function isSecretField(name: string): boolean {
  const lower = name.toLowerCase();
  return (
    SECRET_FIELD_NAMES.has(lower) || SECRET_FIELD_SUFFIXES.some((suffix) => lower.endsWith(suffix))
  );
}

/**
 * Remove secrets from a value before it is logged or returned.
 *
 * Known secret strings are replaced first, then object fields whose name looks
 * like a credential, then anything shaped like a key or a long token. Structure
 * is preserved so the surrounding line stays readable.
 */
export function redact<T>(value: T, knownSecrets: readonly string[] = []): T {
  const secrets = knownSecrets.filter((s) => s.length >= 8);
  return redactValue(value, secrets, new WeakSet()) as T;
}

function redactString(text: string, secrets: readonly string[]): string {
  let out = text;
  for (const secret of secrets) {
    if (secret && out.includes(secret)) out = out.split(secret).join(REDACTED);
  }
  for (const pattern of SECRET_PATTERNS) {
    pattern.lastIndex = 0;
    out = out.replace(pattern, (match) => {
      // Addresses, transaction hashes, and pool ids are public identifiers and
      // stay readable. A 0x-prefixed 32-byte value cannot be told apart from a
      // private key by shape, so the keystore's own values are masked by exact
      // match above and this pass leaves the rest of the hex alone.
      if (/^0x[0-9a-fA-F]{64}$/.test(match)) return match;
      if (/^0x[0-9a-fA-F]{40}$/.test(match)) return match;
      return REDACTED;
    });
  }
  return out;
}

function redactValue(value: unknown, secrets: readonly string[], seen: WeakSet<object>): unknown {
  if (typeof value === 'string') return redactString(value, secrets);
  if (typeof value === 'bigint') return value.toString();
  if (value === null || typeof value !== 'object') return value;
  if (seen.has(value)) return '[circular]';
  seen.add(value);
  if (Array.isArray(value)) return value.map((item) => redactValue(item, secrets, seen));
  if (value instanceof Error) {
    return { name: value.name, message: redactString(value.message, secrets) };
  }
  const out: Record<string, unknown> = {};
  for (const [key, item] of Object.entries(value as Record<string, unknown>)) {
    out[key] = isSecretField(key) ? REDACTED : redactValue(item, secrets, seen);
  }
  return out;
}
