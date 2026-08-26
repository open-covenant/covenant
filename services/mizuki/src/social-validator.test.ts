import { describe, expect, it } from 'vitest';
import type { SocialBrief } from './social.js';
import { validateSocialDraft } from './social-validator.js';

const statsUrl = 'https://mizuki.opencovenant.org/api/mizuki/v1/social/brief?kind=stats';
const prUrl = 'https://github.com/example/project/pull/185';
const now = new Date('2026-08-25T16:05:00.000Z');

const brief: SocialBrief = {
  schemaVersion: 1,
  generatedAt: '2026-08-25T16:00:00.000Z',
  freshUntil: '2026-08-25T16:15:00.000Z',
  cursor: 'stats:cursor',
  sourceHash: 'a'.repeat(64),
  kind: 'stats',
  publishable: true,
  window: { from: null, to: '2026-08-25T16:00:00.000Z' },
  metrics: {
    internalPaidAttempts: { total: 12, delta: 2 },
    externalPaidJobs: { total: 0, delta: 0 },
    unclassifiedPaidAttempts: { total: 0, delta: 0 },
    internalOpenedPrs: { total: 1, delta: 1 },
    externalOpenedPrs: { total: 0, delta: 0 },
    unclassifiedOpenedPrs: { total: 0, delta: 0 },
    internalMergedPrs: { total: 1, delta: 1 },
    externalMergedPrs: { total: 0, delta: 0 },
    unclassifiedMergedPrs: { total: 0, delta: 0 },
    internalRefunds: { total: 11, delta: 1 },
    externalRefunds: { total: 0, delta: 0 },
    unclassifiedRefunds: { total: 0, delta: 0 },
    refundSuccessRate: 1,
    externalMaintainers: { total: 0, delta: 0 },
    grossMarginStatus: 'unverified',
  },
  intake: { enabled: false },
  allowedUrlOrigins: ['https://github.com', 'https://mizuki.opencovenant.org'],
  evidence: [
    { claim: 'stats', url: statsUrl },
    { claim: 'mergedPr', url: prUrl },
  ],
  blockedReasons: [],
  reviewRequiredReasons: ['financial_metrics'],
  previousPostAt: null,
};

const validPosts = [
  `Internal test log: 12 operator-funded attempts, one merged PR, 11 full refunds. No external paid jobs yet. ${statsUrl}`,
  `Internal tests moved by 2 attempts. One PR merged and 11 refunds are final. External paid jobs remain at 0. ${statsUrl}`,
  `Operator-funded work stands at 12 attempts, with 1 internal merge and 11 internal refunds. The record is public. ${statsUrl}`,
  `Internal receipt: 1 merged PR from 12 operator-funded attempts. Refunds stand at 11. ${prUrl}`,
  `There are 0 external paid jobs and 0 external maintainers. Internal testing has 12 attempts and 1 merge. ${statsUrl}`,
  `Internal testing added 2 attempts and 1 merged PR. Total operator-funded attempts: 12. ${statsUrl}`,
  `Internal refund log: 11 final refunds across 12 operator-funded attempts. Refund success is 100%. ${statsUrl}`,
  `One internal PR is merged. It came from operator-funded testing, not customer work. ${prUrl}`,
  `The internal bench shows 12 attempts, 1 opened PR, and 1 merge. External paid jobs remain 0. ${statsUrl}`,
  `Operator-funded results: 12 attempts, 1 merge, 11 refunds. Delivery needs work; the receipts stay public. ${statsUrl}`,
  `Internal tests now include 2 more attempts and 1 more refund. Totals are 12 and 11. ${statsUrl}`,
  `Refund success is 100% across the internal test obligations recorded here. Gross margin is unverified. ${statsUrl}`,
  `Internal work produced 1 merged PR. The review and merge are public. ${prUrl}`,
  `No customer traction claim here: these are 12 operator-funded attempts with 1 internal merge. ${statsUrl}`,
  `The external count is 0 paid jobs and 0 maintainers. Internal testing remains the whole sample. ${statsUrl}`,
  `Internal tests: 12 attempts. Results: 1 merge and 11 refunds. Both sides of the ledger stay visible. ${statsUrl}`,
  `Operator-funded activity changed by 2 attempts. The internal merge count changed by 1. ${statsUrl}`,
  `Internal delivery is 1 merged PR from 12 attempts. The number is plain because the receipt is plain. ${prUrl}`,
  `The internal refund total is 11 and the success rate is 100%. Margin remains unverified. ${statsUrl}`,
  `Internal shop log: 12 attempts, 1 opened PR, 1 merged PR. External paid work is still 0. ${statsUrl}`,
];

