import type { ComputeApp } from '../domain';
import { formatDuration, formatVram } from '../domain';
import { Icon, type IconName } from './Icon';

interface AppCardProps {
  app: ComputeApp;
  selected: boolean;
  onSelect: (app: ComputeApp) => void;
}

const kindIcons: Record<ComputeApp['kind'], IconName> = {
  workspace: 'workspace',
  agent: 'workspace',
  image: 'image',
  chat: 'chat',
};

export function AppCard({ app, selected, onSelect }: AppCardProps) {
  const preview = app.availability === 'preview';

  return (
    <button
      aria-pressed={selected}
      className={`app-card${selected ? ' app-card--selected' : ''}`}
      onClick={() => onSelect(app)}
      type="button"
    >
      <span className="app-card__top">
        <span className="app-card__icon">
          <Icon name={kindIcons[app.kind]} />
        </span>
        <span className={`tag ${preview ? 'tag--preview' : 'tag--available'}`}>
          {preview ? 'Preview' : 'Available'}
        </span>
      </span>
      <span className="app-card__name">{app.name}</span>
      <span className="app-card__summary">{app.summary}</span>
      <span className="app-card__meta">
        <span>{formatVram(app.min_vram_mib)}+ VRAM</span>
        <span>{formatDuration(app.default_duration_secs)} default</span>
      </span>
    </button>
  );
}
