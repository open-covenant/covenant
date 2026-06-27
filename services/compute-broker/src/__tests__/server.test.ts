import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import type { FastifyInstance } from 'fastify';
import * as ed25519 from '@noble/ed25519';
import bs58 from 'bs58';
import { build } from '../server.js';
import { loadConfig } from '../config.js';
import { hexToKey, verify } from '../attestation.js';
import type { ComputeProvider, LeaseRequest, LeaseReservation } from '../providers.js';

const deriveAgentKey = ed25519.getPublicKeyAsync;

class FakeProvider implements ComputeProvider {
  readonly name: 'ionet' | 'akash';
  readonly calls: Array<{ op: string; leaseId: string }> = [];
  private statuses = new Map<string, 'reserved' | 'active' | 'cancelled' | 'reclaimed'>();
  constructor(name: 'ionet' | 'akash') {
    this.name = name;
  }
  async reserve(req: LeaseRequest): Promise<LeaseReservation> {
    const leaseId = `${this.name}-lease-${req.gpuHours}`;
    this.statuses.set(leaseId, 'reserved');
    return {
      leaseId,
      gpuHours: req.gpuHours,
      expiresAt: 1_700_000_000,
      pricedUsdMicro: 50_000_000,
    };
  }
  async activate(leaseId: string): Promise<void> {
    this.calls.push({ op: 'activate', leaseId });
    this.statuses.set(leaseId, 'active');
  }
  async cancel(leaseId: string): Promise<{ refundUsdMicro: number }> {
    this.calls.push({ op: 'cancel', leaseId });
    this.statuses.set(leaseId, 'cancelled');
    return { refundUsdMicro: 0 };
  }
  async reclaim(leaseId: string): Promise<void> {
    this.calls.push({ op: 'reclaim', leaseId });
    this.statuses.set(leaseId, 'reclaimed');
  }
  async status(leaseId: string): Promise<'reserved' | 'active' | 'cancelled' | 'reclaimed'> {
    return this.statuses.get(leaseId) ?? 'reserved';
  }
}

