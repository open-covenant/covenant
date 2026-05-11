import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { randomBytes } from 'node:crypto';
import {
  __resetWitnessEnvelopeCacheForTests,
  ensureWitnessEnvelopeReady,
  generateWitnessKey,
  unwrapWitnessKey,
  wrapWitnessKey,
} from '../witness-envelope.js';

const WRAP_KEY = randomBytes(32).toString('base64');

describe('witness envelope', () => {
  beforeEach(() => {
    process.env.PROOFGEN_WITNESS_WRAP_KEY = WRAP_KEY;
    __resetWitnessEnvelopeCacheForTests();
  });

  afterEach(() => {
    delete process.env.PROOFGEN_WITNESS_WRAP_KEY;
    __resetWitnessEnvelopeCacheForTests();
  });

  it('wrap → unwrap round-trips a generated witness key', () => {
    const key = generateWitnessKey();
    const wrapped = wrapWitnessKey(key);
    const unwrapped = unwrapWitnessKey(wrapped);
    expect(unwrapped.equals(key)).toBe(true);
  });

  it('every wrap of the same key produces a fresh nonce → different ciphertext', () => {
    const key = generateWitnessKey();
    const a = wrapWitnessKey(key);
    const b = wrapWitnessKey(key);
    expect(a).not.toBe(b);
    expect(unwrapWitnessKey(a).equals(unwrapWitnessKey(b))).toBe(true);
  });

  it('refuses to operate without PROOFGEN_WITNESS_WRAP_KEY', () => {
    delete process.env.PROOFGEN_WITNESS_WRAP_KEY;
    __resetWitnessEnvelopeCacheForTests();
    expect(() => ensureWitnessEnvelopeReady()).toThrowError(/PROOFGEN_WITNESS_WRAP_KEY/);
  });

  it('refuses a wrap key whose decoded length is not 32 bytes', () => {
    process.env.PROOFGEN_WITNESS_WRAP_KEY = randomBytes(16).toString('base64');
    __resetWitnessEnvelopeCacheForTests();
    expect(() => ensureWitnessEnvelopeReady()).toThrowError(/32 bytes/);
  });

  it('unwrap fails if the wrap key is rotated to a different secret', () => {
    const key = generateWitnessKey();
    const wrapped = wrapWitnessKey(key);
    process.env.PROOFGEN_WITNESS_WRAP_KEY = randomBytes(32).toString('base64');
    __resetWitnessEnvelopeCacheForTests();
    expect(() => unwrapWitnessKey(wrapped)).toThrow();
  });

  it('rejects a wrapped blob with the wrong total length', () => {
    expect(() => unwrapWitnessKey(Buffer.from('too short').toString('base64'))).toThrowError(
      /unexpected length/,
    );
  });
});
