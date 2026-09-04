import { describe, expect, it } from 'vitest';
import { createLogger } from '../logger.js';

function capture() {
  const lines: Record<string, unknown>[] = [];
  return { lines, sink: (line: string) => lines.push(JSON.parse(line) as Record<string, unknown>) };
}

describe('createLogger', () => {
  it('writes one JSON object per line with level, message, and time', () => {
    const { lines, sink } = capture();
    const log = createLogger({ sink, component: 'desk', now: () => 1_700_000_000_000 });
    log.info('desk started', { port: 46631 });
    expect(lines).toHaveLength(1);
    expect(lines[0]).toMatchObject({
      level: 'info',
      msg: 'desk started',
      component: 'desk',
      port: 46631,
      ts: '2023-11-14T22:13:20.000Z',
    });
  });

  it('drops lines below the configured level', () => {
    const { lines, sink } = capture();
    const log = createLogger({ sink, level: 'warn' });
    log.debug('quiet');
    log.info('quiet');
    log.warn('loud');
    log.error('louder');
    expect(lines.map((line) => line.level)).toEqual(['warn', 'error']);
  });

  it('carries child fields onto every line', () => {
    const { lines, sink } = capture();
    const log = createLogger({ sink }).child({ module: 'chain' });
    log.info('reading feeds');
    expect(lines[0]).toMatchObject({ module: 'chain' });
  });

  it('never writes a registered secret', () => {
    const secret = `0x${'cd'.repeat(32)}`;
    const { lines, sink } = capture();
    const log = createLogger({ sink, secrets: [secret] });
    log.info('signing', { with: secret });
    expect(JSON.stringify(lines[0])).not.toContain('cdcd');
  });

  it('accepts a secret registered after the logger was built', () => {
    const secret = 'a'.repeat(50);
    const { lines, sink } = capture();
    const log = createLogger({ sink });
    log.addSecret(secret);
    log.info('done', { value: secret });
    expect(JSON.stringify(lines[0])).not.toContain(secret);
  });

  it('serialises bigint fields', () => {
    const { lines, sink } = capture();
    const log = createLogger({ sink });
    log.info('block', { blockNumber: 54_423_221n });
    expect(lines[0]).toMatchObject({ blockNumber: '54423221' });
  });
});
