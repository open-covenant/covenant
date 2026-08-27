import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

describe('public work page', () => {
  it('routes paid maintenance through Workbench only', () => {
    const source = readFileSync(new URL('./page.tsx', import.meta.url), 'utf8');

    expect(source).toContain('href="/app/jobs/new"');
    expect(source).not.toContain('QuoteWorkflow');
    expect(source).not.toContain("'/api/mizuki/v1/jobs'");
  });
});
