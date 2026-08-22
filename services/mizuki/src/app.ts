import { timingSafeEqual } from 'node:crypto';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { ActivityStreams, PublicAdmission, RateLimitError } from './admission.js';
import type { ContributorAuth } from './auth.js';
import type { BountyService } from './bounties.js';
import type { Config } from './config.js';
import { dashboard } from './dashboard.js';
import { JobProcessor } from './executor.js';
import { GithubClient } from './github.js';
import { metrics, prometheus } from './metrics.js';
import {
  publicActivity,
  publicActivityFeed,
  publicBounty,
  publicCapabilityHandoff,
  publicCapabilities,
  publicJob,
  publicTreasury,
} from './public-api.js';
import { createQuote } from './quote.js';
import { recordPaymentReceipts } from './receipts.js';
import type { MizukiStore } from './store.js';
import { GithubWebhookHandler, verifyGithubWebhook } from './webhooks.js';
import type { Job } from './types.js';
import { Payments, USDC_DECIMALS, USDC_MAINNET, paymentRequiredHeader } from './x402.js';
import {
  assertRefundCapacity,
  RefundCapacityError,
  type PaymentPolicy,
  type PolicyReadiness,
  type RefundLiability,
} from './policy-client.js';
import type { ServiceReadiness } from './readiness.js';

export type AppDependencies = {
  config: Config;
  store: MizukiStore;
  github: GithubClient;
  payments: Payments;
  processor: JobProcessor;
  auth: ContributorAuth;
  webhooks: GithubWebhookHandler;
  bounties: BountyService;
  policy: PaymentPolicy;
  paymentAdmission: SerialGate;
  readiness: ServiceReadiness;
};

