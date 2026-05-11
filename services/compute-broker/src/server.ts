import { timingSafeEqual } from 'node:crypto';
import Fastify, { type FastifyReply, type FastifyRequest } from 'fastify';
import { z } from 'zod';
import { loadConfig, type Config } from './config.js';
import { verifyAsync } from '@noble/ed25519';
import bs58 from 'bs58';
import {
  hexToKey,
  sign,
  type AttestationPayload,
} from './attestation.js';
import {
  createProviders,
  selectProvider,
  type ComputeProvider,
} from './providers.js';
import {
  attestationsSigned,
  bondRequests,
  leaseLifecycleOps,
  leaseReservationLatency,
  registry,
} from './metrics.js';

const BondRequestBody = z.object({
  agent_did: z.string().min(32).max(44),
  provider: z.enum(['ionet', 'akash']),
  gpu_hours: z.number().int().positive(),
  duration_secs: z.number().int().positive(),
  capability_hints: z.array(z.string()).optional(),
});

const BondCancelBody = z.object({
  lease_id: z.string().min(1),
  agent_did: z.string().min(32).max(44),
  signed_request: z.string().min(1).max(128),
  // Nonce is 16+ random bytes, base58-encoded. Replayed nonces (within
  // BOND_CANCEL_MAX_EXPIRY_SECS) are rejected so a captured signed_request
  // cannot cancel the same lease twice.
  nonce: z.string().min(16).max(64),
  // Unix seconds. Server rejects if expires_at < now (stale) or
  // > now + BOND_CANCEL_MAX_EXPIRY_SECS (over-long window).
  expires_at: z.number().int().positive(),
});

const LeaseActionBody = z.object({
  lease_id: z.string().min(1),
  provider: z.enum(['ionet', 'akash']),
});

const ExpireSweepBody = z.object({
  now_unix: z.number().int().positive().optional(),
  leases: z
    .array(
      z.object({
        lease_id: z.string().min(1),
        provider: z.enum(['ionet', 'akash']),
        slashable_until: z.number().int().positive(),
      }),
    )
    .min(1),
});

export type BuildOpts = {
  cfg: Config;
  providers?: { ionet: ComputeProvider; akash: ComputeProvider };
};

