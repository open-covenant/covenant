import { timingSafeEqual } from 'node:crypto';
import Fastify, { type FastifyReply, type FastifyRequest } from 'fastify';
import pino from 'pino';
import { z } from 'zod';
import { loadConfig, type Config } from './config.js';
import { HttpDaemonClient, type DaemonClient } from './daemon.js';
import { toConductEvent, type ConductEvent } from './mapping.js';

const FETCH_CAP = 100_000;

const QuerySchema = z.object({
  since: z.string().optional(),
  cursor: z.string().optional(),
  limit: z.coerce.number().int().positive().optional(),
});

function parseSince(raw: string | undefined): number | undefined {
  if (raw === undefined) return undefined;
  if (/^\d+$/.test(raw)) return Number(raw);
  const ms = Date.parse(raw);
  if (Number.isNaN(ms)) throw new Error('since must be epoch ms or ISO-8601');
  return ms;
}

async function buildPage(
  daemon: DaemonClient,
  cfg: Config,
  opts: { sinceMs?: number; limit: number; agentId?: string },
): Promise<{ events: ConductEvent[]; nextCursor: number | null; hasMore: boolean }> {
  const raw =
    opts.sinceMs !== undefined
      ? await daemon.recentAudit({ sinceMs: opts.sinceMs, limit: FETCH_CAP })
      : await daemon.recentAudit({ limit: opts.limit });

  let events = raw.map(toConductEvent);
  if (opts.agentId) {
    const id = opts.agentId;
    events = events.filter((e) => e.agent_id === id || e.agent_display === id);
  }
  events.sort((a, b) => a.occurred_at_ms - b.occurred_at_ms || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));

  const page = events.slice(0, opts.limit);
  const hasMore = events.length > opts.limit;
  const last = page.at(-1);
  const nextCursor = last ? last.occurred_at_ms : (opts.sinceMs ?? null);
  return { events: page, nextCursor, hasMore };
}

export interface Deps {
  daemon?: DaemonClient;
  logger?: pino.Logger;
}

export function buildServer(cfg: Config, deps: Deps = {}) {
  const logger =
    deps.logger ?? pino({ level: process.env.LOG_LEVEL ?? 'info', redact: ['req.headers.authorization'] });
  const daemon = deps.daemon ?? new HttpDaemonClient(cfg.daemonUrl, cfg.daemonToken);
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

  app.get('/healthz', async () => ({ ok: true, service: 'fairscale-bridge', daemon: await daemon.health() }));

  app.get('/v1/attestation', { preHandler: requireFairscale }, async (_req, reply) => {
    try {
      const report = await daemon.verify();
      return {
        audit_root: report.root_hash_hex,
        verified: report.valid,
        event_count: report.events,
        anchor_count: report.anchors,
        failures: report.failures,
      };
    } catch (err) {
      reply.log.error({ err }, 'daemon verify failed');
      return reply.code(502).send({ error: 'daemon unreachable' });
    }
  });

  const handleEvents = (agentScoped: boolean) =>
    async (req: FastifyRequest, reply: FastifyReply) => {
      const parsed = QuerySchema.safeParse(req.query);
      if (!parsed.success) return reply.code(400).send({ error: 'invalid query', issues: parsed.error.issues });
      const agentId = agentScoped ? (req.params as { agentId: string }).agentId : undefined;
      let sinceMs: number | undefined;
      try {
        sinceMs = parseSince(parsed.data.cursor ?? parsed.data.since);
      } catch (err) {
        return reply.code(400).send({ error: (err as Error).message });
      }
      const limit = Math.min(parsed.data.limit ?? cfg.defaultLimit, cfg.maxLimit);

      let attestation: Awaited<ReturnType<DaemonClient['verify']>> | null = null;
      try {
        const [{ events, nextCursor, hasMore }, report] = await Promise.all([
          buildPage(daemon, cfg, { sinceMs, limit, agentId }),
          daemon.verify(),
        ]);
        attestation = report;
        return {
          pillar: 'work_history',
          agent_scope: agentId ?? null,
          count: events.length,
          has_more: hasMore,
          next_cursor: nextCursor,
          attestation: {
            audit_root: attestation.root_hash_hex,
            verified: attestation.valid,
            event_count: attestation.events,
            anchor_count: attestation.anchors,
          },
          events,
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

if (process.argv[1] && process.argv[1].endsWith('server.js')) {
  const cfg = loadConfig();
  const app = buildServer(cfg);
  const shutdown = (sig: string) => {
    app.log.info({ sig }, 'shutting down');
    app.close().then(() => process.exit(0));
  };
  process.on('SIGTERM', () => shutdown('SIGTERM'));
  process.on('SIGINT', () => shutdown('SIGINT'));
  app.listen({ port: cfg.port, host: '0.0.0.0' }).catch((err) => {
    app.log.error({ err }, 'failed to start');
    process.exit(1);
  });
}