export function createApp(deps: AppDependencies) {
  const admission = new PublicAdmission(deps.config);
  return async (req: IncomingMessage, res: ServerResponse): Promise<void> => {
    try {
      const url = new URL(req.url ?? '/', 'http://localhost');
      const parts = url.pathname.split('/').filter(Boolean);

      applyCors(req, res, deps.config.webOrigin);
      if (req.method === 'OPTIONS') {
        res.writeHead(204);
        res.end();
        return;
      }

      if (req.method === 'GET' && url.pathname === '/') {
        res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
        res.end(dashboard);
        return;
      }
      if (req.method === 'GET' && url.pathname === '/healthz') {
        return json(res, 200, { ok: true });
      }
      if (req.method === 'GET' && url.pathname === '/readyz') {
        const report = await deps.readiness.check();
        res.setHeader('cache-control', 'no-store');
        return json(res, report.ready ? 200 : 503, report);
      }
      if (req.method === 'GET' && url.pathname === '/v1/metrics') {
        const report = await deps.readiness.check();
        res.setHeader('cache-control', 'no-store');
        return json(res, 200, await metrics(deps.config, deps.store, report));
      }
      if (req.method === 'GET' && url.pathname === '/metrics') {
        const report = await deps.readiness.check();
        res.writeHead(200, {
          'content-type': 'text/plain; version=0.0.4',
          'cache-control': 'no-store',
        });
        res.end(prometheus(await metrics(deps.config, deps.store, report)));
        return;
      }
      if (req.method === 'GET' && url.pathname === '/v1/activity') {
        const limit = boundedInt(url.searchParams.get('limit'), 100, 1, 500);
        return json(res, 200, { events: await publicActivityFeed(deps.store, limit) });
      }
      if (req.method === 'GET' && url.pathname === '/v1/treasury') {
        const report = await deps.readiness.check();
        res.setHeader('cache-control', 'no-store');
        return json(res, 200, await publicTreasury(deps.store, report));
      }
      if (req.method === 'GET' && url.pathname === '/v1/events') {
        return streamActivity(
          req,
          res,
          deps.store,
          admission.source(req),
          admission.streams,
          deps.config.sseIdleTimeoutMs ?? 120_000,
        );
      }
      if (req.method === 'GET' && url.pathname === '/v1/auth/github') {
        admission.consume('oauth_start', req);
        const redirect =
          url.searchParams.get('return_to') ?? url.searchParams.get('redirect') ?? undefined;
        res.writeHead(302, {
          location: deps.auth.authorizeUrl(redirect),
          'cache-control': 'no-store',
        });
        res.end();
        return;
      }
      if (req.method === 'GET' && url.pathname === '/v1/auth/github/callback') {
        admission.consume('oauth_callback', req);
        const code = url.searchParams.get('code');
        const state = url.searchParams.get('state');
        if (!code || !state) return json(res, 400, { error: 'OAuth callback is incomplete' });
        const result = await deps.auth.callback(code, state);
        const origin = deps.config.webOrigin ?? deps.config.publicBaseUrl;
        res.writeHead(302, {
          location: `${origin.replace(/\/$/, '')}${result.redirect}`,
          'set-cookie': sessionCookie(result.session, req, deps.config.trustedProxyHops ?? 0),
          'cache-control': 'no-store',
        });
        res.end();
        return;
      }
      if (req.method === 'GET' && url.pathname === '/v1/auth/session') {
        const session = deps.auth.session(cookies(req).mizuki_session);
        if (!session) return json(res, 401, { error: 'not signed in' });
        const contributor = await deps.store.contributor(session.githubId);
        return json(res, 200, { contributor });
      }
      if (req.method === 'POST' && url.pathname === '/v1/auth/wallet/challenges') {
        admission.consume('wallet_challenge', req);
        const session = requireSession(req, deps.auth);
        const body = await bodyJson<{ wallet?: unknown }>(req);
        if (typeof body.wallet !== 'string') return json(res, 400, { error: 'wallet is required' });
        const challenge = await deps.auth.createWalletChallenge(session, body.wallet);
        return json(res, 201, challenge);
      }
      if (req.method === 'POST' && url.pathname === '/v1/auth/wallet/verify') {
        admission.consume('wallet_verify', req);
        const session = requireSession(req, deps.auth);
        const body = await bodyJson<{ challengeId?: unknown; signature?: unknown }>(req);
        if (typeof body.challengeId !== 'string' || typeof body.signature !== 'string') {
          return json(res, 400, { error: 'challengeId and signature are required' });
        }
        const contributor = await deps.auth.verifyWalletChallenge(
          session,
          body.challengeId,
          body.signature,
        );
        return json(res, 200, { contributor, proofId: body.challengeId });
      }
      if (req.method === 'POST' && url.pathname === '/v1/github/webhook') {
        if (!deps.config.githubWebhookSecret) {
          return json(res, 503, { error: 'GitHub webhook is not configured' });
        }
        const delivery = header(req, 'x-github-delivery');
        const event = header(req, 'x-github-event');
        const signature = header(req, 'x-hub-signature-256');
        if (!delivery || !event || !signature) {
          return json(res, 400, { error: 'GitHub webhook headers are incomplete' });
        }
        const raw = await bodyRaw(req, 1_000_000);
        if (!verifyGithubWebhook(deps.config.githubWebhookSecret, raw, signature)) {
          return json(res, 401, { error: 'invalid GitHub webhook signature' });
        }
        const processed = await deps.webhooks.handle(delivery, event, raw);
        return json(res, processed ? 202 : 200, { accepted: true, duplicate: !processed });
      }
      if (req.method === 'GET' && url.pathname === '/v1/bounties') {
        return json(res, 200, {
          bounties: await Promise.all(
            (await deps.store.bountiesList()).map((bounty) => publicBounty(deps.store, bounty)),
          ),
        });
      }
      if (req.method === 'GET' && parts[0] === 'v1' && parts[1] === 'bounties' && parts[2]) {
        const bounty = await deps.store.bounty(parts[2]);
        if (!bounty) return json(res, 404, { error: 'bounty not found' });
        return json(res, 200, await publicBounty(deps.store, bounty));
      }
      if (
        req.method === 'POST' &&
        parts[0] === 'v1' &&
        parts[1] === 'bounties' &&
        parts[2] &&
        parts[3] === 'wallet-proof'
      ) {
        admission.consume('bounty_wallet_proof', req);
        const session = requireSession(req, deps.auth);
        const bounty = await deps.store.bounty(parts[2]);
        if (!bounty) return json(res, 404, { error: 'bounty not found' });
        if (bounty.state !== 'open') {
          return json(res, 409, { error: 'bounty is not accepting claims' });
        }
        const body = await bodyJson<{
          address?: unknown;
        }>(req);
        if (typeof body.address !== 'string') {
          return json(res, 400, { error: 'address is required' });
        }
        const walletAddress = body.address;
        const contributor = await deps.store.contributor(session.githubId);
        if (!contributor) return json(res, 401, { error: 'contributor not found' });
        if (!session.githubGrantId || !session.githubGrantExpiresAt) {
          return json(res, 401, { error: 'sign in with GitHub again before claiming a bounty' });
        }
        if (Date.parse(session.githubGrantExpiresAt) <= Date.now()) {
          return json(res, 401, { error: 'GitHub claim authorization expired; sign in again' });
        }
        const githubGrantId = session.githubGrantId;
        const challenge = await deps.paymentAdmission.run(async () => {
          await assertOperatorControlOpen(deps.store, 'claims');
          return deps.bounties.createClaimChallenge(
            bounty.id,
            contributor,
            walletAddress,
            githubGrantId,
          );
        });
        return json(res, 201, {
          id: challenge.id,
          challengeId: challenge.id,
          message: challenge.message,
          expiresAt: challenge.expiresAt,
          claimExpiresAt: challenge.claimExpiresAt,
        });
      }
      if (
        req.method === 'POST' &&
        parts[0] === 'v1' &&
        parts[1] === 'bounties' &&
        parts[2] &&
        parts[3] === 'claim'
      ) {
        admission.consume('bounty_claim', req);
        const session = requireSession(req, deps.auth);
        const contributor = await deps.store.contributor(session.githubId);
        if (!contributor) return json(res, 401, { error: 'contributor not found' });
        const body = await bodyJson<{
          challenge_id?: unknown;
          signature?: unknown;
        }>(req);
        if (typeof body.challenge_id !== 'string' || typeof body.signature !== 'string') {
          return json(res, 400, { error: 'challenge_id and signature are required' });
        }
        const challengeId = body.challenge_id;
        const signature = body.signature;
        const claimed = await deps.paymentAdmission.run(async () => {
          await assertOperatorControlOpen(deps.store, 'claims');
          return deps.bounties.claim(parts[2], contributor, challengeId, signature);
        });
        return json(res, 200, await publicBounty(deps.store, claimed));
      }
      if (
        req.method === 'POST' &&
        parts[0] === 'v1' &&
        parts[1] === 'bounties' &&
        parts[2] &&
        parts[3] === 'pr'
      ) {
        admission.consume('bounty_pr', req);
        const session = requireSession(req, deps.auth);
        const contributor = await deps.store.contributor(session.githubId);
        if (!contributor) return json(res, 401, { error: 'contributor not found' });
        const body = await bodyJson<{ pullRequestUrl?: unknown }>(req);
        if (typeof body.pullRequestUrl !== 'string') {
          return json(res, 400, { error: 'pullRequestUrl is required' });
        }
        const bounty = await deps.bounties.submitPullRequest(
          parts[2],
          contributor,
          body.pullRequestUrl,
        );
        return json(res, 200, await publicBounty(deps.store, bounty));
      }
      if (
        req.method === 'POST' &&
        parts[0] === 'v1' &&
        parts[1] === 'bounties' &&
        parts[2] &&
        parts[3] === 'disputes'
      ) {
        admission.consume('bounty_dispute', req);
        const session = requireSession(req, deps.auth);
        const contributor = await deps.store.contributor(session.githubId);
        if (!contributor) return json(res, 401, { error: 'contributor not found' });
        const body = await bodyJson<{ reason?: unknown }>(req);
        if (typeof body.reason !== 'string') return json(res, 400, { error: 'reason is required' });
        const bounty = await deps.bounties.openDispute(parts[2], contributor, body.reason);
        return json(res, 201, await publicBounty(deps.store, bounty));
      }
      if (req.method === 'GET' && url.pathname === '/v1/capabilities') {
        return json(res, 200, { capabilities: await publicCapabilities(deps.store) });
      }
      if (
        req.method === 'GET' &&
        parts[0] === 'v1' &&
        parts[1] === 'capabilities' &&
        parts[2] &&
        parts[3] === 'handoff'
      ) {
        const handoff = await publicCapabilityHandoff(deps.store, parts[2]);
        if (!handoff) return json(res, 404, { error: 'capability handoff not found' });
        return json(res, 200, handoff);
      }
      if (req.method === 'POST' && url.pathname === '/v1/quotes') {
        admission.consume('quote', req);
        const body = await bodyJson<{ github_issue_url?: unknown }>(req);
        if (typeof body.github_issue_url !== 'string') {
          return json(res, 400, { error: 'github_issue_url is required' });
        }
        const issue = await deps.github.issue(body.github_issue_url);
        const quote = await deps.store.saveQuote(createQuote(issue));
        return json(res, 201, { ...quote, payment: await deps.payments.challenge(quote) });
      }
      if (req.method === 'POST' && url.pathname === '/v1/jobs') {
        const key = header(req, 'idempotency-key');
        if (!key || key.length > 128)
          return json(res, 400, { error: 'idempotency-key header is required' });
        const body = await bodyJson<{ quote_id?: unknown }>(req);
        if (typeof body.quote_id !== 'string')
          return json(res, 400, { error: 'quote_id is required' });
        const existing = await deps.store.jobByIdempotencyKey(key);
        if (existing) {
          if (existing.quote.id !== body.quote_id)
            return json(res, 409, { error: 'idempotency key already used' });
          return json(res, 200, publicJob(existing));
        }
        const quote = await deps.store.quote(body.quote_id);
        if (!quote) return json(res, 404, { error: 'quote not found' });
        const reserved = await deps.store.jobByQuote(quote.id);
        if (reserved) return json(res, 200, publicJob(reserved));
        admission.consume('job', req);
        if (Date.parse(quote.expiresAt) <= Date.now())
          return json(res, 409, { error: 'quote expired' });
        await deps.github.assertIssueAuthorization(
          quote.owner,
          quote.repo,
          quote.issueNumber,
          quote.installationId,
          quote.authorizationReceipt?.evidenceHash,
          { title: quote.issueTitle, body: quote.issueBody },
        );
        const head = await deps.github.currentHead(quote.owner, quote.repo, quote.defaultBranch);
        if (head !== quote.baseSha)
          return json(res, 409, { error: 'repository changed; request a new quote' });
        const paymentSignature = header(req, 'payment-signature');
        let pendingJobId: string | undefined;
        let refundLiabilityId: string | undefined;
        let payment;
        try {
          const settle = () =>
            deps.payments.settle(quote, paymentSignature, async (authorized) => {
              const reservation = await deps.store.createJob(quote, authorized, key);
              if (!reservation.created) {
                if (reservation.job.quote.id !== quote.id) {
                  throw new Error('payment proof is already reserved for another quote');
                }
                throw new ConcurrentPaymentReservation(reservation.job);
              }
              pendingJobId = reservation.job.id;
            });
          payment = await deps.paymentAdmission.run(async () => {
            if (deps.config.paymentMode === 'live' && paymentSignature) {
              await ensureRefundCapacity(deps, BigInt(quote.priceAtomic));
            }
            await assertOperatorControlOpen(deps.store, 'intake');
            const result = await settle();
            if (!result.ok || deps.config.paymentMode !== 'live') return result;
            if (!pendingJobId) throw new Error('payment authorization was not persisted');
            const liability = await deps.policy.registerRefundLiability(
              pendingJobId,
              result.payment.transaction,
            );
            assertLiabilityMatchesPayment(liability, pendingJobId, result.payment);
            refundLiabilityId = liability.id;
            await deps.store.patchJob(pendingJobId, { refundLiabilityId });
            return result;
          });
        } catch (cause) {
          if (cause instanceof ConcurrentPaymentReservation) {
            return json(res, 202, publicJob(cause.job));
          }
          throw cause;
        }
        if (!payment.ok) {
          res.setHeader('payment-required', paymentRequiredHeader(payment.challenge));
          res.setHeader('cache-control', 'private, no-store');
          return json(res, 402, {
            ...payment.challenge,
            ...(payment.reason ? { reason: payment.reason } : {}),
          });
        }
        if (!pendingJobId) throw new Error('payment authorization was not persisted');
        const job = await deps.store.transitionJob(pendingJobId, 'settlement_pending', 'paid', {
          payment: payment.payment,
          refundLiabilityId,
        });
        try {
          await recordPaymentReceipts(deps.store, job);
        } catch (receiptError) {
          console.error(
            `failed to publish payment receipts for job ${job.id}: ${receiptError instanceof Error ? receiptError.message : String(receiptError)}`,
          );
        }
        if (payment.responseHeader) res.setHeader('payment-response', payment.responseHeader);
        void deps.processor.process(job.id);
        return json(res, 202, publicJob(job));
      }
      if (req.method === 'GET' && parts[0] === 'v1' && parts[1] === 'jobs' && parts[2]) {
        const job = await deps.store.job(parts[2]);
        if (!job) return json(res, 404, { error: 'job not found' });
        if (parts[3] === 'receipt') {
          const activity = (await deps.store.activity(500)).filter(
            (event) => event.subjectId === job.id,
          );
          return json(res, 200, {
            job: publicJob(job),
            activity: await Promise.all(activity.map((event) => publicActivity(deps.store, event))),
          });
        }
        return json(res, 200, publicJob(job));
      }
      if (url.pathname === '/v1/admin/jobs' && req.method === 'GET') {
        if (!admin(req, deps.config.adminToken)) return json(res, 401, { error: 'unauthorized' });
        return json(res, 200, await deps.store.jobsList());
      }
      if (url.pathname === '/v1/admission' && req.method === 'GET') {
        const controls = await readOperatorControls(deps.store);
        return json(res, 200, {
          intakeEnabled: controls.intakeEnabled,
          claimsEnabled: controls.claimsEnabled,
          revision: controls.revision,
          updatedAt: controls.updatedAt,
        });
      }
      if (url.pathname === '/v1/admin/admission' && req.method === 'GET') {
        if (!admin(req, deps.config.adminToken)) return json(res, 401, { error: 'unauthorized' });
        return json(res, 200, await readOperatorControls(deps.store));
      }
      if (url.pathname === '/v1/admin/admission' && req.method === 'POST') {
        if (!admin(req, deps.config.adminToken)) return json(res, 401, { error: 'unauthorized' });
        const body = await bodyJson<{
          intakeEnabled?: unknown;
          claimsEnabled?: unknown;
          reason?: unknown;
        }>(req);
        if (body.intakeEnabled === undefined && body.claimsEnabled === undefined) {
          return json(res, 400, { error: 'intakeEnabled or claimsEnabled is required' });
        }
        if (
          (body.intakeEnabled !== undefined && typeof body.intakeEnabled !== 'boolean') ||
          (body.claimsEnabled !== undefined && typeof body.claimsEnabled !== 'boolean')
        ) {
          return json(res, 400, { error: 'admission controls must be booleans' });
        }
        if (
          typeof body.reason !== 'string' ||
          body.reason.trim().length < 10 ||
          body.reason.trim().length > 500
        ) {
          return json(res, 400, { error: 'reason must contain 10-500 characters' });
        }
        const reason = body.reason.trim();
        const controls = await deps.paymentAdmission.run(() =>
          deps.store.updateOperatorControls({
            ...(typeof body.intakeEnabled === 'boolean'
              ? { intakeEnabled: body.intakeEnabled }
              : {}),
            ...(typeof body.claimsEnabled === 'boolean'
              ? { claimsEnabled: body.claimsEnabled }
              : {}),
            reason,
            updatedBy: 'operator',
          }),
        );
        return json(res, 200, controls);
      }
      if (url.pathname === '/v1/admin/bounties/fund' && req.method === 'POST') {
        if (!admin(req, deps.config.adminToken)) return json(res, 401, { error: 'unauthorized' });
        await deps.bounties.fundAwaiting();
        return json(res, 200, { ok: true });
      }
      if (url.pathname === '/v1/admin/bounties/expire' && req.method === 'POST') {
        if (!admin(req, deps.config.adminToken)) return json(res, 401, { error: 'unauthorized' });
        return json(res, 200, { expired: await deps.bounties.expireClaims() });
      }
      if (
        req.method === 'POST' &&
        parts[0] === 'v1' &&
        parts[1] === 'admin' &&
        parts[2] === 'bounties' &&
        parts[3] &&
        parts[4] === 'disputes' &&
        parts[5] &&
        parts[6] === 'resolve'
      ) {
        if (!admin(req, deps.config.adminToken)) return json(res, 401, { error: 'unauthorized' });
        const key = header(req, 'idempotency-key');
        if (!key || key.length > 128) {
          return json(res, 400, { error: 'idempotency-key header is required' });
        }
        const body = await bodyJson<{
          decision?: unknown;
          evidence?: { summary?: unknown; references?: unknown };
        }>(req);
        if (body.decision !== 'release' && body.decision !== 'refund') {
          return json(res, 400, { error: 'decision must be release or refund' });
        }
        if (
          !body.evidence ||
          typeof body.evidence.summary !== 'string' ||
          !Array.isArray(body.evidence.references) ||
          !body.evidence.references.every((value) => typeof value === 'string')
        ) {
          return json(res, 400, { error: 'evidence summary and references are required' });
        }
        const bounty = await deps.bounties.resolveDispute(parts[3], parts[5], {
          decision: body.decision,
          evidence: {
            summary: body.evidence.summary,
            references: body.evidence.references as string[],
          },
          idempotencyKey: key,
        });
        return json(res, bounty.state === 'disputed' ? 202 : 200, {
          bounty: await publicBounty(deps.store, bounty),
          dispute: bounty.dispute,
        });
      }
      if (
        req.method === 'POST' &&
        parts[0] === 'v1' &&
        parts[1] === 'admin' &&
        parts[2] === 'refunds' &&
        parts[3]
      ) {
        if (!admin(req, deps.config.adminToken)) return json(res, 401, { error: 'unauthorized' });
        await deps.processor.retryRefund(parts[3]);
        return json(res, 200, publicJob((await deps.store.job(parts[3]))!));
      }
      if (
        req.method === 'POST' &&
        parts[0] === 'v1' &&
        parts[1] === 'admin' &&
        parts[2] === 'settlements' &&
        parts[3]
      ) {
        if (!admin(req, deps.config.adminToken)) return json(res, 401, { error: 'unauthorized' });
        const job = await deps.store.job(parts[3]);
        if (!job) return json(res, 404, { error: 'job not found' });
        if (job.state !== 'settlement_pending')
          return json(res, 409, { error: 'settlement is not pending' });
        const paid = await deps.paymentAdmission.run(async () => {
          // Intake controls new payment attempts. This reservation already exists and may
          // already be settled on-chain, so recovery must remain available during an incident.
          const payment = await deps.payments.retrySettlement(job.quote, job.payment);
          let refundLiabilityId: string | undefined;
          if (deps.config.paymentMode === 'live') {
            const liability = await deps.policy.registerRefundLiability(
              job.id,
              payment.transaction,
            );
            assertLiabilityMatchesPayment(liability, job.id, payment);
            refundLiabilityId = liability.id;
            await deps.store.patchJob(job.id, { refundLiabilityId });
          }
          return deps.store.transitionJob(job.id, 'settlement_pending', 'paid', {
            payment,
            refundLiabilityId,
          });
        });
        void deps.processor.process(job.id);
        return json(res, 202, publicJob(paid));
      }
      return json(res, 404, { error: 'not found' });
    } catch (cause) {
      if (cause instanceof RateLimitError) {
        res.setHeader('retry-after', String(cause.retryAfterSeconds));
        res.setHeader('cache-control', 'private, no-store');
        return json(res, 429, { error: cause.message });
      }
      const message = cause instanceof Error ? cause.message : String(cause);
      const status = /not signed in|unauthorized/i.test(message)
        ? 401
        : cause instanceof InvalidRequestBodyError
          ? 400
          : cause instanceof RefundCapacityError
            ? 503
            : cause instanceof OperatorAdmissionError
              ? 503
              : /not found/i.test(message)
                ? 404
                : /already|changed after the quote|concurrent|expected|not accepting|does not match the active|not funded|dispute intake|can no longer/i.test(
                      message,
                    )
                  ? 409
                  : /outside Mizuki|public GitHub|install the Mizuki|issue is too large|invalid|expired|required|incomplete/i.test(
                        message,
                      )
                    ? 422
                    : 500;
      const publicMessage =
        status < 500
          ? message
          : cause instanceof RefundCapacityError
            ? 'refund protection is temporarily unavailable'
            : cause instanceof OperatorAdmissionError
              ? message
              : 'request failed; retry later';
      return json(res, status, { error: publicMessage });
    }
  };
}