export function build(opts: BuildOpts) {
  const cfg = opts.cfg;
  const providers = opts.providers ?? createProviders(cfg);
  const app = Fastify({ logger: { level: process.env.LOG_LEVEL ?? 'info' } });

  // Replay-protection store for /bonds/cancel. Maps nonce -> expires_at
  // (unix seconds). Entries past expiry are pruned on each insert. Single
  // process scope; horizontal scaling needs Redis (same constraint as
  // proof-gen's in-memory rate limit).
  const seenCancelNonces = new Map<string, number>();
  const pruneExpiredNonces = (nowUnix: number): void => {
    for (const [nonce, exp] of seenCancelNonces) {
      if (exp <= nowUnix) seenCancelNonces.delete(nonce);
    }
  };

  const requireOperator = async (req: FastifyRequest, reply: FastifyReply): Promise<void> => {
    if (!cfg.operatorBearerToken) {
      reply
        .code(503)
        .send({ error: 'operator bearer not configured; set OPERATOR_BEARER_TOKEN' });
      return;
    }
    const header = req.headers.authorization;
    if (typeof header !== 'string' || !header.startsWith('Bearer ')) {
      reply.code(401).send({ error: 'missing Bearer authorization' });
      return;
    }
    const supplied = Buffer.from(header.slice('Bearer '.length).trim(), 'utf8');
    const expected = Buffer.from(cfg.operatorBearerToken, 'utf8');
    if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) {
      reply.code(403).send({ error: 'invalid bearer' });
    }
  };

  app.get('/healthz', async () => ({
    status: 'ok',
    broker_key_loaded: cfg.signingKeyHex !== undefined,
    operator_bearer_loaded: cfg.operatorBearerToken !== undefined,
    // Report only providers whose credentials are actually configured so
    // k8s readiness can spot a half-wired deploy. The hardcoded list lied
    // when IONET_API_KEY / AKASH_WALLET were unset — every call to the
    // unconfigured provider would throw on first hit.
    providers: [
      cfg.ionetApiKey ? 'ionet' : null,
      cfg.akashWallet ? 'akash' : null,
    ].filter(Boolean),
  }));

  app.get('/metrics', async (_req, reply) => {
    reply.header('content-type', registry.contentType);
    return registry.metrics();
  });

  app.get('/leases/:id', async (req, reply) => {
    const id = (req.params as { id: string }).id;
    try {
      const status = await providers.ionet.status(id);
      return { lease_id: id, status };
    } catch (err) {
      return reply.code(501).send({ error: asMessage(err) });
    }
  });

  app.post('/leases/activate', { preHandler: requireOperator }, async (req, reply) => {
    const parsed = LeaseActionBody.safeParse(req.body);
    if (!parsed.success) {
      return reply.code(400).send({ error: parsed.error.message });
    }

    const { lease_id, provider: providerName } = parsed.data;
    const provider = selectProvider(providerName, providers);
    try {
      await provider.activate(lease_id);
      leaseLifecycleOps.inc({ provider: providerName, operation: 'activate', status: 'ok' });
      return reply.send({ lease_id, provider: providerName, status: 'active' });
    } catch (err) {
      leaseLifecycleOps.inc({ provider: providerName, operation: 'activate', status: 'error' });
      return reply.code(502).send({ error: asMessage(err) });
    }
  });

  app.post('/leases/reclaim', { preHandler: requireOperator }, async (req, reply) => {
    const parsed = LeaseActionBody.safeParse(req.body);
    if (!parsed.success) {
      return reply.code(400).send({ error: parsed.error.message });
    }

    const { lease_id, provider: providerName } = parsed.data;
    const provider = selectProvider(providerName, providers);
    try {
      await provider.reclaim(lease_id);
      leaseLifecycleOps.inc({ provider: providerName, operation: 'reclaim', status: 'ok' });
      return reply.send({ lease_id, provider: providerName, status: 'reclaimed' });
    } catch (err) {
      leaseLifecycleOps.inc({ provider: providerName, operation: 'reclaim', status: 'error' });
      return reply.code(502).send({ error: asMessage(err) });
    }
  });

  app.post('/bonds/request', async (req, reply) => {
    const parsed = BondRequestBody.safeParse(req.body);
    if (!parsed.success) {
      bondRequests.inc({ provider: 'unknown', status: 'bad_request' });
      return reply.code(400).send({ error: parsed.error.message });
    }
    const body = parsed.data;

    if (body.duration_secs > cfg.maxBondDurationSecs) {
      bondRequests.inc({ provider: body.provider, status: 'duration_exceeded' });
      return reply.code(400).send({ error: 'duration exceeds maxBondDuration' });
    }

    if (cfg.signingKeyHex === undefined) {
      bondRequests.inc({ provider: body.provider, status: 'no_key' });
      return reply.code(503).send({ error: 'broker key not loaded' });
    }

    const provider = selectProvider(body.provider, providers);
    const stopTimer = leaseReservationLatency.startTimer({ provider: body.provider });

    let reservation;
    try {
      reservation = await provider.reserve({
        gpuHours: body.gpu_hours,
        durationSecs: body.duration_secs,
        capabilityHints: body.capability_hints,
      });
      stopTimer({ status: 'ok' });
    } catch (err) {
      stopTimer({ status: 'error' });
      bondRequests.inc({ provider: body.provider, status: 'provider_error' });
      return reply.code(502).send({ error: asMessage(err) });
    }

    const payload: AttestationPayload = {
      agent_did: body.agent_did,
      provider: body.provider,
      lease_id: reservation.leaseId,
      gpu_hours: reservation.gpuHours,
      expires_at: reservation.expiresAt,
    };

    let attestation;
    try {
      attestation = await sign(payload, hexToKey(cfg.signingKeyHex));
    } catch (err) {
      bondRequests.inc({ provider: body.provider, status: 'sign_error' });
      return reply.code(500).send({ error: asMessage(err) });
    }

    attestationsSigned.inc({ provider: body.provider });
    bondRequests.inc({ provider: body.provider, status: 'ok' });

    return reply.send({
      lease_id: reservation.leaseId,
      attestation_sig: attestation.signatureBs58,
      broker_pubkey: attestation.pubkeyBs58,
      gpu_hours: reservation.gpuHours,
      expires_at: reservation.expiresAt,
      reserved_price_usd_micro: reservation.pricedUsdMicro,
    });
  });

  app.post('/bonds/cancel', async (req, reply) => {
    const parsed = BondCancelBody.safeParse(req.body);
    if (!parsed.success) return reply.code(400).send({ error: parsed.error.message });
    const { lease_id, agent_did, signed_request, nonce, expires_at } = parsed.data;

    if (cfg.signingKeyHex === undefined) {
      return reply.code(503).send({ error: 'broker key not loaded' });
    }

    const nowUnix = Math.floor(Date.now() / 1000);
    pruneExpiredNonces(nowUnix);
    if (expires_at <= nowUnix) {
      bondRequests.inc({ provider: 'unknown', status: 'expired_request' });
      return reply.code(400).send({ error: 'signed_request expired' });
    }
    if (expires_at > nowUnix + cfg.bondCancelMaxExpirySecs) {
      bondRequests.inc({ provider: 'unknown', status: 'expiry_window_too_long' });
      return reply
        .code(400)
        .send({ error: `expires_at exceeds ${cfg.bondCancelMaxExpirySecs}s window` });
    }
    if (seenCancelNonces.has(nonce)) {
      bondRequests.inc({ provider: 'unknown', status: 'replayed_nonce' });
      return reply.code(409).send({ error: 'nonce already used' });
    }

    const cancelMsg = new TextEncoder().encode(
      JSON.stringify({ action: 'cancel', lease_id, agent_did, nonce, expires_at }),
    );

    let sigBytes: Uint8Array;
    try {
      sigBytes = bs58.decode(signed_request);
    } catch {
      return reply.code(400).send({ error: 'invalid signed_request encoding' });
    }

    let agentPk: Uint8Array;
    try {
      agentPk = bs58.decode(agent_did);
    } catch {
      return reply.code(400).send({ error: 'invalid agent_did encoding' });
    }

    let valid = false;
    try {
      if (sigBytes.length === 64 && agentPk.length === 32) {
        valid = await verifyAsync(sigBytes, cancelMsg, agentPk);
      }
    } catch {
      valid = false;
    }
    if (!valid) {
      bondRequests.inc({ provider: 'unknown', status: 'bad_signature' });
      return reply.code(403).send({ error: 'signed_request verification failed' });
    }

    // Record the nonce only after the signature verifies — an unsigned
    // payload should not be able to poison the cache against a later
    // legitimate use of the same nonce by the real signer.
    seenCancelNonces.set(nonce, expires_at);

    let leaseStatus: string;
    try {
      leaseStatus = await providers.ionet.status(lease_id);
    } catch {
      try {
        leaseStatus = await providers.akash.status(lease_id);
      } catch {
        return reply.code(404).send({ error: 'lease not found on any provider' });
      }
    }

    if (leaseStatus === 'cancelled' || leaseStatus === 'reclaimed') {
      return reply.code(409).send({ error: `lease already ${leaseStatus}` });
    }

    let refund: { refundUsdMicro: number };
    try {
      refund = await providers.ionet.cancel(lease_id);
    } catch {
      try {
        refund = await providers.akash.cancel(lease_id);
      } catch (err) {
        return reply.code(502).send({ error: asMessage(err) });
      }
    }

    bondRequests.inc({ provider: 'unknown', status: 'cancelled' });
    return reply.send({
      lease_id,
      status: 'cancelled',
      refund_usd_micro: refund.refundUsdMicro,
    });
  });

  app.post('/leases/expire-sweep', { preHandler: requireOperator }, async (req, reply) => {
    const parsed = ExpireSweepBody.safeParse(req.body);
    if (!parsed.success) {
      return reply.code(400).send({ error: parsed.error.message });
    }

    const nowUnix = parsed.data.now_unix ?? Math.floor(Date.now() / 1000);
    const results: Array<{
      lease_id: string;
      provider: 'ionet' | 'akash';
      status: 'reclaimed' | 'skipped' | 'error';
      reason?: string;
    }> = [];

    for (const lease of parsed.data.leases) {
      if (lease.slashable_until > nowUnix) {
        leaseLifecycleOps.inc({
          provider: lease.provider,
          operation: 'expire_sweep',
          status: 'skipped',
        });
        results.push({
          lease_id: lease.lease_id,
          provider: lease.provider,
          status: 'skipped',
          reason: 'slashable window still active',
        });
        continue;
      }

      const provider = selectProvider(lease.provider, providers);
      try {
        await provider.reclaim(lease.lease_id);
        leaseLifecycleOps.inc({
          provider: lease.provider,
          operation: 'expire_sweep',
          status: 'ok',
        });
        results.push({
          lease_id: lease.lease_id,
          provider: lease.provider,
          status: 'reclaimed',
        });
      } catch (err) {
        leaseLifecycleOps.inc({
          provider: lease.provider,
          operation: 'expire_sweep',
          status: 'error',
        });
        results.push({
          lease_id: lease.lease_id,
          provider: lease.provider,
          status: 'error',
          reason: asMessage(err),
        });
      }
    }

    return reply.send({
      now_unix: nowUnix,
      reclaimed: results.filter((item) => item.status === 'reclaimed').length,
      skipped: results.filter((item) => item.status === 'skipped').length,
      errors: results.filter((item) => item.status === 'error').length,
      results,
    });
  });

  return app;
}

function asMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export async function main(): Promise<void> {
  const cfg = loadConfig();
  const app = build({ cfg });
  await app.listen({ port: cfg.port, host: cfg.host });
}

if (import.meta.url === `file://${process.argv[1]}`) {
  process.on('unhandledRejection', (reason) => {
    process.stderr.write(`unhandled rejection: ${asMessage(reason)}\n`);
    process.exit(1);
  });
  process.on('uncaughtException', (err) => {
    process.stderr.write(`uncaught exception: ${asMessage(err)}\n`);
    process.exit(1);
  });
  main().catch((err) => {
    process.stderr.write(`fatal: ${asMessage(err)}\n`);
    process.exit(1);
  });
}
