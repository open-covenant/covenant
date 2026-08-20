import { describe, expect, it } from 'vitest';
import { encodeCursor } from '../cursor.js';
import { resolveCursor } from '../server.js';

const ISO = '2024-01-01T00:00:00Z';

describe('resolveCursor', () => {
  it('prefers an opaque cursor token over since', () => {
    expect(resolveCursor({ cursor: encodeCursor(5000, 'abc'), since: '99999' })).toEqual({
      ms: 5000,
      id: 'abc',
    });
  });

  it('treats an all-digit since as epoch milliseconds', () => {
    expect(resolveCursor({ since: '12345' })).toEqual({ ms: 12345, id: '' });
  });

  it('parses an ISO-8601 since via Date.parse', () => {
    expect(resolveCursor({ since: ISO })).toEqual({ ms: Date.parse(ISO), id: '' });
  });

  it('rejects an unparseable since', () => {
    expect(() => resolveCursor({ since: 'notadate' })).toThrow('since must be epoch ms or ISO-8601');
  });
});