export async function ensureRefundCapacity(
  deps: Pick<AppDependencies, 'config' | 'store' | 'policy'>,
  proposedPaymentRaw: bigint,
): Promise<PolicyReadiness> {
  let readiness;
  try {
    readiness = await deps.policy.readiness();
  } catch {
    throw new RefundCapacityError('refund signer readiness check failed');
  }
  const jobs = await deps.store.jobsList();
  const unfinishedLiabilityRaw = jobs
    .filter((job) => !['delivered', 'refunded'].includes(job.state) && !job.refundLiabilityId)
    .reduce((total, job) => total + BigInt(job.payment.amountAtomic), 0n);
  assertRefundCapacity({
    readiness,
    treasury: deps.config.payTo,
    mint: USDC_MAINNET,
    decimals: USDC_DECIMALS,
    escrowAuthority: deps.config.clawPumpPayoutWallet,
    unfinishedLiabilityRaw,
    proposedPaymentRaw,
  });
  if (
    readiness.availableEscrowReserveLamports === null ||
    BigInt(readiness.availableEscrowReserveLamports) <
      BigInt(deps.config.escrowReadinessMinLamports)
  ) {
    throw new RefundCapacityError('escrow capacity cannot fund the configured rescue reserve');
  }
  return readiness;
}

function assertLiabilityMatchesPayment(
  liability: RefundLiability,
  jobId: string,
  payment: Job['payment'],
): void {
  if (
    liability.jobId !== jobId ||
    liability.settlementSignature !== payment.transaction ||
    liability.payer !== payment.payer ||
    liability.mint !== USDC_MAINNET ||
    liability.decimals !== USDC_DECIMALS ||
    liability.rawAmount !== payment.amountAtomic ||
    liability.amountUsdCents !== Number(payment.amountAtomic) / 10_000
  ) {
    throw new Error('refund liability evidence does not match the settled payment');
  }
}

