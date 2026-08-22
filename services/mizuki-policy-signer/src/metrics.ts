import type { OperationStatus } from './domain.js';
import type { StoreStats } from './store.js';

export class SignerMetrics {
  private readonly counters = new Map<string, number>();

  increment(name: 'requests' | 'rejections' | 'broadcasts' | 'recoveries' | 'errors'): void {
    this.counters.set(name, (this.counters.get(name) ?? 0) + 1);
  }

  render(stats: StoreStats): string {
    const lines = [
      '# HELP mizuki_signer_requests_total Accepted signer API requests.',
      '# TYPE mizuki_signer_requests_total counter',
      `mizuki_signer_requests_total ${this.counters.get('requests') ?? 0}`,
      '# HELP mizuki_signer_rejections_total Requests rejected by schema or policy.',
      '# TYPE mizuki_signer_rejections_total counter',
      `mizuki_signer_rejections_total ${this.counters.get('rejections') ?? 0}`,
      '# HELP mizuki_signer_broadcasts_total Signed transaction broadcast attempts.',
      '# TYPE mizuki_signer_broadcasts_total counter',
      `mizuki_signer_broadcasts_total ${this.counters.get('broadcasts') ?? 0}`,
      '# HELP mizuki_signer_recoveries_total Durable operations processed by recovery.',
      '# TYPE mizuki_signer_recoveries_total counter',
      `mizuki_signer_recoveries_total ${this.counters.get('recoveries') ?? 0}`,
      '# HELP mizuki_signer_errors_total Unexpected signer errors.',
      '# TYPE mizuki_signer_errors_total counter',
      `mizuki_signer_errors_total ${this.counters.get('errors') ?? 0}`,
      '# HELP mizuki_signer_operations Number of durable operations by state.',
      '# TYPE mizuki_signer_operations gauge',
    ];
    const statuses: OperationStatus[] = [
      'reserved',
      'prepared',
      'broadcasting',
      'submitted',
      'reconciling',
      'finalized',
      'rejected',
    ];
    for (const status of statuses) {
      lines.push(`mizuki_signer_operations{status="${status}"} ${stats.byStatus[status] ?? 0}`);
    }
    lines.push(
      '# HELP mizuki_signer_operations_total Total durable operations.',
      '# TYPE mizuki_signer_operations_total gauge',
      `mizuki_signer_operations_total ${stats.total}`,
    );
    return `${lines.join('\n')}\n`;
  }
}
