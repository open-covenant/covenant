import { describe, expect, it } from 'vitest';
import { BoundsExceededError, DeskError, NotImplementedError, isDeskError, toErrorResponse } from '../errors.js';

describe('DeskError', () => {
  it('carries a code and a reason a reader can act on', () => {
    const error = new DeskError('not_found', 'NVDA has no USDG pool.');
    expect(error.code).toBe('not_found');
    expect(error.toJSON()).toEqual({ error: 'not_found', reason: 'NVDA has no USDG pool.' });
    expect(isDeskError(error)).toBe(true);
  });

  it('names the limit and the value that tripped it', () => {
    const error = new BoundsExceededError('The single order limit', 250, 400, 'USD');
    expect(error.reason).toBe('The single order limit is 250 USD and this order needs 400 USD.');
    expect(error.toJSON().detail).toEqual({ bound: 'The single order limit', limit: 250, actual: 400, unit: 'USD' });
  });
});

describe('toErrorResponse', () => {
  it('passes desk errors through and wraps anything else', () => {
    expect(toErrorResponse(new NotImplementedError('chain.pools.discover')).error).toBe('not_implemented');
    expect(toErrorResponse(new Error('socket closed'))).toEqual({ error: 'upstream_failed', reason: 'socket closed' });
    expect(toErrorResponse('nope')).toEqual({ error: 'upstream_failed', reason: 'nope' });
  });
});