async function streamActivity(
  req: IncomingMessage,
  res: ServerResponse,
  store: MizukiStore,
  source: string,
  streams: ActivityStreams,
  idleTimeoutMs: number,
): Promise<void> {
  const release = streams.acquire(source);
  res.writeHead(200, {
    'content-type': 'text/event-stream',
    'cache-control': 'no-cache, no-transform',
    connection: 'keep-alive',
    'x-accel-buffering': 'no',
  });
  let lastId = header(req, 'last-event-id');
  let closed = false;
  let lastActivityAt = Date.now();
  res.once('close', () => {
    closed = true;
  });

  try {
    while (!closed && Date.now() - lastActivityAt < idleTimeoutMs) {
      const events = (await store.activity(100)).reverse();
      const start = lastId ? events.findIndex((event) => event.id === lastId) + 1 : 0;
      const pending = events.slice(Math.max(0, start));
      for (const event of pending) {
        const value = await publicActivity(store, event);
        res.write(`id: ${event.id}\nevent: activity\ndata: ${JSON.stringify(value)}\n\n`);
        lastId = event.id;
      }
      if (pending.length > 0) lastActivityAt = Date.now();
      res.write(': heartbeat\n\n');
      const remaining = Math.max(0, idleTimeoutMs - (Date.now() - lastActivityAt));
      if (remaining === 0) break;
      await new Promise((resolve) => setTimeout(resolve, Math.min(5_000, remaining)));
    }
  } finally {
    release();
    res.end();
  }
}

