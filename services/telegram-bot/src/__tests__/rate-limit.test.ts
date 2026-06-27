import { afterEach, describe, expect, it, vi } from 'vitest';
import { RATE_LIMIT_PER_MIN, rateLimit } from '../index.js';

// rateLimit shares one module-global bucket map, so every case takes a
// never-before-seen userId to keep state from leaking between tests.
let lastUserId = 900_000;
const freshUser = () => ++lastUserId;

afterEach(() => {
  vi.useRealTimers();
});

describe('rateLimit per-user window guard', () => {
  it('admits RATE_LIMIT_PER_MIN requests in a window then rejects the next', () => {
    const uid = freshUser();
    for (let i = 0; i < RATE_LIMIT_PER_MIN; i++) {
      expect(rateLimit(uid)).toBe(true);
    }
    expect(rateLimit(uid)).toBe(false);
  });

  it('resets the bucket once the 60s window elapses', () => {
    vi.useFakeTimers();
    const start = 1_000_000;
    vi.setSystemTime(new Date(start));
    const uid = freshUser();

    for (let i = 0; i < RATE_LIMIT_PER_MIN; i++) {
      expect(rateLimit(uid)).toBe(true);
    }
    expect(rateLimit(uid)).toBe(false);

    vi.setSystemTime(new Date(start + 60_000));
    expect(rateLimit(uid)).toBe(true);
  });
});
