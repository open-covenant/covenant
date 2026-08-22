import { upgradeStates, type UpgradeStats } from './domain.js';

type Counter =
  | 'submissions'
  | 'rejections'
  | 'artifact_verifications'
  | 'pull_requests'
  | 'check_polls'
  | 'shadow_deployments'
  | 'shadow_health_polls'
  | 'promotions'
  | 'promotion_health_polls'
  | 'rollbacks'
  | 'retries'
  | 'completions'
  | 'failures'
  | 'errors';

export class UpdaterMetrics {
  private readonly counters = new Map<Counter, number>();

  increment(name: Counter): void {
    this.counters.set(name, (this.counters.get(name) ?? 0) + 1);
  }

  render(stats: UpgradeStats): string {
    const definitions: Array<[Counter, string]> = [
      ['submissions', 'Valid signed proposals submitted.'],
      ['rejections', 'API requests rejected.'],
      ['artifact_verifications', 'Artifacts verified by SHA-256 and size.'],
      ['pull_requests', 'Pull requests created or synchronized.'],
      ['check_polls', 'Required-check polls.'],
      ['shadow_deployments', 'Shadow deployments started.'],
      ['shadow_health_polls', 'Shadow deployment health polls.'],
      ['promotions', 'Promotion hooks confirmed.'],
      ['promotion_health_polls', 'Promoted deployment health polls.'],
      ['rollbacks', 'Rollback hooks completed.'],
      ['retries', 'Retryable actions rescheduled.'],
      ['completions', 'Upgrades completed.'],
      ['failures', 'Upgrades ending in failure or rollback.'],
      ['errors', 'Unexpected service errors.'],
    ];
    const lines: string[] = [];
    for (const [name, help] of definitions) {
      lines.push(
        `# HELP mizuki_updater_${name}_total ${help}`,
        `# TYPE mizuki_updater_${name}_total counter`,
        `mizuki_updater_${name}_total ${this.counters.get(name) ?? 0}`,
      );
    }
    lines.push(
      '# HELP mizuki_updater_upgrades Current durable upgrades by state.',
      '# TYPE mizuki_updater_upgrades gauge',
    );
    for (const state of upgradeStates) {
      lines.push(`mizuki_updater_upgrades{state="${state}"} ${stats.byState[state] ?? 0}`);
    }
    lines.push(
      '# HELP mizuki_updater_upgrades_total Total durable upgrades.',
      '# TYPE mizuki_updater_upgrades_total gauge',
      `mizuki_updater_upgrades_total ${stats.total}`,
    );
    return `${lines.join('\n')}\n`;
  }
}