export async function assertOperatorControlOpen(
  store: MizukiStore,
  control: 'intake' | 'claims',
): Promise<void> {
  const controls = await readOperatorControls(store);
  const enabled = control === 'intake' ? controls.intakeEnabled : controls.claimsEnabled;
  if (!enabled) throw new OperatorAdmissionError(`${control} is paused by the operator`);
}

async function readOperatorControls(store: MizukiStore) {
  try {
    return await store.operatorControls();
  } catch {
    throw new OperatorAdmissionError('operator admission controls are unavailable');
  }
}

function json(res: ServerResponse, status: number, value: unknown): void {
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(JSON.stringify(value));
}

async function bodyJson<T>(req: IncomingMessage): Promise<T> {
  const raw = await bodyRaw(req, 64_000);
  try {
    return JSON.parse(raw.toString('utf8') || '{}') as T;
  } catch {
    throw new InvalidRequestBodyError('request body must be valid JSON');
  }
}

async function bodyRaw(req: IncomingMessage, maxBytes: number): Promise<Buffer> {
  const chunks: Buffer[] = [];
  let length = 0;
  for await (const chunk of req) {
    const value = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    length += value.length;
    if (length > maxBytes) throw new Error(`request body exceeds ${maxBytes} bytes`);
    chunks.push(value);
  }
  return Buffer.concat(chunks);
}

