import { timingSafeEqual } from 'node:crypto';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { ActivityStreams, PublicAdmission, RateLimitError, requestScheme } from './admission.js';
import type { ContributorAuth } from './auth.js';
import type { BountyService } from './bounties.js';
import type { Config } from './config.js';
import { dashboard } from './dashboard.js';
import { JobProcessor } from './executor.js';
import { GithubAccessError, GithubClient, GithubReadinessError } from './github.js';
import { metrics, prometheus } from './metrics.js';
import {
  isPublicBounty,
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
import { assertLiabilityMatchesPayment, recoverSettlement } from './settlement-recovery.js';
import { StateConflictError, type AccountJobsPage, type MizukiStore } from './store.js';
import { GithubWebhookHandler, verifyGithubWebhook } from './webhooks.js';
import type { Job, RepositoryAdmissionReceipt } from './types.js';
import { Payments, USDC_DECIMALS, USDC_MAINNET, paymentRequiredHeader } from './x402.js';
import {
  assertRefundCapacity,
  refundLiabilityCommitment,
  repositoryAdmissionBinding,
  PolicyRequestError,
  RefundCapacityError,
  type PaymentPolicy,
  type PolicyReadiness,
} from './policy-client.js';
import type { ServiceReadiness } from './readiness.js';

const MAX_POSTGRES_INTEGER = 2_147_483_647;

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
      if (parts[0] === 'v1' && parts[1] === 'admin') {
        res.setHeader('cache-control', 'private, no-store');
      }
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
      if (req.method === 'GET' && url.pathname === '/internal/mizuki/functional-readiness') {
        res.setHeader('cache-control', 'no-store');
        if (!admin(req, deps.config.releaseProbeToken)) {
          return json(res, 401, { error: 'unauthorized' });
        }
        const report = await deps.readiness.checkApplication();
        if (!report.ready) return json(res, 503, { status: 'unavailable' });
        return json(res, 200, {
          status: 'ok',
          service: 'mizuki-api',
          checks: {
            database: 'ok',
            policySigner: 'ok',
            codingGateway: 'ok',
            settlement: 'ok',
          },
        });
      }
      if (req.method === 'GET' && url.pathname === '/deployz') {
        res.setHeader('cache-control', 'no-store');
        try {
          const controls = await deps.store.operatorControls();
          const closed = !controls.intakeEnabled && !controls.claimsEnabled;
          if (closed) return json(res, 200, { ok: true });
          if (deps.config.runtimeRole === 'shadow') return json(res, 503, { ok: false });

          const report = await deps.readiness.check();
          return json(res, report.ready ? 200 : 503, { ok: report.ready });
        } catch {
          return json(res, 503, { ok: false });
        }
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
          'set-cookie': sessionCookie(
            result.session,
            req,
            deps.config.trustedProxyHops ?? 0,
            deps.config.webProxySecret,
          ),
          'cache-control': 'no-store',
        });
        res.end();
        return;
      }
      if (req.method === 'GET' && url.pathname === '/v1/auth/session') {
        const session = deps.auth.session?.(cookies(req).mizuki_session);
        if (!session) return json(res, 401, { error: 'not signed in' });
        const contributor = await deps.store.contributor(session.githubId);
        return json(res, 200, { contributor });
      }
      if (req.method === 'POST' && url.pathname === '/v1/auth/logout') {
        res.setHeader(
          'set-cookie',
          expiredSessionCookie(req, deps.config.trustedProxyHops ?? 0, deps.config.webProxySecret),
        );
        res.setHeader('cache-control', 'private, no-store');
        return json(res, 200, { ok: true });
      }
      if (req.method === 'GET' && url.pathname === '/v1/account') {
        res.setHeader('cache-control', 'private, no-store');
        const session = requireSession(req, deps.auth);
        const account = await deps.store.contributor(session.githubId);
        if (!account) return json(res, 401, { error: 'not signed in' });
        return json(res, 200, {
          account: {
            githubId: account.githubId,
            githubLogin: account.githubLogin,
            ...(account.wallet ? { wallet: account.wallet } : {}),
            ...(account.walletVerifiedAt ? { walletVerifiedAt: account.walletVerifiedAt } : {}),
          },
        });
      }
      if (req.method === 'GET' && url.pathname === '/v1/account/jobs') {
        res.setHeader('cache-control', 'private, no-store');
        const session = requireSession(req, deps.auth);
        const page = await deps.store.jobsForAccount(session.githubId, 100);
        return json(res, 200, {
          jobs: page.jobs.map(publicJob),
          limit: page.limit,
          truncated: page.truncated,
        });
      }
      if (req.method === 'GET' && url.pathname === '/v1/account/billing') {
        res.setHeader('cache-control', 'private, no-store');
        const session = requireSession(req, deps.auth);
        const page = await deps.store.jobsForAccount(session.githubId, 1_000);
        return json(res, 200, accountBilling(deps.config.paymentMode, page));
      }
      if (req.method === 'GET' && url.pathname === '/v1/account/bounties') {
        res.setHeader('cache-control', 'private, no-store');
        const session = requireSession(req, deps.auth);
        const page = await deps.store.bountiesForAccount(session.githubId, 100);
        return json(res, 200, {
          bounties: await Promise.all(
            page.bounties.map((bounty) => publicBounty(deps.store, bounty)),
          ),
          limit: page.limit,
          truncated: page.truncated,
        });
      }
      if (req.method === 'GET' && url.pathname === '/v1/account/repositories') {
        res.setHeader('cache-control', 'private, no-store');
        const session = requireSession(req, deps.auth);
        admission.consumeAccount('account_repositories', req, session.githubId);
        const page = await deps.store.repositoriesForAccount(session.githubId, 25);
        const repositories = await mapConcurrent(page.repositories, 4, async (saved) => {
          const checkedAt = new Date().toISOString();
          try {
            const [repository, policy] = await Promise.all([
              deps.github.repositoryMetadataForMaintainer(
                saved.owner,
                saved.repo,
                session.githubLogin,
              ),
              repositoryPolicyReadiness(deps, saved.repository),
            ]);
            const blockers = policy.status === 'ready' ? [] : [policy.reason];
            return {
              owner: repository.owner,
              repo: repository.repo,
              repository: repository.repository,
              defaultBranch: repository.defaultBranch,
              permission: repository.permission,
              core: { status: 'ready' as const },
              policy,
              validationCommands: [],
              checkedAt,
              readyForWork: blockers.length === 0,
              blockers,
            };
          } catch (cause) {
            return {
              owner: saved.owner,
              repo: saved.repo,
              repository: saved.repository,
              defaultBranch: '',
              permission: null,
              core: {
                status:
                  cause instanceof GithubAccessError
                    ? ('action_required' as const)
                    : ('unavailable' as const),
              },
              policy: { status: 'unknown' as const },
              validationCommands: [],
              checkedAt,
              readyForWork: false,
              blockers: [accountRepositoryBlocker(cause)],
            };
          }
        });
        return json(res, 200, {
          repositories,
          limit: page.limit,
          truncated: page.truncated,
        });
      }
      if (req.method === 'POST' && url.pathname === '/v1/account/repositories') {
        const session = requireSession(req, deps.auth);
        admission.consumeAccount('repository_connect', req, session.githubId);
        const body = await bodyJson<{
          repository?: unknown;
          owner?: unknown;
          repo?: unknown;
        }>(req);
        const { owner, repo } = accountRepositoryInput(body);
        const repository = await deps.github.repositoryMetadataForMaintainer(
          owner,
          repo,
          session.githubLogin,
        );
        await deps.store.linkAccountRepository(session.githubId, repository.owner, repository.repo);
        const policy = await repositoryPolicyReadiness(deps, repository.repository);
        const blockers = policy.status === 'ready' ? [] : [policy.reason];
        const checkedAt = new Date().toISOString();
        res.setHeader('cache-control', 'private, no-store');
        return json(res, 201, {
          repository: {
            owner: repository.owner,
            repo: repository.repo,
            repository: repository.repository,
            defaultBranch: repository.defaultBranch,
            permission: repository.permission,
            core: { status: 'ready' },
            policy,
            validationCommands: [],
            checkedAt,
            readyForWork: blockers.length === 0,
            blockers,
          },
        });
      }
      if (req.method === 'POST' && url.pathname === '/v1/preflights') {
        const session = requireSession(req, deps.auth);
        admission.consumeAccount('preflight', req, session.githubId);
        const body = await bodyJson<{ github_issue_url?: unknown }>(req);
        if (typeof body.github_issue_url !== 'string') {
          return json(res, 400, { error: 'github_issue_url is required' });
        }
        const inspected = await deps.github.preflightIssue(
          body.github_issue_url,
          session.githubLogin,
        );
        const policy = await repositoryPolicyReadiness(deps, inspected.repository);
        const blockers = [...inspected.blockers];
        if (policy.status !== 'ready') blockers.push(policy.reason);
        if (inspected.maintainer.verified) {
          await deps.store.linkAccountRepository(session.githubId, inspected.owner, inspected.repo);
        }
        const checkedAt = new Date().toISOString();
        return json(res, 200, {
          repository: {
            owner: inspected.owner,
            repo: inspected.repo,
            repository: inspected.repository,
            defaultBranch: inspected.defaultBranch,
          },
          issue: inspected.issue,
          checks: {
            core: inspected.core,
            policy,
            maintainer: {
              status: inspected.maintainer.verified
                ? 'ready'
                : inspected.maintainer.unavailable
                  ? 'unavailable'
                  : 'unverified',
              ...(inspected.maintainer.permission
                ? { permission: inspected.maintainer.permission }
                : {}),
            },
            authorization: {
              status: inspected.issue.authorized
                ? 'ready'
                : inspected.issue.authorizationUnavailable
                  ? 'unavailable'
                  : 'action_required',
            },
            eligibility: {
              status: inspected.issue.scopeEligible ? 'ready' : 'action_required',
            },
          },
          blockers,
          class: inspected.issue.class,
          priceAtomic: inspected.issue.priceAtomic,
          maxFiles: inspected.issue.maxFiles,
          validationCommands: inspected.issue.validationCommands,
          checkedAt,
          readyForWork: blockers.length === 0,
        });
      }
      if (
        req.method === 'GET' &&
        parts[0] === 'v1' &&
        parts[1] === 'repositories' &&
        parts[2] &&
        parts[3] &&
        parts[4] === 'issues'
      ) {
        const session = requireSession(req, deps.auth);
        admission.consumeAccount('repository_issues', req, session.githubId);
        const result = await deps.github.issuesForMaintainer(
          parts[2],
          parts[3],
          session.githubLogin,
        );
        res.setHeader('cache-control', 'private, no-store');
        return json(res, 200, { issues: result.issues });
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
        const bounties: Awaited<ReturnType<typeof publicBounty>>[] = [];
        for (const bounty of await deps.store.bountiesList()) {
          if (await isPublicBounty(deps.store, bounty)) {
            bounties.push(await publicBounty(deps.store, bounty));
          }
        }
        return json(res, 200, { bounties });
      }
      if (req.method === 'GET' && parts[0] === 'v1' && parts[1] === 'bounties' && parts[2]) {
        const bounty = await deps.store.bounty(parts[2]);
        if (!bounty || !(await isPublicBounty(deps.store, bounty))) {
          return json(res, 404, { error: 'bounty not found' });
        }
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
        if (!bounty || !(await isPublicBounty(deps.store, bounty))) {
          return json(res, 404, { error: 'bounty not found' });
        }
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
          await assertOperatorControlOpen(deps.store, 'claims', deps.readiness);
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
          await assertOperatorControlOpen(deps.store, 'claims', deps.readiness);
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
        await assertOperatorControlOpen(deps.store, 'intake', deps.readiness);
        const issue = await deps.github.issue(body.github_issue_url);
        const session = deps.auth.session?.(cookies(req).mizuki_session);
        let accountRepository:
          | Awaited<ReturnType<GithubClient['repositoryMetadataForMaintainer']>>
          | undefined;
        if (session) {
          try {
            accountRepository = await deps.github.repositoryMetadataForMaintainer(
              issue.owner,
              issue.repo,
              session.githubLogin,
            );
          } catch (cause) {
            if (!(cause instanceof GithubAccessError)) throw cause;
            accountRepository = undefined;
          }
        }
        const result = await deps.paymentAdmission.run(async () => {
          await assertOperatorControlOpen(deps.store, 'intake', deps.readiness);
          const quote = await deps.store.saveQuote(createQuote(issue));
          if (session && accountRepository) {
            await deps.store.linkQuoteToAccount(quote.id, session.githubId);
            await deps.store.linkAccountRepository(
              session.githubId,
              accountRepository.owner,
              accountRepository.repo,
            );
          }
          return { ...quote, payment: await deps.payments.challenge(quote) };
        });
        return json(res, 201, result);
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
        let repositoryAdmission: RepositoryAdmissionReceipt | undefined;
        let payment;
        try {
          const settle = () =>
            deps.payments.settle(quote, paymentSignature, async (authorized) => {
              if (deps.config.paymentMode === 'live') {
                if (!authorized.signature) {
                  throw new Error('verified payment authorization is unavailable');
                }
                repositoryAdmission = await deps.policy.createRepositoryAdmission(
                  repositoryAdmissionBinding(quote, key, authorized.signature),
                  authorized.signature,
                );
              }
              const reservation = await deps.store.createJob(
                quote,
                authorized,
                key,
                repositoryAdmission,
              );
              repositoryAdmission = reservation.job.repositoryAdmission;
              if (deps.config.paymentMode === 'live' && !repositoryAdmission) {
                throw new Error('durable repository admission was not persisted');
              }
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
            await assertOperatorControlOpen(deps.store, 'intake', deps.readiness);
            const result = await settle();
            if (!result.ok || deps.config.paymentMode !== 'live') return result;
            if (!pendingJobId) throw new Error('payment authorization was not persisted');
            if (!repositoryAdmission) {
              throw new Error('durable repository admission is unavailable');
            }
            await deps.store.patchJob(pendingJobId, { payment: result.payment });
            const liability = await deps.policy.registerRefundLiability(
              pendingJobId,
              result.payment.transaction,
              refundLiabilityCommitment(quote),
              repositoryAdmission,
            );
            assertLiabilityMatchesPayment(
              liability,
              pendingJobId,
              result.payment,
              quote,
              repositoryAdmission,
            );
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
        res.setHeader('cache-control', 'no-store');
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
      if (url.pathname === '/v1/admin/admission/audit' && req.method === 'GET') {
        if (!admin(req, deps.config.adminToken)) return json(res, 401, { error: 'unauthorized' });
        return json(res, 200, await deps.store.operatorControlsAudit());
      }
      if (url.pathname === '/v1/admin/admission' && req.method === 'POST') {
        if (!admin(req, deps.config.adminToken)) return json(res, 401, { error: 'unauthorized' });
        const body = await bodyJson<{
          expectedRevision?: unknown;
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
          typeof body.expectedRevision !== 'number' ||
          !Number.isSafeInteger(body.expectedRevision) ||
          body.expectedRevision < 0 ||
          body.expectedRevision > MAX_POSTGRES_INTEGER
        ) {
          return json(res, 400, {
            error: `expectedRevision must be an integer between 0 and ${MAX_POSTGRES_INTEGER}`,
          });
        }
        const expectedRevision = body.expectedRevision;
        if (
          typeof body.reason !== 'string' ||
          body.reason.trim().length < 10 ||
          body.reason.trim().length > 500
        ) {
          return json(res, 400, { error: 'reason must contain 10-500 characters' });
        }
        const reason = body.reason.trim();
        if (
          deps.config.runtimeRole === 'shadow' &&
          (body.intakeEnabled === true || body.claimsEnabled === true)
        ) {
          return json(res, 409, { error: 'shadow admission is permanently closed' });
        }
        const controls = await deps.paymentAdmission.run(async () => {
          const current = await readOperatorControls(deps.store);
          const opensAdmission = operatorControlTransitionOpens(current, body);
          if (
            expectedRevision > current.revision ||
            (expectedRevision < current.revision && opensAdmission)
          ) {
            throw new StateConflictError(
              `expected operator admission revision ${expectedRevision}; current revision is ${current.revision}`,
            );
          }
          if (opensAdmission) await assertServiceReady(deps.readiness);
          return deps.store.updateOperatorControls({
            expectedRevision,
            ...(typeof body.intakeEnabled === 'boolean'
              ? { intakeEnabled: body.intakeEnabled }
              : {}),
            ...(typeof body.claimsEnabled === 'boolean'
              ? { claimsEnabled: body.claimsEnabled }
              : {}),
            reason,
            updatedBy: 'operator',
          });
        });
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
          return recoverSettlement(job, {
            paymentMode: deps.config.paymentMode,
            payTo: deps.config.payTo,
            store: deps.store,
            payments: deps.payments,
            policy: deps.policy,
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
      const status =
        cause instanceof GithubAccessError
          ? 403
          : cause instanceof GithubReadinessError
            ? 503
            : /not signed in|unauthorized/i.test(message)
              ? 401
              : cause instanceof InvalidRequestBodyError
                ? 400
                : cause instanceof StateConflictError
                  ? 409
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
        cause instanceof GithubReadinessError
          ? cause.message
          : status < 500
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

function operatorControlTransitionOpens(
  current: { intakeEnabled: boolean; claimsEnabled: boolean },
  patch: { intakeEnabled?: unknown; claimsEnabled?: unknown },
): boolean {
  return (
    (patch.intakeEnabled === true && !current.intakeEnabled) ||
    (patch.claimsEnabled === true && !current.claimsEnabled)
  );
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
    escrowAuthority: deps.config.escrowRefundTo,
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
      let published = false;
      for (const event of pending) {
        const value = await publicActivity(store, event);
        lastId = event.id;
        if (!value) continue;
        res.write(`id: ${event.id}\nevent: activity\ndata: ${JSON.stringify(value)}\n\n`);
        published = true;
      }
      if (published) lastActivityAt = Date.now();
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
  readiness?: ServiceReadiness,
): Promise<void> {
  const controls = await readOperatorControls(store);
  const enabled = control === 'intake' ? controls.intakeEnabled : controls.claimsEnabled;
  if (!enabled) throw new OperatorAdmissionError(`${control} is paused by the operator`);
  if (readiness) await assertServiceReady(readiness);
}

async function assertServiceReady(readiness: ServiceReadiness): Promise<void> {
  const report = await readiness.check();
  if (!report.ready) throw new OperatorAdmissionError('service dependencies are not ready');
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

function accountRepositoryInput(body: { repository?: unknown; owner?: unknown; repo?: unknown }): {
  owner: string;
  repo: string;
} {
  if (typeof body.owner === 'string' && typeof body.repo === 'string') {
    return validatedRepository(body.owner, body.repo);
  }
  if (typeof body.repository !== 'string') {
    throw new InvalidRequestBodyError('repository or owner and repo are required');
  }
  const value = body.repository.trim();
  const url = value.match(/^https:\/\/github\.com\/([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)\/?$/i);
  if (url) return validatedRepository(url[1]!, url[2]!);
  const identity = value.match(/^([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)$/);
  if (!identity) throw new InvalidRequestBodyError('repository must be owner/repo');
  return validatedRepository(identity[1]!, identity[2]!);
}

function validatedRepository(owner: string, repo: string): { owner: string; repo: string } {
  const segment = /^[A-Za-z0-9_.-]{1,100}$/;
  if (!segment.test(owner) || !segment.test(repo)) {
    throw new InvalidRequestBodyError('repository identity is invalid');
  }
  return { owner, repo };
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

function sessionCookie(
  value: string,
  req: IncomingMessage,
  trustedProxyHops: number,
  webProxySecret: string | undefined,
): string {
  const secure = requestScheme(req, trustedProxyHops, webProxySecret) === 'https';
  return [
    `mizuki_session=${encodeURIComponent(value)}`,
    'Path=/',
    'HttpOnly',
    'SameSite=Lax',
    'Max-Age=604800',
    ...(secure ? ['Secure'] : []),
  ].join('; ');
}

function expiredSessionCookie(
  req: IncomingMessage,
  trustedProxyHops: number,
  webProxySecret: string | undefined,
): string {
  const secure = requestScheme(req, trustedProxyHops, webProxySecret) === 'https';
  return [
    'mizuki_session=',
    'Path=/',
    'HttpOnly',
    'SameSite=Lax',
    'Max-Age=0',
    'Expires=Thu, 01 Jan 1970 00:00:00 GMT',
    ...(secure ? ['Secure'] : []),
  ].join('; ');
}

function accountBilling(mode: Config['paymentMode'], page: AccountJobsPage) {
  const { jobs } = page;
  const paid = jobs.filter(
    (job) => job.state !== 'settlement_pending' && job.payment.transaction !== 'pending',
  );
  const refunded = paid.filter((job) => job.state === 'refunded' && job.refundTransaction);
  const delivered = paid.filter((job) => job.state === 'delivered');
  const protectedJobs = paid.filter((job) => !['delivered', 'refunded'].includes(job.state));
  const transactions = paid
    .flatMap((job) => [
      {
        jobId: job.id,
        type: 'payment' as const,
        status: 'finalized' as const,
        amountAtomic: job.payment.amountAtomic,
        transaction: job.payment.transaction,
        createdAt: job.createdAt,
      },
      ...(job.refundTransaction
        ? [
            {
              jobId: job.id,
              type: 'refund' as const,
              status: 'finalized' as const,
              amountAtomic: job.payment.amountAtomic,
              transaction: job.refundTransaction,
              createdAt: job.updatedAt,
            },
          ]
        : job.state === 'refund_pending'
          ? [
              {
                jobId: job.id,
                type: 'refund' as const,
                status: 'pending' as const,
                amountAtomic: job.payment.amountAtomic,
                transaction: null,
                createdAt: job.updatedAt,
              },
            ]
          : []),
    ])
    .sort((left, right) => right.createdAt.localeCompare(left.createdAt));
  return {
    mode,
    asset: 'USDC',
    decimals: USDC_DECIMALS,
    limit: page.limit,
    truncated: page.truncated,
    totalsScope: page.truncated ? ('latest_jobs' as const) : ('account_lifetime' as const),
    totals: {
      paidAtomic: sumJobAmounts(paid),
      refundedAtomic: sumJobAmounts(refunded),
      deliveredAtomic: sumJobAmounts(delivered),
      protectedAtomic: sumJobAmounts(protectedJobs),
    },
    transactions,
  };
}

function sumJobAmounts(jobs: Job[]): string {
  return jobs.reduce((total, job) => total + BigInt(job.payment.amountAtomic), 0n).toString();
}

async function repositoryPolicyReadiness(
  deps: Pick<AppDependencies, 'policy'>,
  repository: string,
): Promise<
  | { status: 'ready'; verifierAppId: string; installationId: number }
  | { status: 'action_required'; reason: string }
  | { status: 'unavailable'; reason: string }
> {
  try {
    const readiness = await deps.policy.assertRepositoryReady(repository);
    return {
      status: 'ready',
      verifierAppId: readiness.verifierAppId,
      installationId: readiness.installationId,
    };
  } catch (cause) {
    if (cause instanceof PolicyRequestError && cause.code === 'github_app_not_installed') {
      return {
        status: 'action_required',
        reason: 'Install the read-only policy verifier on this repository.',
      };
    }
    return {
      status: 'unavailable',
      reason: 'The read-only policy verifier is temporarily unavailable.',
    };
  }
}

function accountRepositoryBlocker(cause: unknown): string {
  if (cause instanceof GithubAccessError) return cause.message;
  if (cause instanceof GithubReadinessError) return cause.message;
  return 'Repository readiness could not be verified. Try again shortly.';
}

async function mapConcurrent<T, R>(
  values: T[],
  concurrency: number,
  operation: (value: T) => Promise<R>,
): Promise<R[]> {
  const results = new Array<R>(values.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(concurrency, values.length) }, async () => {
    while (next < values.length) {
      const index = next;
      next += 1;
      results[index] = await operation(values[index]!);
    }
  });
  await Promise.all(workers);
  return results;
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

  constructor(private readonly exclusive?: <T>(operation: () => Promise<T>) => Promise<T>) {}

  async run<T>(operation: () => Promise<T>): Promise<T> {
    const previous = this.tail;
    let release!: () => void;
    this.tail = new Promise<void>((resolve) => {
      release = resolve;
    });
    await previous;
    try {
      return this.exclusive ? await this.exclusive(operation) : await operation();
    } finally {
      release();
    }
  }
}
