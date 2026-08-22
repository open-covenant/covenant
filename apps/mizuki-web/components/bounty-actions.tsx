'use client';

import { useEffect, useState } from 'react';
import { useRouter } from 'next/navigation';
import { GithubClaimButton } from './github-claim-button';
import type { BountyState } from '@/lib/types';

type SessionState = 'loading' | 'anonymous' | 'claimant' | 'other';

export function BountyActions({
  bountyId,
  state,
  claimantLogin,
  pullRequestUrl,
  hasDispute,
}: {
  bountyId: string;
  state: BountyState;
  claimantLogin: string;
  pullRequestUrl?: string;
  hasDispute: boolean;
}) {
  const router = useRouter();
  const [session, setSession] = useState<SessionState>('loading');
  const [prUrl, setPrUrl] = useState(pullRequestUrl ?? '');
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState<'pr' | 'dispute' | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    void fetch('/api/mizuki/v1/auth/session', {
      credentials: 'include',
      signal: controller.signal,
      cache: 'no-store',
    })
      .then(async (response) => {
        if (response.status === 401) return setSession('anonymous');
        const body = (await response.json()) as { contributor?: { githubLogin?: string } };
        const login = body.contributor?.githubLogin;
        setSession(login?.toLowerCase() === claimantLogin.toLowerCase() ? 'claimant' : 'other');
      })
      .catch((cause) => {
        if (cause instanceof DOMException && cause.name === 'AbortError') return;
        setSession('anonymous');
      });
    return () => controller.abort();
  }, [claimantLogin]);

  async function submitPullRequest(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    await mutate('pr', { pullRequestUrl: prUrl.trim() }, 'Pull request submitted for review.');
  }

  async function openDispute(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    await mutate('dispute', { reason: reason.trim() }, 'Dispute opened. Escrow is frozen.');
  }

  async function mutate(kind: 'pr' | 'dispute', body: Record<string, string>, message: string) {
    setBusy(kind);
    setError(null);
    setSuccess(null);
    try {
      const response = await fetch(
        `/api/mizuki/v1/bounties/${encodeURIComponent(bountyId)}/${kind === 'pr' ? 'pr' : 'disputes'}`,
        {
          method: 'POST',
          credentials: 'include',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(body),
        },
      );
      const result = (await response.json().catch(() => ({}))) as { error?: string };
      if (!response.ok) throw new Error(result.error ?? `Request failed (${response.status})`);
      setSuccess(message);
      router.refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : 'The request could not be completed');
    } finally {
      setBusy(null);
    }
  }

  if (session === 'loading') return <p className="claim-unavailable">Checking claim identity…</p>;
  if (session === 'anonymous') {
    return (
      <div className="claimant-actions">
        <p>Sign in as @{claimantLogin} to submit work or open a dispute.</p>
        <GithubClaimButton bountyId={bountyId} />
      </div>
    );
  }
  if (session === 'other') {
    return <p className="claim-unavailable">This work is assigned to @{claimantLogin}.</p>;
  }

  const canSubmit = state === 'claimed' && !pullRequestUrl;
  const canDispute = ['claimed', 'pr_submitted', 'validating'].includes(state) && !hasDispute;
  return (
    <div className="claimant-actions">
      {canSubmit && (
        <form className="claimant-form" onSubmit={submitPullRequest}>
          <label htmlFor="bounty-pr">Draft pull request URL</label>
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
            {busy === 'pr' ? 'Reviewing…' : 'Submit pull request'}
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
            {busy === 'dispute' ? 'Freezing escrow…' : 'Open dispute'}
          </button>
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