function header(req: IncomingMessage, name: string): string | undefined {
  const value = req.headers[name];
  return Array.isArray(value) ? value[0] : value;
}

function applyCors(
  req: IncomingMessage,
  res: ServerResponse,
  allowedOrigin: string | undefined,
): void {
  const origin = header(req, 'origin');
  if (!origin || !allowedOrigin || origin !== allowedOrigin) return;
  res.setHeader('access-control-allow-origin', origin);
  res.setHeader('access-control-allow-credentials', 'true');
  res.setHeader(
    'access-control-allow-headers',
    'content-type,idempotency-key,payment-signature,last-event-id',
  );
  res.setHeader('access-control-allow-methods', 'GET,POST,OPTIONS');
  res.setHeader('vary', 'origin');
}

function boundedInt(value: string | null, fallback: number, min: number, max: number): number {
  if (value === null) return fallback;
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed >= min && parsed <= max ? parsed : fallback;
}

function admin(req: IncomingMessage, expected: string | undefined): boolean {
  if (!expected) return false;
  const supplied = header(req, 'authorization')?.replace(/^Bearer\s+/i, '');
  if (!supplied || supplied.length !== expected.length) return false;
  return timingSafeEqual(Buffer.from(supplied), Buffer.from(expected));
}

function requireSession(req: IncomingMessage, auth: ContributorAuth) {
  const session = auth.session(cookies(req).mizuki_session);
  if (!session) throw new Error('not signed in');
  return session;
}

