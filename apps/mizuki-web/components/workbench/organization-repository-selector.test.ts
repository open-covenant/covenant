import { describe, expect, it } from 'vitest';
import type { WorkbenchRepository } from '@/lib/workbench';
import { organizationsForRepositories } from './organization-repository-selector';

describe('organization repository selection', () => {
  it('deduplicates owners without losing their display casing', () => {
    const repositories = [
      repository('Open-Covenant', 'covenant'),
      repository('open-covenant', 'docs'),
      repository('Mizuki0x', 'sample'),
    ];

    expect(organizationsForRepositories(repositories)).toEqual(['Mizuki0x', 'Open-Covenant']);
  });
});

function repository(owner: string, repo: string): WorkbenchRepository {
  return {
    owner,
    repo,
    fullName: `${owner}/${repo}`,
    readiness: 'ready',
    maintenanceAppStatus: 'installed',
    verifierAppStatus: 'installed',
    validationCommands: [],
  };
}
