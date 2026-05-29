import { describe, expect, it } from 'vitest';
import { after, decodeCursor, encodeCursor } from './cursor.js';

describe('cursor', () => {
  it('round-trips an opaque (ms,id) token', () => {
    const tok = encodeCursor(1700, 'abc-123');
    expect(decodeCursor(tok)).toEqual({ ms: 1700, id: 'abc-123' });
  });

  it('accepts a bare epoch-ms as an inclusive-ms cursor', () => {
    expect(decodeCursor('1700')).toEqual({ ms: 1700, id: '' });
  });

  it('accepts an ISO-8601 string', () => {
    const iso = '2026-01-01T00:00:00.000Z';
    expect(decodeCursor(iso)).toEqual({ ms: Date.parse(iso), id: '' });
  });

  it('rejects garbage', () => {
    expect(() => decodeCursor('not-a-cursor!!')).toThrow();
  });

  it('after() is strict on id at the same ms but inclusive for a bare-ms cursor', () => {
    expect(after(1700, 'b', { ms: 1700, id: 'a' })).toBe(true);
    expect(after(1700, 'a', { ms: 1700, id: 'a' })).toBe(false);
    expect(after(1699, 'z', { ms: 1700, id: 'a' })).toBe(false);
    expect(after(1700, 'any', { ms: 1700, id: '' })).toBe(true);
  });
});
