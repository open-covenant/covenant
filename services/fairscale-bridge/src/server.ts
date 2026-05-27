import { timingSafeEqual } from 'node:crypto';
import Fastify, { type FastifyReply, type FastifyRequest } from 'fastify';
import pino from 'pino';
import { z } from 'zod';
import { loadConfig, type Config } from './config.js';
import { HttpDaemonClient, type DaemonClient } from './daemon.js';
import { toConductEvent, type ConductEvent } from './mapping.js';
import { after, decodeCursor, encodeCursor, START, type Cursor } from './cursor.js';

const QuerySchema = z.object({
  since: z.string().optional(),
  cursor: z.string().optional(),
  limit: z.coerce.number().int().positive().optional(),
});

function resolveCursor(q: { since?: string; cursor?: string }): Cursor {
  if (q.cursor) return decodeCursor(q.cursor);
  if (q.since) {
    if (/^\d+$/.test(q.since)) return { ms: Number(q.since), id: '' };
    const ms = Date.parse(q.since);
    if (Number.isNaN(ms)) throw new Error('since must be epoch ms or ISO-8601');
    return { ms, id: '' };
  }
  return START;
}

interface Page {
  events: ConductEvent[];
  nextCursor: string | null;
  hasMore: boolean;
  truncated: boolean;
}

async function buildPage(
  daemon: DaemonClient,
  cfg: Config,
  opts: { cursor: Cursor; limit: number; agentId?: string },
): Promise<Page> {
  const raw = await daemon.recentAudit({ sinceMs: opts.cursor.ms, limit: cfg.fetchCap });
  const truncated = raw.length >= cfg.fetchCap;

  let events = raw.map(toConductEvent);
  if (opts.agentId) {
    const id = opts.agentId;
    events = events.filter((e) => e.agent_id === id || e.agent_display === id);
  }
  events.sort((a, b) => a.occurred_at_ms - b.occurred_at_ms || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  events = events.filter((e) => after(e.occurred_at_ms, e.id, opts.cursor));

  const page = events.slice(0, opts.limit);
  const hasMore = events.length > opts.limit;
  const last = page.at(-1);
  const atStart = opts.cursor.ms === START.ms && opts.cursor.id === START.id;
  const nextCursor = last
    ? encodeCursor(last.occurred_at_ms, last.id)
    : atStart
      ? null
      : encodeCursor(opts.cursor.ms, opts.cursor.id);
  return { events: page, nextCursor, hasMore, truncated };
}

export interface Deps {
  daemon?: DaemonClient;
  logger?: pino.Logger;
}

export function buildServer(cfg: Config, deps: Deps = {}) {
  const logger =
    deps.logger ?? pino({ level: process.env.LOG_LEVEL ?? 'info', redact: ['req.headers.authorization'] });
  const daemon = deps.daemon ?? new HttpDaemonClient(cfg.daemonUrl, cfg.daemonToken, cfg.daemonTimeoutMs, cfg.daemonRetries);
  const app = Fastify({ loggerInstance: logger });

  const expected = Buffer.from(cfg.apiToken, 'utf8');
  const requireFairscale = async (req: FastifyRequest, reply: FastifyReply): Promise<void> => {
    const header = req.headers.authorization;
    if (!header?.startsWith('Bearer ')) {
      reply.code(401).send({ error: 'missing bearer authorization' });
      return;
    }
    const supplied = Buffer.from(header.slice('Bearer '.length).trim(), 'utf8');
    if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) {
      reply.code(403).send({ error: 'invalid token' });
    }
  };

  app.get('/healthz', async () => ({ ok: true, service: 'fairscale-bridge', version: '1', daemon: await daemon.health() }));

  app.get('/v1/attestation', { preHandler: requireFairscale }, async (_req, reply) => {
    reply.header('cache-control', 'no-store');
    try {
      const r = await daemon.verify();
      return { audit_root: r.root_hash_hex, verified: r.valid, event_count: r.events, anchor_count: r.anchors, failures: r.failures };
    } catch (err) {
      reply.log.error({ err }, 'daemon verify failed');
      return reply.code(502).send({ error: 'daemon unreachable' });
    }
  });

  const handleEvents =
    (agentScoped: boolean) =>
    async (req: FastifyRequest, reply: FastifyReply) => {
      const parsed = QuerySchema.safeParse(req.query);
      if (!parsed.success) return reply.code(400).send({ error: 'invalid query', issues: parsed.error.issues });

      let cursor: Cursor;
      try {
        cursor = resolveCursor(parsed.data);
      } catch (err) {
        return reply.code(400).send({ error: (err as Error).message });
      }
      const agentId = agentScoped ? (req.params as { agentId: string }).agentId : undefined;
      const limit = Math.min(parsed.data.limit ?? cfg.defaultLimit, cfg.maxLimit);
      reply.header('cache-control', 'no-store');

      try {
        const [page, report] = await Promise.all([
          buildPage(daemon, cfg, { cursor, limit, agentId }),
          daemon.verify().catch((err) => {
            reply.log.warn({ err }, 'attestation unavailable');
            return null;
          }),
        ]);
        return {
          version: '1',
          pillar: 'work_history',
          agent_scope: agentId ?? null,
          count: page.events.length,
          has_more: page.hasMore,
          next_cursor: page.nextCursor,
          truncated: page.truncated,
          attested: !!report && report.valid,
          attestation: report
            ? { audit_root: report.root_hash_hex, verified: report.valid, event_count: report.events, anchor_count: report.anchors }
            : null,
          events: page.events,
        };
      } catch (err) {
        reply.log.error({ err }, 'daemon call failed');
        return reply.code(502).send({ error: 'daemon unreachable' });
      }
    };

  app.get('/v1/conduct-events', { preHandler: requireFairscale }, handleEvents(false));
  app.get('/v1/agents/:agentId/conduct-events', { preHandler: requireFairscale }, handleEvents(true));

  return app;
}

if (process.argv[1]?.endsWith('server.js')) {
  const cfg = loadConfig();
  const app = buildServer(cfg);
  const shutdown = (sig: string) => {
    app.log.info({ sig }, 'shutting down');
    app.close().then(() => process.exit(0), () => process.exit(1));
  };
  process.on('SIGTERM', () => shutdown('SIGTERM'));
  process.on('SIGINT', () => shutdown('SIGINT'));
  app.listen({ port: cfg.port, host: '0.0.0.0' }).catch((err) => {
    app.log.error({ err }, 'failed to start');
    process.exit(1);
  });
}