describe('validateSocialDraft', () => {
  it.each(validPosts)('accepts an evidence-bound fixture', (text) => {
    expect(validateSocialDraft(brief, `POST\n${text}`, { now })).toEqual({
      valid: true,
      decision: 'post',
      text,
    });
  });

  it.each([
    ['invalid_output_format', 'DRAFT\nNothing'],
    [
      'brief_not_publishable',
      `POST\nInternal test log: 12 attempts. ${statsUrl}`,
      { brief: { ...brief, publishable: false, blockedReasons: ['no_changes'] } },
    ],
    [
      'brief_expired',
      `POST\nInternal test log: 12 attempts. ${statsUrl}`,
      { now: new Date('2026-08-25T16:16:00Z') },
    ],
    [
      'brief_from_future',
      `POST\nInternal test log: 12 attempts. ${statsUrl}`,
      { now: new Date('2026-08-25T15:50:00Z') },
    ],
    [
      'duplicate_cursor',
      `POST\nInternal test log: 12 attempts. ${statsUrl}`,
      { seenCursors: [brief.cursor] },
    ],
    [
      'duplicate_source',
      `POST\nInternal test log: 12 attempts. ${statsUrl}`,
      { seenSourceHashes: [brief.sourceHash] },
    ],
    ['empty_post', 'POST\n'],
    ['post_too_long', `POST\nInternal ${'quiet '.repeat(50)}${statsUrl}`],
    ['multiline_post', `POST\nInternal test log: 12 attempts.\n${statsUrl}`],
    ['exclamation_mark', `POST\nInternal test log: 12 attempts! ${statsUrl}`],
    ['hashtag', `POST\nInternal test log: 12 attempts. #build ${statsUrl}`],
    ['emoji', `POST\nInternal test log: 12 attempts. 🔧 ${statsUrl}`],
    ['banned_phrase', `POST\nInternal test log: we cooked 12 attempts. ${statsUrl}`],
    ['hype_or_superiority', `POST\nA massive internal test log: 12 attempts. ${statsUrl}`],
    [
      'token_or_financial_promotion',
      `POST\nBuy after this internal test log: 12 attempts. ${statsUrl}`,
    ],
    ['missing_evidence_url', 'POST\nInternal test log: 12 attempts.'],
    ['unsupported_url', 'POST\nInternal test log: 12 attempts. https://example.com/nope'],
    ['disallowed_url_origin', 'POST\nInternal test log: 12 attempts. https://example.com/nope'],
    ['unsupported_number', `POST\nInternal test log: 47 attempts. ${statsUrl}`],
    ['unsupported_number_word', `POST\nInternal test log: seven attempts. ${statsUrl}`],
    [
      'internal_provenance_omitted',
      `POST\nShop log: 12 paid jobs, 1 merged PR, 11 refunds. ${statsUrl}`,
    ],
    ['metric_value_mismatch', `POST\nInternal test log: 11 external paid jobs. ${statsUrl}`],
    [
      'metric_value_mismatch',
      `POST\nInternal tests: 12 attempts. There are 11 external maintainers. ${statsUrl}`,
    ],
    ['metric_provenance_ambiguous', `POST\nThe log records 1 merged PR. ${prUrl}`],
    [
      'unverified_margin_omitted',
      `POST\nInternal margin is healthy across 12 attempts. ${statsUrl}`,
    ],
    ['duplicate_copy', `POST\n${validPosts[0]}`, { previousTexts: [validPosts[0]] }],
  ])('rejects adversarial fixture: %s', (reason, output, overrides = {}) => {
    const testBrief = 'brief' in overrides ? (overrides.brief as SocialBrief) : brief;
    const options = { now, ...overrides };
    delete (options as { brief?: SocialBrief }).brief;
    const result = validateSocialDraft(testBrief, output, options);
    expect(result.valid).toBe(false);
    if (result.valid) return;
    expect(result.errors).toContain(reason);
  });

  it('accepts an explicit skip without trying to publish', () => {
    expect(validateSocialDraft(brief, 'SKIP\nNo public state changed.', { now })).toEqual({
      valid: true,
      decision: 'skip',
      reason: 'No public state changed.',
    });
  });
});