describe('compute-broker server', () => {
  const key = 'ab'.repeat(32);
  const operatorBearer = 'test-operator-bearer-token';
  const operatorAuth = { authorization: `Bearer ${operatorBearer}` };
  const cfg = loadConfig({
    BROKER_SIGNING_KEY_HEX: key,
    OPERATOR_BEARER_TOKEN: operatorBearer,
  });
  let app: FastifyInstance;
  let ionet: FakeProvider;
  let akash: FakeProvider;

  beforeAll(async () => {
    ionet = new FakeProvider('ionet');
    akash = new FakeProvider('akash');
    app = build({
      cfg,
      providers: { ionet, akash },
    });
    await app.ready();
  });

  afterAll(async () => {
    await app.close();
  });

  it('healthz reports key loaded', async () => {
    const res = await app.inject({ method: 'GET', url: '/healthz' });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ broker_key_loaded: true });
  });

  it('metrics exposes prometheus text', async () => {
    const res = await app.inject({ method: 'GET', url: '/metrics' });
    expect(res.statusCode).toBe(200);
    expect(res.body).toContain('compute_broker_bond_requests_total');
    expect(res.body).toContain('compute_broker_lease_lifecycle_ops_total');
  });

  it('bonds/request rejects bad body', async () => {
    const res = await app.inject({ method: 'POST', url: '/bonds/request', payload: {} });
    expect(res.statusCode).toBe(400);
  });

  it('bonds/request rejects over-max duration', async () => {
    const res = await app.inject({
      method: 'POST',
      url: '/bonds/request',
      payload: {
        agent_did: '11111111111111111111111111111111',
        provider: 'ionet',
        gpu_hours: 4,
        duration_secs: 20 * 24 * 3600,
      },
    });
    expect(res.statusCode).toBe(400);
  });

  it('bonds/request returns attestation that verifies under broker pubkey', async () => {
    const res = await app.inject({
      method: 'POST',
      url: '/bonds/request',
      payload: {
        agent_did: '11111111111111111111111111111111',
        provider: 'ionet',
        gpu_hours: 4,
        duration_secs: 7 * 24 * 3600,
      },
    });
    expect(res.statusCode).toBe(200);
    const body = res.json() as {
      lease_id: string;
      attestation_sig: string;
      broker_pubkey: string;
      gpu_hours: number;
      expires_at: number;
    };
    const ok = await verify(
      {
        agent_did: '11111111111111111111111111111111',
        provider: 'ionet',
        lease_id: body.lease_id,
        gpu_hours: body.gpu_hours,
        expires_at: body.expires_at,
      },
      body.attestation_sig,
      body.broker_pubkey,
    );
    expect(ok).toBe(true);
  });

  it('bonds/request returns 503 without broker key', async () => {
    const nokey = build({
      cfg: loadConfig({}),
      providers: { ionet: new FakeProvider('ionet'), akash: new FakeProvider('akash') },
    });
    await nokey.ready();
    const res = await nokey.inject({
      method: 'POST',
      url: '/bonds/request',
      payload: {
        agent_did: '11111111111111111111111111111111',
        provider: 'ionet',
        gpu_hours: 4,
        duration_secs: 3600,
      },
    });
    expect(res.statusCode).toBe(503);
    await nokey.close();
  });

  const freshCancelPayload = async (leaseId: string, opts: { expirySecs?: number } = {}) => {
    const agentKey = hexToKey('cd'.repeat(32));
    const agentPk = await deriveAgentKey(agentKey);
    const agentDid = bs58.encode(agentPk);
    const nonce = bs58.encode(Buffer.from(globalThis.crypto.getRandomValues(new Uint8Array(16))));
    const expires_at = Math.floor(Date.now() / 1000) + (opts.expirySecs ?? 60);
    const cancelMsg = new TextEncoder().encode(
      JSON.stringify({ action: 'cancel', lease_id: leaseId, agent_did: agentDid, nonce, expires_at }),
    );
    const sig = await ed25519.signAsync(cancelMsg, agentKey);
    return { agentDid, nonce, expires_at, signed_request: bs58.encode(sig) };
  };

  it('bonds/cancel rejects invalid signature', async () => {
    const { agentDid, nonce, expires_at } = await freshCancelPayload('lease-1');
    const res = await app.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: {
        lease_id: 'lease-1',
        agent_did: agentDid,
        signed_request: bs58.encode(new Uint8Array(64)),
        nonce,
        expires_at,
      },
    });
    expect(res.statusCode).toBe(403);
  });

  it('bonds/cancel rejects expired signed_request', async () => {
    const leaseId = 'ionet-lease-stale';
    const { agentDid, nonce, signed_request } = await freshCancelPayload(leaseId, {
      expirySecs: -10,
    });
    const res = await app.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: {
        lease_id: leaseId,
        agent_did: agentDid,
        signed_request,
        nonce,
        expires_at: Math.floor(Date.now() / 1000) - 10,
      },
    });
    expect(res.statusCode).toBe(400);
    expect(res.json()).toMatchObject({ error: 'signed_request expired' });
  });

  it('bonds/cancel rejects an expiry window beyond the configured cap', async () => {
    const leaseId = 'ionet-lease-too-long';
    const { agentDid, nonce, signed_request } = await freshCancelPayload(leaseId, {
      expirySecs: 9999,
    });
    const res = await app.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: {
        lease_id: leaseId,
        agent_did: agentDid,
        signed_request,
        nonce,
        expires_at: Math.floor(Date.now() / 1000) + 9999,
      },
    });
    expect(res.statusCode).toBe(400);
    expect(res.json()).toMatchObject({ error: /expires_at exceeds/ });
  });

  it('bonds/cancel succeeds with valid agent signature', async () => {
    const leaseId = 'ionet-lease-4';
    const { agentDid, nonce, expires_at, signed_request } = await freshCancelPayload(leaseId);
    const res = await app.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: {
        lease_id: leaseId,
        agent_did: agentDid,
        signed_request,
        nonce,
        expires_at,
      },
    });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ lease_id: leaseId, status: 'cancelled' });
  });

  it('bonds/cancel rejects a replayed nonce', async () => {
    const leaseId = 'ionet-lease-replay';
    const payload = await freshCancelPayload(leaseId);
    const first = await app.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: {
        lease_id: leaseId,
        agent_did: payload.agentDid,
        signed_request: payload.signed_request,
        nonce: payload.nonce,
        expires_at: payload.expires_at,
      },
    });
    expect(first.statusCode).toBe(200);
    const replay = await app.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: {
        lease_id: leaseId,
        agent_did: payload.agentDid,
        signed_request: payload.signed_request,
        nonce: payload.nonce,
        expires_at: payload.expires_at,
      },
    });
    expect(replay.statusCode).toBe(409);
    expect(replay.json()).toMatchObject({ error: 'nonce already used' });
  });

  it('bonds/cancel does not burn the nonce on a signature-verification failure', async () => {
    const leaseId = 'ionet-lease-badsig-nonce';
    const { agentDid, nonce, expires_at, signed_request } = await freshCancelPayload(leaseId);

    // A bad signature carrying a fresh, valid nonce must be rejected *before* the
    // nonce is recorded — otherwise an attacker could poison a victim's nonce.
    const bad = await app.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: {
        lease_id: leaseId,
        agent_did: agentDid,
        signed_request: bs58.encode(new Uint8Array(64)),
        nonce,
        expires_at,
      },
    });
    expect(bad.statusCode).toBe(403);

    // The real signer retries the SAME nonce with the valid signature and must
    // succeed; a 409 here would mean the failed attempt consumed the nonce.
    const good = await app.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: {
        lease_id: leaseId,
        agent_did: agentDid,
        signed_request,
        nonce,
        expires_at,
      },
    });
    expect(good.statusCode).toBe(200);
    expect(good.json()).toMatchObject({ lease_id: leaseId, status: 'cancelled' });
  });

  it('bonds/cancel refuses a second cancel of an already-cancelled lease without re-refunding', async () => {
    const leaseId = 'ionet-lease-double-cancel';
    const first = await freshCancelPayload(leaseId);
    const firstRes = await app.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: {
        lease_id: leaseId,
        agent_did: first.agentDid,
        signed_request: first.signed_request,
        nonce: first.nonce,
        expires_at: first.expires_at,
      },
    });
    expect(firstRes.statusCode).toBe(200);

    const second = await freshCancelPayload(leaseId);
    const secondRes = await app.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: {
        lease_id: leaseId,
        agent_did: second.agentDid,
        signed_request: second.signed_request,
        nonce: second.nonce,
        expires_at: second.expires_at,
      },
    });
    expect(secondRes.statusCode).toBe(409);
    expect(secondRes.json()).toMatchObject({ error: 'lease already cancelled' });
    const cancels = ionet.calls.filter((c) => c.op === 'cancel' && c.leaseId === leaseId);
    expect(cancels).toHaveLength(1);
  });

  it('bonds/cancel refuses cancel of a reclaimed lease', async () => {
    const leaseId = 'ionet-lease-reclaimed';
    const reclaim = await app.inject({
      method: 'POST',
      url: '/leases/reclaim',
      headers: operatorAuth,
      payload: { lease_id: leaseId, provider: 'ionet' },
    });
    expect(reclaim.statusCode).toBe(200);

    const { agentDid, nonce, expires_at, signed_request } = await freshCancelPayload(leaseId);
    const res = await app.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: {
        lease_id: leaseId,
        agent_did: agentDid,
        signed_request,
        nonce,
        expires_at,
      },
    });
    expect(res.statusCode).toBe(409);
    expect(res.json()).toMatchObject({ error: 'lease already reclaimed' });
    expect(ionet.calls).not.toContainEqual({ op: 'cancel', leaseId });
  });

  it('bonds/cancel returns 404 when the lease exists on no provider and burns the nonce first', async () => {
    class UnavailableProvider extends FakeProvider {
      async status(): Promise<'reserved' | 'active' | 'cancelled' | 'reclaimed'> {
        throw new Error('provider status backend unavailable');
      }
    }
    const isolated = build({
      cfg,
      providers: { ionet: new UnavailableProvider('ionet'), akash: new UnavailableProvider('akash') },
    });
    await isolated.ready();

    const leaseId = 'ghost-lease';
    const { agentDid, nonce, expires_at, signed_request } = await freshCancelPayload(leaseId);
    const res = await isolated.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: { lease_id: leaseId, agent_did: agentDid, signed_request, nonce, expires_at },
    });
    expect(res.statusCode).toBe(404);
    expect(res.json()).toMatchObject({ error: 'lease not found on any provider' });

    // The nonce is recorded before the provider lookup, so a same-nonce replay is
    // refused as used — it is never re-evaluated against the providers (anti-probe).
    const replay = await isolated.inject({
      method: 'POST',
      url: '/bonds/cancel',
      payload: { lease_id: leaseId, agent_did: agentDid, signed_request, nonce, expires_at },
    });
    expect(replay.statusCode).toBe(409);
    expect(replay.json()).toMatchObject({ error: 'nonce already used' });
    await isolated.close();
  });

  it('leases/activate requires operator bearer', async () => {
    const noAuth = await app.inject({
      method: 'POST',
      url: '/leases/activate',
      payload: { lease_id: 'ionet-lease-noauth', provider: 'ionet' },
    });
    expect(noAuth.statusCode).toBe(401);
    const badAuth = await app.inject({
      method: 'POST',
      url: '/leases/activate',
      headers: { authorization: 'Bearer wrong-token-value-1234567890' },
      payload: { lease_id: 'ionet-lease-badauth', provider: 'ionet' },
    });
    expect(badAuth.statusCode).toBe(403);
  });

  it('leases/activate rejects a same-length wrong operator bearer', async () => {
    // The badauth test above sends a different-length token, so requireOperator
    // short-circuits on the cheap length check and never runs timingSafeEqual.
    // Flip one byte while keeping the length identical so the constant-time
    // byte-comparison is the only thing that can reject it — without this, a
    // mutation trusting length alone would accept any same-length forgery.
    const last = operatorBearer.slice(-1);
    const sameLengthWrong = `${operatorBearer.slice(0, -1)}${last === 'x' ? 'y' : 'x'}`;
    expect(sameLengthWrong).toHaveLength(operatorBearer.length);
    expect(sameLengthWrong).not.toBe(operatorBearer);
    const res = await app.inject({
      method: 'POST',
      url: '/leases/activate',
      headers: { authorization: `Bearer ${sameLengthWrong}` },
      payload: { lease_id: 'ionet-lease-samelen', provider: 'ionet' },
    });
    expect(res.statusCode).toBe(403);
    expect(res.json()).toMatchObject({ error: 'invalid bearer' });
  });

  it('leases/activate activates the selected provider lease', async () => {
    const res = await app.inject({
      method: 'POST',
      url: '/leases/activate',
      headers: operatorAuth,
      payload: { lease_id: 'ionet-lease-9', provider: 'ionet' },
    });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ lease_id: 'ionet-lease-9', status: 'active' });
    expect(ionet.calls).toContainEqual({ op: 'activate', leaseId: 'ionet-lease-9' });
  });

  it('leases/reclaim reclaims the selected provider lease', async () => {
    const res = await app.inject({
      method: 'POST',
      url: '/leases/reclaim',
      headers: operatorAuth,
      payload: { lease_id: 'akash-lease-5', provider: 'akash' },
    });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ lease_id: 'akash-lease-5', status: 'reclaimed' });
    expect(akash.calls).toContainEqual({ op: 'reclaim', leaseId: 'akash-lease-5' });
  });

  it('leases/expire-sweep reclaims expired leases and skips active windows', async () => {
    const res = await app.inject({
      method: 'POST',
      url: '/leases/expire-sweep',
      headers: operatorAuth,
      payload: {
        now_unix: 1_700_000_100,
        leases: [
          { lease_id: 'ionet-expired', provider: 'ionet', slashable_until: 1_700_000_000 },
          { lease_id: 'akash-still-live', provider: 'akash', slashable_until: 1_700_000_500 },
        ],
      },
    });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({
      reclaimed: 1,
      skipped: 1,
      errors: 0,
    });
    expect(ionet.calls).toContainEqual({ op: 'reclaim', leaseId: 'ionet-expired' });
  });
});
