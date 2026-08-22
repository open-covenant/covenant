'use client';

import { useState } from 'react';
import { useRouter } from 'next/navigation';
import type { SolanaSignMessageFeature } from '@solana/wallet-standard-features';
import { bytesToBase64, useStandardWallet } from '@/lib/wallet-standard';
import { truncateAddress } from '@/lib/format';

type Challenge = { challengeId?: string; id?: string; message: string };

async function responseJson(response: Response): Promise<Record<string, unknown>> {
  const value = (await response.json().catch(() => ({}))) as Record<string, unknown>;
  if (!response.ok)
    throw new Error(
      typeof value.error === 'string' ? value.error : `Request failed (${response.status})`,
    );
  return value;
}

export function WalletProof({
  bountyId,
  disabled = false,
}: {
  bountyId: string;
  disabled?: boolean;
}) {
  const router = useRouter();
  const {
    wallets,
    connected,
    connecting,
    error: walletError,
    connect,
  } = useStandardWallet('message');
  const [state, setState] = useState<'idle' | 'challenging' | 'signing' | 'claiming' | 'complete'>(
    'idle',
  );
  const [error, setError] = useState<string | null>(null);

  async function proveAndClaim() {
    if (!connected) return;
    setError(null);
    try {
      setState('challenging');
      const challengeResponse = await fetch(
        `/api/mizuki/v1/bounties/${encodeURIComponent(bountyId)}/wallet-proof`,
        {
          method: 'POST',
          credentials: 'include',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ address: connected.account.address }),
        },
      );
      const challenge = (await responseJson(challengeResponse)) as Challenge;
      if (typeof challenge.message !== 'string' || !challenge.message)
        throw new Error('The API returned an invalid wallet challenge');

      setState('signing');
      const feature = connected.wallet.features[
        'solana:signMessage'
      ] as SolanaSignMessageFeature['solana:signMessage'];
      const [signed] = await feature.signMessage({
        account: connected.account,
        message: new TextEncoder().encode(challenge.message),
      });
      if (!signed) throw new Error('The wallet did not return a signature');

      setState('claiming');
      const claimResponse = await fetch(
        `/api/mizuki/v1/bounties/${encodeURIComponent(bountyId)}/claim`,
        {
          method: 'POST',
          credentials: 'include',
          headers: {
            'content-type': 'application/json',
            'idempotency-key': crypto.randomUUID(),
          },
          body: JSON.stringify({
            challenge_id: challenge.challengeId || challenge.id,
            signature: bytesToBase64(signed.signature),
          }),
        },
      );
      await responseJson(claimResponse);
      setState('complete');
      router.refresh();
    } catch (cause) {
      setState('idle');
      setError(cause instanceof Error ? cause.message : 'The claim could not be completed');
    }
  }

  if (disabled) {
    return <p className="claim-unavailable">This bounty is not accepting new claims.</p>;
  }

  return (
    <div className="wallet-proof">
      <div className="claim-step-heading">
        <span>02</span>
        <div>
          <strong>Prove your payout wallet</strong>
          <p>This signature is free. It cannot move funds or authorize a transaction.</p>
        </div>
      </div>

      {!connected ? (
        wallets.length > 0 ? (
          <div className="wallet-options" aria-label="Compatible Solana wallets">
            {wallets.map((wallet) => (
              <button
                type="button"
                key={wallet.name}
                disabled={Boolean(connecting)}
                onClick={() => void connect(wallet)}
              >
                <span>{wallet.name}</span>
                <span>{connecting === wallet.name ? 'Connecting…' : 'Connect ↗'}</span>
              </button>
            ))}
          </div>
        ) : (
          <div className="wallet-missing">
            <strong>No compatible wallet detected</strong>
            <p>Open this page in a browser with a Wallet Standard-compatible Solana wallet.</p>
          </div>
        )
      ) : (
        <div className="connected-wallet">
          <div>
            <span>Connected account</span>
            <strong>{truncateAddress(connected.account.address, 7)}</strong>
          </div>
          <button
            className="button button-primary"
            type="button"
            onClick={() => void proveAndClaim()}
            disabled={state !== 'idle'}
          >
            {state === 'challenging'
              ? 'Preparing proof…'
              : state === 'signing'
                ? 'Confirm in wallet…'
                : state === 'claiming'
                  ? 'Claiming…'
                  : state === 'complete'
                    ? 'Bounty claimed'
                    : 'Sign proof and claim'}
          </button>
        </div>
      )}
      {(walletError || error) && (
        <p className="form-error" role="alert">
          {error || walletError}
        </p>
      )}
      {state === 'complete' && (
        <p className="form-success">
          Claim accepted. Mizuki has started your immutable 48-hour work window.
        </p>
      )}
    </div>
  );
}