function cookies(req: IncomingMessage): Record<string, string> {
  const values: Record<string, string> = {};
  for (const item of (header(req, 'cookie') ?? '').split(';')) {
    const index = item.indexOf('=');
    if (index < 1) continue;
    values[item.slice(0, index).trim()] = decodeURIComponent(item.slice(index + 1).trim());
  }
  return values;
}

function sessionCookie(value: string, req: IncomingMessage, trustedProxyHops: number): string {
  const forwardedProto = trustedProxyHops > 0 ? header(req, 'x-forwarded-proto') : undefined;
  const secure = forwardedProto?.split(',').at(-1)?.trim() === 'https';
  return [
    `mizuki_session=${encodeURIComponent(value)}`,
    'Path=/',
    'HttpOnly',
    'SameSite=Lax',
    'Max-Age=604800',
    ...(secure ? ['Secure'] : []),
  ].join('; ');
}

class ConcurrentPaymentReservation extends Error {
  constructor(readonly job: Job) {
    super('payment is already being settled');
  }
}

class InvalidRequestBodyError extends Error {}

export class OperatorAdmissionError extends Error {}

export class SerialGate {
  private tail = Promise.resolve();

  async run<T>(operation: () => Promise<T>): Promise<T> {
    const previous = this.tail;
    let release!: () => void;
    this.tail = new Promise<void>((resolve) => {
      release = resolve;
    });
    await previous;
    try {
      return await operation();
    } finally {
      release();
    }
  }
}
