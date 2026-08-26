'use client';

import { useEffect, useMemo, useState } from 'react';
import type { WorkbenchRepository } from '@/lib/workbench';
import { WorkbenchSelect } from './workbench-select';

export function OrganizationRepositorySelector({
  repositories,
  selected,
  disabled,
  onSelect,
}: {
  repositories: readonly WorkbenchRepository[];
  selected: string;
  disabled: boolean;
  onSelect: (repository: string) => void;
}) {
  const organizations = useMemo(() => organizationsForRepositories(repositories), [repositories]);
  const selectedRepository = repositories.find(
    (repository) => repository.fullName.toLowerCase() === selected.toLowerCase(),
  );
  const [organization, setOrganization] = useState(selectedRepository?.owner ?? '');

  useEffect(() => {
    if (selectedRepository) setOrganization(selectedRepository.owner);
  }, [selectedRepository]);

  const visible = repositories.filter(
    (repository) => repository.owner.toLowerCase() === organization.toLowerCase(),
  );

  return (
    <div className="wizard-organization-selector">
      <div className="workbench-field">
        <span id="workbench-organization-label">GitHub organization or owner</span>
        <WorkbenchSelect
          id="workbench-organization"
          labelledBy="workbench-organization-label"
          value={organization}
          placeholder="Choose an organization"
          options={organizations.map((owner) => ({ value: owner, label: owner }))}
          disabled={disabled}
          onChange={(owner) => {
            setOrganization(owner);
            onSelect('');
          }}
        />
      </div>

      {organization && (
        <div className="wizard-repository-grid" aria-label={`Repositories in ${organization}`}>
          {visible.map((repository) => {
            const active = selected.toLowerCase() === repository.fullName.toLowerCase();
            return (
              <button
                type="button"
                className={active ? 'selected' : ''}
                onClick={() => onSelect(repository.fullName)}
                aria-pressed={active}
                disabled={repository.readiness !== 'ready' || disabled}
                key={repository.fullName}
              >
                <span>{repository.owner}</span>
                <strong>{repository.repo}</strong>
                <small>
                  {repository.readiness === 'ready'
                    ? 'Ready for work'
                    : repository.readiness === 'unavailable'
                      ? 'Status unavailable'
                      : 'Setup required'}
                </small>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

export function organizationsForRepositories(
  repositories: readonly WorkbenchRepository[],
): string[] {
  const owners = new Map<string, string>();
  for (const repository of repositories) {
    const key = repository.owner.toLowerCase();
    if (!owners.has(key)) owners.set(key, repository.owner);
  }
  return [...owners.values()].sort((left, right) => left.localeCompare(right));
}
