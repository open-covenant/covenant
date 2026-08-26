'use client';

import { useEffect, useMemo, useState } from 'react';
import type { WorkbenchRepository } from '@/lib/workbench';

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
      <label htmlFor="workbench-organization">
        GitHub organization or owner
        <span className="wizard-select-control">
          <select
            id="workbench-organization"
            value={organization}
            disabled={disabled}
            onChange={(event) => {
              setOrganization(event.target.value);
              onSelect('');
            }}
          >
            <option value="">Choose an organization</option>
            {organizations.map((owner) => (
              <option value={owner} key={owner.toLowerCase()}>
                {owner}
              </option>
            ))}
          </select>
        </span>
      </label>

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
