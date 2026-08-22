import { describe, expect, it } from 'vitest';
import { githubIssuePattern } from './github-url';

describe('githubIssuePattern', () => {
  it('is valid under the Unicode-set mode used by HTML pattern', () => {
    const pattern = new RegExp(`^(?:${githubIssuePattern})$`, 'v');

    expect(pattern.test('https://github.com/open-covenant/covenant/issues/42')).toBe(true);
    expect(pattern.test('https://github.com/open-covenant/covenant/pull/42')).toBe(false);
    expect(pattern.test('https://example.com/open-covenant/covenant/issues/42')).toBe(false);
  });
});
