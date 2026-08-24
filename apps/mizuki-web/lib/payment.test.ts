import { describe, expect, it } from 'vitest';
import { quoteMatchesIssue } from './payment';

describe('quoteMatchesIssue', () => {
  const quote = { owner: 'open-covenant', repo: 'covenant', issueNumber: 42 };

  it('accepts the canonical issue and harmless URL casing', () => {
    expect(quoteMatchesIssue(quote, 'https://github.com/open-covenant/covenant/issues/42')).toBe(
      true,
    );
    expect(quoteMatchesIssue(quote, 'https://github.com/Open-Covenant/Covenant/issues/42/')).toBe(
      true,
    );
  });

  it('rejects a different issue or non-issue URL', () => {
    expect(quoteMatchesIssue(quote, 'https://github.com/open-covenant/covenant/issues/43')).toBe(
      false,
    );
    expect(quoteMatchesIssue(quote, 'https://example.com/open-covenant/covenant/issues/42')).toBe(
      false,
    );
  });
});
