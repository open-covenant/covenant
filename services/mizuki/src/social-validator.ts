import type { SocialBrief } from './social.js';

const bannedPhrases = [
  'we cooked',
  'built different',
  'skill issue',
  'ngmi',
  'cope',
  'lmao',
  'probably nothing',
  'let that sink in',
  "we're so back",
  'while you slept',
  'everyone talks, we ship',
  'game-changing',
  'revolutionary',
] as const;

const numericWords = new Map<string, number>([
  ['zero', 0],
  ['one', 1],
  ['two', 2],
  ['three', 3],
  ['four', 4],
  ['five', 5],
  ['six', 6],
  ['seven', 7],
  ['eight', 8],
  ['nine', 9],
  ['ten', 10],
  ['eleven', 11],
  ['twelve', 12],
]);

export type SocialDraftValidation =
  | { valid: true; decision: 'post'; text: string }
  | { valid: true; decision: 'skip'; reason: string }
  | { valid: false; decision: 'reject'; errors: string[] };

export function validateSocialDraft(
  brief: SocialBrief,
  output: string,
  options: {
    now?: Date;
    previousTexts?: string[];
    seenCursors?: string[];
    seenSourceHashes?: string[];
  } = {},
): SocialDraftValidation {
  const normalized = output.replace(/\r\n/g, '\n').trim();
  if (/^POST\s*$/.test(normalized)) {
    return { valid: false, decision: 'reject', errors: ['empty_post'] };
  }
  if (normalized.startsWith('SKIP\n')) {
    const reason = normalized.slice(5).trim();
    if (!reason || reason.includes('\n')) {
      return { valid: false, decision: 'reject', errors: ['invalid_skip_format'] };
    }
    return { valid: true, decision: 'skip', reason };
  }
  if (!normalized.startsWith('POST\n')) {
    return { valid: false, decision: 'reject', errors: ['invalid_output_format'] };
  }

  const text = normalized.slice(5).trim();
  const errors: string[] = [];
  const now = options.now ?? new Date();
  if (!brief.publishable || brief.blockedReasons.length > 0) errors.push('brief_not_publishable');
  if (now.getTime() > Date.parse(brief.freshUntil)) errors.push('brief_expired');
  if (now.getTime() < Date.parse(brief.generatedAt) - 60_000) errors.push('brief_from_future');
  if (options.seenCursors?.includes(brief.cursor)) errors.push('duplicate_cursor');
  if (options.seenSourceHashes?.includes(brief.sourceHash)) errors.push('duplicate_source');
  if (!text) errors.push('empty_post');
  if (Array.from(text).length > 280) errors.push('post_too_long');
  if (text.includes('\n')) errors.push('multiline_post');
  if (text.includes('!')) errors.push('exclamation_mark');
  if (/(?:^|\s)#[a-z]/i.test(text)) errors.push('hashtag');
  if (/\p{Extended_Pictographic}/u.test(text)) errors.push('emoji');

  const lower = text.toLowerCase();
  if (bannedPhrases.some((phrase) => lower.includes(phrase))) errors.push('banned_phrase');
  if (/\b(?:huge|massive|insane|smarter|best|unstoppable)\b/i.test(text)) {
    errors.push('hype_or_superiority');
  }
  if (/\b(?:buy|ape|pump|price target|market cap|guaranteed returns?)\b/i.test(text)) {
    errors.push('token_or_financial_promotion');
  }

  const urls = extractUrls(text);
  const evidenceUrls = new Set(brief.evidence.map(({ url }) => url));
  if (urls.length === 0) errors.push('missing_evidence_url');
  if (urls.some((url) => !evidenceUrls.has(url))) errors.push('unsupported_url');
  if (urls.some((url) => !allowedOrigin(url, brief.allowedUrlOrigins))) {
    errors.push('disallowed_url_origin');
  }

  const allowedNumbers = factNumbers(brief);
  const textWithoutUrls = urls.reduce((value, url) => value.replace(url, ''), text);
  const numericTokens = textWithoutUrls.match(/(?<![\p{L}\p{N}])-?\d+(?:\.\d+)?/gu) ?? [];
  if (numericTokens.some((token) => !allowedNumbers.has(normalizeNumber(token)))) {
    errors.push('unsupported_number');
  }
  const words = textWithoutUrls.toLowerCase().match(/[a-z]+/g) ?? [];
  if (
    words.some((word) => {
      const value = numericWords.get(word);
      return value !== undefined && !allowedNumbers.has(String(value));
    })
  ) {
    errors.push('unsupported_number_word');
  }
  errors.push(...metricClaimErrors(brief, textWithoutUrls));

  const hasInternalActivity =
    brief.metrics.internalPaidAttempts.total > 0 ||
    brief.metrics.internalOpenedPrs.total > 0 ||
    brief.metrics.internalMergedPrs.total > 0 ||
    brief.metrics.internalRefunds.total > 0;
  if (
    hasInternalActivity &&
    /\b(?:attempts?|paid jobs?|opened prs?|merged prs?|refunds?)\b/i.test(text) &&
    !/\b(?:internal|operator-funded)\b/i.test(text)
  ) {
    errors.push('internal_provenance_omitted');
  }
  if (
    /\bmargin\b/i.test(text) &&
    brief.metrics.grossMarginStatus === 'unverified' &&
    !/\bunverified\b/i.test(text)
  ) {
    errors.push('unverified_margin_omitted');
  }
  if (
    /\b(?:merged|refunded|refund|shipped|deployed|activated)\b/i.test(text) &&
    urls.length === 0
  ) {
    errors.push('completion_without_evidence');
  }

  if (
    options.previousTexts?.some(
      (previous) => similarity(normalizedText(previous), normalizedText(text)) >= 0.84,
    )
  ) {
    errors.push('duplicate_copy');
  }

  return errors.length > 0
    ? { valid: false, decision: 'reject', errors: [...new Set(errors)] }
    : { valid: true, decision: 'post', text };
}

function factNumbers(brief: SocialBrief): Set<string> {
  const values = new Set<string>();
  for (const metric of Object.values(brief.metrics)) {
    if (typeof metric === 'number') {
      values.add(normalizeNumber(String(metric)));
      values.add(normalizeNumber(String(metric * 100)));
      continue;
    }
    if (!metric || typeof metric !== 'object') continue;
    values.add(normalizeNumber(String(metric.total)));
    values.add(normalizeNumber(String(metric.delta)));
  }
  return values;
}

function metricClaimErrors(brief: SocialBrief, text: string): string[] {
  const normalized = [...numericWords.entries()].reduce(
    (value, [word, number]) => value.replace(new RegExp(`\\b${word}\\b`, 'gi'), String(number)),
    text.replace(/\bno\b/gi, '0'),
  );
  const dimensions = [
    {
      pattern: /\bpaid jobs?\b|\battempts?\b/gi,
      metrics: {
        internal: brief.metrics.internalPaidAttempts,
        external: brief.metrics.externalPaidJobs,
        unclassified: brief.metrics.unclassifiedPaidAttempts,
      },
    },
    {
      pattern: /\bopened prs?\b|\bprs?\s+(?:(?:is|are|was|were)\s+)?opened\b/gi,
      metrics: {
        internal: brief.metrics.internalOpenedPrs,
        external: brief.metrics.externalOpenedPrs,
        unclassified: brief.metrics.unclassifiedOpenedPrs,
      },
    },
    {
      pattern: /\bmerged prs?\b|\bmerges?\b|\bprs?\s+(?:(?:is|are|was|were)\s+)?merged\b/gi,
      metrics: {
        internal: brief.metrics.internalMergedPrs,
        external: brief.metrics.externalMergedPrs,
        unclassified: brief.metrics.unclassifiedMergedPrs,
      },
    },
    {
      pattern: /\brefunds?\b(?!\s+success)/gi,
      metrics: {
        internal: brief.metrics.internalRefunds,
        external: brief.metrics.externalRefunds,
        unclassified: brief.metrics.unclassifiedRefunds,
      },
    },
    {
      pattern: /\bmaintainers?\b/gi,
      metrics: { external: brief.metrics.externalMaintainers },
    },
  ] as const;
  const errors: string[] = [];
  let previousProvenance: 'internal' | 'external' | 'unclassified' | undefined;

  for (const sentence of normalized.split(/[.;]/)) {
    const sentenceProvenance = provenances(sentence);
    const fallback = sentenceProvenance.length === 1 ? sentenceProvenance[0] : previousProvenance;
    const numbers = [...sentence.matchAll(/-?\d+(?:\.\d+)?/g)].map((match) => ({
      value: normalizeNumber(match[0]),
      start: match.index,
      end: match.index + match[0].length,
    }));
    if (numbers.length === 0) continue;

    for (const dimension of dimensions) {
      for (const claim of sentence.matchAll(dimension.pattern)) {
        const start = claim.index;
        const end = claim.index + claim[0].length;
        const candidates = numbers.map((candidate) => ({
          ...candidate,
          distance:
            candidate.end < start
              ? start - candidate.end
              : candidate.start > end
                ? candidate.start - end
                : 0,
        }));
        const number =
          candidates
            .filter((candidate) => candidate.end <= start)
            .sort((left, right) => left.distance - right.distance)[0] ??
          candidates.sort((left, right) => left.distance - right.distance)[0];
        if (!number || number.distance > 48) continue;

        const provenance = nearestProvenance(sentence, start, end) ?? fallback;
        if (!provenance || !(provenance in dimension.metrics)) {
          errors.push('metric_provenance_ambiguous');
          continue;
        }
        const metric = dimension.metrics[provenance as keyof typeof dimension.metrics];
        if (!metric || ![metric.total, metric.delta].map(String).includes(number.value)) {
          errors.push('metric_value_mismatch');
        }
      }
    }
    if (sentenceProvenance.length === 1) previousProvenance = sentenceProvenance[0];
  }
  return [...new Set(errors)];
}

function provenances(sentence: string): Array<'internal' | 'external' | 'unclassified'> {
  return [
    ...new Set(
      [...sentence.matchAll(/\b(?:internal|operator-funded|external|unclassified)\b/gi)].map(
        (match) => {
          const value = match[0].toLowerCase();
          return value === 'internal' || value === 'operator-funded'
            ? ('internal' as const)
            : (value as 'external' | 'unclassified');
        },
      ),
    ),
  ];
}

function nearestProvenance(
  sentence: string,
  claimStart: number,
  claimEnd: number,
): 'internal' | 'external' | 'unclassified' | undefined {
  const matches = [
    ...sentence.matchAll(/\b(?:internal|operator-funded|external|unclassified)\b/gi),
  ].map((match) => {
    const value = match[0].toLowerCase();
    const start = match.index;
    const end = start + match[0].length;
    return {
      provenance:
        value === 'internal' || value === 'operator-funded'
          ? ('internal' as const)
          : (value as 'external' | 'unclassified'),
      distance: end < claimStart ? claimStart - end : start > claimEnd ? start - claimEnd : 0,
    };
  });
  const nearest = matches.sort((left, right) => left.distance - right.distance)[0];
  return nearest && nearest.distance <= 48 ? nearest.provenance : undefined;
}

function extractUrls(text: string): string[] {
  return (text.match(/https?:\/\/[^\s<>()]+/g) ?? []).map((url) => url.replace(/[.,;!?]+$/, ''));
}

function allowedOrigin(url: string, origins: string[]): boolean {
  try {
    return origins.includes(new URL(url).origin);
  } catch {
    return false;
  }
}

function normalizeNumber(value: string): string {
  const number = Number(value);
  return Number.isFinite(number) ? String(number) : value;
}

function normalizedText(value: string): string {
  return value
    .toLowerCase()
    .replace(/https?:\/\/\S+/g, '')
    .replace(/[^a-z0-9\s]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
}

function similarity(left: string, right: string): number {
  if (left === right) return 1;
  const leftTokens = trigrams(left);
  const rightTokens = trigrams(right);
  if (leftTokens.size === 0 || rightTokens.size === 0) return 0;
  const intersection = [...leftTokens].filter((token) => rightTokens.has(token)).length;
  return intersection / (leftTokens.size + rightTokens.size - intersection);
}

function trigrams(value: string): Set<string> {
  const tokens = value.split(' ');
  if (tokens.length < 3) return new Set(tokens.length ? [tokens.join(' ')] : []);
  return new Set(tokens.slice(0, -2).map((_, index) => tokens.slice(index, index + 3).join(' ')));
}
