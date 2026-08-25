'use client';

import { useEffect, useState } from 'react';
import { useRouter } from 'next/navigation';
import { GithubClaimButton } from './github-claim-button';
import type { BountyState } from '@/lib/types';
import { sessionCsrfToken } from '@/lib/workbench-client';

type SessionState = 'loading' | 'anonymous' | 'claimant' | 'other' | 'unavailable';

export function BountyActions({
  bountyId,
  state,
  claimantLogin,
  pullRequestUrl,
  hasDispute,
  returnTo,
  onMutated,
}: {
  bountyId: string;
  state: BountyState;
  claimantLogin: string;
  pullRequestUrl?: string;
  hasDispute: boolean;
  returnTo?: string;
  onMutated?: () => void | Promise<void>;
}) {
  const router = useRouter();
  const [session, setSession] = useState<SessionState>('loading');
  const [prUrl, setPrUrl] = useState(pullRequestUrl ?? '');
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState<'pr' | 'dispute' | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [sessionAttempt, setSessionAttempt] = useState(0);

  useEffect(() => {
    const controller = new AbortController();
    void fetch('/api/mizuki/v1/auth/session', {
      credentials: 'include',
      signal: controller.signal,
      cache: 'no-store',
    })
      .then(async (response) => {
        if (response.status === 401) return setSession('anonymous');
        if (!response.ok) throw new Error('session unavailable');
        const body = (await response.json()) as { contributor?: { githubLogin?: string } };
        const login = body.contributor?.githubLogin;
        if (!login) throw new Error('session response is incomplete');
        setSession(login?.toLowerCase() === claimantLogin.toLowerCase() ? 'claimant' : 'other');
      })
      .catch((cause) => {
        if (cause instanceof DOMException && cause.name === 'AbortError') return;
        setSession('unavailable');
      });
    return () => controller.abort();
  }, [claimantLogin, sessionAttempt]);

  async function submitPullRequest(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    await mutate(
      'pr',
      { pullRequestUrl: prUrl.trim() },
      'Pull request submitted. Mizuki is checking the exact commit, changed files, repository checks, and separate AI review.',
    );
  }

  async function openDispute(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    await mutate(
      'dispute',
      { reason: reason.trim() },
      'Dispute opened. The contributor payout is paused while the evidence is reviewed.',
    );
  }

  async function mutate(kind: 'pr' | 'dispute', body: Record<string, string>, message: string) {
    setBusy(kind);
    setError(null);
    setSuccess(null);
    try {
      const csrfToken = await sessionCsrfToken();
      const response = await fetch(
        `/api/mizuki/v1/bounties/${encodeURIComponent(bountyId)}/${kind === 'pr' ? 'pr' : 'disputes'}`,
        {
          method: 'POST',
          credentials: 'include',
          headers: {
            'content-type': 'application/json',
            'x-mizuki-csrf-token': csrfToken,
          },
          body: JSON.stringify(body),
        },
      );
      await response.json().catch(() => ({}));
      if (!response.ok) {
        setError(actionError(response.status, kind));
        return;
      }
      setSuccess(message);
      if (onMutated) await onMutated();
      else router.refresh();
    } catch {
      setError(actionError(0, kind));
    } finally {
      setBusy(null);
    }
  }

  if (session === 'loading') return <p className="claim-unavailable">Checking GitHub sign-in…</p>;
  if (session === 'unavailable') {
    return (
      <div className="claimant-actions">
        <p>GitHub sign-in status is temporarily unavailable. No bounty state was changed.</p>
        <button
          className="button button-secondary"
          type="button"
          onClick={() => {
            setSession('loading');
            setSessionAttempt((attempt) => attempt + 1);
          }}
        >
          Try again
        </button>
      </div>
    );
  }
  if (session === 'anonymous') {
    return (
      <div className="claimant-actions">
        <p>Sign in as @{claimantLogin} to submit work or open a dispute.</p>
        <GithubClaimButton bountyId={bountyId} returnTo={returnTo} />
      </div>
    );
  }
  if (session === 'other') {
    return (
      <p className="claim-unavailable">This bounty is currently assigned to @{claimantLogin}.</p>
    );
  }

  const canSubmit = state === 'claimed' && !pullRequestUrl;
  const canDispute = ['claimed', 'pr_submitted', 'validating'].includes(state) && !hasDispute;
  return (
    <div className="claimant-actions">
      {canSubmit && (
        <form className="claimant-form" onSubmit={submitPullRequest}>
          <label htmlFor="bounty-pr">Pull request URL</label>
          <input
            id="bounty-pr"
            type="url"
            value={prUrl}
            onChange={(event) => setPrUrl(event.target.value)}
            placeholder="https://github.com/owner/repository/pull/123"
            pattern="https://github\.com/[^/]+/[^/]+/pull/[0-9]+.*"
            required
          />
          <button className="button button-primary" type="submit" disabled={busy !== null}>
            {busy === 'pr' ? 'Submitting for review…' : 'Submit pull request'}
          </button>
        </form>
      )}
      {pullRequestUrl && (
        <a className="receipt-link" href={pullRequestUrl} target="_blank" rel="noreferrer">
          <span>Submitted pull request</span>
          <strong>Inspect on GitHub ↗</strong>
        </a>
      )}
      {canDispute && (
        <form className="claimant-form dispute-form" onSubmit={openDispute}>
          <label htmlFor="bounty-dispute">Dispute reason</label>
          <textarea
            id="bounty-dispute"
            value={reason}
            onChange={(event) => setReason(event.target.value)}
            minLength={10}
            maxLength={2000}
            placeholder="Explain the disputed scope, review, or payout evidence."
            required
          />
          <button className="button button-secondary" type="submit" disabled={busy !== null}>
            {busy === 'dispute' ? 'Opening dispute…' : 'Open dispute'}
          </button>
          <p>
            Opening a dispute pauses payout until a documented decision to release the contributor
            payout or return the SOL escrow is recorded.
          </p>
        </form>
      )}
      {error && (
        <p className="form-error" role="alert">
          {error}
        </p>
      )}
      {success && <p className="form-success">{success}</p>}
    </div>
  );
}

function actionError(status: number, kind: 'pr' | 'dispute'): string {
  if (status === 401) return 'Sign in with the assigned GitHub account and try again.';
  if (status === 409) return 'This bounty changed. Refresh the page before trying again.';
  return kind === 'pr'
    ? 'We could not submit the pull request for review. Confirm the URL and try again.'
    : 'We could not open the dispute. Refresh the page and try again.';
}
