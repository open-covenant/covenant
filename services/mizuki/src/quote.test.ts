import { describe, expect, it } from 'vitest';
import { createQuote, parseIssueUrl } from './quote.js';
import type { GithubIssue } from './types.js';

const issue: GithubIssue = {
  owner: 'example',
  repo: 'project',
  number: 12,
  title: 'Fix README typo',
  body: 'Correct the command name in the installation guide.',
  labels: [],
  defaultBranch: 'main',
  baseSha: 'a'.repeat(40),
  rootFiles: ['package.json', 'pnpm-lock.yaml'],
  installationId: 42,
};

describe('createQuote', () => {
  it('prices a documentation issue as a constrained micro job', () => {
    const quote = createQuote(issue, new Date('2026-08-22T10:00:00Z'));
    expect(quote).toMatchObject({
      class: 'micro',
      priceAtomic: '2000000',
      maxFiles: 3,
      validationCommands: ['pnpm test'],
      installationId: 42,
    });
    expect(quote.expiresAt).toBe('2026-08-22T10:15:00.000Z');
  });

  it('rejects security-sensitive work before payment', () => {
    expect(() => createQuote({ ...issue, title: 'Rotate OAuth credentials' })).toThrow(
      "outside Mizuki's safe MVP scope",
    );
  });

  it.each(['enhancement', 'type: feature', 'security/vulnerability'])(
    'rejects blocked issue label %s before payment',
    (label) => {
      expect(() => createQuote({ ...issue, labels: [label] })).toThrow('maintenance-only scope');
    },
  );

  it.each([
    { title: 'Add a reset button', body: issue.body },
    { title: 'Add reset button tests', body: issue.body },
    { title: '[Feature] Fix the export flow', body: issue.body },
    { title: 'New export endpoint', body: issue.body },
    { title: issue.title, body: '## Feature request\nProvide a new export endpoint.' },
    { title: issue.title, body: 'Please add a new --fail-on flag.' },
    { title: issue.title, body: 'For CSV output, we should add a new export mode.' },
  ])('rejects explicit new-capability requests before payment: $title — $body', (request) => {
    expect(() => createQuote({ ...issue, ...request })).toThrow('maintenance-only scope');
  });

  it.each([
    'Add regression tests for parser bug',
    '[Test] Add more edge-case unit tests to generator',
    'Add parser regression tests for missing terminators',
  ])('allows bounded tests for existing behavior: %s', (title) => {
    expect(() => createQuote({ ...issue, title })).not.toThrow();
  });

  it('rejects repositories without a deterministic validation command before payment', () => {
    expect(() => createQuote({ ...issue, rootFiles: ['README.md'] })).toThrow(
      'no supported deterministic validation command',
    );
  });
});

describe('parseIssueUrl', () => {
  it('accepts only canonical public GitHub issue URLs', () => {
    expect(parseIssueUrl('https://github.com/example/project/issues/12')).toEqual({
      owner: 'example',
      repo: 'project',
      number: 12,
    });
    expect(() => parseIssueUrl('https://gitlab.com/example/project/issues/12')).toThrow();
  });
});
